// Copyright 2018-2025 the Deno authors. MIT license.
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use deno_core::{GarbageCollected, OpState, op2, v8};
use deno_error::JsErrorBox;
use hyper::http::Version;

use super::super::Constructors;
use super::utils::{
  RequestParts, ShouldReadState, V8Cached, upgrade_from_parts,
};

pub(crate) struct IncomingMessageInner {
  pub parts: Option<RequestParts>,
  /// HTTP method (GET, POST, etc.) - only for server-side requests
  pub method: Option<String>,
  /// Request URL - only for server-side requests
  pub url: Option<String>,
  /// HTTP status code - only for client-side responses
  pub status_code: Option<u16>,
  /// HTTP status message - only for client-side responses
  pub status_message: Option<String>,
  /// HTTP version string (e.g., "1.1")
  pub http_version: Option<String>,
  /// Major HTTP version number
  pub http_version_major: u8,
  /// Minor HTTP version number
  pub http_version_minor: u8,
  /// Parsed headers (lowercase keys, values joined according to spec)
  pub headers: V8Cached<HashMap<String, String>>,
  /// Raw headers as alternating key/value pairs
  pub raw_headers: V8Cached<Vec<String>>,
  /// Headers with distinct values (array for each key)
  pub headers_distinct: V8Cached<HashMap<String, Vec<String>>>,
  /// Parsed trailers (lazy - None until accessed)
  pub trailers: Option<HashMap<String, String>>,
  /// Raw trailers as alternating key/value pairs (lazy)
  pub raw_trailers: Option<Vec<String>>,
  /// Trailers with distinct values (lazy)
  pub trailers_distinct: Option<HashMap<String, Vec<String>>>,
  /// Whether the message has been fully received
  pub complete: bool,
  /// Whether the request was aborted
  pub aborted: bool,
  /// Whether this is an upgrade request
  pub upgrade: Option<bool>,
  /// Whether to join duplicate headers
  pub join_duplicate_headers: bool,
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
  pub(crate) inner: RefCell<IncomingMessageInner>,
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
  pub fn new(
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
  #[rename("httpVersion")]
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
  #[rename("httpVersionMajor")]
  #[smi]
  fn http_version_major(&self, _isolate: &v8::Isolate) -> u8 {
    let mut inner = self.inner.borrow_mut();
    ensure_version(&mut inner);
    inner.http_version_major
  }

  #[getter]
  #[rename("httpVersionMinor")]
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
    let headers = inner.headers.get_or_init_v8(scope, HashMap::new)?;
    Ok(headers)
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
    self.inner.borrow().trailers.clone().unwrap_or_default()
  }

  #[getter]
  #[serde]
  #[rename("rawTrailers")]
  fn raw_trailers(&self, _isolate: &v8::Isolate) -> Vec<String> {
    self.inner.borrow().raw_trailers.clone().unwrap_or_default()
  }

  #[getter]
  #[serde]
  #[rename("trailersDistinct")]
  fn trailers_distinct(
    &self,
    _isolate: &v8::Isolate,
  ) -> HashMap<String, Vec<String>> {
    self
      .inner
      .borrow()
      .trailers_distinct
      .clone()
      .unwrap_or_default()
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
  fn set_method(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.inner.borrow_mut().method = value;
  }

  #[setter]
  fn set_url(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.inner.borrow_mut().url = value;
  }

  #[setter]
  fn set_status_code(
    &self,
    _isolate: &mut v8::Isolate,
    #[smi] value: Option<u16>,
  ) {
    self.inner.borrow_mut().status_code = value;
  }

  #[setter]
  fn set_status_message(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: Option<String>,
  ) {
    self.inner.borrow_mut().status_message = value;
  }

  #[rename("httpVersion")]
  #[setter]
  fn set_http_version(
    &self,
    _isolate: &mut v8::Isolate,
    #[string] value: String,
  ) {
    let mut inner = self.inner.borrow_mut();
    inner.http_version = Some(value);
  }

  #[rename("httpVersionMajor")]
  #[setter]
  fn set_http_version_major(
    &self,
    _isolate: &mut v8::Isolate,
    #[smi] value: u8,
  ) {
    self.inner.borrow_mut().http_version_major = value;
  }

  #[rename("httpVersionMinor")]
  #[setter]
  fn set_http_version_minor(
    &self,
    _isolate: &mut v8::Isolate,
    #[smi] value: u8,
  ) {
    self.inner.borrow_mut().http_version_minor = value;
  }

  #[setter]
  fn set_complete(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().complete = value;
  }

  #[setter]
  fn set_aborted(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().aborted = value;
  }

  #[setter]
  fn set_upgrade(&self, _isolate: &mut v8::Isolate, value: bool) {
    self.inner.borrow_mut().upgrade = Some(value);
  }

  #[setter]
  fn set_join_duplicate_headers(
    &self,
    _isolate: &mut v8::Isolate,
    value: bool,
  ) {
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
  pub fn new_inner(
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
        trailers: None,
        raw_trailers: None,
        trailers_distinct: None,
        complete: false,
        aborted: false,
        upgrade: None,
        join_duplicate_headers: false,
      }),
      should_read,
    }
  }
}
