use std::{
  cell::RefCell,
  rc::Rc,
  sync::atomic::{AtomicBool, Ordering},
};

use deno_core::serde;
use deno_core::v8;
use deno_error::JsErrorBox;
use hyper::http::{HeaderMap, HeaderValue, Version};
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use tokio::sync::Notify;

use super::super::net::{ServerInner, Socket};

/// Backpressure signal for Readable streams.
pub struct ShouldReadState {
  should_read: AtomicBool,
  notify: Notify,
}

impl ShouldReadState {
  pub fn new() -> Self {
    Self {
      should_read: AtomicBool::new(true),
      notify: Notify::new(),
    }
  }

  pub fn set_should_read(&self) {
    if !self.should_read.swap(true, Ordering::Relaxed) {
      self.notify.notify_waiters();
    }
  }

  pub fn clear_should_read(&self) {
    self.should_read.store(false, Ordering::Relaxed);
  }

  pub async fn wait_for_should_read(&self) {
    if self.should_read.load(Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

/// HTTP request parts extracted from hyper.
pub struct RequestParts {
  pub method: hyper::http::Method,
  pub uri: hyper::http::Uri,
  pub version: Version,
  pub headers: HeaderMap,
}

/// A cached value with optional V8 representation.
pub struct V8Cached<T> {
  value: Option<T>,
  v8: Option<v8::Global<v8::Value>>,
}

impl<T> V8Cached<T> {
  pub fn new() -> Self {
    Self {
      value: None,
      v8: None,
    }
  }

  pub fn is_initialized(&self) -> bool {
    self.value.is_some()
  }

  pub fn set(&mut self, value: T) {
    self.value = Some(value);
    self.v8 = None;
  }

  #[allow(unused)]
  pub fn get_or_init_v8_with<'a>(
    &mut self,
    scope: &mut v8::PinScope<'a, '_>,
    init: impl FnOnce() -> T,
    serialize: impl FnOnce(
      &mut v8::PinScope<'a, '_>,
      &T,
    ) -> v8::Local<'a, v8::Value>,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox>
  where
    T: serde::Serialize,
  {
    if let Some(value) = self.v8.as_ref() {
      return Ok(v8::Local::new(scope, value));
    }
    if self.value.is_none() {
      self.value = Some(init());
    }
    let value = self.value.as_ref().unwrap();
    let v8_value = serialize(scope, value);
    self.v8 = Some(v8::Global::new(scope, v8_value));
    Ok(v8_value)
  }

  pub fn get_or_init_v8<'a>(
    &mut self,
    scope: &mut v8::PinScope<'a, '_>,
    init: impl FnOnce() -> T,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox>
  where
    T: serde::Serialize,
  {
    if let Some(value) = self.v8.as_ref() {
      return Ok(v8::Local::new(scope, value));
    }
    if self.value.is_none() {
      self.value = Some(init());
    }
    let value = self.value.as_ref().unwrap();
    let v8_value =
      deno_core::serde_v8::to_v8(scope, value).map_err(JsErrorBox::from_err)?;
    self.v8 = Some(v8::Global::new(scope, v8_value));
    Ok(v8_value)
  }
}

/// Lazily creates a Socket object from a raw file descriptor.
pub struct LazySocket {
  inner: Rc<ServerInner>,
  #[cfg(unix)]
  raw_fd: RawFd,
  host: Option<String>,
  port: Option<u16>,
  socket_obj: RefCell<Option<v8::Global<v8::Object>>>,
}

impl LazySocket {
  pub fn new(
    inner: Rc<ServerInner>,
    stream: &tokio::net::TcpStream,
    addr: std::net::SocketAddr,
  ) -> LazySocket {
    LazySocket {
      inner,
      #[cfg(unix)]
      raw_fd: stream.as_raw_fd(),
      host: Some(addr.ip().to_string()),
      port: Some(addr.port() as u16),
      socket_obj: RefCell::new(None),
    }
  }

  #[cfg(unix)]
  fn dup_stream(&self) -> Result<tokio::net::TcpStream, JsErrorBox> {
    let fd = unsafe { libc::dup(self.raw_fd) };
    if fd < 0 {
      return Err(JsErrorBox::from_err(std::io::Error::last_os_error()));
    }
    let std_stream = unsafe { std::net::TcpStream::from_raw_fd(fd) };
    std_stream
      .set_nonblocking(true)
      .map_err(JsErrorBox::from_err)?;
    tokio::net::TcpStream::from_std(std_stream).map_err(JsErrorBox::from_err)
  }

  #[cfg(not(unix))]
  fn dup_stream(&self) -> Result<tokio::net::TcpStream, JsErrorBox> {
    Err(JsErrorBox::generic(
      "res.socket is not supported on this platform yet",
    ))
  }

  pub fn get_or_create<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Object>, JsErrorBox> {
    if let Some(socket_obj) = self.socket_obj.borrow().as_ref() {
      return Ok(v8::Local::new(scope, socket_obj));
    }
    let stream = self.dup_stream()?;
    let socket_obj = deno_core::cppgc::make_cppgc_empty_object::<Socket>(scope);
    let socket = Socket::new_server(
      v8::Global::new(scope, socket_obj),
      scope,
      self.inner.op_state(),
      self.host.clone(),
      self.port,
    )?;
    socket.attach_stream(stream);
    let socket_obj = deno_core::cppgc::wrap_object(scope, socket_obj, socket);
    let socket_global = v8::Global::new(scope, socket_obj);
    *self.socket_obj.borrow_mut() = Some(socket_global);
    Ok(socket_obj)
  }
}

/// Determine if connection should close based on headers and HTTP version.
pub fn should_close_from_parts(headers: &HeaderMap, version: Version) -> bool {
  if let Some(value) = headers.get("connection") {
    if let Ok(value) = value.to_str() {
      let value = value.to_ascii_lowercase();
      if value.contains("close") {
        return true;
      }
      if value.contains("keep-alive") {
        return false;
      }
    }
  }
  matches!(version, Version::HTTP_10)
}

/// Check if request is an upgrade request.
pub fn upgrade_from_parts(headers: &HeaderMap) -> bool {
  if let Some(value) = headers.get("connection") {
    if let Ok(value) = value.to_str() {
      if value.to_ascii_lowercase().contains("upgrade") {
        return true;
      }
    }
  }
  headers.contains_key("upgrade")
}

/// Cached HTTP date HeaderValue that updates every second.
/// HTTP dates have 1-second resolution, so caching avoids
/// repeated syscalls, string formatting, and HeaderValue creation per request.
pub mod cached_date {
  use super::HeaderValue;
  use std::cell::RefCell;
  use std::time::{Duration, Instant};

  struct CachedDate {
    header_value: HeaderValue,
    last_update: Instant,
  }

  thread_local! {
    static CACHED: RefCell<CachedDate> = RefCell::new(CachedDate {
      header_value: format_now(),
      last_update: Instant::now(),
    });
  }

  fn format_now() -> HeaderValue {
    let date_str =
      httpdate::HttpDate::from(std::time::SystemTime::now()).to_string();
    // HTTP dates are always valid ASCII, so this won't fail
    HeaderValue::from_str(&date_str).unwrap()
  }

  pub fn http_date_header_value() -> HeaderValue {
    CACHED.with(|cached| {
      let mut cached = cached.borrow_mut();
      // Update if more than 1 second has passed
      if cached.last_update.elapsed() >= Duration::from_secs(1) {
        cached.header_value = format_now();
        cached.last_update = Instant::now();
      }
      cached.header_value.clone()
    })
  }
}

pub use cached_date::http_date_header_value;
