// Copyright 2018-2025 the Deno authors. MIT license.
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use bytes::Bytes;
use deno_core::JsBuffer;
use deno_core::ToV8;
use deno_core::error::JsError;
use deno_core::v8::cppgc::GcCell;
use deno_core::v8::cppgc::Traced;
use deno_core::{GarbageCollected, OpState, op2, v8};
use deno_error::JsErrorBox;
use http_body_util::Either;
use http_body_util::Full;
use hyper::Response;
use hyper::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use tokio::sync::oneshot;

use crate::checkin::runner::extensions::node::GlobalHandle;
use crate::checkin::runner::extensions::node::ScopeHolder;

use super::outgoing_message::OutgoingMessage;
use super::response_body::{
  HttpResponseBody, RESPONSE_BODY_HIGH_WATER, ResponseBodyHandle,
  response_body_pair,
};
use super::utils::{LazySocket, http_date_header_value};

pub type ResponseTxSlot =
  Rc<RefCell<Option<oneshot::Sender<Response<HttpResponseBody>>>>>;

#[derive(deno_core::CppgcInherits)]
#[cppgc_base(OutgoingMessage)]
#[repr(C)]
pub struct ServerResponse {
  pub(crate) base: OutgoingMessage,
  status_code: GcCell<Option<u16>>,
  status_message: GcCell<Option<String>>,
  pub(crate) response_tx_slot: ResponseTxSlot,
  pub(crate) body_handle: RefCell<Option<ResponseBodyHandle>>,
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

#[op2(inherit = OutgoingMessage)]
impl ServerResponse {
  #[constructor]
  #[cppgc]
  pub fn new(
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
        if first.is_int32()
          && let Some(value) = first.int32_value(scope)
          && value as u16 == status_code
        {
          start = 1;
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
  fn write<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    #[buffer] data: JsBuffer,
    _encoding: v8::Local<'a, v8::String>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    // Get any pending data from a previous buffered write
    let pending = self.base.pending_body.get(scope).clone();
    if pending.is_some() {
      self.base.pending_body.set(scope, None);
    }

    // Start the response if not already started
    // This will use chunked encoding since Content-Length is not set
    self.ensure_response(scope)?;

    // Build the payload from pending + new data
    let mut payload = Vec::new();
    if let Some(pending) = pending {
      payload.extend_from_slice(&pending);
    }
    if !data.is_empty() {
      payload.extend_from_slice(&data);
    }
    if payload.is_empty() {
      // Call callback directly using the scope we already have
      let cb = v8::Local::new(scope, &cb);
      let this = self.this.get(scope).unwrap();
      call_write_cb(scope, cb, this, None);
      return Ok(());
    }

    let handle = self
      .body_handle
      .borrow()
      .clone()
      .ok_or_else(|| JsErrorBox::generic("Response body missing"))?;
    let pending_bytes = handle.push_bytes(Bytes::from(payload))?;
    if pending_bytes > RESPONSE_BODY_HIGH_WATER {
      let scope_holder = self.scope_holder.clone();
      let this = self.this.clone();
      deno_core::unsync::spawn(async move {
        handle.wait_for_drain(RESPONSE_BODY_HIGH_WATER).await;
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb, this, None);
        });
      });
    } else {
      let cb = v8::Local::new(scope, &cb);
      let this = self.this.get(scope).unwrap();
      call_write_cb(scope, cb, this, None);
    }

    Ok(())
  }

  #[reentrant]
  #[rename("_final")]
  fn final_<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    #[global] cb: v8::Global<v8::Function>,
  ) {
    let mut pending = None;
    if !self.base.header_sent(scope) {
      let has_length = self.base.has_header(scope, "content-length");
      let has_te = self.base.has_header(scope, "transfer-encoding");
      pending = self.base.pending_body.get(scope).clone();
      self.base.pending_body.set(scope, None);
      if !has_length && !has_te {
        let len = pending.as_ref().map(|buf| buf.len()).unwrap_or(0);
        self.base.set_content_length(scope, len);
      }
    }

    // Fast path: simple response with no prior streaming
    if !self.base.header_sent(scope)
      && self.body_handle.borrow().is_none()
      && self.response_tx_slot.borrow().is_some()
    {
      let response_tx = self.response_tx_slot.borrow_mut().take().unwrap();
      let payload = pending.unwrap_or_default();
      let body = Full::new(Bytes::from(payload));
      let response = match self.build_response(scope, Either::Left(body)) {
        Ok(response) => response,
        Err(err) => {
          let cb = v8::Local::new(scope, &cb);
          let this = self.this.get(scope).unwrap();
          call_write_cb(scope, cb.into(), this, Some(err));
          return;
        }
      };
      let _ = response_tx.send(response);
      self.base.set_header_sent(scope, true);
      // Call callback directly using the scope we already have
      let cb = v8::Local::new(scope, &cb);
      let this = self.this.get(scope).unwrap();
      call_write_cb(scope, cb.into(), this, None);
      return;
    }

    if let Err(err) = self.ensure_response(scope) {
      let cb = v8::Local::new(scope, &cb);
      let this = self.this.get(scope).unwrap();
      call_write_cb(scope, cb.into(), this, Some(err));
      return;
    }

    let handle = match self.body_handle.borrow().clone() {
      Some(handle) => handle,
      None => {
        let cb = v8::Local::new(scope, &cb);
        let this = self.this.get(scope).unwrap();
        call_write_cb(
          scope,
          cb.into(),
          this,
          Some(JsErrorBox::generic("Response body missing")),
        );
        return;
      }
    };

    let pending_bytes =
      if let Some(pending) = pending.filter(|buf| !buf.is_empty()) {
        match handle.push_bytes(Bytes::from(pending)) {
          Ok(pending) => pending,
          Err(err) => {
            let cb = v8::Local::new(scope, &cb);
            let this = self.this.get(scope).unwrap();
            call_write_cb(scope, cb.into(), this, Some(err));
            return;
          }
        }
      } else {
        0
      };
    handle.close();
    if pending_bytes > RESPONSE_BODY_HIGH_WATER {
      let scope_holder = self.scope_holder.clone();
      let this = self.this.clone();
      deno_core::unsync::spawn(async move {
        handle.wait_for_drain(RESPONSE_BODY_HIGH_WATER).await;
        scope_holder.with_scope_immediately(move |scope| {
          let cb = v8::Local::new(scope, &cb);
          let this = this.get(scope).unwrap();
          call_write_cb(scope, cb.into(), this, None);
        });
      });
    } else {
      let cb = v8::Local::new(scope, &cb);
      let this = self.this.get(scope).unwrap();
      call_write_cb(scope, cb.into(), this, None);
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
  #[allow(dead_code)]
  pub fn new_inner(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    response_tx: Option<oneshot::Sender<Response<HttpResponseBody>>>,
    body_handle: Option<ResponseBodyHandle>,
    close_after_response: bool,
    socket_state: Option<Rc<LazySocket>>,
  ) -> ServerResponse {
    let response_tx_slot = Rc::new(RefCell::new(response_tx));
    Self::new_inner_with_slot(
      me,
      scope,
      op_state,
      response_tx_slot,
      body_handle,
      close_after_response,
      socket_state,
    )
  }

  pub fn new_inner_with_slot(
    me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    response_tx_slot: ResponseTxSlot,
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
    let context =
      GlobalHandle::new(v8::Global::new(scope, scope.get_current_context()));
    ServerResponse {
      base: OutgoingMessage::new_inner(me, scope, op_state),
      status_code: GcCell::new(None),
      status_message: GcCell::new(None),
      response_tx_slot,
      body_handle: RefCell::new(body_handle),
      socket_state: RefCell::new(socket_state),
      scope_holder: Rc::new(ScopeHolder::new(spawner, isolate_ptr, context)),
      this,
      close_after_response,
    }
  }

  pub fn build_response(
    &self,
    isolate: &mut v8::Isolate,
    body: HttpResponseBody,
  ) -> Result<Response<HttpResponseBody>, JsErrorBox> {
    let status_code = (*self.status_code.get(isolate)).unwrap_or(200);
    let status = StatusCode::from_u16(status_code)
      .map_err(|_| JsErrorBox::type_error("Invalid status code"))?;
    let mut response = Response::new(body);
    *response.status_mut() = status;

    let headers = self.base.out_headers.get(isolate).clone();
    // Pre-allocate: user headers + Connection + Date
    let mut header_map = HeaderMap::with_capacity(headers.len() + 2);
    for (_, (name, value)) in headers.iter() {
      let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| JsErrorBox::type_error("Invalid header name"))?;
      let value = HeaderValue::from_str(value)
        .map_err(|_| JsErrorBox::type_error("Invalid header value"))?;
      header_map.append(name, value);
    }

    // Add Connection: close AFTER user headers if needed
    if self.close_after_response
      && !header_map.contains_key(hyper::header::CONNECTION)
    {
      header_map
        .append(hyper::header::CONNECTION, HeaderValue::from_static("close"));
    }

    // Add Date header if sendDate is true
    if *self.base.send_date.get(isolate) {
      header_map.insert(hyper::header::DATE, http_date_header_value());
    }

    *response.headers_mut() = header_map;
    Ok(response)
  }

  pub fn ensure_response(
    &self,
    isolate: &mut v8::Isolate,
  ) -> Result<(), JsErrorBox> {
    let response_tx = self.response_tx_slot.borrow_mut().take();
    if response_tx.is_none() {
      return Ok(());
    }
    let (handle, body) = response_body_pair();
    let response = self.build_response(isolate, Either::Right(body))?;
    self.body_handle.borrow_mut().replace(handle);
    let _ = response_tx.unwrap().send(response);
    self.base.set_header_sent(isolate, true);
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
