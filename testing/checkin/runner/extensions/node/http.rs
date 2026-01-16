use std::{
  cell::RefCell,
  collections::HashMap,
  collections::VecDeque,
  error::Error as _,
  io::ErrorKind,
  pin::Pin,
  rc::Rc,
  sync::atomic::{AtomicBool, Ordering},
  sync::{Arc, Mutex},
  task::{Context, Poll, Waker},
};

use bytes::Bytes;
use deno_core::JsBuffer;
use deno_core::convert::Uint8Array;
use deno_core::error::JsError;
use deno_core::serde;
use deno_core::v8::cppgc::{GcCell, Traced};
use deno_core::{GarbageCollected, OpState, ToV8, op2, v8};
use deno_error::JsErrorBox;
use futures::StreamExt;
use http_body::Body;
use http_body::Frame;
use http_body::SizeHint;
use http_body_util::BodyDataStream;
use http_body_util::Either;
use http_body_util::Full;
use hyper::Request;
use hyper::Response;
use hyper::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Version};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use tokio::sync::Notify;
use tokio::sync::oneshot;

use crate::checkin::runner::extensions::node::ScopeHolder;

use super::Constructors;
use super::net::Server;
use super::net::{EventEmitter, OnAccept, ServerInner, SocketCb};

#[derive(deno_core::CppgcInherits)]
#[cppgc_base(Server)]
#[repr(C)]
pub struct HttpServer {
  base: Server,
  inner: Rc<HttpServerInner>,
}

pub struct HttpServerInner {}

unsafe impl GarbageCollected for HttpServer {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.base.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Server"
  }
}

#[op2(inherit = Server)]
impl HttpServer {
  #[constructor]
  #[cppgc]
  fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> HttpServer {
    HttpServer {
      base: Server::new_inner(me, scope, op_state),
      inner: Rc::new(HttpServerInner {}),
    }
  }

  #[fast]
  fn listen(
    &self,
    #[smi] port: u16,
    #[string] host: String,
    scope: &mut v8::PinScope,
    on_listen: Option<v8::Local<v8::Function>>,
  ) {
    if let Some(on_listen) = on_listen {
      self.base.inner.on_event(
        scope,
        &[internalized(scope, "listening").into(), on_listen.into()],
      );
    }
    self
      .base
      .inner
      .listen_inner::<HttpServerCallback>(port, host);
  }
}

struct HttpServerCallback;
fn is_connection_closed(err: &std::io::Error) -> bool {
  matches!(
    err.kind(),
    ErrorKind::ConnectionReset
      | ErrorKind::ConnectionAborted
      | ErrorKind::BrokenPipe
      | ErrorKind::UnexpectedEof
  )
}

struct ShouldReadState {
  should_read: AtomicBool,
  notify: Notify,
}

impl ShouldReadState {
  fn new() -> Self {
    Self {
      should_read: AtomicBool::new(true),
      notify: Notify::new(),
    }
  }

  fn set_should_read(&self) {
    if !self.should_read.swap(true, Ordering::Relaxed) {
      self.notify.notify_waiters();
    }
  }

  fn clear_should_read(&self) {
    self.should_read.store(false, Ordering::Relaxed);
  }

  async fn wait_for_should_read(&self) {
    if self.should_read.load(Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

const RESPONSE_BODY_HIGH_WATER: usize = 64 * 1024;

struct ResponseBodyState {
  queue: VecDeque<Bytes>,
  closed: bool,
  pending_bytes: usize,
  waker: Option<Waker>,
}

#[derive(Clone)]
struct ResponseBodyHandle {
  inner: Arc<Mutex<ResponseBodyState>>,
  drain_notify: Arc<Notify>,
}

struct ResponseBody {
  inner: Arc<Mutex<ResponseBodyState>>,
  drain_notify: Arc<Notify>,
}

type HttpResponseBody = Either<Full<Bytes>, ResponseBody>;

fn response_body_pair() -> (ResponseBodyHandle, ResponseBody) {
  let inner = Arc::new(Mutex::new(ResponseBodyState {
    queue: VecDeque::new(),
    closed: false,
    pending_bytes: 0,
    waker: None,
  }));
  let drain_notify = Arc::new(Notify::new());
  (
    ResponseBodyHandle {
      inner: inner.clone(),
      drain_notify: drain_notify.clone(),
    },
    ResponseBody {
      inner,
      drain_notify,
    },
  )
}

impl ResponseBodyHandle {
  fn push_bytes(&self, bytes: Bytes) -> Result<usize, JsErrorBox> {
    if bytes.is_empty() {
      return Ok(0);
    }
    let (pending, waker) = {
      let mut state = self.inner.lock().unwrap();
      if state.closed {
        return Err(JsErrorBox::generic("Response body closed"));
      }
      state.pending_bytes = state.pending_bytes.saturating_add(bytes.len());
      state.queue.push_back(bytes);
      (state.pending_bytes, state.waker.take())
    };
    if let Some(waker) = waker {
      waker.wake();
    }
    Ok(pending)
  }

  fn close(&self) {
    let waker = {
      let mut state = self.inner.lock().unwrap();
      state.closed = true;
      state.waker.take()
    };
    if let Some(waker) = waker {
      waker.wake();
    }
    self.drain_notify.notify_waiters();
  }

  async fn wait_for_drain(&self, target: usize) {
    loop {
      let pending = {
        let state = self.inner.lock().unwrap();
        if state.closed || state.pending_bytes <= target {
          return;
        }
        state.pending_bytes
      };
      let _ = pending;
      self.drain_notify.notified().await;
    }
  }
}

impl Body for ResponseBody {
  type Data = Bytes;
  type Error = hyper::Error;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    let (frame, closed, pending_bytes) = {
      let mut state = self.inner.lock().unwrap();
      if let Some(bytes) = state.queue.pop_front() {
        state.pending_bytes = state.pending_bytes.saturating_sub(bytes.len());
        let pending_bytes = state.pending_bytes;
        let frame = Frame::data(bytes);
        (Some(Ok(frame)), state.closed, pending_bytes)
      } else {
        if state.closed {
          (None, true, state.pending_bytes)
        } else {
          state.waker = Some(cx.waker().clone());
          return Poll::Pending;
        }
      }
    };

    if pending_bytes <= RESPONSE_BODY_HIGH_WATER {
      self.drain_notify.notify_waiters();
    }

    if closed && frame.is_none() {
      return Poll::Ready(None);
    }
    Poll::Ready(frame)
  }

  fn is_end_stream(&self) -> bool {
    let state = self.inner.lock().unwrap();
    state.closed && state.queue.is_empty()
  }

  fn size_hint(&self) -> SizeHint {
    SizeHint::new()
  }
}

async fn push_chunk_with_backpressure(
  inner: &Rc<ServerInner>,
  req_handle: &Rc<v8::Global<v8::Object>>,
  push_handle: &Rc<v8::Global<v8::Function>>,
  data: Vec<u8>,
) -> bool {
  if data.is_empty() {
    return true;
  }
  let (tx, rx) = oneshot::channel();
  let inner = inner.clone();
  let req_handle = req_handle.clone();
  let push_handle = push_handle.clone();
  inner.with_scope(move |scope| {
    v8::tc_scope!(let scope, scope);
    let req_obj = v8::Local::<v8::Object>::new(scope, &*req_handle);
    let push = v8::Local::<v8::Function>::new(scope, &*push_handle);
    let data = Uint8Array(data);
    let arg = data.to_v8(scope).map_err(JsErrorBox::from_err).unwrap();
    let result = push.call(scope, req_obj.into(), &[arg]);
    let ok = result
      .and_then(|value| value.try_cast::<v8::Boolean>().ok())
      .map(|value| value.is_true())
      .unwrap_or(true);
    let _ = tx.send(ok);
  });
  rx.await.unwrap_or(true)
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

fn finish_request(
  inner: Rc<ServerInner>,
  req_handle: Rc<v8::Global<v8::Object>>,
  push_handle: Rc<v8::Global<v8::Function>>,
  mark_complete: bool,
  aborted: bool,
) {
  inner.with_scope(move |scope| {
    v8::tc_scope!(let scope, scope);
    let req_obj = v8::Local::<v8::Object>::new(scope, &*req_handle);
    let push = v8::Local::<v8::Function>::new(scope, &*push_handle);

    if mark_complete || aborted {
      if let Some(req_obj) = deno_core::cppgc::try_unwrap_cppgc_object::<
        IncomingMessage,
      >(scope, req_obj.into())
      {
        if mark_complete {
          unsafe { req_obj.as_ref() }.inner.borrow_mut().complete = true;
        }
        if aborted {
          unsafe { req_obj.as_ref() }.inner.borrow_mut().aborted = true;
        }
      } else {
        if mark_complete {
          let _ = req_obj.set(
            scope,
            internalized(scope, "complete").into(),
            v8::Boolean::new(scope, true).into(),
          );
        }
        if aborted {
          let _ = req_obj.set(
            scope,
            internalized(scope, "aborted").into(),
            v8::Boolean::new(scope, true).into(),
          );
        }
      }
    }
    let _ = push.call(scope, req_obj.into(), &[v8::null(scope).into()]);
  });
}

struct RequestParts {
  method: hyper::http::Method,
  uri: hyper::http::Uri,
  version: Version,
  headers: HeaderMap,
}

struct V8Cached<T> {
  value: Option<T>,
  v8: Option<v8::Global<v8::Value>>,
}

impl<T> V8Cached<T> {
  fn new() -> Self {
    Self {
      value: None,
      v8: None,
    }
  }

  fn is_initialized(&self) -> bool {
    self.value.is_some()
  }

  fn set(&mut self, value: T) {
    self.value = Some(value);
    self.v8 = None;
  }

  fn get_or_init_v8_with<'a>(
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

  fn get_or_init_v8<'a>(
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

struct LazySocket {
  inner: Rc<ServerInner>,
  #[cfg(unix)]
  raw_fd: RawFd,
  host: Option<String>,
  port: Option<u16>,
  socket_obj: RefCell<Option<v8::Global<v8::Object>>>,
}

impl LazySocket {
  fn new(
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

  fn get_or_create<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Object>, JsErrorBox> {
    if let Some(socket_obj) = self.socket_obj.borrow().as_ref() {
      return Ok(v8::Local::new(scope, socket_obj));
    }
    let stream = self.dup_stream()?;
    let socket_obj =
      deno_core::cppgc::make_cppgc_empty_object::<SocketCb>(scope);
    let socket = SocketCb::new_server(
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

fn should_close_from_parts(headers: &HeaderMap, version: Version) -> bool {
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

fn upgrade_from_parts(headers: &HeaderMap) -> bool {
  if let Some(value) = headers.get("connection") {
    if let Ok(value) = value.to_str() {
      if value.to_ascii_lowercase().contains("upgrade") {
        return true;
      }
    }
  }
  headers.contains_key("upgrade")
}

fn ensure_method(inner: &mut IncomingMessageInner) {
  if inner.method.is_some() {
    return;
  }
  if let Some(parts) = inner.parts.as_ref() {
    inner.method = Some(parts.method.as_str().to_string());
  }
}

fn ensure_url(inner: &mut IncomingMessageInner) {
  if inner.url.is_some() {
    return;
  }
  if let Some(parts) = inner.parts.as_ref() {
    inner.url = Some(parts.uri.to_string());
  }
}

fn ensure_version(inner: &mut IncomingMessageInner) {
  if inner.http_version.is_some() {
    return;
  }
  if let Some(parts) = inner.parts.as_ref() {
    let (http_version_major, http_version_minor, http_version) =
      match parts.version {
        Version::HTTP_10 => (1, 0, "1.0".to_string()),
        Version::HTTP_11 => (1, 1, "1.1".to_string()),
        Version::HTTP_2 => (2, 0, "2.0".to_string()),
        Version::HTTP_3 => (3, 0, "3.0".to_string()),
        _ => (1, 1, "1.1".to_string()),
      };
    inner.http_version_major = http_version_major;
    inner.http_version_minor = http_version_minor;
    inner.http_version = Some(http_version);
  }
}

fn ensure_headers(inner: &mut IncomingMessageInner) {
  if inner.headers.is_initialized()
    && inner.raw_headers.is_initialized()
    && inner.headers_distinct.is_initialized()
  {
    return;
  }
  let mut headers = HashMap::new();
  let mut raw_headers = Vec::new();
  let mut headers_distinct = HashMap::new();
  if let Some(parts) = inner.parts.as_ref() {
    for (name, value) in parts.headers.iter() {
      let name_str = name.as_str().to_string();
      let value_str = String::from_utf8_lossy(value.as_bytes()).to_string();
      raw_headers.push(name_str.clone());
      raw_headers.push(value_str.clone());
      let lower = name_str.to_ascii_lowercase();
      headers_distinct
        .entry(lower.clone())
        .or_insert_with(Vec::new)
        .push(value_str.clone());
      headers
        .entry(lower)
        .and_modify(|existing: &mut String| {
          if !existing.is_empty() {
            existing.push_str(", ");
          }
          existing.push_str(&value_str);
        })
        .or_insert(value_str);
    }
  }
  inner.headers.set(headers);
  inner.raw_headers.set(raw_headers);
  inner.headers_distinct.set(headers_distinct);
}

fn ensure_upgrade(inner: &mut IncomingMessageInner) {
  if inner.upgrade.is_some() {
    return;
  }
  let upgrade = inner
    .parts
    .as_ref()
    .map(|parts| upgrade_from_parts(&parts.headers))
    .unwrap_or(false);
  inner.upgrade = Some(upgrade);
}

fn init_request_objects(
  inner: &Rc<ServerInner>,
  parts: hyper::http::request::Parts,
  has_body: bool,
  should_read: Rc<ShouldReadState>,
  response_tx: oneshot::Sender<Response<HttpResponseBody>>,
  should_close: bool,
  socket_state: Rc<LazySocket>,
) -> (Rc<v8::Global<v8::Object>>, Rc<v8::Global<v8::Function>>) {
  let request_parts = RequestParts {
    method: parts.method,
    uri: parts.uri,
    version: parts.version,
    headers: parts.headers,
  };

  let req_slot: Rc<RefCell<Option<v8::Global<v8::Object>>>> =
    Rc::new(RefCell::new(None));
  let push_slot: Rc<RefCell<Option<v8::Global<v8::Function>>>> =
    Rc::new(RefCell::new(None));

  inner.with_scope_immediately({
    let inner = inner.clone();
    let req_slot = req_slot.clone();
    let push_slot = push_slot.clone();
    let should_read = should_read.clone();
    move |scope| {
      v8::tc_scope!(let scope, scope);
      let req_empty =
        deno_core::cppgc::make_cppgc_empty_object::<IncomingMessage>(scope);
      let req = IncomingMessage::new_inner(
        v8::Global::new(scope, req_empty),
        scope,
        inner.op_state(),
        should_read,
      );
      {
        let mut inner = req.inner.borrow_mut();
        inner.parts = Some(request_parts);
        inner.complete = !has_body;
      }
      let req_obj = deno_core::cppgc::wrap_object(scope, req_empty, req);

      let res_empty =
        deno_core::cppgc::make_cppgc_empty_object::<ServerResponse>(scope);
      let res = ServerResponse::new_inner(
        v8::Global::new(scope, res_empty),
        scope,
        inner.op_state(),
        Some(response_tx),
        None,
        should_close,
        Some(socket_state.clone()),
      );
      if should_close {
        res.base.set_header_internal(scope, "Connection", "close");
      }
      let res_obj = deno_core::cppgc::wrap_object(scope, res_empty, res);

      let request_event = internalized(scope, "request");
      inner.emit_event(
        scope,
        &[request_event.into(), req_obj.into(), res_obj.into()],
      );

      let push = req_obj
        .get(scope, internalized(scope, "push").into())
        .unwrap()
        .cast::<v8::Function>();
      *req_slot.borrow_mut() = Some(v8::Global::new(scope, req_obj));
      *push_slot.borrow_mut() = Some(v8::Global::new(scope, push));
    }
  });

  (
    Rc::new(req_slot.borrow_mut().take().unwrap()),
    Rc::new(push_slot.borrow_mut().take().unwrap()),
  )
}

fn spawn_request_body_stream(
  inner: Rc<ServerInner>,
  body: hyper::body::Incoming,
  should_read: Rc<ShouldReadState>,
  req_handle: Rc<v8::Global<v8::Object>>,
  push_handle: Rc<v8::Global<v8::Function>>,
) {
  deno_core::unsync::spawn(async move {
    let mut body_stream = BodyDataStream::new(body);
    loop {
      should_read.wait_for_should_read().await;
      let chunk = match body_stream.next().await {
        Some(chunk) => chunk,
        None => {
          finish_request(inner, req_handle, push_handle, true, false);
          break;
        }
      };
      match chunk {
        Ok(bytes) => {
          let ok = push_chunk_with_backpressure(
            &inner,
            &req_handle,
            &push_handle,
            bytes.to_vec(),
          )
          .await;
          if !ok {
            should_read.clear_should_read();
          }
        }
        Err(_) => {
          finish_request(inner, req_handle, push_handle, false, true);
          break;
        }
      }
    }
  });
}

async fn await_response(
  response_rx: oneshot::Receiver<Response<HttpResponseBody>>,
) -> Response<HttpResponseBody> {
  match response_rx.await {
    Ok(response) => response,
    Err(_) => {
      let mut response = Response::new(Either::Left(Full::new(Bytes::new())));
      *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
      response
    }
  }
}

async fn handle_hyper_request(
  inner: Rc<ServerInner>,
  socket_state: Rc<LazySocket>,
  req: Request<hyper::body::Incoming>,
) -> Result<Response<HttpResponseBody>, hyper::Error> {
  let (parts, body) = req.into_parts();
  let should_close = should_close_from_parts(&parts.headers, parts.version);
  let has_body = !body.is_end_stream();
  let should_read = Rc::new(ShouldReadState::new());

  let (response_tx, response_rx) = oneshot::channel();

  let (req_handle, push_handle) = init_request_objects(
    &inner,
    parts,
    has_body,
    should_read.clone(),
    response_tx,
    should_close,
    socket_state,
  );

  if has_body {
    spawn_request_body_stream(
      inner.clone(),
      body,
      should_read,
      req_handle.clone(),
      push_handle.clone(),
    );
  } else {
    finish_request(
      inner.clone(),
      req_handle.clone(),
      push_handle.clone(),
      true,
      false,
    );
  }

  let response = await_response(response_rx).await;

  Ok(response)
}

impl OnAccept for HttpServerCallback {
  async fn on_accept(
    inner: &Rc<ServerInner>,
    stream: tokio::net::TcpStream,
    _addr: std::net::SocketAddr,
  ) -> Result<(), JsErrorBox> {
    let socket_state = Rc::new(LazySocket::new(inner.clone(), &stream, _addr));
    let service = {
      let inner = inner.clone();
      let socket_state = socket_state.clone();
      service_fn(move |req| {
        handle_hyper_request(inner.clone(), socket_state.clone(), req)
      })
    };
    let result = http1::Builder::new()
      .serve_connection(TokioIo::new(stream), service)
      .await;
    match result {
      Ok(()) => Ok(()),
      Err(err) => {
        if err.is_closed()
          || err.is_incomplete_message()
          || err.is_shutdown()
          || err.is_canceled()
        {
          return Ok(());
        }
        if let Some(io_err) = err
          .source()
          .and_then(|source| source.downcast_ref::<std::io::Error>())
        {
          if is_connection_closed(io_err) {
            return Ok(());
          }
        }
        Err(JsErrorBox::generic(format!("hyper error: {err}")))
      }
    }
  }
}
/// IncomingMessage represents an incoming HTTP request (for server) or response (for client).
/// This is a Readable stream that wraps the incoming HTTP message data.
///
/// Fields based on Node.js http.IncomingMessage:
/// - socket/client: the underlying socket
/// - method: HTTP method (for server requests)
/// - url: request URL (for server requests)
/// - statusCode/statusMessage: response status (for client responses)
/// - httpVersion, httpVersionMajor, httpVersionMinor
/// - headers, rawHeaders, headersDistinct
/// - trailers, rawTrailers, trailersDistinct
/// - complete, aborted, upgrade
/// - joinDuplicateHeaders
pub struct IncomingMessage {
  inner: RefCell<IncomingMessageInner>,
  /// Backpressure signal from Readable
  should_read: Rc<ShouldReadState>,
}

struct IncomingMessageInner {
  parts: Option<RequestParts>,
  /// HTTP method (GET, POST, etc.) - only for server-side requests
  method: Option<String>,
  /// Request URL - only for server-side requests
  url: Option<String>,
  /// HTTP status code - only for client-side responses
  status_code: Option<u16>,
  /// HTTP status message - only for client-side responses
  status_message: Option<String>,
  /// HTTP version string (e.g., "1.1")
  http_version: Option<String>,
  /// Major HTTP version number
  http_version_major: u8,
  /// Minor HTTP version number
  http_version_minor: u8,
  /// Parsed headers (lowercase keys, values joined according to spec)
  headers: V8Cached<HashMap<String, String>>,
  /// Raw headers as alternating key/value pairs
  raw_headers: V8Cached<Vec<String>>,
  /// Headers with distinct values (array for each key)
  headers_distinct: V8Cached<HashMap<String, Vec<String>>>,
  /// Parsed trailers
  trailers: HashMap<String, String>,
  /// Raw trailers as alternating key/value pairs
  raw_trailers: Vec<String>,
  /// Trailers with distinct values
  trailers_distinct: HashMap<String, Vec<String>>,
  /// Whether the message has been fully received
  complete: bool,
  /// Whether the request was aborted
  aborted: bool,
  /// Whether this is an upgrade request
  upgrade: Option<bool>,
  /// Whether to join duplicate headers
  join_duplicate_headers: bool,
}

unsafe impl GarbageCollected for IncomingMessage {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"IncomingMessage"
  }
}

#[op2]
impl IncomingMessage {
  #[constructor]
  #[cppgc]
  fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> IncomingMessage {
    IncomingMessage::new_inner(
      me,
      scope,
      op_state,
      Rc::new(ShouldReadState::new()),
    )
  }

  // --- Getters ---

  #[getter]
  #[string]
  fn method(&self, _isolate: &v8::Isolate) -> Option<String> {
    let mut inner = self.inner.borrow_mut();
    ensure_method(&mut inner);
    inner.method.clone()
  }

  #[getter]
  #[string]
  fn url(&self, _isolate: &v8::Isolate) -> Option<String> {
    let mut inner = self.inner.borrow_mut();
    ensure_url(&mut inner);
    inner.url.clone()
  }

  #[getter]
  #[smi]
  fn status_code(&self, _isolate: &v8::Isolate) -> Option<u16> {
    self.inner.borrow().status_code
  }

  #[getter]
  #[string]
  fn status_message(&self, _isolate: &v8::Isolate) -> Option<String> {
    self.inner.borrow().status_message.clone()
  }

  #[getter]
  #[string]
  fn http_version(&self, _isolate: &v8::Isolate) -> String {
    let mut inner = self.inner.borrow_mut();
    ensure_version(&mut inner);
    inner
      .http_version
      .clone()
      .unwrap_or_else(|| "1.1".to_string())
  }

  #[getter]
  #[smi]
  fn http_version_major(&self, _isolate: &v8::Isolate) -> u8 {
    let mut inner = self.inner.borrow_mut();
    ensure_version(&mut inner);
    inner.http_version_major
  }

  #[getter]
  #[smi]
  fn http_version_minor(&self, _isolate: &v8::Isolate) -> u8 {
    let mut inner = self.inner.borrow_mut();
    ensure_version(&mut inner);
    inner.http_version_minor
  }

  #[getter]
  fn headers<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox> {
    let mut inner = self.inner.borrow_mut();
    ensure_headers(&mut inner);
    inner
      .headers
      .get_or_init_v8_with(scope, HashMap::new, to_v8_map)
  }

  #[getter]
  #[rename("rawHeaders")]
  fn raw_headers<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox> {
    let mut inner = self.inner.borrow_mut();
    ensure_headers(&mut inner);
    inner.raw_headers.get_or_init_v8(scope, Vec::new)
  }

  #[getter]
  #[rename("headersDistinct")]
  fn headers_distinct<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox> {
    let mut inner = self.inner.borrow_mut();
    ensure_headers(&mut inner);
    inner.headers_distinct.get_or_init_v8(scope, HashMap::new)
  }

  #[getter]
  #[serde]
  fn trailers(&self, _isolate: &v8::Isolate) -> HashMap<String, String> {
    self.inner.borrow().trailers.clone()
  }

  #[getter]
  #[serde]
  #[rename("rawTrailers")]
  fn raw_trailers(&self, _isolate: &v8::Isolate) -> Vec<String> {
    self.inner.borrow().raw_trailers.clone()
  }

  #[getter]
  #[serde]
  #[rename("trailersDistinct")]
  fn trailers_distinct(
    &self,
    _isolate: &v8::Isolate,
  ) -> HashMap<String, Vec<String>> {
    self.inner.borrow().trailers_distinct.clone()
  }

  #[getter]
  fn complete(&self, _isolate: &v8::Isolate) -> bool {
    self.inner.borrow().complete
  }

  #[getter]
  fn aborted(&self, _isolate: &v8::Isolate) -> bool {
    self.inner.borrow().aborted
  }

  #[getter]
  fn upgrade(&self, _isolate: &v8::Isolate) -> bool {
    let mut inner = self.inner.borrow_mut();
    ensure_upgrade(&mut inner);
    inner.upgrade.unwrap_or(false)
  }

  #[getter]
  fn join_duplicate_headers(&self, _isolate: &v8::Isolate) -> bool {
    self.inner.borrow().join_duplicate_headers
  }

  // --- Setters ---

  #[setter]
  fn method(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.inner.borrow_mut().method = value;
  }

  #[setter]
  fn url(&self, _isolate: &mut v8::Isolate, #[string] value: Option<String>) {
    self.inner.borrow_mut().url = value;
  }

  #[setter]
  fn status_code(&self, _isolate: &mut v8::Isolate, #[smi] value: Option<u16>) {
    self.inner.borrow_mut().status_code = value;
  }

  #[setter]
  fn status_message(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.inner.borrow_mut().status_message = value;
  }

  #[setter]
  fn http_version(&self, _isolate: &mut v8::Isolate, #[string] value: String) {
    let mut inner = self.inner.borrow_mut();
    inner.http_version = Some(value);
  }

  #[setter]
  fn http_version_major(&self, _isolate: &mut v8::Isolate, #[smi] value: u8) {
    self.inner.borrow_mut().http_version_major = value;
  }

  #[setter]
  fn http_version_minor(&self, _isolate: &mut v8::Isolate, #[smi] value: u8) {
    self.inner.borrow_mut().http_version_minor = value;
  }

  #[setter]
  fn complete(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().complete = value;
  }

  #[setter]
  fn aborted(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().aborted = value;
  }

  #[setter]
  fn upgrade(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().upgrade = Some(value);
  }

  #[setter]
  fn join_duplicate_headers(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().join_duplicate_headers = value;
  }

  // --- Methods ---

  #[fast]
  #[rename("_read")]
  fn read(&self) {
    self.should_read.set_should_read();
  }

  #[fast]
  #[rename("_destroy")]
  fn destroy(&self, _isolate: &mut v8::Isolate) {
    // Mark as aborted if not complete
    let mut inner = self.inner.borrow_mut();
    if !inner.complete {
      inner.aborted = true;
    }
  }

  #[fast]
  #[rename("_dump")]
  fn dump(&self) {
    // Placeholder - drains unread data in full implementation
  }
}

impl IncomingMessage {
  fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    should_read: Rc<ShouldReadState>,
  ) -> IncomingMessage {
    let super_cons = {
      let op_state = op_state.borrow();
      op_state.borrow::<Constructors>().clone()
    };
    let local_me = v8::Local::new(scope, &me);
    let cons = super_cons.readable(scope);
    cons.call(scope, local_me.into(), &[]).unwrap();
    IncomingMessage {
      inner: RefCell::new(IncomingMessageInner {
        parts: None,
        method: None,
        url: None,
        status_code: None,
        status_message: None,
        http_version: None,
        http_version_major: 1,
        http_version_minor: 1,
        headers: V8Cached::new(),
        raw_headers: V8Cached::new(),
        headers_distinct: V8Cached::new(),
        trailers: HashMap::new(),
        raw_trailers: Vec::new(),
        trailers_distinct: HashMap::new(),
        complete: false,
        aborted: false,
        upgrade: None,
        join_duplicate_headers: false,
      }),
      should_read,
    }
  }
}

/// OutgoingMessage is the base class for ServerResponse and ClientRequest.
/// It represents an outgoing HTTP message being sent.
///
/// Fields based on Node.js http.OutgoingMessage:
/// - _header, _headerSent, finished
/// - chunkedEncoding, _contentLength
/// - shouldKeepAlive, _last, _trailer
/// - kOutHeaders map
/// - strictContentLength, joinDuplicateHeaders
/// - highWaterMark, _closed, writable, destroyed
#[derive(deno_core::CppgcBase)]
#[repr(C)]
pub struct OutgoingMessage {
  /// The first line + headers as a string
  header: GcCell<Option<String>>,
  /// Whether headers have been sent
  header_sent: GcCell<bool>,
  /// Whether the response has finished
  finished: GcCell<bool>,
  /// Whether chunked transfer encoding is used
  chunked_encoding: GcCell<bool>,
  /// Content-Length if set
  content_length: GcCell<Option<u64>>,
  /// Whether to keep the connection alive
  should_keep_alive: GcCell<bool>,
  /// Whether this is the last message on the connection
  last: GcCell<bool>,
  /// Trailer headers
  trailer: GcCell<String>,
  /// Headers to be sent (key -> [name, value] pairs)
  out_headers: GcCell<HashMap<String, (String, String)>>,
  /// Whether to strictly enforce Content-Length
  strict_content_length: GcCell<bool>,
  /// Whether to join duplicate headers
  join_duplicate_headers: GcCell<bool>,
  /// Whether the stream is closed
  closed: GcCell<bool>,
  /// Whether the stream is writable
  writable: GcCell<bool>,
  /// Whether the stream is destroyed
  destroyed: GcCell<bool>,
  /// Send date header automatically
  send_date: GcCell<bool>,
  /// Buffered first body chunk (used to decide Content-Length vs chunked)
  pending_body: GcCell<Option<Vec<u8>>>,
}

unsafe impl GarbageCollected for OutgoingMessage {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"OutgoingMessage"
  }
}

#[op2]
impl OutgoingMessage {
  #[constructor]
  #[cppgc]
  fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> OutgoingMessage {
    OutgoingMessage::new_inner(me, scope, op_state)
  }

  // --- Getters ---

  #[getter]
  #[rename("_header")]
  #[string]
  fn header(&self, isolate: &v8::Isolate) -> Option<String> {
    self.header.get(isolate).clone()
  }

  #[getter]
  #[rename("_headerSent")]
  fn header_sent(&self, isolate: &v8::Isolate) -> bool {
    *self.header_sent.get(isolate)
  }

  #[getter]
  fn finished(&self, isolate: &v8::Isolate) -> bool {
    *self.finished.get(isolate)
  }

  #[getter]
  fn chunked_encoding(&self, isolate: &v8::Isolate) -> bool {
    *self.chunked_encoding.get(isolate)
  }

  #[getter]
  #[rename("_contentLength")]
  fn content_length(&self, isolate: &v8::Isolate) -> Option<u32> {
    self.content_length.get(isolate).map(|v| v as u32)
  }

  #[getter]
  fn should_keep_alive(&self, isolate: &v8::Isolate) -> bool {
    *self.should_keep_alive.get(isolate)
  }

  #[getter]
  #[rename("_last")]
  fn last(&self, isolate: &v8::Isolate) -> bool {
    *self.last.get(isolate)
  }

  #[getter]
  fn strict_content_length(&self, isolate: &v8::Isolate) -> bool {
    *self.strict_content_length.get(isolate)
  }

  #[getter]
  fn join_duplicate_headers(&self, isolate: &v8::Isolate) -> bool {
    *self.join_duplicate_headers.get(isolate)
  }

  #[getter]
  #[rename("_closed")]
  fn closed(&self, isolate: &v8::Isolate) -> bool {
    *self.closed.get(isolate)
  }

  #[getter]
  fn writable(&self, isolate: &v8::Isolate) -> bool {
    *self.writable.get(isolate)
  }

  #[getter]
  fn destroyed(&self, isolate: &v8::Isolate) -> bool {
    *self.destroyed.get(isolate)
  }

  #[getter]
  fn send_date(&self, isolate: &v8::Isolate) -> bool {
    *self.send_date.get(isolate)
  }

  // --- Setters ---

  #[rename("_header")]
  #[setter]
  fn header(&self, isolate: &mut v8::Isolate, #[string] value: Option<String>) {
    self.header.set(isolate, value);
  }

  #[rename("_headerSent")]
  #[setter]
  fn header_sent(&self, isolate: &mut v8::Isolate, value: bool) {
    self.header_sent.set(isolate, value);
  }

  #[setter]
  fn finished(&self, isolate: &mut v8::Isolate, value: bool) {
    self.finished.set(isolate, value);
  }

  #[setter]
  fn chunked_encoding(&self, isolate: &mut v8::Isolate, value: bool) {
    self.chunked_encoding.set(isolate, value);
  }

  #[rename("_contentLength")]
  #[setter]
  fn content_length(
    &self,
    isolate: &mut v8::Isolate,
    #[number] value: Option<u64>,
  ) {
    self.content_length.set(isolate, value);
  }

  #[setter]
  fn should_keep_alive(&self, isolate: &mut v8::Isolate, value: bool) {
    self.should_keep_alive.set(isolate, value);
  }

  #[rename("_last")]
  #[setter]
  fn last(&self, isolate: &mut v8::Isolate, value: bool) {
    self.last.set(isolate, value);
  }

  #[setter]
  fn strict_content_length(&self, isolate: &mut v8::Isolate, value: bool) {
    self.strict_content_length.set(isolate, value);
  }

  #[setter]
  fn join_duplicate_headers(&self, isolate: &mut v8::Isolate, value: bool) {
    self.join_duplicate_headers.set(isolate, value);
  }

  #[rename("_closed")]
  #[setter]
  fn closed(&self, isolate: &mut v8::Isolate, value: bool) {
    self.closed.set(isolate, value);
  }

  #[setter]
  fn writable(&self, isolate: &mut v8::Isolate, value: bool) {
    self.writable.set(isolate, value);
  }

  #[setter]
  fn destroyed(&self, isolate: &mut v8::Isolate, value: bool) {
    self.destroyed.set(isolate, value);
  }

  #[setter]
  fn send_date(&self, isolate: &mut v8::Isolate, value: bool) {
    self.send_date.set(isolate, value);
  }

  // --- Methods ---

  #[fast]
  fn set_header(
    &self,
    isolate: &mut v8::Isolate,
    #[string] name: String,
    #[string] value: String,
  ) {
    let lowercase_name = name.to_lowercase();
    let mut headers = self.out_headers.get(isolate).clone();
    headers.insert(lowercase_name, (name, value));
    self.out_headers.set(isolate, headers);
  }

  #[string]
  fn get_header(
    &self,
    isolate: &v8::Isolate,
    #[string] name: String,
  ) -> Option<String> {
    let lowercase_name = name.to_lowercase();
    self
      .out_headers
      .get(isolate)
      .get(&lowercase_name)
      .map(|(_, v)| v.clone())
  }

  #[fast]
  fn has_header(&self, isolate: &v8::Isolate, #[string] name: String) -> bool {
    let lowercase_name = name.to_lowercase();
    self.out_headers.get(isolate).contains_key(&lowercase_name)
  }

  #[fast]
  fn remove_header(&self, isolate: &mut v8::Isolate, #[string] name: String) {
    let lowercase_name = name.to_lowercase();
    let mut headers = self.out_headers.get(isolate).clone();
    headers.remove(&lowercase_name);
    self.out_headers.set(isolate, headers);
  }

  #[fast]
  fn flush_headers(&self, isolate: &mut v8::Isolate) {
    // Mark headers as sent
    self.header_sent.set(isolate, true);
  }

  #[fast]
  #[rename("_storeHeader")]
  fn store_header_op(
    &self,
    isolate: &mut v8::Isolate,
    #[string] first_line: String,
  ) -> Result<(), JsErrorBox> {
    self.store_header(isolate, &first_line)
  }

  #[fast]
  #[rename("_destroy")]
  fn destroy(&self, isolate: &mut v8::Isolate) {
    self.destroyed.set(isolate, true);
    self.writable.set(isolate, false);
  }
}

impl OutgoingMessage {
  fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> OutgoingMessage {
    let super_cons = {
      let op_state = op_state.borrow();
      op_state.borrow::<Constructors>().clone()
    };
    let local_me = v8::Local::new(scope, &me);
    let cons = super_cons.writable(scope);
    cons.call(scope, local_me.into(), &[]).unwrap();
    OutgoingMessage {
      header: GcCell::new(None),
      header_sent: GcCell::new(false),
      finished: GcCell::new(false),
      chunked_encoding: GcCell::new(false),
      content_length: GcCell::new(None),
      should_keep_alive: GcCell::new(true),
      last: GcCell::new(false),
      trailer: GcCell::new(String::new()),
      out_headers: GcCell::new(HashMap::new()),
      strict_content_length: GcCell::new(false),
      join_duplicate_headers: GcCell::new(false),
      closed: GcCell::new(false),
      writable: GcCell::new(true),
      destroyed: GcCell::new(false),
      send_date: GcCell::new(true),
      pending_body: GcCell::new(None),
    }
  }

  fn render_header(
    &self,
    isolate: &mut v8::Isolate,
    first_line: &str,
  ) -> Result<String, JsErrorBox> {
    let mut header = first_line.to_string();
    if !header.ends_with("\r\n") {
      header.push_str("\r\n");
    }
    let headers = self.out_headers.get(isolate).clone();
    let mut header_map = HeaderMap::new();
    for (_, (name, value)) in headers.iter() {
      let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| JsErrorBox::type_error("Invalid header name"))?;
      let value = HeaderValue::from_str(value)
        .map_err(|_| JsErrorBox::type_error("Invalid header value"))?;
      header_map.append(name, value);
    }
    for (name, value) in header_map.iter() {
      header.push_str(name.as_str());
      header.push_str(": ");
      header.push_str(value.to_str().unwrap_or_default());
      header.push_str("\r\n");
    }
    header.push_str("\r\n");
    Ok(header)
  }

  fn store_header(
    &self,
    isolate: &mut v8::Isolate,
    first_line: &str,
  ) -> Result<(), JsErrorBox> {
    let header = self.render_header(isolate, first_line)?;
    self.header.set(isolate, Some(header));
    Ok(())
  }

  fn has_header(&self, isolate: &v8::Isolate, name: &str) -> bool {
    self.out_headers.get(isolate).contains_key(name)
  }

  fn set_header_internal(
    &self,
    isolate: &mut v8::Isolate,
    name: &str,
    value: &str,
  ) {
    let mut headers = self.out_headers.get(isolate).clone();
    headers.insert(
      name.to_ascii_lowercase(),
      (name.to_string(), value.to_string()),
    );
    self.out_headers.set(isolate, headers);
  }

  fn clear_header_cache(&self, isolate: &mut v8::Isolate) {
    if self.header.get(isolate).is_some() {
      self.header.set(isolate, None);
    }
  }

  fn set_content_length(&self, isolate: &mut v8::Isolate, len: usize) {
    let mut headers = self.out_headers.get(isolate).clone();
    headers.insert(
      "content-length".to_string(),
      ("Content-Length".to_string(), len.to_string()),
    );
    self.out_headers.set(isolate, headers);
    self.content_length.set(isolate, Some(len as u64));
    self.clear_header_cache(isolate);
  }
}

#[derive(deno_core::CppgcInherits)]
#[cppgc_base(OutgoingMessage)]
#[repr(C)]
pub struct ServerResponse {
  base: OutgoingMessage,
  status_code: GcCell<Option<u16>>,
  status_message: GcCell<Option<String>>,
  response_tx: RefCell<Option<oneshot::Sender<Response<HttpResponseBody>>>>,
  body_handle: RefCell<Option<ResponseBodyHandle>>,
  socket_state: RefCell<Option<Rc<LazySocket>>>,
  scope_holder: Rc<ScopeHolder>,
  this: Rc<v8::TracedReference<v8::Object>>,
  close_after_response: bool,
}

unsafe impl GarbageCollected for ServerResponse {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.base.trace(visitor);
    self.this.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"ServerResponse"
  }
}

#[op2(inherit = OutgoingMessage)]
impl ServerResponse {
  #[constructor]
  #[cppgc]
  fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
  ) -> ServerResponse {
    ServerResponse::new_inner(me, scope, op_state, None, None, false, None)
  }

  #[fast]
  #[rename("writeHead")]
  fn write_head<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    #[smi] status_code: u16,
    #[varargs] args: Option<&v8::FunctionCallbackArguments<'a>>,
  ) -> Result<(), JsErrorBox> {
    let mut reason: Option<String> = None;
    let mut headers: Option<HashMap<String, String>> = None;
    if let Some(args) = args {
      let mut start = 0i32;
      let args_len = args.length();
      if args_len > 0 {
        let first = args.get(0);
        if first.is_int32() {
          if let Some(value) = first.int32_value(scope) {
            if value as u16 == status_code {
              start = 1;
            }
          }
        }
      }
      if args_len > start {
        let value = args.get(start);
        if !(value.is_undefined() || value.is_null()) {
          if value.is_string() {
            reason = Some(value.to_rust_string_lossy(scope));
          } else {
            headers = Some(
              deno_core::serde_v8::from_v8(scope, value)
                .map_err(JsErrorBox::from_err)?,
            );
          }
        }
      }
      if args_len > start + 1 {
        let value = args.get(start + 1);
        if !(value.is_undefined() || value.is_null()) {
          headers = Some(
            deno_core::serde_v8::from_v8(scope, value)
              .map_err(JsErrorBox::from_err)?,
          );
        }
      }
    }

    self.status_code.set(scope, Some(status_code));
    if let Some(reason) = &reason {
      self.status_message.set(scope, Some(reason.clone()));
    }
    if let Some(headers) = headers {
      let mut out_headers = self.base.out_headers.get(scope).clone();
      for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        out_headers.insert(lower, (name, value));
      }
      self.base.out_headers.set(scope, out_headers);
    }
    let status_line =
      self.status_line(scope, Some(status_code), reason.as_deref())?;
    self.base.store_header(scope, &status_line)?;
    Ok(())
  }

  #[getter]
  fn socket<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Result<v8::Local<'a, v8::Value>, JsErrorBox> {
    let socket_state = self.socket_state.borrow().clone();
    let Some(socket_state) = socket_state else {
      return Ok(v8::undefined(scope).into());
    };
    let socket_obj = socket_state.get_or_create(scope)?;
    Ok(socket_obj.into())
  }

  #[reentrant]
  #[rename("_write")]
  fn write(
    &self,
    isolate: &mut v8::Isolate,
    #[buffer] data: JsBuffer,
    _encoding: v8::Local<v8::String>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    let header_sent = *self.base.header_sent.get(isolate);
    let has_length = self.base.has_header(isolate, "content-length");
    let has_te = self.base.has_header(isolate, "transfer-encoding");
    if !header_sent && !has_length && !has_te {
      if self.base.pending_body.get(isolate).is_none() {
        self.base.pending_body.set(isolate, Some(data.to_vec()));
        let scope_holder = self.scope_holder.clone();
        let this = self.this.clone();
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb, this, None);
        });
        return Ok(());
      }
    }

    let pending = self.base.pending_body.get(isolate).clone();
    if pending.is_some() {
      self.base.pending_body.set(isolate, None);
    }
    self.ensure_response(isolate)?;

    let mut payload = Vec::new();
    if let Some(pending) = pending {
      payload.extend_from_slice(&pending);
    }
    if !data.is_empty() {
      payload.extend_from_slice(&data);
    }
    if payload.is_empty() {
      let scope_holder = self.scope_holder.clone();
      let this = self.this.clone();
      scope_holder.with_scope_immediately(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb, this, None);
      });
      return Ok(());
    }

    let handle = self
      .body_handle
      .borrow()
      .clone()
      .ok_or_else(|| JsErrorBox::generic("Response body missing"))?;
    let scope_holder = self.scope_holder.clone();
    let this = self.this.clone();
    let pending_bytes = handle.push_bytes(Bytes::from(payload))?;
    if pending_bytes > RESPONSE_BODY_HIGH_WATER {
      deno_core::unsync::spawn(async move {
        handle.wait_for_drain(RESPONSE_BODY_HIGH_WATER).await;
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb, this, None);
        });
      });
    } else {
      scope_holder.with_scope_immediately(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb, this, None);
      });
    }

    Ok(())
  }

  #[reentrant]
  #[rename("_final")]
  fn final_(
    &self,
    isolate: &mut v8::Isolate,
    #[global] cb: v8::Global<v8::Function>,
  ) {
    let mut pending = None;
    if !*self.base.header_sent.get(isolate) {
      let has_length = self.base.has_header(isolate, "content-length");
      let has_te = self.base.has_header(isolate, "transfer-encoding");
      pending = self.base.pending_body.get(isolate).clone();
      self.base.pending_body.set(isolate, None);
      if !has_length && !has_te {
        let len = pending.as_ref().map(|buf| buf.len()).unwrap_or(0);
        self.base.set_content_length(isolate, len);
      }
    }

    if !*self.base.header_sent.get(isolate)
      && self.body_handle.borrow().is_none()
      && self.response_tx.borrow().is_some()
    {
      let response_tx = self.response_tx.borrow_mut().take().unwrap();
      let payload = pending.unwrap_or_default();
      let body = Full::new(Bytes::from(payload));
      let response = match self.build_response(isolate, Either::Left(body)) {
        Ok(response) => response,
        Err(err) => {
          let scope_holder = self.scope_holder.clone();
          let this = self.this.clone();
          scope_holder.with_scope_immediately(move |scope| {
            let cb = v8::Local::new(scope, &cb);
            let this = this.get(scope).unwrap();
            call_write_cb(scope, cb.into(), this, Some(err));
          });
          return;
        }
      };
      let _ = response_tx.send(response);
      self.base.header_sent.set(isolate, true);
      let scope_holder = self.scope_holder.clone();
      let this = self.this.clone();
      scope_holder.with_scope_immediately(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, None);
      });
      return;
    }

    if let Err(err) = self.ensure_response(isolate) {
      let scope_holder = self.scope_holder.clone();
      let this = self.this.clone();
      scope_holder.with_scope_immediately(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, Some(err));
      });
      return;
    }

    let handle = match self.body_handle.borrow().clone() {
      Some(handle) => handle,
      None => {
        let scope_holder = self.scope_holder.clone();
        let this = self.this.clone();
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(
            scope,
            cb.into(),
            this,
            Some(JsErrorBox::generic("Response body missing")),
          );
        });
        return;
      }
    };

    let scope_holder = self.scope_holder.clone();
    let this = self.this.clone();
    let pending_bytes =
      if let Some(pending) = pending.filter(|buf| !buf.is_empty()) {
        match handle.push_bytes(Bytes::from(pending)) {
          Ok(pending) => pending,
          Err(err) => {
            scope_holder.with_scope_immediately(move |scope| {
              let cb = v8::Local::new(scope, &cb);
              let this = this.get(scope).unwrap();
              call_write_cb(scope, cb.into(), this, Some(err));
            });
            return;
          }
        }
      } else {
        0
      };
    handle.close();
    if pending_bytes > RESPONSE_BODY_HIGH_WATER {
      deno_core::unsync::spawn(async move {
        handle.wait_for_drain(RESPONSE_BODY_HIGH_WATER).await;
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb.into(), this, None);
        });
      });
    } else {
      scope_holder.with_scope_immediately(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, None);
      });
    }
  }

  #[rename("_destroy")]
  fn destroy(
    &self,
    _isolate: &mut v8::Isolate,
    #[global] _error: v8::Global<v8::Value>,
    #[global] _cb: v8::Global<v8::Function>,
  ) {
  }
}
impl ServerResponse {
  fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    response_tx: Option<oneshot::Sender<Response<HttpResponseBody>>>,
    body_handle: Option<ResponseBodyHandle>,
    close_after_response: bool,
    socket_state: Option<Rc<LazySocket>>,
  ) -> ServerResponse {
    let (spawner, this) = {
      let op_state = op_state.borrow();
      let spawner = op_state.borrow::<deno_core::V8TaskSpawner>().clone();
      let local_me = v8::Local::new(scope, &me);
      let this = Rc::new(v8::TracedReference::new(scope, local_me));
      (spawner, this)
    };
    let isolate_ptr = unsafe { scope.as_raw_isolate_ptr() };
    let context = Rc::new(v8::Global::new(scope, scope.get_current_context()));
    ServerResponse {
      base: OutgoingMessage::new_inner(me, scope, op_state),
      status_code: GcCell::new(None),
      status_message: GcCell::new(None),
      response_tx: RefCell::new(response_tx),
      body_handle: RefCell::new(body_handle),
      socket_state: RefCell::new(socket_state),
      scope_holder: Rc::new(ScopeHolder::new(spawner, isolate_ptr, context)),
      this,
      close_after_response,
    }
  }

  fn build_response(
    &self,
    isolate: &mut v8::Isolate,
    body: HttpResponseBody,
  ) -> Result<Response<HttpResponseBody>, JsErrorBox> {
    let status_code = (*self.status_code.get(isolate)).unwrap_or(200);
    let status = StatusCode::from_u16(status_code)
      .map_err(|_| JsErrorBox::type_error("Invalid status code"))?;
    let mut response = Response::new(body);
    *response.status_mut() = status;

    let mut header_map = HeaderMap::new();
    let headers = self.base.out_headers.get(isolate).clone();
    for (_, (name, value)) in headers.iter() {
      let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| JsErrorBox::type_error("Invalid header name"))?;
      let value = HeaderValue::from_str(value)
        .map_err(|_| JsErrorBox::type_error("Invalid header value"))?;
      header_map.append(name, value);
    }
    *response.headers_mut() = header_map;
    Ok(response)
  }

  fn ensure_response(
    &self,
    isolate: &mut v8::Isolate,
  ) -> Result<(), JsErrorBox> {
    let response_tx = self.response_tx.borrow_mut().take();
    if response_tx.is_none() {
      return Ok(());
    }
    let (handle, body) = response_body_pair();
    let response = self.build_response(isolate, Either::Right(body))?;
    self.body_handle.borrow_mut().replace(handle);
    let _ = response_tx.unwrap().send(response);
    self.base.header_sent.set(isolate, true);
    Ok(())
  }

  fn status_line(
    &self,
    isolate: &v8::Isolate,
    status_code_override: Option<u16>,
    reason_override: Option<&str>,
  ) -> Result<String, JsErrorBox> {
    let status_code = status_code_override
      .or_else(|| *self.status_code.get(isolate))
      .unwrap_or(200);
    let status = StatusCode::from_u16(status_code)
      .map_err(|_| JsErrorBox::type_error("Invalid status code"))?;
    let reason = reason_override
      .map(|reason| reason.to_string())
      .or_else(|| self.status_message.get(isolate).clone())
      .or_else(|| status.canonical_reason().map(|reason| reason.to_string()))
      .unwrap_or_else(|| "unknown".to_string());
    Ok(format!("HTTP/1.1 {} {}", status.as_u16(), reason))
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
      let _ = cb.call(scope, this.into(), &[error]);
    } else {
      let _ = cb.call(scope, this.into(), &[]);
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

fn to_v8_map<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  headers: &HashMap<String, String>,
) -> v8::Local<'a, v8::Value> {
  let map = v8::Map::new(scope);
  for (key, value) in headers.iter() {
    let key = v8::String::new_from_utf8(
      scope,
      key.as_bytes(),
      v8::NewStringType::Normal,
    )
    .unwrap();
    let value = v8::String::new_from_utf8(
      scope,
      value.as_bytes(),
      v8::NewStringType::Normal,
    )
    .unwrap();
    map.set(scope, key.into(), value.into()).unwrap();
  }
  map.into()
}
