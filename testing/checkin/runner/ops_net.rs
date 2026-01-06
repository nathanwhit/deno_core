use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ExternalOpsTracker;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ToV8;
use deno_core::convert::Uint8Array;
use deno_core::error::JsError;
use deno_core::op2;
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
  host: String,
  port: u16,
  this: Rc<v8::TracedReference<v8::Object>>,

  ref_tracker: RefTracker,
  cancel: Rc<CancelHandle>,
  connected: Rc<ConnectedState>,
  scope_holder: ScopeHolder,

  should_read: Rc<ShouldReadState>,
}

pub struct SocketCb {
  inner: Rc<SocketCbInner>,
}

unsafe impl deno_core::GarbageCollected for SocketCb {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.inner.push_func.trace(visitor);
    self.inner.this.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"SocketCb"
  }
}

#[op2]
pub fn op_set_duplex_constructor(
  op_state: &mut OpState,
  #[global] cons: v8::Global<v8::Function>,
) {
  op_state.put(SocketConstructor(Rc::new(cons)));
}

#[derive(Clone)]
struct SocketConstructor(Rc<v8::Global<v8::Function>>);

struct ScopeHolder(deno_core::V8TaskSpawner);

impl ScopeHolder {
  pub fn new(spawner: deno_core::V8TaskSpawner) -> Self {
    Self(spawner)
  }

  pub fn with_scope(&self, f: impl FnOnce(&mut v8::PinScope) + 'static) {
    self.0.spawn(move |scope| {
      v8::tc_scope!(let scope, scope);

      f(scope)
    })
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
    #[string] host: String,
    #[smi] port: u16,
  ) -> Result<SocketCb, JsErrorBox> {
    let (ops_tracker, super_cons, spawner) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<SocketConstructor>().clone(),
        op_state.borrow::<deno_core::V8TaskSpawner>().clone(),
      )
    };

    let local_me = v8::Local::new(&scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.0);
    let this = Rc::new(v8::TracedReference::new(scope, local_me));

    cons.call(scope, local_me.into(), &[]).unwrap();
    let push = internalized(scope, "push");
    let push_func = local_me
      .get(scope, push.into())
      .unwrap()
      .cast::<v8::Function>();
    let push_func = Rc::new(v8::TracedReference::new(scope, push_func));
    let scope_holder = ScopeHolder::new(spawner);

    let cb = SocketCb {
      inner: Rc::new(SocketCbInner {
        write: Rc::new(AsyncRefCell::new(None)),
        read: Rc::new(AsyncRefCell::new(None)),
        push_func,
        cancel: Rc::new(CancelHandle::new()),
        host,
        port,
        this,
        ref_tracker: RefTracker::new(ops_tracker),
        connected: Rc::new(ConnectedState::new()),
        scope_holder,
        should_read: Rc::new(ShouldReadState::new()),
      }),
    };
    Ok(cb)
  }

  #[async_method]
  pub fn connect<'a, 'b>(
    &self,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    let inner = self.inner.clone();
    inner.ref_tracker.ref_();
    async move {
      let stream =
        tokio::net::TcpStream::connect((inner.host.as_str(), inner.port))
          .await
          .map_err(JsErrorBox::from_err)
          .unwrap();
      let (read, write) = stream.into_split();
      *inner.write.borrow_mut().await = Some(write);
      *inner.read.borrow_mut().await = Some(read);
      inner.connected.set_connected(true);
      Ok(())
    }
  }

  #[fast]
  #[rename("_read")]
  fn read(&self, #[this] me: v8::Global<v8::Object>) {
    self.inner.should_read.set_should_read();
    // If we're already reading, don't start another read or set kSync
    // This prevents kSync from being set while the async task is calling push()
    if self.inner.should_read.swap_read_once() {
      return;
    }

    self.start_read(Rc::new(me));
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

impl SocketCb {
  fn start_read(&self, this: Rc<v8::Global<v8::Object>>) {
    let inner = self.inner.clone();
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
              eprintln!("canceled");
              break;
            }
          };

        if nread == 0 {
          // Push EOF (null)
          {
            let inner2 = inner.clone();
            let this = this.clone();
            inner.scope_holder.with_scope(move |scope| {
              let this = v8::Local::new(scope, &*this);
              let result = inner2
                .push_func
                .get(scope)
                .unwrap()
                .call(scope, this.into(), &[v8::null(scope).into()])
                .unwrap();
              let result = result.cast::<v8::Boolean>();
              if result.is_false() {
                inner2.should_read.clear_should_read();
              }
            });
          }
        }

        if nread > 0 {
          let this = this.clone();
          let buf = buf[..nread].to_vec();
          let inner2 = inner.clone();
          inner.scope_holder.with_scope(move |scope| {
            v8::tc_scope!(let scope, scope);
            let this = v8::Local::new(scope, &*this);
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

impl SocketCbInner {
  fn emit_event(
    &self,
    scope: &mut v8::PinScope,
    args: &[v8::Local<v8::Value>],
  ) {
    let this = self.this.get(scope).unwrap();
    let emit = internalized(scope, "emit");
    let emit_func =
      this.get(scope, emit.into()).unwrap().cast::<v8::Function>();
    emit_func.call(scope, this.into(), args);
  }
}

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
  async fn constructor_accepts_host_and_port() {
    let result = js_test(
      "
      const { SocketCb } = Deno.core.ops; 
      new SocketCb('localhost', 8080);
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
    import { SocketCb } from 'checkin:net';
    import { equal } from 'checkin:testing';
    const socket = new SocketCb('localhost', ${PORT});
    
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

    await socket.connect();
    await prom.promise;
  "
    .replace("${PORT}", &port.to_string());
    let result = js_test(&code);
    result.await.unwrap();
  }
}
