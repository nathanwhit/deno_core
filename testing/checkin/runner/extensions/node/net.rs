use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ExternalOpsTracker;
use deno_core::GarbageCollected;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ToV8;
use deno_core::convert::Smi;
use deno_core::convert::Uint8Array;
use deno_core::error::JsError;
use deno_core::op2;
use deno_core::serde;
use deno_core::v8;
use deno_core::v8::cppgc::Traced;
use deno_error::JsErrorBox;
use std::cell::RefCell;
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use crate::checkin::runner::extensions::node::ScopeHolder;

use super::Constructors;

fn is_ipv4(s: &str) -> bool {
  std::net::Ipv4Addr::from_str(s).is_ok()
}

fn is_ipv6(s: &str) -> bool {
  std::net::Ipv6Addr::from_str(s).is_ok()
}

fn is_ip(s: &str) -> u8 {
  match std::net::IpAddr::from_str(s) {
    Ok(std::net::IpAddr::V4(_)) => 4,
    Ok(std::net::IpAddr::V6(_)) => 6,
    Err(_) => 0,
  }
}

#[inline(always)]
fn map_valid_utf8<R>(
  scope: &mut v8::Isolate,
  string: v8::Local<v8::String>,
  f: impl FnOnce(&str) -> R,
) -> Option<R> {
  let view = v8::ValueView::new(scope, string);
  match view.data() {
    v8::ValueViewData::OneByte(onebyte) => {
      std::str::from_utf8(onebyte).map(f).ok()
    }
    v8::ValueViewData::TwoByte(items) => {
      if !string.is_external_twobyte() {
        return None;
      }
      String::from_utf16(items).map(|s| f(&s)).ok()
    }
  }
}

#[op2(fast)]
pub fn op_is_ipv4(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> bool {
  map_valid_utf8(scope, ip, is_ipv4).unwrap_or(false)
}

#[op2(fast)]
pub fn op_is_ipv6(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> bool {
  map_valid_utf8(scope, ip, is_ipv6).unwrap_or(false)
}

#[op2(fast)]
#[smi]
pub fn op_is_ip(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> u8 {
  map_valid_utf8(scope, ip, is_ip).unwrap_or(0)
}

struct ShouldReadState {
  read_once: AtomicBool,
  should_read: AtomicBool,
  notify: tokio::sync::Notify,
}
impl ShouldReadState {
  fn new() -> Self {
    Self {
      read_once: AtomicBool::new(false),
      should_read: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
    }
  }
}
impl ShouldReadState {
  fn set_should_read(&self) {
    if self
      .should_read
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return;
    }
    self.notify.notify_waiters();
  }
  fn swap_read_once(&self) -> bool {
    self
      .read_once
      .swap(true, std::sync::atomic::Ordering::Relaxed)
  }
  fn clear_should_read(&self) {
    self
      .should_read
      .store(false, std::sync::atomic::Ordering::Relaxed);
  }
  async fn wait_for_should_read(&self) {
    if self.should_read.load(std::sync::atomic::Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

struct ConnectedState {
  connected: AtomicBool,
  notify: tokio::sync::Notify,
}
impl ConnectedState {
  fn new() -> Self {
    Self {
      connected: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
    }
  }
}
impl ConnectedState {
  fn set_connected(&self, connected: bool) {
    self
      .connected
      .store(connected, std::sync::atomic::Ordering::Relaxed);
    self.notify.notify_waiters();
  }

  async fn wait_for_connected(&self) {
    if self.connected.load(std::sync::atomic::Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

struct RefTrackerInner {
  ops_tracker: ExternalOpsTracker,
  refed: AtomicBool,
}

#[derive(Clone)]
struct RefTracker(Arc<RefTrackerInner>);

impl RefTracker {
  fn new(ops_tracker: ExternalOpsTracker) -> Self {
    Self(Arc::new(RefTrackerInner {
      ops_tracker,
      refed: AtomicBool::new(false),
    }))
  }

  fn ref_(&self) {
    if !self
      .0
      .refed
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      self.0.ops_tracker.ref_op();
    }
  }

  fn unref(&self) {
    if self
      .0
      .refed
      .swap(false, std::sync::atomic::Ordering::Relaxed)
    {
      self.0.ops_tracker.unref_op();
    }
  }
}

struct SocketCbInner {
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
  read: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedReadHalf>>>,
  push_func: Rc<v8::TracedReference<v8::Function>>,
  host: RefCell<Option<String>>,
  port: RefCell<Option<u16>>,
  this: Rc<v8::TracedReference<v8::Object>>,

  ref_tracker: RefTracker,
  cancel: Rc<CancelHandle>,
  connected: Rc<ConnectedState>,
  scope_holder: ScopeHolder,

  should_read: Rc<ShouldReadState>,

  emit_func: Rc<v8::TracedReference<v8::Function>>,
  on_event_func: Rc<v8::TracedReference<v8::Function>>,
}

pub struct SocketCb {
  inner: Rc<SocketCbInner>,
}

unsafe impl deno_core::GarbageCollected for SocketCb {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.inner.push_func.trace(visitor);
    self.inner.this.trace(visitor);
    self.inner.emit_func.trace(visitor);
    self.inner.on_event_func.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"SocketCb"
  }
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", crate = "serde")]
struct SocketOptions {
  #[serde(default)]
  allow_half_open: Option<bool>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", crate = "serde")]
struct ConnectOptions {
  host: Option<String>,
  port: Option<u16>,
  path: Option<String>,
  #[serde(default)]
  allow_half_open: Option<bool>,
}

fn normalize_connect_args<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  port_or_options: Option<v8::Local<'s, v8::Value>>,
  host_or_connect_cb: Option<v8::Local<'s, v8::Value>>,
  connect_cb: Option<v8::Local<'s, v8::Function>>,
) -> Result<
  (
    Option<u16>,
    Option<String>,
    Option<v8::Local<'s, v8::Function>>,
    Option<ConnectOptions>,
  ),
  JsErrorBox,
> {
  enum ConnectArgsKind {
    Port,
    Options,
    Path,
  }

  let mut kind = None;
  let mut port = None;
  let mut host = None;
  let mut connect_listener: Option<v8::Local<v8::Function>> = None;
  let mut connect_options = None;
  let invalid_args = || JsErrorBox::type_error("Invalid connect arguments");
  let parse_port = |value: v8::Local<v8::Value>| {
    let port_value = value
      .integer_value(scope)
      .ok_or_else(|| JsErrorBox::type_error("Invalid port"))?;
    let port_value: u16 = port_value
      .try_into()
      .map_err(|_| JsErrorBox::type_error("Invalid port"))?;
    Ok::<_, JsErrorBox>(port_value)
  };

  if let Some(port_or_options) = port_or_options {
    let value = port_or_options;
    if value.is_function() {
      connect_listener = Some(value.cast::<v8::Function>());
    } else if value.is_number() {
      port = Some(parse_port(value)?);
      kind = Some(ConnectArgsKind::Port);
    } else if value.is_string() {
      kind = Some(ConnectArgsKind::Path);
    } else if value.is_object() {
      let options: ConnectOptions = deno_core::serde_v8::from_v8(scope, value)
        .map_err(JsErrorBox::from_err)?;
      if options.path.is_some() {
        kind = Some(ConnectArgsKind::Path);
      } else {
        port = options.port;
        host = options.host.clone();
        kind = Some(ConnectArgsKind::Options);
        connect_options = Some(options);
      }
    } else if !value.is_null_or_undefined() {
      return Err(invalid_args());
    }
  }

  if matches!(kind, Some(ConnectArgsKind::Path)) {
    return Err(JsErrorBox::type_error("IPC connections are not supported"));
  }

  if let Some(host_or_connect_cb) = host_or_connect_cb {
    let value = host_or_connect_cb;
    if value.is_function() {
      connect_listener = Some(value.cast::<v8::Function>());
    } else if value.is_string() {
      if matches!(kind, Some(ConnectArgsKind::Port)) {
        host = Some(value.to_rust_string_lossy(scope));
      } else if !value.is_null_or_undefined() {
        return Err(invalid_args());
      }
    } else if !value.is_null_or_undefined() {
      return Err(invalid_args());
    }
  }

  if connect_listener.is_none() {
    if let Some(connect_cb) = connect_cb {
      connect_listener = Some(connect_cb);
    }
  }

  Ok((port, host, connect_listener, connect_options))
}

#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase", crate = "serde")]
struct DuplexOptions {
  allow_half_open: Option<bool>,
  emit_close: bool,
  auto_destroy: bool,
}

impl SocketCb {
  fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    options: SocketOptions,
    host: Option<String>,
    port: Option<u16>,
  ) -> Result<SocketCb, JsErrorBox> {
    let (ops_tracker, super_cons, spawner) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<Constructors>().clone(),
        op_state.borrow::<deno_core::V8TaskSpawner>().clone(),
      )
    };

    let local_me = v8::Local::new(&scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.duplex);
    let this = Rc::new(v8::TracedReference::new(scope, local_me));

    let duplex_options = DuplexOptions {
      allow_half_open: Some(options.allow_half_open.unwrap_or(false)),
      emit_close: false,
      auto_destroy: true,
    };
    let duplex_options =
      deno_core::serde_v8::to_v8(scope, &duplex_options).unwrap();
    cons
      .call(scope, local_me.into(), &[duplex_options])
      .unwrap();
    let push = internalized(scope, "push");
    let push_func = local_me
      .get(scope, push.into())
      .unwrap()
      .cast::<v8::Function>();
    let push_func = Rc::new(v8::TracedReference::new(scope, push_func));
    let scope_holder = ScopeHolder::new_from_scope(spawner, scope);

    let emit = internalized(scope, "emit");
    let emit_func = local_me
      .get(scope, emit.into())
      .unwrap()
      .cast::<v8::Function>();
    let on_event = internalized(scope, "on");
    let on_event_func = local_me
      .get(scope, on_event.into())
      .unwrap()
      .cast::<v8::Function>();
    let emit_func = Rc::new(v8::TracedReference::new(scope, emit_func));
    let on_event_func = Rc::new(v8::TracedReference::new(scope, on_event_func));
    let cb = SocketCb {
      inner: Rc::new(SocketCbInner {
        write: Rc::new(AsyncRefCell::new(None)),
        read: Rc::new(AsyncRefCell::new(None)),
        push_func,
        cancel: Rc::new(CancelHandle::new()),
        host: RefCell::new(host),
        port: RefCell::new(port),
        this,
        ref_tracker: RefTracker::new(ops_tracker),
        connected: Rc::new(ConnectedState::new()),
        scope_holder,
        should_read: Rc::new(ShouldReadState::new()),
        emit_func,
        on_event_func,
      }),
    };
    Ok(cb)
  }
}

#[op2]
impl SocketCb {
  #[constructor]
  #[cppgc]
  #[reentrant]
  pub fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    #[serde] options: Option<SocketOptions>,
  ) -> Result<SocketCb, JsErrorBox> {
    let options = options.unwrap_or_default();
    SocketCb::new_inner(me, scope, op_state, options, None, None)
  }

  #[async_method]
  pub fn connect<'a, 'b>(
    &self,
    #[global] port_or_options: Option<v8::Global<v8::Value>>,
    #[global] host_or_connect_cb: Option<v8::Global<v8::Value>>,
    #[global] connect_cb: Option<v8::Global<v8::Function>>,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    let inner = self.inner.clone();
    inner.ref_tracker.ref_();
    let inner_for_scope = inner.clone();
    let normalized = inner.scope_holder.with_scope_immediately(move |scope| {
      let port_or_options =
        port_or_options.map(|value| v8::Local::new(scope, &value));
      let host_or_connect_cb =
        host_or_connect_cb.map(|value| v8::Local::new(scope, &value));
      let connect_cb = connect_cb.map(|value| v8::Local::new(scope, &value));
      let (port, host, connect_listener, _connect_options) =
        normalize_connect_args(
          scope,
          port_or_options,
          host_or_connect_cb,
          connect_cb,
        )?;
      if let Some(connect_listener) = connect_listener {
        inner_for_scope.on_event(
          scope,
          &[
            internalized(scope, "connect").into(),
            connect_listener.into(),
          ],
        );
      }

      Ok::<_, JsErrorBox>((port, host))
    });
    async move {
      let (port, host) = normalized?;
      inner.connect_inner(port, host).await
    }
  }

  #[fast]
  #[rename("_read")]
  fn read(&self) {
    self.inner.should_read.set_should_read();

    self.inner.start_read();
  }

  #[fast]
  #[rename("ref")]
  fn r#ref(&self) {
    self.inner.ref_tracker.ref_();
  }

  #[fast]
  fn unref(&self) {
    self.inner.ref_tracker.unref();
  }

  #[rename("_final")]
  fn final_(&self, #[global] cb: v8::Global<v8::Function>) {
    let inner = self.inner.clone();
    deno_core::unsync::spawn(async move {
      inner.connected.wait_for_connected().await;

      let mut write = inner.write.borrow_mut().await;
      let write = write.deref_mut().as_mut().unwrap();
      let result = write.shutdown().await.map_err(JsErrorBox::from_err).err();
      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = inner2.this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, result);
      });
    });
  }

  #[reentrant]
  #[rename("_destroy")]
  fn destroy(
    &self,
    #[global] error: v8::Global<v8::Value>,
    #[global] cb: v8::Global<v8::Function>,
  ) {
    self.inner.cancel.cancel();
    self.inner.should_read.clear_should_read();
    self.inner.connected.set_connected(false);
    let inner = self.inner.clone();

    deno_core::unsync::spawn(async move {
      inner.read.borrow_mut().await.take();
      inner.write.borrow_mut().await.take();
      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = inner2.this.get(scope).unwrap();
        let error = v8::Local::new(scope, &error);
        cb.call(scope, this.into(), &[error]).unwrap();
        inner2.emit_event(scope, &[internalized(scope, "close").into()]);
      });
      inner.ref_tracker.unref();
    });
  }

  #[reentrant]
  #[rename("_write")]
  pub fn write(
    &self,
    #[buffer] data: JsBuffer,
    _encoding: v8::Local<v8::String>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    let inner = self.inner.clone();

    let num_wrote = if let Some(mut write) = inner.write.try_borrow_mut() {
      let write = write.deref_mut().as_mut().unwrap();
      let nwritten = write.try_write(&data);
      match nwritten {
        Ok(nwritten) => nwritten,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
        Err(e) => {
          eprintln!("error writing: {:?}", e);
          return Err(JsErrorBox::from_err(e));
        }
      }
    } else {
      0
    };

    if num_wrote >= data.len() {
      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let local_cb = v8::Local::new(scope, &cb);
        let this = inner2.this.get(scope).unwrap();
        call_write_cb(scope, local_cb, this, None);
      });
      return Ok(());
    }

    deno_core::unsync::spawn(async move {
      let result = inner
        .write
        .borrow_mut()
        .await
        .deref_mut()
        .as_mut()
        .unwrap()
        .write_all(&data[num_wrote..])
        .or_cancel(inner.cancel.clone())
        .await
        .unwrap()
        .map_err(JsErrorBox::from_err)
        .err();

      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = inner2.this.get(scope).unwrap();
        call_write_cb(scope, cb, this, result);
      });
    });

    Ok(())
  }
}

fn call_write_cb(
  scope: &mut v8::PinScope,
  cb: v8::Local<v8::Value>,
  this: v8::Local<v8::Object>,
  result: Option<JsErrorBox>,
) {
  v8::tc_scope!(let scope, scope);
  if let Ok(cb) = cb.try_cast::<v8::Function>() {
    if let Some(result) = result {
      let error = result.to_v8(scope).unwrap();
      let _result = cb.call(scope, this.into(), &[error]).unwrap();
    } else {
      let _result = cb.call(scope, this.into(), &[]);
    }
  } else {
    eprintln!("cb is not a function, it's a {}", cb.type_repr());
  }
  if scope.has_caught() {
    let exception = scope.exception().unwrap();
    let error = JsError::from_v8_exception(scope, exception);
    eprintln!("error: {:?}", error);
  }
}

impl SocketCbInner {
  fn connect_inner(
    self: Rc<Self>,
    port: Option<u16>,
    host: Option<String>,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    async move {
      *self.host.borrow_mut() =
        Some(host.unwrap_or_else(|| "localhost".to_string()));
      *self.port.borrow_mut() = Some(port.unwrap_or(0));
      let stream = tokio::net::TcpStream::connect((
        self.host.borrow().as_deref().unwrap(),
        self.port.borrow().unwrap(),
      ))
      .await
      .map_err(JsErrorBox::from_err)
      .unwrap();
      let (read, write) = stream.into_split();
      *self.write.borrow_mut().await = Some(write);
      *self.read.borrow_mut().await = Some(read);
      self.connected.set_connected(true);
      self.start_read();
      self.scope_holder.with_scope({
        let inner = self.clone();
        move |scope| {
          let zero = Smi(0u8).to_v8(scope).unwrap();
          inner.call_method(scope, "read", &[zero]).unwrap();
          inner.emit_event(scope, &[internalized(scope, "connect").into()]);
        }
      });
      Ok(())
    }
  }

  fn start_read(self: &Rc<Self>) {
    if self.should_read.swap_read_once() {
      return;
    }
    let inner = self.clone();
    let this = inner.this.clone();
    deno_core::unsync::spawn(async move {
      inner.connected.wait_for_connected().await;

      let this = this.clone();
      let mut read = inner.read.borrow_mut().await;
      let read = read.deref_mut().as_mut().unwrap();

      let mut buf = vec![0; 64 * 1024];
      loop {
        let _ = inner
          .should_read
          .wait_for_should_read()
          .or_cancel(inner.cancel.clone())
          .await;
        let nread =
          match read.read(&mut buf).or_cancel(inner.cancel.clone()).await {
            Ok(Ok(nread)) => nread,
            Ok(Err(e)) => {
              eprintln!("error reading: {:?}", e);
              let inner2 = inner.clone();
              inner.scope_holder.with_scope(move |scope| {
                let error = JsErrorBox::from_err(e).to_v8(scope).unwrap();
                inner2.emit_event(
                  scope,
                  &[internalized(scope, "error").into(), error.into()],
                );
              });

              break;
            }
            Err(deno_core::Canceled) => {
              break;
            }
          };
        if nread == 0 {
          // Push EOF (null)
          {
            let this = this.clone();
            inner.scope_holder.with_scope({
              let inner = inner.clone();
              move |scope| {
                let this = this.get(scope).unwrap();
                let _result = inner
                  .push_func
                  .get(scope)
                  .unwrap()
                  .call(scope, this.into(), &[v8::null(scope).into()])
                  .unwrap();
                let zero = Smi(0u8).to_v8(scope).unwrap();
                inner.call_method(scope, "read", &[zero]).unwrap();
              }
            });
            // if allowHalfOpen is true, we need to call _final
            break;
          }
        }

        if nread > 0 {
          let this = this.clone();
          let buf = buf[..nread].to_vec();
          let inner2 = inner.clone();
          inner.scope_holder.with_scope(move |scope| {
            v8::tc_scope!(let scope, scope);
            let this = this.get(scope).unwrap();
            let data = Uint8Array(buf);
            let arg = data.to_v8(scope).map_err(JsErrorBox::from_err).unwrap();
            let result = inner2.push_func.get(scope).unwrap().call(
              scope,
              this.into(),
              &[arg],
            );
            if result.is_none() {
              let exception = scope.exception().unwrap();
              let error = JsError::from_v8_exception(scope, exception);
              eprintln!("error in push: {:?}", error);
              return;
            }
            let result = result.unwrap().cast::<v8::Boolean>();
            if result.is_false() {
              eprintln!("push returned false");
              inner2.should_read.clear_should_read();
            }
          });
        }
      }
      inner.should_read.clear_should_read();
    });
  }

  // pub fn push_data()
}

impl GetThis for SocketCbInner {
  fn this<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Object> {
    self.this.get(scope).unwrap()
  }
}

impl EventEmitter for SocketCbInner {
  fn cached_on_event_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.on_event_func.get(scope).unwrap()
  }
  fn cached_emit_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.emit_func.get(scope).unwrap()
  }
}

impl Obj for SocketCbInner {}

fn internalized<'a>(
  scope: &v8::PinScope<'a, '_>,
  s: &str,
) -> v8::Local<'a, v8::String> {
  v8::String::new_from_one_byte(
    scope,
    s.as_bytes(),
    v8::NewStringType::Internalized,
  )
  .unwrap()
}

unsafe impl GarbageCollected for Server {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    if let Some(on_listening) = &self.inner.on_listening {
      on_listening.trace(visitor);
    }
  }
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Server"
  }
}

#[derive(deno_core::CppgcBase)]
#[repr(C)]
pub struct Server {
  pub(crate) inner: Rc<ServerInner>,
}

pub(crate) struct ServerInner {
  host: RefCell<Option<String>>,
  port: RefCell<Option<u16>>,
  on_listening: Option<v8::TracedReference<v8::Function>>,
  holder: Rc<ScopeHolder>,
  op_state: Rc<RefCell<OpState>>,
  this: Rc<v8::Global<v8::Object>>,
  ref_tracker: RefTracker,
  emit_func: Rc<v8::Global<v8::Function>>,
  on_event_func: Rc<v8::Global<v8::Function>>,
}

#[derive(deno_core::ToV8)]
struct ServerAddress {
  address: String,
  port: u16,
}

impl Server {
  pub fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> Server {
    let (ops_tracker, super_cons, spawner) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<Constructors>().clone(),
        op_state.borrow::<deno_core::V8TaskSpawner>().clone(),
      )
    };
    let local_me = v8::Local::new(scope, &me);
    let holder = ScopeHolder::new_from_scope(spawner, scope);
    super_cons
      .event_emitter(scope)
      .call(scope, v8::Local::new(scope, &me).into(), &[])
      .unwrap();

    let this = Rc::new(v8::Global::new(scope, local_me));
    let emit = internalized(scope, "emit");
    let emit_func = local_me
      .get(scope, emit.into())
      .unwrap()
      .cast::<v8::Function>();
    let emit_func = Rc::new(v8::Global::new(scope, emit_func));
    let on_event = internalized(scope, "on");
    let on_event_func = local_me
      .get(scope, on_event.into())
      .unwrap()
      .cast::<v8::Function>();
    let on_event_func = Rc::new(v8::Global::new(scope, on_event_func));
    Server {
      inner: Rc::new(ServerInner {
        host: RefCell::new(None),
        port: RefCell::new(None),
        on_listening: None,
        holder: Rc::new(holder),
        op_state,
        this,
        ref_tracker: RefTracker::new(ops_tracker),
        emit_func,
        on_event_func,
      }),
    }
  }
}

impl ServerInner {
  pub(crate) fn with_scope(&self, f: impl FnOnce(&mut v8::PinScope) + 'static) {
    self.holder.with_scope(f);
  }

  pub(crate) fn with_scope_immediately(
    &self,
    f: impl FnOnce(&mut v8::PinScope),
  ) {
    self.holder.with_scope_immediately(f);
  }

  pub(crate) fn op_state(&self) -> Rc<RefCell<OpState>> {
    self.op_state.clone()
  }
}

#[op2]
impl Server {
  #[constructor]
  #[cppgc]
  fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> Server {
    Server::new_inner(me, scope, op_state)
  }

  #[fast]
  fn unref(&self) {
    self.inner.ref_tracker.unref();
  }

  #[to_v8]
  fn address(&self) -> Option<ServerAddress> {
    let inner = self.inner.clone();
    let host = inner.host.borrow();
    let port = *inner.port.borrow();
    match (&*host, port) {
      (Some(host), Some(port)) => Some(ServerAddress {
        address: host.to_string(),
        port,
      }),
      _ => None,
    }
  }

  #[fast]
  #[reentrant]
  fn listen(
    &self,
    scope: &mut v8::PinScope,
    #[smi] port: u16,
    #[string] host: String,
    on_listen: Option<v8::Local<v8::Function>>,
  ) {
    if let Some(on_listen) = on_listen {
      self.inner.on_event(
        scope,
        &[internalized(scope, "listening").into(), on_listen.into()],
      );
    }
    self.inner.listen_inner::<SocketCallback>(port, host);
  }
}

pub trait OnAccept {
  async fn on_accept(
    inner: &Rc<ServerInner>,
    stream: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
  ) -> Result<(), JsErrorBox>;
}

pub struct SocketCallback;

impl OnAccept for SocketCallback {
  async fn on_accept(
    inner: &Rc<ServerInner>,
    stream: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
  ) -> Result<(), JsErrorBox> {
    inner.holder.with_scope({
      let inner = inner.clone();
      move |scope| {
        let empty =
          deno_core::cppgc::make_cppgc_empty_object::<SocketCb>(scope);
        let socket = SocketCb::new_inner(
          v8::Global::new(scope, empty),
          scope,
          inner.op_state.clone(),
          SocketOptions {
            allow_half_open: None,
          },
          Some(addr.ip().to_string()),
          Some(addr.port() as u16),
        );
        match socket {
          Ok(socket) => {
            let socket_inner = socket.inner.clone();
            let socket_obj =
              deno_core::cppgc::wrap_object(scope, empty, socket);
            let socket_obj = v8::Global::new(scope, socket_obj);
            let (read, write) = stream.into_split();
            *socket_inner.write.try_borrow_mut().unwrap() = Some(write);
            *socket_inner.read.try_borrow_mut().unwrap() = Some(read);
            socket_inner.connected.set_connected(true);
            let zero = Smi(0u8).to_v8(scope).unwrap();
            socket_inner.call_method(scope, "read", &[zero]).unwrap();
            socket_inner.start_read();
            inner.holder.with_scope({
              let inner = inner.clone();
              move |scope| {
                let socket_obj = v8::Local::new(scope, socket_obj);
                inner.emit_event(
                  scope,
                  &[
                    internalized(scope, "connection").into(),
                    socket_obj.into(),
                  ],
                );
              }
            });
          }
          Err(e) => {
            eprintln!("error creating socket: {:?}", e);
          }
        }
      }
    });
    Ok(())
  }
}

impl ServerInner {
  pub fn listen_inner<T: OnAccept>(self: &Rc<Self>, port: u16, host: String) {
    let inner = self.clone();
    inner.ref_tracker.ref_();
    deno_core::unsync::spawn(async move {
      let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .map_err(JsErrorBox::from_err)
        .unwrap();
      let addr = listener.local_addr().unwrap();
      *inner.port.borrow_mut() = Some(addr.port());
      *inner.host.borrow_mut() = Some(addr.ip().to_string());
      inner.holder.with_scope({
        let inner = inner.clone();
        move |scope| {
          inner.emit_event(scope, &[internalized(scope, "listening").into()]);
        }
      });

      loop {
        let (stream, addr) = listener.accept().await.unwrap();
        let inner = inner.clone();
        deno_core::unsync::spawn(async move {
          if let Err(err) = T::on_accept(&inner, stream, addr).await {
            eprintln!("error in on_accept: {:?}", err);
          }
        });
      }
    });
  }
}

#[op2(reentrant)]
pub fn op_net_connect<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  op_state: Rc<RefCell<OpState>>,
  port_or_options: Option<v8::Local<'a, v8::Value>>,
  host_or_connect_cb: Option<v8::Local<'a, v8::Value>>,
  connect_cb: Option<v8::Local<'a, v8::Function>>,
) -> Result<v8::Local<'a, v8::Object>, JsErrorBox> {
  let (port, host, connect_listener, connect_options) = normalize_connect_args(
    scope,
    port_or_options,
    host_or_connect_cb,
    connect_cb,
  )?;
  let connect_host = host.clone();
  let connect_port = port;

  let socket_obj = deno_core::cppgc::make_cppgc_empty_object::<SocketCb>(scope);
  let socket = SocketCb::new_inner(
    v8::Global::new(scope, socket_obj),
    scope,
    op_state,
    SocketOptions {
      allow_half_open: connect_options
        .as_ref()
        .and_then(|options| options.allow_half_open),
    },
    host,
    port,
  )?;
  let inner = socket.inner.clone();
  let socket_obj = deno_core::cppgc::wrap_object(scope, socket_obj, socket);
  inner.ref_tracker.ref_();
  if let Some(connect_listener) = connect_listener {
    inner.on_event(
      scope,
      &[
        internalized(scope, "connect").into(),
        connect_listener.into(),
      ],
    );
  }
  deno_core::unsync::spawn(async move {
    if let Err(err) =
      SocketCbInner::connect_inner(inner, connect_port, connect_host).await
    {
      eprintln!("error in op_net_connect: {:?}", err);
    }
  });
  Ok(socket_obj)
}

pub trait GetThis {
  fn this<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Object>;
}

pub trait Obj: GetThis {
  fn call_method<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    args: &[v8::Local<'s, v8::Value>],
  ) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
    v8::tc_scope!(let scope, scope);
    let this = self.this(scope);
    let method = this
      .get(scope, internalized(scope, name).into())
      .unwrap()
      .cast::<v8::Function>();
    let value = method.call(scope, this.into(), args);
    match value {
      Some(value) => Ok(value),
      None => {
        let exception = scope.exception().unwrap();
        Err(JsErrorBox::from_err(JsError::from_v8_exception(
          scope, exception,
        )))
      }
    }
  }
}

pub trait EventEmitter: GetThis {
  fn cached_emit_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function>;
  fn emit_event(
    &self,
    scope: &mut v8::PinScope,
    args: &[v8::Local<v8::Value>],
  ) {
    let this = self.this(scope);
    let emit_func = self.cached_emit_func(scope);
    emit_func.call(scope, this.into(), args);
  }
  fn cached_on_event_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function>;
  fn on_event(&self, scope: &mut v8::PinScope, args: &[v8::Local<v8::Value>]) {
    let this = self.this(scope);
    let on_event_func = self.cached_on_event_func(scope);
    on_event_func.call(scope, this.into(), args);
  }
}

impl GetThis for ServerInner {
  fn this<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Object> {
    v8::Local::new(scope, &*self.this)
  }
}

impl EventEmitter for ServerInner {
  fn cached_emit_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.emit_func)
  }
  fn cached_on_event_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.on_event_func)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::{OnceLock, atomic::AtomicUsize};

  use deno_core::{ModuleSpecifier, RequestedModuleType, RuntimeOptions};

  use super::*;

  #[test]
  fn test_is_ipv4() {
    let pass_cases = [
      "127.0.0.1",
      "192.168.1.1",
      "10.0.0.1",
      "172.16.0.1",
      "172.31.255.255",
      "100.64.0.1",
      "100.127.255.255",
      "169.254.0.1",
      "169.254.255.255",
    ];
    let fail_cases = [
      "127.0.0.1.1",
      "127.0.0.0.1",
      "127.0.0.256",
      "127.0.0.-1",
      "127.0.0.255.256",
      "127.0.0.255.255.255",
    ];
    for case in pass_cases {
      assert!(is_ipv4(case));
    }
    for case in fail_cases {
      assert!(!is_ipv4(case));
    }
  }

  #[test]
  fn test_is_ipv6() {
    let pass_cases = [
      "::1",
      "::ffff:127.0.0.1",
      "::ffff:192.168.1.1",
      "::ffff:10.0.0.1",
      "::ffff:172.16.0.1",
      "::ffff:172.31.255.255",
      "::ffff:100.64.0.1",
      "::ffff:100.127.255.255",
    ];
    let fail_cases = [
      "::1.1",
      "::ffff:127.0.0.1.1",
      "::ffff:127.0.0.0.1",
      "::ffff:127.0.0.256",
      "::ffff:127.0.0.-1",
      "::ffff:127.0.0.255.256",
      "::ffff:127.0.0.255.255.255",
    ];
    for case in pass_cases {
      assert!(is_ipv6(case));
    }
    for case in fail_cases {
      assert!(!is_ipv6(case));
    }
  }

  #[test]
  fn test_is_ip() {
    let ipv4_cases = [
      "127.0.0.1",
      "192.168.1.1",
      "10.0.0.1",
      "172.16.0.1",
      "172.31.255.255",
      "100.64.0.1",
      "100.127.255.255",
      "169.254.0.1",
      "169.254.255.255",
    ];
    let ipv6_cases = [
      "::1",
      "::ffff:127.0.0.1",
      "::ffff:192.168.1.1",
      "::ffff:10.0.0.1",
      "::ffff:172.16.0.1",
      "::ffff:172.31.255.255",
      "::ffff:100.64.0.1",
      "::ffff:100.127.255.255",
      "::ffff:169.254.0.1",
      "::ffff:169.254.255.255",
    ];
    let fail_cases = [
      "127.0.0.1.1",
      "127.0.0.0.1",
      "127.0.0.256",
      "127.0.0.-1",
      "127.0.0.255.256",
      "127.0.0.255.255.255",
    ];
    for case in ipv4_cases {
      assert_eq!(is_ip(case), 4);
    }
    for case in ipv6_cases {
      assert_eq!(is_ip(case), 6);
    }
    for case in fail_cases {
      assert_eq!(is_ip(case), 0);
    }
  }

  static ID: OnceLock<AtomicUsize> = OnceLock::new();
  fn next_id() -> usize {
    ID.get_or_init(|| AtomicUsize::new(0))
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
  }

  async fn js_test(
    contents: &str,
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (mut runtime, _worker_host_side) =
      crate::checkin::runner::create_runtime_without_snapshot(
        false,
        None,
        vec![],
        RuntimeOptions::default(),
      );
    let specifier =
      ModuleSpecifier::parse(&format!("file:///test-{}.ts", next_id()))
        .unwrap();

    let id = runtime
      .load_main_es_module_from_code(&specifier, contents.to_string())
      .await
      .map_err(JsErrorBox::from_err)?;

    let module = runtime.mod_evaluate(id);

    runtime
      .run_event_loop(Default::default())
      .await
      .map_err(JsErrorBox::from_err)?;
    let _ = module.await.map_err(JsErrorBox::from_err)?;
    let namespace = runtime
      .get_module_namespace_by_name(
        &specifier.to_string(),
        RequestedModuleType::None,
      )
      .unwrap();
    deno_core::scope!(scope, runtime);
    let namespace = v8::Local::new(scope, namespace);
    let namespace = namespace.cast::<v8::Value>();
    Ok(v8::Global::new(scope, namespace))
  }

  #[tokio::test]
  async fn constructor_accepts_options() {
    let result = js_test(
      "
      import { Socket } from 'node:net';
      new Socket({ allowHalfOpen: true });
    ",
    );
    assert!(result.await.is_ok());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_read() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let data = "hello world".as_bytes().to_vec();
      stream
        .write_all(&data)
        .await
        .map_err(JsErrorBox::from_err)
        .unwrap();
    });
    let port = addr.port();
    let code = "
    import { Socket } from 'node:net';
    import { equal } from 'checkin:testing';
    const socket = new Socket();
    
    const expected = new Uint8Array([
      104, 101, 108, 108,
      111,  32, 119, 111,
      114, 108, 100
    ]);

    const prom = Promise.withResolvers();
    let data = new Uint8Array();
    socket.on('data', (chunk) => {
      data = new Uint8Array([...data, ...chunk]);
      if (!equal(data, expected)) {
        throw new Error('data is not equal to expected');
      }
      prom.resolve();
      socket.destroy();
    });

    await socket.connect(${PORT}, '127.0.0.1');
    await prom.promise;
  "
    .replace("${PORT}", &port.to_string());
    let result = js_test(&code);
    result.await.unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_connect_registers_callback() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
      for _ in 0..2 {
        let _ = listener.accept().await.unwrap();
      }
    });
    let port = addr.port();
    let code = "
    import { Socket } from 'node:net';
    
    const first = Promise.withResolvers();
    const second = Promise.withResolvers();
    
    const socket1 = new Socket();
    socket1.connect(${PORT}, () => {
      first.resolve();
      socket1.destroy();
    });
    
    const socket2 = new Socket();
    socket2.connect(${PORT}, '127.0.0.1', () => {
      second.resolve();
      socket2.destroy();
    });
    
    await first.promise;
    await second.promise;
  "
    .replace("${PORT}", &port.to_string());
    let result = js_test(&code);
    result.await.unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn server_accepts_connections() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();
    drop(listener);

    let code = "
    import { Server, Socket } from 'node:net';
    
    const server = new Server();
    
    const listeningProm = Promise.withResolvers();
    const connectionProm = Promise.withResolvers();
    
    let connectionCount = 0;
    const expectedConnections = 3;
    
    server.on('listening', () => {
      console.log('listening');
      listeningProm.resolve();
    });
    
    server.on('connection', (socket) => {
      connectionCount++;
      console.log('connection', connectionCount);
      socket.destroy();
      if (connectionCount === expectedConnections) {
      console.log('resolving connection prom');
        connectionProm.resolve();
      }
    });
    
    server.listen(${PORT}, '127.0.0.1');
    console.log('listening');
    await listeningProm.promise;
    console.log('listening prom resolved');

    async function makeConnection() {
      const socket = new Socket();
      const closeProm = Promise.withResolvers();

      socket.on('close', () => {
        console.log('close');
        closeProm.resolve();
      });
      await socket.connect(${PORT}, '127.0.0.1');
      await closeProm.promise;
    }
    

    console.log('making connections');
    await Promise.all([
      makeConnection(),
      makeConnection(),
      makeConnection(),
    ]);
    
    await connectionProm.promise;
    console.log('connection prom resolved');
    
    if (connectionCount !== expectedConnections) {
      throw new Error(`Expected ${expectedConnections} connections, got ${connectionCount}`);
    }
    
    console.log('unrefing server');
    server.unref();
    export const success = true;
  "
    .replace("${PORT}", &port.to_string());

    let timeout = tokio::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout, js_test(&code)).await;
    match result {
      Ok(Ok(_)) => {}
      Ok(Err(e)) => panic!("Test failed: {:?}", e),
      Err(_) => panic!("Test timed out after {:?}", timeout),
    }
  }
}
