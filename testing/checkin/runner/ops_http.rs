use std::{cell::RefCell, collections::HashMap, rc::Rc};

use deno_core::v8::cppgc::GcCell;
use deno_core::{GarbageCollected, OpState, op2, v8};

use crate::checkin::runner::Constructors;

use super::ops_net::Server;

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

#[op2]
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
    self.base.inner.listen_inner(port, host);
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
    let (ops_tracker, super_cons) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<Constructors>().clone(),
      )
    };
    let local_me = v8::Local::new(scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.readable);
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
    }
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
    // Placeholder - resumes socket in full implementation
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
    let (ops_tracker, super_cons) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<Constructors>().clone(),
      )
    };
    let local_me = v8::Local::new(scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.stream);
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
    }
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
  fn destroy(&self, isolate: &mut v8::Isolate) {
    self.destroyed.set(isolate, true);
    self.writable.set(isolate, false);
  }
}

#[derive(deno_core::CppgcInherits)]
#[cppgc_base(OutgoingMessage)]
#[repr(C)]
pub struct ServerResponse {
  base: OutgoingMessage,
  status_code: GcCell<Option<u16>>,
  status_message: GcCell<Option<String>>,
}

unsafe impl GarbageCollected for ServerResponse {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.base.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"ServerResponse"
  }
}
