use std::{
  cell::RefCell,
  collections::HashMap,
  io::ErrorKind,
  ops::DerefMut,
  rc::Rc,
  sync::atomic::{AtomicBool, Ordering},
};

use deno_core::AsyncRefCell;
use deno_core::JsBuffer;
use deno_core::convert::Uint8Array;
use deno_core::error::JsError;
use deno_core::v8::cppgc::{GcCell, Traced};
use deno_core::{GarbageCollected, OpState, ToV8, op2, v8};
use deno_error::JsErrorBox;
use hyper::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio::time::timeout;

use crate::checkin::runner::extensions::node::ScopeHolder;

use super::Constructors;
use super::net::Server;
use super::net::{EventEmitter, OnAccept, ServerInner};

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
  fn listen(&self, #[smi] port: u16, #[string] host: String) {
    self
      .base
      .inner
      .listen_inner::<HttpServerCallback>(port, host);
  }
}

struct HttpServerCallback;

const MAX_HEADER_SIZE: usize = 16 * 1024;
const READ_BUFFER_SIZE: usize = 16 * 1024;
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(5);
struct ParsedRequest {
  method: String,
  url: String,
  http_version: String,
  http_version_major: u8,
  http_version_minor: u8,
  headers: HashMap<String, String>,
  raw_headers: Vec<String>,
  headers_distinct: HashMap<String, Vec<String>>,
}

fn parse_request_head(
  buf: &[u8],
) -> Result<Option<(ParsedRequest, usize)>, JsErrorBox> {
  let mut headers_storage = [httparse::EMPTY_HEADER; 64];
  let mut request = httparse::Request::new(&mut headers_storage);
  let status = request
    .parse(buf)
    .map_err(|_| JsErrorBox::generic("invalid http"))?;
  let header_len = match status {
    httparse::Status::Complete(len) => len,
    httparse::Status::Partial => return Ok(None),
  };
  let method = request
    .method
    .ok_or_else(|| JsErrorBox::generic("missing method"))?;
  let url = request
    .path
    .ok_or_else(|| JsErrorBox::generic("missing url"))?;
  let version = request.version.unwrap_or(1);
  let http_version_major = 1;
  let http_version_minor = version as u8;
  let http_version = format!("{http_version_major}.{http_version_minor}");

  let mut headers = HashMap::new();
  let mut raw_headers = Vec::new();
  let mut headers_distinct = HashMap::new();
  for header in request.headers.iter() {
    let name = header.name;
    let value = std::str::from_utf8(header.value)
      .map_err(JsErrorBox::from_err)?
      .trim();
    raw_headers.push(name.to_string());
    raw_headers.push(value.to_string());
    let lower = name.to_ascii_lowercase();
    headers_distinct
      .entry(lower.clone())
      .or_insert_with(Vec::new)
      .push(value.to_string());
    headers
      .entry(lower)
      .and_modify(|existing: &mut String| {
        existing.push_str(", ");
        existing.push_str(value);
      })
      .or_insert_with(|| value.to_string());
  }

  Ok(Some((
    ParsedRequest {
      method: method.to_string(),
      url: url.to_string(),
      http_version,
      http_version_major,
      http_version_minor,
      headers,
      raw_headers,
      headers_distinct,
    },
    header_len,
  )))
}

fn is_chunked(headers: &HashMap<String, String>) -> bool {
  headers.get("transfer-encoding").map_or(false, |value| {
    value.to_ascii_lowercase().contains("chunked")
  })
}

fn parse_content_length(
  headers: &HashMap<String, String>,
) -> Result<Option<usize>, JsErrorBox> {
  let value = match headers.get("content-length") {
    Some(value) => value,
    None => return Ok(None),
  };
  let value = value.split(',').next().unwrap_or("").trim();
  if value.is_empty() {
    return Ok(None);
  }
  let len = value
    .parse::<usize>()
    .map_err(|_| JsErrorBox::type_error("Invalid Content-Length"))?;
  Ok(Some(len))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
  buf.windows(4).position(|window| window == b"\r\n\r\n")
}

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

async fn read_socket(
  read: &mut OwnedReadHalf,
  buf: &mut [u8],
) -> Result<Option<usize>, JsErrorBox> {
  match read.read(buf).await {
    Ok(0) => Ok(None),
    Ok(nread) => Ok(Some(nread)),
    Err(err) if is_connection_closed(&err) => Ok(None),
    Err(err) => Err(JsErrorBox::from_err(err)),
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

fn should_close_connection(
  headers: &HashMap<String, String>,
  http_version_major: u8,
  http_version_minor: u8,
) -> bool {
  if let Some(value) = headers.get("connection") {
    let value = value.to_ascii_lowercase();
    if value.contains("close") {
      return true;
    }
    if value.contains("keep-alive") {
      return false;
    }
  }
  http_version_major == 1 && http_version_minor == 0
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

impl OnAccept for HttpServerCallback {
  async fn on_accept(
    inner: &Rc<ServerInner>,
    stream: tokio::net::TcpStream,
    _addr: std::net::SocketAddr,
  ) -> Result<(), JsErrorBox> {
    let (mut read, write_half) = stream.into_split();
    let write = Rc::new(AsyncRefCell::new(Some(write_half)));
    let mut buf = Vec::with_capacity(READ_BUFFER_SIZE);
    let mut scratch = [0u8; READ_BUFFER_SIZE];

    loop {
      let (parsed, header_len) = loop {
        if let Some(parsed) = parse_request_head(&buf)? {
          break parsed;
        }
        let nread = if buf.is_empty() {
          match timeout(KEEP_ALIVE_TIMEOUT, read_socket(&mut read, &mut scratch))
            .await
          {
            Ok(result) => match result? {
              Some(nread) => nread,
              None => return Ok(()),
            },
            Err(_) => return Ok(()),
          }
        } else {
          match read_socket(&mut read, &mut scratch).await? {
            Some(nread) => nread,
            None => return Ok(()),
          }
        };
        buf.extend_from_slice(&scratch[..nread]);
        if buf.len() > MAX_HEADER_SIZE {
          return Err(JsErrorBox::generic("http header too large"));
        }
      };

      let upgrade = parsed.headers.get("connection").map_or(false, |value| {
        value.to_ascii_lowercase().contains("upgrade")
      }) || parsed.headers.contains_key("upgrade");
      let chunked = is_chunked(&parsed.headers);
      let content_length = if chunked {
        None
      } else {
        parse_content_length(&parsed.headers)?
      };
      let has_body = chunked || content_length.unwrap_or(0) > 0;
      let should_close = should_close_connection(
        &parsed.headers,
        parsed.http_version_major,
        parsed.http_version_minor,
      );
      let should_read = Rc::new(ShouldReadState::new());

      let mut body_buf = buf.split_off(header_len);
      let req_slot: Rc<RefCell<Option<v8::Global<v8::Object>>>> =
        Rc::new(RefCell::new(None));
      let push_slot: Rc<RefCell<Option<v8::Global<v8::Function>>>> =
        Rc::new(RefCell::new(None));
      inner.with_scope_immediately({
        let inner = inner.clone();
        let req_slot = req_slot.clone();
        let push_slot = push_slot.clone();
        let should_read = should_read.clone();
        let parsed_method = parsed.method;
        let parsed_url = parsed.url;
        let parsed_http_version = parsed.http_version;
        let parsed_http_version_major = parsed.http_version_major;
        let parsed_http_version_minor = parsed.http_version_minor;
        let parsed_headers = parsed.headers;
        let parsed_raw_headers = parsed.raw_headers;
        let parsed_headers_distinct = parsed.headers_distinct;
        let write = write.clone();
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
          req.method.set(scope, Some(parsed_method));
          req.url.set(scope, Some(parsed_url));
          req.http_version.set(scope, parsed_http_version);
          req.http_version_major.set(scope, parsed_http_version_major);
          req.http_version_minor.set(scope, parsed_http_version_minor);
          req.headers.set(scope, parsed_headers);
          req.raw_headers.set(scope, parsed_raw_headers);
          req.headers_distinct.set(scope, parsed_headers_distinct);
          req.complete.set(scope, !has_body);
          req.upgrade.set(scope, upgrade);
          let req_obj = deno_core::cppgc::wrap_object(scope, req_empty, req);

          let res_empty =
            deno_core::cppgc::make_cppgc_empty_object::<ServerResponse>(scope);
          let res = ServerResponse::new_inner(
            v8::Global::new(scope, res_empty),
            scope,
            inner.op_state(),
            write,
            should_close,
          );
          if should_close {
            res
              .base
              .set_header_internal(scope, "Connection", "close");
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
      let req_handle = Rc::new(req_slot.borrow_mut().take().unwrap());
      let push_handle = Rc::new(push_slot.borrow_mut().take().unwrap());
      let should_read = should_read;

      let finish_req = |mark_complete: bool| {
        let inner = inner.clone();
        let req_handle = req_handle.clone();
        let push_handle = push_handle.clone();
        inner.with_scope(move |scope| {
          v8::tc_scope!(let scope, scope);
          let req_obj = v8::Local::<v8::Object>::new(scope, &*req_handle);
          let push = v8::Local::<v8::Function>::new(scope, &*push_handle);
          if mark_complete {
            let _ = req_obj.set(
              scope,
              internalized(scope, "complete").into(),
              v8::Boolean::new(scope, true).into(),
            );
          }
          let _ = push.call(scope, req_obj.into(), &[v8::null(scope).into()]);
        });
      };

      if !has_body {
        buf = body_buf;
        finish_req(true);
        if upgrade {
          break;
        }
        continue;
      }

      if chunked {
        let mut cursor = 0usize;
        loop {
          should_read.wait_for_should_read().await;
          if cursor >= body_buf.len() {
            let nread = match read_socket(&mut read, &mut scratch).await? {
              Some(nread) => nread,
              None => return Ok(()),
            };
            body_buf.extend_from_slice(&scratch[..nread]);
            continue;
          }

          let status = httparse::parse_chunk_size(&body_buf[cursor..])
            .map_err(|_| JsErrorBox::generic("invalid chunk size"))?;
          let (size_line_len, size) = match status {
            httparse::Status::Complete(data) => data,
            httparse::Status::Partial => {
              let nread = match read_socket(&mut read, &mut scratch).await? {
                Some(nread) => nread,
                None => return Ok(()),
              };
              body_buf.extend_from_slice(&scratch[..nread]);
              continue;
            }
          };
          let size = usize::try_from(size)
            .map_err(|_| JsErrorBox::generic("chunk too large"))?;
          let chunk_start = cursor + size_line_len;

          if size == 0 {
            if body_buf.len() >= chunk_start + 2
              && &body_buf[chunk_start..chunk_start + 2] == b"\r\n"
            {
              cursor = chunk_start + 2;
              break;
            }
            if let Some(end) = find_double_crlf(&body_buf[chunk_start..]) {
              cursor = chunk_start + end + 4;
              break;
            }
            let nread = match read_socket(&mut read, &mut scratch).await? {
              Some(nread) => nread,
              None => return Ok(()),
            };
            body_buf.extend_from_slice(&scratch[..nread]);
            continue;
          }

          if body_buf.len() < chunk_start + size + 2 {
            let nread = match read_socket(&mut read, &mut scratch).await? {
              Some(nread) => nread,
              None => return Ok(()),
            };
            body_buf.extend_from_slice(&scratch[..nread]);
            continue;
          }
          let chunk_end = chunk_start + size;
          if &body_buf[chunk_end..chunk_end + 2] != b"\r\n" {
            return Err(JsErrorBox::generic("invalid chunk terminator"));
          }
          let ok = push_chunk_with_backpressure(
            inner,
            &req_handle,
            &push_handle,
            body_buf[chunk_start..chunk_end].to_vec(),
          )
          .await;
          if !ok {
            should_read.clear_should_read();
          }
          cursor = chunk_end + 2;
        }
        body_buf.drain(..cursor);
        buf = body_buf;
        finish_req(true);
      } else if let Some(mut remaining) = content_length {
        if remaining == 0 {
          buf = body_buf;
          finish_req(true);
        } else {
          if !body_buf.is_empty() {
            let mut offset = 0usize;
            while remaining > 0 && offset < body_buf.len() {
              should_read.wait_for_should_read().await;
              let available = body_buf.len() - offset;
              let take = remaining.min(available);
              let ok = push_chunk_with_backpressure(
                inner,
                &req_handle,
                &push_handle,
                body_buf[offset..offset + take].to_vec(),
              )
              .await;
              if !ok {
                should_read.clear_should_read();
              }
              remaining -= take;
              offset += take;
            }
            if body_buf.len() > offset {
              buf = body_buf[offset..].to_vec();
              remaining = 0;
            }
          }
          while remaining > 0 {
            should_read.wait_for_should_read().await;
            let nread = match read_socket(&mut read, &mut scratch).await? {
              Some(nread) => nread,
              None => return Ok(()),
            };
            if nread <= remaining {
              let ok = push_chunk_with_backpressure(
                inner,
                &req_handle,
                &push_handle,
                scratch[..nread].to_vec(),
              )
              .await;
              if !ok {
                should_read.clear_should_read();
              }
              remaining -= nread;
            } else {
              let ok = push_chunk_with_backpressure(
                inner,
                &req_handle,
                &push_handle,
                scratch[..remaining].to_vec(),
              )
              .await;
              if !ok {
                should_read.clear_should_read();
              }
              buf.extend_from_slice(&scratch[remaining..nread]);
              remaining = 0;
            }
          }
          if remaining == 0 {
            finish_req(true);
          }
        }
      }

      if upgrade || should_close {
        break;
      }
    }

    Ok(())
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
  /// HTTP method (GET, POST, etc.) - only for server-side requests
  method: GcCell<Option<String>>,
  /// Request URL - only for server-side requests  
  url: GcCell<Option<String>>,
  /// HTTP status code - only for client-side responses
  status_code: GcCell<Option<u16>>,
  /// HTTP status message - only for client-side responses
  status_message: GcCell<Option<String>>,
  /// HTTP version string (e.g., "1.1")
  http_version: GcCell<String>,
  /// Major HTTP version number
  http_version_major: GcCell<u8>,
  /// Minor HTTP version number
  http_version_minor: GcCell<u8>,
  /// Parsed headers (lowercase keys, values joined according to spec)
  headers: GcCell<HashMap<String, String>>,
  /// Raw headers as alternating key/value pairs
  raw_headers: GcCell<Vec<String>>,
  /// Headers with distinct values (array for each key)
  headers_distinct: GcCell<HashMap<String, Vec<String>>>,
  /// Parsed trailers
  trailers: GcCell<HashMap<String, String>>,
  /// Raw trailers as alternating key/value pairs
  raw_trailers: GcCell<Vec<String>>,
  /// Trailers with distinct values
  trailers_distinct: GcCell<HashMap<String, Vec<String>>>,
  /// Whether the message has been fully received
  complete: GcCell<bool>,
  /// Whether the request was aborted
  aborted: GcCell<bool>,
  /// Whether this is an upgrade request
  upgrade: GcCell<bool>,
  /// Whether to join duplicate headers
  join_duplicate_headers: GcCell<bool>,
  /// Backpressure signal from Readable
  should_read: Rc<ShouldReadState>,
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
  fn method(&self, isolate: &v8::Isolate) -> Option<String> {
    self.method.get(isolate).clone()
  }

  #[getter]
  #[string]
  fn url(&self, isolate: &v8::Isolate) -> Option<String> {
    self.url.get(isolate).clone()
  }

  #[getter]
  #[smi]
  fn status_code(&self, isolate: &v8::Isolate) -> Option<u16> {
    *self.status_code.get(isolate)
  }

  #[getter]
  #[string]
  fn status_message(&self, isolate: &v8::Isolate) -> Option<String> {
    self.status_message.get(isolate).clone()
  }

  #[getter]
  #[string]
  fn http_version(&self, isolate: &v8::Isolate) -> String {
    self.http_version.get(isolate).clone()
  }

  #[getter]
  #[smi]
  fn http_version_major(&self, isolate: &v8::Isolate) -> u8 {
    *self.http_version_major.get(isolate)
  }

  #[getter]
  #[smi]
  fn http_version_minor(&self, isolate: &v8::Isolate) -> u8 {
    *self.http_version_minor.get(isolate)
  }

  #[getter]
  #[serde]
  fn headers(&self, isolate: &v8::Isolate) -> HashMap<String, String> {
    self.headers.get(isolate).clone()
  }

  #[getter]
  #[serde]
  #[rename("rawHeaders")]
  fn raw_headers(&self, isolate: &v8::Isolate) -> Vec<String> {
    self.raw_headers.get(isolate).clone()
  }

  #[getter]
  #[serde]
  #[rename("headersDistinct")]
  fn headers_distinct(
    &self,
    isolate: &v8::Isolate,
  ) -> HashMap<String, Vec<String>> {
    self.headers_distinct.get(isolate).clone()
  }

  #[getter]
  #[serde]
  fn trailers(&self, isolate: &v8::Isolate) -> HashMap<String, String> {
    self.trailers.get(isolate).clone()
  }

  #[getter]
  #[serde]
  #[rename("rawTrailers")]
  fn raw_trailers(&self, isolate: &v8::Isolate) -> Vec<String> {
    self.raw_trailers.get(isolate).clone()
  }

  #[getter]
  #[serde]
  #[rename("trailersDistinct")]
  fn trailers_distinct(
    &self,
    isolate: &v8::Isolate,
  ) -> HashMap<String, Vec<String>> {
    self.trailers_distinct.get(isolate).clone()
  }

  #[getter]
  fn complete(&self, isolate: &v8::Isolate) -> bool {
    *self.complete.get(isolate)
  }

  #[getter]
  fn aborted(&self, isolate: &v8::Isolate) -> bool {
    *self.aborted.get(isolate)
  }

  #[getter]
  fn upgrade(&self, isolate: &v8::Isolate) -> bool {
    *self.upgrade.get(isolate)
  }

  #[getter]
  fn join_duplicate_headers(&self, isolate: &v8::Isolate) -> bool {
    *self.join_duplicate_headers.get(isolate)
  }

  // --- Setters ---

  #[setter]
  fn method(&self, isolate: &mut v8::Isolate, #[string] value: Option<String>) {
    self.method.set(isolate, value);
  }

  #[setter]
  fn url(&self, isolate: &mut v8::Isolate, #[string] value: Option<String>) {
    self.url.set(isolate, value);
  }

  #[setter]
  fn status_code(&self, isolate: &mut v8::Isolate, #[smi] value: Option<u16>) {
    self.status_code.set(isolate, value);
  }

  #[setter]
  fn status_message(
    &self,
    isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.status_message.set(isolate, value);
  }

  #[setter]
  fn http_version(&self, isolate: &mut v8::Isolate, #[string] value: String) {
    self.http_version.set(isolate, value);
  }

  #[setter]
  fn http_version_major(&self, isolate: &mut v8::Isolate, #[smi] value: u8) {
    self.http_version_major.set(isolate, value);
  }

  #[setter]
  fn http_version_minor(&self, isolate: &mut v8::Isolate, #[smi] value: u8) {
    self.http_version_minor.set(isolate, value);
  }

  #[setter]
  fn complete(&self, isolate: &mut v8::Isolate, value: bool) {
    self.complete.set(isolate, value);
  }

  #[setter]
  fn aborted(&self, isolate: &mut v8::Isolate, value: bool) {
    self.aborted.set(isolate, value);
  }

  #[setter]
  fn upgrade(&self, isolate: &mut v8::Isolate, value: bool) {
    self.upgrade.set(isolate, value);
  }

  #[setter]
  fn join_duplicate_headers(&self, isolate: &mut v8::Isolate, value: bool) {
    self.join_duplicate_headers.set(isolate, value);
  }

  // --- Methods ---

  #[fast]
  #[rename("_read")]
  fn read(&self) {
    self.should_read.set_should_read();
  }

  #[fast]
  #[rename("_destroy")]
  fn destroy(&self, isolate: &mut v8::Isolate) {
    // Mark as aborted if not complete
    if !*self.complete.get(isolate) {
      self.aborted.set(isolate, true);
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
      method: GcCell::new(None),
      url: GcCell::new(None),
      status_code: GcCell::new(None),
      status_message: GcCell::new(None),
      http_version: GcCell::new("1.1".to_string()),
      http_version_major: GcCell::new(1),
      http_version_minor: GcCell::new(1),
      headers: GcCell::new(HashMap::new()),
      raw_headers: GcCell::new(Vec::new()),
      headers_distinct: GcCell::new(HashMap::new()),
      trailers: GcCell::new(HashMap::new()),
      raw_trailers: GcCell::new(Vec::new()),
      trailers_distinct: GcCell::new(HashMap::new()),
      complete: GcCell::new(false),
      aborted: GcCell::new(false),
      upgrade: GcCell::new(false),
      join_duplicate_headers: GcCell::new(false),
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

  fn ensure_header(
    &self,
    isolate: &mut v8::Isolate,
    first_line: &str,
  ) -> Result<String, JsErrorBox> {
    if let Some(header) = self.header.get(isolate).clone() {
      return Ok(header);
    }
    let header = self.render_header(isolate, first_line)?;
    self.header.set(isolate, Some(header.clone()));
    Ok(header)
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

  fn update_chunked_encoding(&self, isolate: &mut v8::Isolate) {
    if *self.chunked_encoding.get(isolate) {
      return;
    }
    let headers = self.out_headers.get(isolate);
    if let Some((_, value)) = headers.get("transfer-encoding") {
      if value.to_ascii_lowercase().contains("chunked") {
        self.chunked_encoding.set(isolate, true);
      }
    }
  }

  fn maybe_enable_chunked(&self, isolate: &mut v8::Isolate) {
    if *self.chunked_encoding.get(isolate) {
      return;
    }
    let mut headers = self.out_headers.get(isolate).clone();
    if headers.contains_key("content-length") {
      return;
    }
    if headers.contains_key("transfer-encoding") {
      return;
    }
    self.clear_header_cache(isolate);
    headers.insert(
      "transfer-encoding".to_string(),
      ("Transfer-Encoding".to_string(), "chunked".to_string()),
    );
    self.out_headers.set(isolate, headers);
    self.chunked_encoding.set(isolate, true);
  }
}

#[derive(deno_core::CppgcInherits)]
#[cppgc_base(OutgoingMessage)]
#[repr(C)]
pub struct ServerResponse {
  base: OutgoingMessage,
  status_code: GcCell<Option<u16>>,
  status_message: GcCell<Option<String>>,
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
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
    ServerResponse::new_inner(
      me,
      scope,
      op_state,
      Rc::new(AsyncRefCell::new(None)),
      false,
    )
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

  #[reentrant]
  #[rename("_write")]
  fn write(
    &self,
    isolate: &mut v8::Isolate,
    #[buffer] data: JsBuffer,
    _encoding: v8::Local<v8::String>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    let mut payload = Vec::new();
    let header_sent = *self.base.header_sent.get(isolate);
    let has_length = self.base.has_header(isolate, "content-length");
    let has_te = self.base.has_header(isolate, "transfer-encoding");
    if !header_sent && !has_length && !has_te {
      if self.base.pending_body.get(isolate).is_none() {
        self.base.pending_body.set(isolate, Some(data.to_vec()));
        let scope_holder = self.scope_holder.clone();
        let this = self.this.clone();
        scope_holder.with_scope(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb, this, None);
        });
        return Ok(());
      }
      self.base.maybe_enable_chunked(isolate);
    } else {
      self.base.update_chunked_encoding(isolate);
    }

    if !*self.base.header_sent.get(isolate) {
      let status_line = self.status_line(isolate, None, None)?;
      let header = self.base.ensure_header(isolate, &status_line)?;
      self.base.header_sent.set(isolate, true);
      payload.extend_from_slice(header.as_bytes());
    }

    let mut buffered = None;
    if !header_sent {
      buffered = self.base.pending_body.get(isolate).clone();
      self.base.pending_body.set(isolate, None);
    }

    let chunked = *self.base.chunked_encoding.get(isolate);
    if let Some(buffered) = buffered {
      if !buffered.is_empty() {
        if chunked {
          let prefix = format!("{:X}\r\n", buffered.len());
          payload.extend_from_slice(prefix.as_bytes());
          payload.extend_from_slice(&buffered);
          payload.extend_from_slice(b"\r\n");
        } else {
          payload.extend_from_slice(&buffered);
        }
      }
    }
    if !data.is_empty() {
      if chunked {
        let prefix = format!("{:X}\r\n", data.len());
        payload.extend_from_slice(prefix.as_bytes());
        payload.extend_from_slice(&data);
        payload.extend_from_slice(b"\r\n");
      } else {
        payload.extend_from_slice(&data);
      }
    }
    let write = self.write.clone();
    let scope_holder = self.scope_holder.clone();
    let this = self.this.clone();
    let num_wrote = if let Some(mut slot) = write.try_borrow_mut() {
      if let Some(write) = slot.deref_mut().as_mut() {
        let nwritten = write.try_write(&payload);
        match nwritten {
          Ok(nwritten) => nwritten,
          Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
          Err(e) => return Err(JsErrorBox::from_err(e)),
        }
      } else {
        0
      }
    } else {
      0
    };

    if num_wrote >= payload.len() {
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb, this, None);
      });
      return Ok(());
    }

    deno_core::unsync::spawn(async move {
      let result = write
        .borrow_mut()
        .await
        .deref_mut()
        .as_mut()
        .unwrap()
        .write_all(&payload[num_wrote..])
        .await
        .map_err(JsErrorBox::from_err)
        .err();
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb, this, result);
      });
    });

    Ok(())
  }

  #[rename("_final")]
  fn final_(
    &self,
    isolate: &mut v8::Isolate,
    #[global] cb: v8::Global<v8::Function>,
  ) {
    let mut header_bytes = None;
    let mut body_bytes = None;
    if !*self.base.header_sent.get(isolate) {
      let has_length = self.base.has_header(isolate, "content-length");
      let has_te = self.base.has_header(isolate, "transfer-encoding");
      let pending = self.base.pending_body.get(isolate).clone();
      self.base.pending_body.set(isolate, None);
      if !has_length && !has_te {
        let len = pending.as_ref().map(|buf| buf.len()).unwrap_or(0);
        self.base.set_content_length(isolate, len);
      } else {
        self.base.update_chunked_encoding(isolate);
      }
      match self
        .status_line(isolate, None, None)
        .and_then(|status_line| self.base.ensure_header(isolate, &status_line))
      {
        Ok(header) => {
          self.base.header_sent.set(isolate, true);
          header_bytes = Some(header.into_bytes());
          body_bytes = pending;
        }
        Err(err) => {
          let scope_holder = self.scope_holder.clone();
          let this = self.this.clone();
          scope_holder.with_scope(move |scope| {
            let cb = v8::Local::new(scope, &cb);
            let this = this.get(scope).unwrap();
            call_write_cb(scope, cb.into(), this, Some(err));
          });
          return;
        }
      }
    }
    let chunked = *self.base.chunked_encoding.get(isolate);
    let close_after_response = self.close_after_response;
    let write = self.write.clone();
    let scope_holder = self.scope_holder.clone();
    let this = self.this.clone();
    let mut segments = Vec::new();
    if let Some(header_bytes) = header_bytes {
      if !header_bytes.is_empty() {
        segments.push(header_bytes);
      }
    }
    if let Some(body_bytes) = body_bytes {
      if !body_bytes.is_empty() {
        if chunked {
          segments.push(format!("{:X}\r\n", body_bytes.len()).into_bytes());
          segments.push(body_bytes);
          segments.push(b"\r\n".to_vec());
        } else {
          segments.push(body_bytes);
        }
      }
    }
    if chunked {
      segments.push(b"0\r\n\r\n".to_vec());
    }

    if segments.is_empty() && !close_after_response {
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, None);
      });
      return;
    }

    let mut remaining: Option<Vec<u8>> = None;
    let mut result: Option<JsErrorBox> = None;
    let mut wrote_all = segments.is_empty();

    if let Some(mut slot) = write.try_borrow_mut() {
      if let Some(write) = slot.deref_mut().as_mut() {
        let mut idx = 0usize;
        let mut offset = 0usize;
        while idx < segments.len() {
          let segment = &segments[idx];
          if offset >= segment.len() {
            idx += 1;
            offset = 0;
            continue;
          }
          match write.try_write(&segment[offset..]) {
            Ok(0) => break,
            Ok(nwritten) => {
              offset += nwritten;
              if offset == segment.len() {
                idx += 1;
                offset = 0;
              }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(err) => {
              result = Some(JsErrorBox::from_err(err));
              break;
            }
          }
        }
        if result.is_none() {
          if idx == segments.len() {
            wrote_all = true;
          } else {
            let mut rem = Vec::new();
            if idx < segments.len() {
              rem.extend_from_slice(&segments[idx][offset..]);
              for segment in segments.iter().skip(idx + 1) {
                rem.extend_from_slice(segment);
              }
            }
            if !rem.is_empty() {
              remaining = Some(rem);
            }
          }
        }
      }
    } else if !segments.is_empty() {
      let mut rem = Vec::new();
      for segment in &segments {
        rem.extend_from_slice(segment);
      }
      remaining = Some(rem);
    }

    if let Some(result) = result {
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, Some(result));
      });
      return;
    }

    if wrote_all && !close_after_response {
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, None);
      });
      return;
    }

    deno_core::unsync::spawn(async move {
      let mut result = None;
      if let Some(write) = write.borrow_mut().await.deref_mut().as_mut() {
        if let Some(remaining) = remaining {
          result = write
            .write_all(&remaining)
            .await
            .map_err(JsErrorBox::from_err)
            .err();
        }
        if result.is_none() && close_after_response {
          result = write
            .shutdown()
            .await
            .map_err(JsErrorBox::from_err)
            .err();
        }
      }
      scope_holder.with_scope(move |scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = this.get(scope).unwrap();
        call_write_cb(scope, cb.into(), this, result);
      });
    });
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
    write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
    close_after_response: bool,
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
      write,
      scope_holder: Rc::new(ScopeHolder::new(spawner, isolate_ptr, context)),
      this,
      close_after_response,
    }
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
