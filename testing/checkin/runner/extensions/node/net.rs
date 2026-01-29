use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ExternalOpsTracker;
use deno_core::GarbageCollected;
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

use crate::checkin::runner::extensions::node::JsMethod;
use crate::checkin::runner::extensions::node::NextTickFunc;
use crate::checkin::runner::extensions::node::ScopeHolder;
use crate::checkin::runner::extensions::node::internalized;

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
pub(crate) struct RefTracker(Arc<RefTrackerInner>);

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

  pub(crate) fn unref(&self) {
    if self
      .0
      .refed
      .swap(false, std::sync::atomic::Ordering::Relaxed)
    {
      self.0.ops_tracker.unref_op();
    }
  }
}

struct SocketInner {
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
  read: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedReadHalf>>>,
  host: RefCell<Option<String>>,
  port: RefCell<Option<u16>>,

  ref_tracker: RefTracker,
  cancel: Rc<CancelHandle>,
  connected: Rc<ConnectedState>,
  scope_holder: ScopeHolder,
  should_read: Rc<ShouldReadState>,

  this: Rc<v8::Global<v8::Object>>,

  push_func: JsMethod,
  emit_func: JsMethod,
  on_event_func: JsMethod,
  next_tick_func: NextTickFunc,
}

pub struct Socket {
  inner: Rc<SocketInner>,
}

unsafe impl deno_core::GarbageCollected for Socket {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {
    // self.inner.push_func.trace(visitor);
    // self.inner.this.trace(visitor);
    // self.inner.emit_func.trace(visitor);
    // self.inner.on_event_func.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Socket"
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
  decode_strings: bool,
}

impl Socket {
  pub(crate) fn new_server(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    host: Option<String>,
    port: Option<u16>,
  ) -> Result<Socket, JsErrorBox> {
    Socket::new_inner(
      me,
      scope,
      op_state,
      SocketOptions {
        allow_half_open: None,
      },
      host,
      port,
    )
  }

  pub(crate) fn attach_stream(&self, stream: tokio::net::TcpStream) {
    let (read, write) = stream.into_split();
    *self.inner.write.try_borrow_mut().unwrap() = Some(write);
    *self.inner.read.try_borrow_mut().unwrap() = Some(read);
    self.inner.connected.set_connected(true);
  }

  fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    options: SocketOptions,
    host: Option<String>,
    port: Option<u16>,
  ) -> Result<Socket, JsErrorBox> {
    let (ops_tracker, super_cons, spawner, next_tick_func) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<Constructors>().clone(),
        op_state.borrow::<deno_core::V8TaskSpawner>().clone(),
        op_state.borrow::<NextTickFunc>().clone(),
      )
    };

    let local_me = v8::Local::new(&scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.duplex);
    let this = Rc::new(v8::Global::new(scope, local_me));

    let duplex_options = DuplexOptions {
      allow_half_open: Some(options.allow_half_open.unwrap_or(false)),
      emit_close: false,
      auto_destroy: true,
      decode_strings: false,
    };
    let duplex_options =
      deno_core::serde_v8::to_v8(scope, &duplex_options).unwrap();
    cons
      .call(scope, local_me.into(), &[duplex_options])
      .unwrap();
    let push_func = JsMethod::capture(scope, local_me, "push");
    let emit_func = JsMethod::capture(scope, local_me, "emit");
    let on_event_func = JsMethod::capture(scope, local_me, "on");
    let scope_holder = ScopeHolder::new_from_scope(spawner, scope);

    let cb = Socket {
      inner: Rc::new(SocketInner {
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
        next_tick_func,
      }),
    };
    Ok(cb)
  }
}

#[op2]
impl Socket {
  #[constructor]
  #[cppgc]
  #[reentrant]
  pub fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    #[serde] options: Option<SocketOptions>,
  ) -> Result<Socket, JsErrorBox> {
    let options = options.unwrap_or_default();
    Socket::new_inner(me, scope, op_state, options, None, None)
  }

  #[fast]
  #[reentrant]
  pub fn connect<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    port_or_options: Option<v8::Local<'a, v8::Value>>,
    host_or_connect_cb: Option<v8::Local<'a, v8::Value>>,
    connect_cb: Option<v8::Local<'a, v8::Function>>,
  ) -> Result<(), JsErrorBox> {
    let inner = self.inner.clone();
    inner.ref_tracker.ref_();

    let (port, host, connect_listener, _connect_options) =
      normalize_connect_args(
        scope,
        port_or_options,
        host_or_connect_cb,
        connect_cb,
      )?;

    // Register connect listener if provided (like Node.js does with this.once('connect', cb))
    if let Some(connect_listener) = connect_listener {
      inner.on_event(
        scope,
        &[
          internalized(scope, "connect").into(),
          connect_listener.into(),
        ],
      );
    }

    // Spawn async connection work - returns immediately like Node.js
    deno_core::unsync::spawn(async move {
      if let Err(e) = inner.connect_inner(port, host).await {
        eprintln!("error in connect: {:?}", e);
      }
    });

    Ok(())
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
        let this = v8::Local::new(scope, &*inner2.this);
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
      {
        let mut read = inner.read.borrow_mut().await;
        let mut write = inner.write.borrow_mut().await;
        let read_half = read.take();
        let write_half = write.take();
        drop(read);
        drop(write);
        #[cfg(unix)]
        {
          if let (Some(read_half), Some(write_half)) = (read_half, write_half) {
            if let Ok(stream) = read_half.reunite(write_half) {
              if let Ok(std_stream) = stream.into_std() {
                let _ = std_stream.shutdown(std::net::Shutdown::Both);
              }
            }
          }
        }
        #[cfg(not(unix))]
        {
          let _ = read_half;
          let _ = write_half;
        }
      }
      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = v8::Local::new(scope, &*inner2.this);
        let error = v8::Local::new(scope, &error);
        cb.call(scope, this.into(), &[error]).unwrap();
        inner2.emit_event(scope, &[internalized(scope, "close").into()]);
      });
      inner.ref_tracker.unref();
    });
  }

  #[reentrant]
  #[rename("_write")]
  pub fn write<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    data: v8::Local<'a, v8::Value>,
    encoding: v8::Local<'a, v8::Value>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    let inner = self.inner.clone();

    // Convert data to bytes based on type and encoding
    let bytes: Vec<u8> = if let Ok(str_val) = data.try_cast::<v8::String>() {
      // It's a string - encode based on encoding parameter
      let encoding_str = if encoding.is_string() {
        encoding
          .to_string(scope)
          .map(|s| s.to_rust_string_lossy(scope))
          .unwrap_or_else(|| "utf8".to_string())
      } else {
        "utf8".to_string()
      };

      let rust_str = str_val.to_rust_string_lossy(scope);
      encode_string(&rust_str, &encoding_str)
    } else if let Ok(array_buffer_view) = data.try_cast::<v8::ArrayBufferView>()
    {
      // It's a TypedArray or DataView
      let len = array_buffer_view.byte_length();
      let mut buf = vec![0u8; len];
      array_buffer_view.copy_contents(&mut buf);
      buf
    } else if let Ok(array_buffer) = data.try_cast::<v8::ArrayBuffer>() {
      // It's an ArrayBuffer
      let backing_store = array_buffer.get_backing_store();
      backing_store.iter().map(|c| c.get()).collect()
    } else {
      // Fallback: try to convert to string
      let str_val = data.to_string(scope).ok_or_else(|| {
        JsErrorBox::generic("Failed to convert data to string")
      })?;
      str_val.to_rust_string_lossy(scope).into_bytes()
    };

    let num_wrote = if let Some(mut write) = inner.write.try_borrow_mut() {
      let write = write.deref_mut().as_mut().unwrap();
      let nwritten = write.try_write(&bytes);
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

    if num_wrote >= bytes.len() {
      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let local_cb = v8::Local::new(scope, &cb);
        let this = v8::Local::new(scope, &*inner2.this);
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
        .write_all(&bytes[num_wrote..])
        .or_cancel(inner.cancel.clone())
        .await
        .unwrap()
        .map_err(JsErrorBox::from_err)
        .err();

      let inner2 = inner.clone();
      inner.scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = v8::Local::new(scope, &*inner2.this);
        call_write_cb(scope, cb, this, result);
      });
    });

    Ok(())
  }
}

/// Encode a string to bytes based on the specified encoding
fn encode_string(s: &str, encoding: &str) -> Vec<u8> {
  match encoding.to_lowercase().as_str() {
    "utf8" | "utf-8" => s.as_bytes().to_vec(),
    "ascii" | "latin1" | "binary" => {
      // ASCII/Latin1: take only the low byte of each char
      s.chars().map(|c| c as u8).collect()
    }
    "hex" => {
      // Decode hex string to bytes
      hex_decode(s).unwrap_or_else(|_| s.as_bytes().to_vec())
    }
    "base64" => {
      // Decode base64 string to bytes
      base64_decode(s).unwrap_or_else(|_| s.as_bytes().to_vec())
    }
    "base64url" => {
      // Decode base64url string to bytes
      base64url_decode(s).unwrap_or_else(|_| s.as_bytes().to_vec())
    }
    "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => {
      // UTF-16LE encoding
      s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect()
    }
    _ => s.as_bytes().to_vec(), // Default to UTF-8
  }
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
  if s.len() % 2 != 0 {
    return Err(());
  }
  (0..s.len())
    .step_by(2)
    .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
    .collect()
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
  // Simple base64 decoder
  const ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

  let mut result = Vec::new();
  let mut buf: u32 = 0;
  let mut bits: u32 = 0;

  for c in s.bytes() {
    if c == b'=' {
      break;
    }
    let val = ALPHABET.iter().position(|&x| x == c).ok_or(())? as u32;
    buf = (buf << 6) | val;
    bits += 6;
    if bits >= 8 {
      bits -= 8;
      result.push((buf >> bits) as u8);
      buf &= (1 << bits) - 1;
    }
  }
  Ok(result)
}

fn base64url_decode(s: &str) -> Result<Vec<u8>, ()> {
  // Convert base64url to standard base64 and decode
  let standard: String = s
    .chars()
    .map(|c| match c {
      '-' => '+',
      '_' => '/',
      c => c,
    })
    .collect();
  base64_decode(&standard)
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

impl SocketInner {
  fn emit_error_next_tick(self: &Rc<Self>, error: JsErrorBox) {
    let inner = self.clone();
    self.scope_holder.with_scope(move |scope| {
      let error = error.to_v8(scope).unwrap();
      let next_tick_func = inner.next_tick_func.clone();
      next_tick_func
        .get(scope)
        .call(
          scope,
          v8::null(scope).into(),
          &[
            inner.emit_func.get(scope).into(),
            v8::Local::new(scope, &*inner.this).into(),
            internalized(scope, "error").into(),
            error.into(),
          ],
        )
        .unwrap();
    });
  }
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
      .map_err(JsErrorBox::from_err);
      let stream = match stream {
        Ok(stream) => stream,
        Err(e) => {
          self.emit_error_next_tick(e);
          return Ok(());
        }
      };
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
                v8::tc_scope!(let scope, scope);
                let this = v8::Local::new(scope, &*this);
                let result = inner.push_func.get(scope).call(
                  scope,
                  this.into(),
                  &[v8::null(scope).into()],
                );
                if result.is_none() {
                  if let Some(exception) = scope.exception() {
                    let error = JsError::from_v8_exception(scope, exception);
                    eprintln!("error in push(null): {:?}", error);
                  }
                  return;
                }
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
            let this = v8::Local::new(scope, &*this);
            let data = Uint8Array(buf);
            let arg = data.to_v8(scope).map_err(JsErrorBox::from_err).unwrap();
            let result =
              inner2.push_func.get(scope).call(scope, this.into(), &[arg]);
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

impl GetThis for SocketInner {
  fn this<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Object> {
    v8::Local::new(scope, &*self.this)
  }
}

impl EventEmitter for SocketInner {
  fn cached_on_event_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.on_event_func.get(scope)
  }
  fn cached_emit_func<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.emit_func.get(scope)
  }
}

impl Obj for SocketInner {}

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
  pub(crate) host: RefCell<Option<String>>,
  pub(crate) port: RefCell<Option<u16>>,
  on_listening: Option<v8::TracedReference<v8::Function>>,
  pub(crate) holder: Rc<ScopeHolder>,
  op_state: Rc<RefCell<OpState>>,
  this: Rc<v8::Global<v8::Object>>,
  pub(crate) ref_tracker: RefTracker,
  emit_func: Rc<v8::Global<v8::Function>>,
  on_event_func: Rc<v8::Global<v8::Function>>,
  cancel: Rc<CancelHandle>,
}

#[derive(deno_core::ToV8)]
pub struct ServerAddress {
  pub address: String,
  pub port: u16,
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
        cancel: Rc::new(CancelHandle::new()),
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

  #[fast]
  #[reentrant]
  fn close(
    &self,
    scope: &mut v8::PinScope,
    cb: Option<v8::Local<v8::Function>>,
  ) {
    // Register callback for 'close' event if provided
    if let Some(cb) = cb {
      self
        .inner
        .on_event(scope, &[internalized(scope, "close").into(), cb.into()]);
    }
    // Cancel the listener loop - this will emit 'close' event
    self.inner.close();
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
        let empty = deno_core::cppgc::make_cppgc_empty_object::<Socket>(scope);
        let socket = Socket::new_inner(
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
    let cancel = inner.cancel.clone();
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
        let accept_result = listener.accept().or_cancel(cancel.clone()).await;
        match accept_result {
          Ok(Ok((stream, addr))) => {
            let inner = inner.clone();
            deno_core::unsync::spawn(async move {
              if let Err(err) = T::on_accept(&inner, stream, addr).await {
                eprintln!("error in on_accept: {:?}", err);
              }
            });
          }
          Ok(Err(e)) => {
            eprintln!("error in listener.accept: {:?}", e);
            continue;
          }
          Err(deno_core::Canceled) => {
            // Server was closed
            inner.holder.with_scope({
              let inner = inner.clone();
              move |scope| {
                inner.emit_event(scope, &[internalized(scope, "close").into()]);
              }
            });
            inner.ref_tracker.unref();
            break;
          }
        }
      }
    });
  }

  pub fn close(&self) {
    self.cancel.cancel();
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

  let socket_obj = deno_core::cppgc::make_cppgc_empty_object::<Socket>(scope);
  let socket = Socket::new_inner(
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
      SocketInner::connect_inner(inner, connect_port, connect_host).await
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
  use std::cell::Cell;
  use std::sync::{OnceLock, atomic::AtomicUsize};

  use deno_core::{
    JsRuntime, ModuleSpecifier, PollEventLoopOptions, RequestedModuleType,
    RuntimeOptions,
  };
  use tokio::sync::oneshot;

  use crate::checkin::runner::extensions::node::test_utils::{
    JsObject, NetTest, import_from, js_callback, send_cb, signal_cb,
  };

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

  fn jsruntime() -> JsRuntime {
    let (runtime, _worker_host_side) =
      crate::checkin::runner::create_runtime_without_snapshot(
        false,
        None,
        vec![],
        RuntimeOptions::default(),
      );
    runtime
  }

  async fn js_test(
    contents: &str,
  ) -> Result<(JsRuntime, v8::Global<v8::Value>), JsErrorBox> {
    let mut runtime = jsruntime();
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
    let namespace = {
      deno_core::scope!(scope, runtime);
      let namespace = v8::Local::new(scope, namespace);
      let namespace = namespace.cast::<v8::Value>();
      v8::Global::new(scope, namespace)
    };
    Ok((runtime, namespace))
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
    let (_, _) = result.await.unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_connect_callback() {
    let mut runtime = jsruntime();
    let socket = import_from(&mut runtime, "node:net", "Socket").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();
    tokio::task::spawn(async move {
      for _ in 0..2 {
        let _ = listener.accept().await.unwrap();
      }
    });

    fn mk_callback<'a>(
      scope: &mut v8::PinScope<'a, '_>,
      closed_tx: oneshot::Sender<()>,
    ) -> v8::Local<'a, v8::Function> {
      js_callback(scope, Some(closed_tx), |scope, closed_tx, args, _| {
        closed_tx.take().unwrap().send(()).unwrap();
        JsObject::new(scope, args.this()).call(scope, "destroy", ());
      })
    }

    let (closed_tx, closed_rx) = oneshot::channel::<()>();
    let (closed_tx2, closed_rx2) = oneshot::channel::<()>();

    runtime.with_scope(|scope| {
      let socket_cons = v8::Local::new(scope, socket).cast::<v8::Function>();

      let socket = JsObject::construct(scope, socket_cons, ());
      let callback = mk_callback(scope, closed_tx);
      socket.call(scope, "connect", (port, "127.0.0.1", callback));

      let socket2 = JsObject::construct(scope, socket_cons, ());
      let callback2 = mk_callback(scope, closed_tx2);
      socket2.call(scope, "connect", (port, "127.0.0.1", callback2));
    });

    runtime
      .run_event_loop(PollEventLoopOptions::default())
      .await
      .unwrap();

    let _ = closed_rx.await.unwrap();
    let _ = closed_rx2.await.unwrap();
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_write_sends_data() {
    use tokio::io::AsyncReadExt;
    use tokio::sync::oneshot;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let mut buf = [0u8; 5];
      stream.read_exact(&mut buf).await.unwrap();
      let _ = tx.send(buf.to_vec());
    });

    let port = addr.port();
    let code = "
    import { Socket } from 'node:net';
    const socket = new Socket();
    const done = Promise.withResolvers();
    const data = 'hello';

    socket.on('connect', () => {
      socket.write(data, () => {
        socket.end();
      });
    });
    socket.on('close', () => done.resolve());

    socket.connect(${PORT}, '127.0.0.1');
    await done.promise;
  "
    .replace("${PORT}", &port.to_string());

    let timeout = tokio::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout, js_test(&code)).await;
    match result {
      Ok(Ok(_)) => {}
      Ok(Err(e)) => panic!("Test failed: {:?}", e),
      Err(_) => panic!("Test timed out after {:?}", timeout),
    }

    let received = tokio::time::timeout(timeout, rx)
      .await
      .expect("timed out waiting for server read")
      .expect("server read failed");
    assert_eq!(received, b"hello".to_vec());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_write_sends_data_rust() {
    use tokio::io::AsyncReadExt;

    let mut runtime = jsruntime();
    let socket = import_from(&mut runtime, "node:net", "Socket").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();

    let (tx, rx) = oneshot::channel::<Vec<u8>>();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let mut buf = [0u8; 5];
      stream.read_exact(&mut buf).await.unwrap();
      let _ = tx.send(buf.to_vec());
    });

    let (closed_tx, closed_rx) = oneshot::channel::<()>();

    runtime.with_scope(|scope| {
      let socket_cons = v8::Local::new(scope, socket).cast::<v8::Function>();
      let no_args: &[v8::Local<v8::Value>] = &[];
      let socket_local = socket_cons.new_instance(scope, no_args).unwrap();
      let socket_obj = JsObject::new(scope, socket_local);

      // Set up 'connect' handler: write data and end
      let connect_cb = {
        let socket = socket_obj.clone();
        js_callback(scope, socket, |scope, socket, _, _| {
          let write_cb =
            js_callback(scope, socket.clone(), |scope, socket, _, _| {
              socket.call(scope, "end", ());
            });
          socket.call(scope, "write", ("hello", write_cb));
        })
      };
      socket_obj.call(scope, "on", ("connect", connect_cb));

      // Set up 'close' handler
      let close_cb =
        js_callback(scope, Some(closed_tx), |_scope, closed_tx, _, _| {
          closed_tx.take().unwrap().send(()).unwrap();
        });
      socket_obj.call(scope, "on", ("close", close_cb));

      // Connect
      socket_obj.call(scope, "connect", (port, "127.0.0.1"));
    });

    runtime
      .run_event_loop(PollEventLoopOptions::default())
      .await
      .unwrap();

    let _ = closed_rx.await.unwrap();

    let received = rx.await.expect("server read failed");
    assert_eq!(received, b"hello".to_vec());
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

  #[tokio::test(flavor = "current_thread")]
  async fn socket_receives_end_event_on_server_close() {
    use std::pin::pin;
    use tokio::io::AsyncWriteExt;

    let mut runtime = jsruntime();
    let socket_mod = import_from(&mut runtime, "node:net", "Socket").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();

    // Server: accept connection, send data, then close
    tokio::task::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      stream.write_all(b"hello").await.unwrap();
      stream.shutdown().await.unwrap();
    });

    let (end_tx, end_rx) = oneshot::channel::<()>();

    runtime.with_scope(|scope| {
      let socket_cons =
        v8::Local::new(scope, &socket_mod).cast::<v8::Function>();
      let socket = JsObject::construct(scope, socket_cons, ());

      // Set up 'data' event handler to consume data (required for 'end' to fire)
      let data_cb = js_callback(scope, None::<()>, |_scope, _, _, _| {});
      socket.call(scope, "on", ("data", data_cb));

      // Set up 'end' event handler
      let end_cb = js_callback(scope, Some(end_tx), |_scope, end_tx, _, _| {
        end_tx.take().unwrap().send(()).unwrap();
      });
      socket.call(scope, "on", ("end", end_cb));

      // Connect to server
      socket.call(scope, "connect", (port, "127.0.0.1"));
    });

    let timeout = tokio::time::Duration::from_secs(5);
    let mut event_loop =
      pin!(runtime.run_event_loop(PollEventLoopOptions::default()));
    let mut end_rx = pin!(end_rx);

    let result = loop {
      tokio::select! {
        _ = &mut event_loop => {}
        result = &mut end_rx => {
          break result.ok();
        }
        _ = tokio::time::sleep(timeout) => {
          break None;
        }
      }
    };

    assert!(
      result.is_some(),
      "Socket should have received 'end' event when server closed connection"
    );
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_write_string_with_encoding() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let mut buf = Vec::new();
      stream.read_to_end(&mut buf).await.unwrap();
      let _ = tx.send(buf);
    });

    let port = addr.port();
    // Test writing a string with utf8 encoding
    let code = "
    import { Socket } from 'node:net';
    const socket = new Socket();
    const done = Promise.withResolvers();

    socket.on('connect', () => {
      socket.write('hello', 'utf8', () => {
        socket.end();
      });
    });
    socket.on('close', () => done.resolve());

    socket.connect(${PORT}, '127.0.0.1');
    await done.promise;
  "
    .replace("${PORT}", &port.to_string());

    let timeout = tokio::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout, js_test(&code)).await;
    match result {
      Ok(Ok(_)) => {}
      Ok(Err(e)) => panic!("Test failed: {:?}", e),
      Err(_) => panic!("Test timed out after {:?}", timeout),
    }

    let received = tokio::time::timeout(timeout, rx)
      .await
      .expect("timed out waiting for server read")
      .expect("server read failed");
    assert_eq!(received, b"hello".to_vec());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_write_hex_encoding() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let mut buf = Vec::new();
      stream.read_to_end(&mut buf).await.unwrap();
      let _ = tx.send(buf);
    });

    let port = addr.port();
    // Test writing a hex-encoded string
    let code = "
    import { Socket } from 'node:net';
    const socket = new Socket();
    const done = Promise.withResolvers();

    socket.on('connect', () => {
      // '48656c6c6f' is 'Hello' in hex
      socket.write('48656c6c6f', 'hex', () => {
        socket.end();
      });
    });
    socket.on('close', () => done.resolve());

    socket.connect(${PORT}, '127.0.0.1');
    await done.promise;
  "
    .replace("${PORT}", &port.to_string());

    let timeout = tokio::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout, js_test(&code)).await;
    match result {
      Ok(Ok(_)) => {}
      Ok(Err(e)) => panic!("Test failed: {:?}", e),
      Err(_) => panic!("Test timed out after {:?}", timeout),
    }

    let received = tokio::time::timeout(timeout, rx)
      .await
      .expect("timed out waiting for server read")
      .expect("server read failed");
    assert_eq!(received, b"Hello".to_vec());
  }

  /// Test that setEncoding causes data events to receive strings instead of Buffers
  #[tokio::test(flavor = "current_thread")]
  async fn socket_set_encoding_receives_string_data() {
    use tokio::io::AsyncWriteExt;

    let mut test = NetTest::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // Server: send data and close
    tokio::task::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      stream.write_all(b"hello world").await.unwrap();
      stream.shutdown().await.unwrap();
    });

    let (tx, rx) = oneshot::channel::<Result<String, String>>();

    test.with_socket(|scope, socket| {
      socket.call(scope, "setEncoding", ("utf8",));

      // Track received data
      let data = Rc::new(RefCell::new(String::new()));
      let is_string = Rc::new(Cell::new(true));

      let data_cb = js_callback(
        scope,
        (data.clone(), is_string.clone()),
        |scope, (data, is_string), args, _| {
          let chunk = args.get(0);
          if chunk.is_string() {
            data.borrow_mut().push_str(&chunk.to_rust_string_lossy(scope));
          } else {
            is_string.set(false);
          }
        },
      );
      socket.call(scope, "on", ("data", data_cb));

      let end_cb = send_cb(scope, tx, move |_, _| {
        if !is_string.get() {
          Err("Expected string, got non-string".into())
        } else {
          Ok(data.borrow().clone())
        }
      });
      socket.call(scope, "on", ("end", end_cb));
      socket.call(scope, "connect", (port, "127.0.0.1"));
    });

    match test.run_until(rx).await {
      Some(Ok(data)) => assert_eq!(data, "hello world"),
      Some(Err(e)) => panic!("{}", e),
      None => panic!("Timed out"),
    }
  }

  /// Test that listeners registered AFTER socket.connect() still receive the event
  #[tokio::test(flavor = "current_thread")]
  async fn socket_connect_listener_registered_after_connect_call() {
    let mut test = NetTest::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::task::spawn(async move {
      let _ = listener.accept().await.unwrap();
    });

    let (tx, rx) = oneshot::channel::<()>();

    test.with_socket(|scope, socket| {
      // Call connect FIRST, then register listener
      socket.call(scope, "connect", (port, "127.0.0.1"));
      let cb = signal_cb(scope, tx);
      socket.call(scope, "on", ("connect", cb));
    });

    assert!(
      test.run_until(rx).await.is_some(),
      "Connect listener registered after connect() should still receive the event"
    );
  }
}
