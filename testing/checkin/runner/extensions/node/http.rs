mod incoming_message;
mod outgoing_message;
mod response_body;
mod server_response;
mod utils;

pub use incoming_message::IncomingMessage;
pub use outgoing_message::OutgoingMessage;
pub use response_body::HttpResponseBody;
pub use server_response::{ResponseTxSlot, ServerResponse};
pub use utils::{LazySocket, RequestParts, ShouldReadState};

use utils::should_close_from_parts;

use std::{cell::RefCell, error::Error as _, io::ErrorKind, rc::Rc};

use bytes::Bytes;
use deno_core::convert::Uint8Array;
use deno_core::{GarbageCollected, OpState, ToV8, op2, v8};
use deno_error::JsErrorBox;
use futures::StreamExt;
use http_body::Body;
use http_body_util::BodyDataStream;
use http_body_util::Either;
use http_body_util::Full;
use hyper::Request;
use hyper::Response;
use hyper::http::StatusCode;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::oneshot;

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
  #[reentrant]
  fn listen<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    #[smi] port: u16,
    host_or_callback: Option<v8::Local<'a, v8::Value>>,
    on_listen_arg: Option<v8::Local<'a, v8::Function>>,
  ) {
    let (host, on_listen) = match host_or_callback {
      Some(value) if value.is_function() => {
        // Second arg is callback, no host specified
        (None, Some(value.cast::<v8::Function>()))
      }
      Some(value) if value.is_string() => {
        // Second arg is host string
        (Some(value.to_rust_string_lossy(scope)), on_listen_arg)
      }
      Some(value) if !value.is_null_or_undefined() => {
        // Try to use as host string
        (Some(value.to_rust_string_lossy(scope)), on_listen_arg)
      }
      _ => (None, on_listen_arg),
    };

    let host = host.unwrap_or_else(|| "0.0.0.0".to_string());
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

  #[fast]
  fn unref(&self) {
    self.base.inner.ref_tracker.unref();
  }

  #[to_v8]
  fn address(&self) -> Option<super::net::ServerAddress> {
    let inner = &self.base.inner;
    let host = inner.host.borrow();
    let port = *inner.port.borrow();
    match (&*host, port) {
      (Some(host), Some(port)) => Some(super::net::ServerAddress {
        address: host.to_string(),
        port,
      }),
      _ => None,
    }
  }

  #[fast]
  #[reentrant]
  fn close<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    cb: Option<v8::Local<'a, v8::Function>>,
  ) {
    // Register the callback for the 'close' event if provided
    if let Some(cb) = cb {
      self
        .base
        .inner
        .on_event(scope, &[internalized(scope, "close").into(), cb.into()]);
    }
    // Delegate to the base server's close method
    self.base.inner.close();
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

/// Result of init_request_objects.
/// On success: returns handles needed for body streaming.
/// On failure (handler threw): returns None, and caller should take response_tx
/// from the slot to send an error response.
fn init_request_objects(
  inner: &Rc<ServerInner>,
  parts: hyper::http::request::Parts,
  has_body: bool,
  should_read: Rc<ShouldReadState>,
  response_tx_slot: ResponseTxSlot,
  should_close: bool,
  socket_state: Rc<LazySocket>,
) -> Option<(Rc<v8::Global<v8::Object>>, Rc<v8::Global<v8::Function>>)> {
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
    let response_tx_slot = response_tx_slot.clone();
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
      let res = ServerResponse::new_inner_with_slot(
        v8::Global::new(scope, res_empty),
        scope,
        inner.op_state(),
        response_tx_slot,
        None,
        should_close,
        Some(socket_state.clone()),
      );
      // Note: Connection: close is added in build_response AFTER user headers
      // to preserve expected header ordering
      let res_obj = deno_core::cppgc::wrap_object(scope, res_empty, res);

      let request_event = internalized(scope, "request");
      inner.emit_event(
        scope,
        &[request_event.into(), req_obj.into(), res_obj.into()],
      );
      if scope.has_caught() {
        eprintln!("error in emit_event");
        scope.throw_exception(scope.exception().unwrap());
        return;
      }

      let push = req_obj
        .get(scope, internalized(scope, "push").into())
        .unwrap()
        .cast::<v8::Function>();
      *req_slot.borrow_mut() = Some(v8::Global::new(scope, req_obj));
      *push_slot.borrow_mut() = Some(v8::Global::new(scope, push));
    }
  });

  let req_handle = req_slot.borrow_mut().take()?;
  let push_handle = push_slot.borrow_mut().take()?;
  Some((Rc::new(req_handle), Rc::new(push_handle)))
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
  let response_tx_slot: ResponseTxSlot =
    Rc::new(RefCell::new(Some(response_tx)));

  let handles = init_request_objects(
    &inner,
    parts,
    has_body,
    should_read.clone(),
    response_tx_slot.clone(),
    should_close,
    socket_state,
  );

  // If init_request_objects returned None, the request handler threw an exception.
  // Take response_tx from the slot and send 500 immediately.
  if let Some((req_handle, push_handle)) = handles {
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
  } else {
    // Handler threw - send 500 if response_tx is still available
    // (handler might have already sent a response before throwing)
    if let Some(response_tx) = response_tx_slot.borrow_mut().take() {
      let mut response = Response::new(Either::Left(Full::new(Bytes::new())));
      *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
      let _ = response_tx.send(response);
    }
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
      .auto_date_header(false)
      .title_case_headers(true)
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

#[cfg(test)]
mod tests {
  use std::pin::pin;

  use deno_core::{JsRuntime, PollEventLoopOptions, RuntimeOptions};

  use crate::checkin::runner::extensions::node::test_utils::{
    HttpTestServer, JsObject,
  };

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

  /// Test that HTTP response body streams chunks as they are written,
  /// rather than buffering until the response ends.
  ///
  /// The server writes a chunk but never calls end(). If streaming works,
  /// the client receives the chunk. If buffered, nothing is sent.
  #[tokio::test(flavor = "current_thread")]
  async fn http_response_streams_body() {
    let mut runtime = jsruntime();

    // Create server that writes a chunk immediately (doesn't call end)
    let server = HttpTestServer::start(&mut runtime, |scope, _req, res| {
      let res = JsObject::new(scope, res);
      res.call(scope, "write", ("chunk1",));
    })
    .await;

    // Make request and get response chunks
    let mut rx = server.get();

    // Run event loop while waiting for first chunk (generous timeout)
    let timeout = tokio::time::Duration::from_secs(5);

    let result = {
      let mut event_loop =
        pin!(runtime.run_event_loop(PollEventLoopOptions::default()));

      let mut received = String::new();
      let deadline = tokio::time::Instant::now() + timeout;

      loop {
        tokio::select! {
          _ = &mut event_loop => {}
          chunk = rx.recv() => {
            match chunk {
              Some(data) => {
                received.push_str(&String::from_utf8_lossy(&data));
                if received.contains("chunk1") {
                  break;
                }
              }
              None => break,
            }
          }
          _ = tokio::time::sleep_until(deadline) => {
            break;
          }
        }
      }
      received
    };

    assert!(
      result.contains("chunk1"),
      "Response should contain 'chunk1' (streaming). Got: '{}'. \
       This suggests the response body is being buffered instead of streamed.",
      result
    );
    println!("Response streaming test passed!");
  }

  /// Test that HTTP request body streams chunks as they arrive,
  /// rather than buffering until the request ends.
  ///
  /// This test uses synchronization instead of timing to avoid flakiness:
  /// 1. Client sends chunk1
  /// 2. Client waits for server to confirm receipt (via channel)
  /// 3. Only then does client send chunk2
  ///
  /// If streaming works, the server receives chunk1 while the client waits.
  /// If buffered, the server wouldn't receive anything until the request ends.
  #[tokio::test(flavor = "current_thread")]
  async fn http_request_streams_body() {
    use crate::checkin::runner::extensions::node::test_utils::js_callback;
    use deno_core::v8;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut runtime = jsruntime();

    // Signal from server to client: "I received chunk1, you can send chunk2"
    let chunk1_received = Arc::new(AtomicBool::new(false));
    let chunk1_notify = Arc::new(tokio::sync::Notify::new());

    // Create server that signals when it receives chunk1
    let server = HttpTestServer::start_with_state(
      &mut runtime,
      (chunk1_received.clone(), chunk1_notify.clone()),
      |scope, state, req, res| {
        let (chunk1_received, chunk1_notify) = state.clone();
        let req = JsObject::new(scope, req);

        // Set up 'data' event handler on request
        let data_cb = js_callback(
          scope,
          (chunk1_received.clone(), chunk1_notify.clone()),
          |scope, state, args, _| {
            let (chunk1_received, chunk1_notify) = state;

            // Get the chunk data
            let data = args.get(0);
            let data_str = if let Ok(str_val) = data.try_cast::<v8::String>() {
              str_val.to_rust_string_lossy(scope)
            } else if let Ok(arr) = data.try_cast::<v8::ArrayBufferView>() {
              let len = arr.byte_length();
              let mut buf = vec![0u8; len];
              arr.copy_contents(&mut buf);
              String::from_utf8_lossy(&buf).to_string()
            } else {
              "<unknown type>".to_string()
            };

            // Signal when we receive chunk1
            if data_str.contains("chunk1") {
              chunk1_received.store(true, Ordering::SeqCst);
              chunk1_notify.notify_one();
            }
          },
        );
        req.call(scope, "on", ("data", data_cb));

        // Set up 'end' event handler to send response
        let res = JsObject::new(scope, res);
        let end_cb = js_callback(scope, res, |scope, res, _, _| {
          res.call(scope, "end", ("ok",));
        });
        req.call(scope, "on", ("end", end_cb));
      },
    )
    .await;

    let port = server.port;
    let chunk1_received_client = chunk1_received.clone();
    let chunk1_notify_client = chunk1_notify.clone();

    // Channel to signal when client is done
    let (client_done_tx, client_done_rx) = tokio::sync::oneshot::channel();

    // Spawn client that waits for chunk1 confirmation before sending chunk2
    tokio::task::spawn(async move {
      let mut stream =
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
          .await
          .expect("Failed to connect");

      // Send HTTP request headers with chunked transfer encoding
      let headers = "POST / HTTP/1.1\r\n\
                     Host: localhost\r\n\
                     Transfer-Encoding: chunked\r\n\
                     Connection: close\r\n\r\n";
      stream
        .write_all(headers.as_bytes())
        .await
        .expect("Failed to write headers");

      // Send first chunk
      let chunk1 = "6\r\nchunk1\r\n";
      stream
        .write_all(chunk1.as_bytes())
        .await
        .expect("Failed to write chunk1");
      stream.flush().await.expect("Failed to flush");

      // Wait for server to confirm receipt of chunk1
      // This is the key: if streaming works, this returns quickly
      // If buffered, this would timeout
      let wait_result = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        chunk1_notify_client.notified(),
      )
      .await;

      let streaming_works =
        wait_result.is_ok() && chunk1_received_client.load(Ordering::SeqCst);

      // Send second chunk and finish regardless
      let chunk2 = "6\r\nchunk2\r\n";
      let _ = stream.write_all(chunk2.as_bytes()).await;
      let end_chunk = "0\r\n\r\n";
      let _ = stream.write_all(end_chunk.as_bytes()).await;

      // Read response
      let mut response = Vec::new();
      let _ = stream.read_to_end(&mut response).await;

      let _ = client_done_tx.send(streaming_works);
    });

    // Run event loop until client is done (with generous timeout)
    let timeout = tokio::time::Duration::from_secs(10);
    let streaming_works = {
      let mut event_loop =
        pin!(runtime.run_event_loop(PollEventLoopOptions::default()));
      let client_future = async { client_done_rx.await.unwrap_or(false) };
      let mut client_future = pin!(client_future);

      let result = loop {
        tokio::select! {
          _ = &mut event_loop => {
            // Event loop yielded, keep going
          }
          result = &mut client_future => {
            break Some(result);
          }
          _ = tokio::time::sleep(timeout) => {
            break None;
          }
        }
      };
      result.unwrap_or(false)
    };

    assert!(
      streaming_works,
      "Request body streaming test failed: server did not receive chunk1 \
       before chunk2 was sent. This suggests the request body is being \
       buffered instead of streamed."
    );

    println!("Request streaming test passed!");
  }

  /// Test that server.listen(0) chooses an available port and defaults host to 0.0.0.0.
  /// Also verifies that server.address() returns the bound address.
  #[tokio::test(flavor = "current_thread")]
  async fn http_server_listen_port_zero() {
    use crate::checkin::runner::extensions::node::test_utils::{
      import_from, js_callback,
    };
    use deno_core::v8;

    let mut runtime = jsruntime();
    let create_server =
      import_from(&mut runtime, "node:http", "createServer").unwrap();

    // Channel to get address info after listening
    let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<(String, u16)>();

    runtime.with_scope(|scope| {
      let create_server_fn =
        v8::Local::new(scope, &create_server).cast::<v8::Function>();

      // Create a simple request handler
      let request_handler = js_callback(scope, (), |scope, _, args, _| {
        let res = args.get(1).cast::<v8::Object>();
        let res = JsObject::new(scope, res);
        res.call(scope, "end", ("ok",));
      });

      // Create server
      let server_val = create_server_fn
        .call(
          scope,
          v8::undefined(scope).into(),
          &[request_handler.into()],
        )
        .unwrap();
      let server = JsObject::new(scope, server_val.cast::<v8::Object>());

      // Set up listening callback that checks address()
      let server_for_cb = server.clone();
      let listen_cb = js_callback(
        scope,
        (server_for_cb, Some(addr_tx)),
        |scope, (server, addr_tx), _, _| {
          // Call server.address() to get the bound address
          let addr_result = server.call(scope, "address", ());

          if addr_result.is_null_or_undefined() {
            panic!("server.address() returned null/undefined");
          }

          let addr_obj = addr_result.cast::<v8::Object>();

          // Get address property
          let address_key = v8::String::new(scope, "address").unwrap();
          let address_val = addr_obj.get(scope, address_key.into()).unwrap();
          let address = address_val
            .to_string(scope)
            .unwrap()
            .to_rust_string_lossy(scope);

          // Get port property
          let port_key = v8::String::new(scope, "port").unwrap();
          let port_val = addr_obj.get(scope, port_key.into()).unwrap();
          let port = port_val.uint32_value(scope).unwrap() as u16;

          if let Some(tx) = addr_tx.take() {
            let _ = tx.send((address, port));
          }
        },
      );

      // Listen with just port 0 - should default host to 0.0.0.0
      server.call(scope, "listen", (0i32, listen_cb));
    });

    // Run event loop until we get the address
    let timeout = tokio::time::Duration::from_secs(5);
    let mut event_loop =
      pin!(runtime.run_event_loop(PollEventLoopOptions::default()));
    let mut addr_rx = pin!(addr_rx);

    let result = loop {
      tokio::select! {
        _ = &mut event_loop => {
          // Keep going
        }
        result = &mut addr_rx => {
          break result.ok();
        }
        _ = tokio::time::sleep(timeout) => {
          break None;
        }
      }
    };

    let (address, port) = result.expect("Failed to get server address");

    // Verify the address is 0.0.0.0 (default when host not specified)
    assert_eq!(
      address, "0.0.0.0",
      "Default host should be 0.0.0.0, got: {}",
      address
    );

    // Verify a port was assigned (not 0)
    assert!(port > 0, "Port should be assigned (got {})", port);

    println!(
      "listen(0) test passed! Server bound to {}:{}",
      address, port
    );
  }
}
