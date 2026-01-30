use std::{cell::RefCell, rc::Rc};

use deno_core::v8::cppgc::GcCell;
use deno_core::{GarbageCollected, OpState, op2, v8};
use deno_error::JsErrorBox;
use hyper::http::{HeaderMap, HeaderName, HeaderValue};
use indexmap::IndexMap;

use super::super::Constructors;

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
  pub(crate) out_headers: GcCell<IndexMap<String, (String, String)>>,
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
  pub(crate) send_date: GcCell<bool>,
  /// Buffered first body chunk (used to decide Content-Length vs chunked)
  pub(crate) pending_body: GcCell<Option<Vec<u8>>>,
}

unsafe impl GarbageCollected for OutgoingMessage {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"OutgoingMessage"
  }
}

#[op2(base)]
impl OutgoingMessage {
  #[constructor]
  #[cppgc]
  pub fn new(
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
  fn header_sent_getter(&self, isolate: &v8::Isolate) -> bool {
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
  #[rename("sendDate")]
  fn send_date(&self, isolate: &v8::Isolate) -> bool {
    *self.send_date.get(isolate)
  }

  // --- Setters ---

  #[rename("_header")]
  #[setter]
  fn set_header(
    &self,
    isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.header.set(isolate, value);
  }

  #[rename("_headerSent")]
  #[setter]
  fn set_header_sent(&self, isolate: &mut v8::Isolate, value: bool) {
    self.header_sent.set(isolate, value);
  }

  #[setter]
  fn set_finished(&self, isolate: &mut v8::Isolate, value: bool) {
    self.finished.set(isolate, value);
  }

  #[setter]
  fn set_chunked_encoding(&self, isolate: &mut v8::Isolate, value: bool) {
    self.chunked_encoding.set(isolate, value);
  }

  #[rename("_contentLength")]
  #[setter]
  fn set_content_length(
    &self,
    isolate: &mut v8::Isolate,
    #[number] value: Option<u64>,
  ) {
    self.content_length.set(isolate, value);
  }

  #[setter]
  fn set_should_keep_alive(&self, isolate: &mut v8::Isolate, value: bool) {
    self.should_keep_alive.set(isolate, value);
  }

  #[rename("_last")]
  #[setter]
  fn set_last(&self, isolate: &mut v8::Isolate, value: bool) {
    self.last.set(isolate, value);
  }

  #[setter]
  fn set_strict_content_length(&self, isolate: &mut v8::Isolate, value: bool) {
    self.strict_content_length.set(isolate, value);
  }

  #[setter]
  fn set_join_duplicate_headers(&self, isolate: &mut v8::Isolate, value: bool) {
    self.join_duplicate_headers.set(isolate, value);
  }

  #[rename("_closed")]
  #[setter]
  fn set_closed(&self, isolate: &mut v8::Isolate, value: bool) {
    self.closed.set(isolate, value);
  }

  #[setter]
  fn set_writable(&self, isolate: &mut v8::Isolate, value: bool) {
    self.writable.set(isolate, value);
  }

  #[setter]
  fn set_destroyed(&self, isolate: &mut v8::Isolate, value: bool) {
    self.destroyed.set(isolate, value);
  }

  #[rename("sendDate")]
  #[setter]
  fn set_send_date(&self, isolate: &mut v8::Isolate, value: bool) {
    self.send_date.set(isolate, value);
  }

  // --- Methods ---

  #[fast]
  fn set_header_method(
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
  fn has_header_method(
    &self,
    isolate: &v8::Isolate,
    #[string] name: String,
  ) -> bool {
    let lowercase_name = name.to_lowercase();
    self.out_headers.get(isolate).contains_key(&lowercase_name)
  }

  #[fast]
  fn remove_header(&self, isolate: &mut v8::Isolate, #[string] name: String) {
    let lowercase_name = name.to_lowercase();
    let mut headers = self.out_headers.get(isolate).clone();
    headers.shift_remove(&lowercase_name);
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

  // Node.js _send method - flushes headers and optionally sends data
  #[fast]
  #[rename("_send")]
  fn send<'a>(
    &self,
    isolate: &mut v8::Isolate,
    _data: Option<v8::Local<'a, v8::Value>>,
  ) -> bool {
    if self.header.get(isolate).is_some() && !*self.header_sent.get(isolate) {
      self.header_sent.set(isolate, true);
    }
    true
  }
}

impl OutgoingMessage {
  pub fn new_inner(
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
      out_headers: GcCell::new(IndexMap::new()),
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

  pub fn store_header(
    &self,
    isolate: &mut v8::Isolate,
    first_line: &str,
  ) -> Result<(), JsErrorBox> {
    let header = self.render_header(isolate, first_line)?;
    self.header.set(isolate, Some(header));
    Ok(())
  }

  pub fn has_header(&self, isolate: &v8::Isolate, name: &str) -> bool {
    self.out_headers.get(isolate).contains_key(name)
  }

  #[allow(dead_code)]
  pub fn set_header_internal(
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

  pub fn clear_header_cache(&self, isolate: &mut v8::Isolate) {
    if self.header.get(isolate).is_some() {
      self.header.set(isolate, None);
    }
  }

  pub fn set_content_length(&self, isolate: &mut v8::Isolate, len: usize) {
    let mut headers = self.out_headers.get(isolate).clone();
    headers.insert(
      "content-length".to_string(),
      ("Content-Length".to_string(), len.to_string()),
    );
    self.out_headers.set(isolate, headers);
    self.content_length.set(isolate, Some(len as u64));
    self.clear_header_cache(isolate);
  }

  pub fn header_sent(&self, isolate: &v8::Isolate) -> bool {
    *self.header_sent.get(isolate)
  }

  pub fn set_header_sent(&self, isolate: &mut v8::Isolate, value: bool) {
    self.header_sent.set(isolate, value);
  }
}
