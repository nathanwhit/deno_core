// Copyright 2018-2025 the Deno authors. MIT license.
use std::cell::RefCell;
use std::rc::Rc;

use super::GlobalHandle;
use super::internalized;
use deno_core::GarbageCollected;
use deno_core::JsRuntime;
use deno_core::RequestedModuleType;
use deno_core::ToV8;
use deno_core::v8;
use deno_error::JsErrorBox;

pub fn import_from(
  runtime: &mut JsRuntime,
  specifier: &str,
  name: &str,
) -> Result<v8::Global<v8::Value>, JsErrorBox> {
  let namespace = runtime
    .get_module_namespace_by_name(specifier, RequestedModuleType::None)
    .map_err(JsErrorBox::from_err)?;
  deno_core::scope!(scope, runtime);
  let namespace = v8::Local::new(scope, namespace);
  let value = namespace
    .get(scope, internalized(scope, name).into())
    .unwrap();
  Ok(v8::Global::new(scope, value))
}

fn cast_fn<
  F: for<'a, 'b, 'c> Fn(
    &'a mut v8::PinScope<'b, 'c>,
    v8::FunctionCallbackArguments<'b>,
    v8::ReturnValue<'b>,
  ),
>(
  f: F,
) -> F {
  f
}

pub trait ToArgs<'a>: Sized {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>>;
}

macro_rules! impl_to_args_for_tuples {
  ($(($($name: ident),*)),+) => {
    $(

      impl<'a, $($name),+> ToArgs<'a> for ($($name,)+)
      where
        $($name: ToV8<'a>,)+
      {
        fn to_args(
          self,
          scope: &mut v8::PinScope<'a, '_>,
        ) -> Vec<v8::Local<'a, v8::Value>> {
          #[allow(non_snake_case)]
          let ($($name,)+) = self;
          vec![
            $($name.to_v8(scope).unwrap().into()),+
          ]
        }
      }
    )+
  };
}

impl<'a, T: ToV8<'a> + Clone> ToArgs<'a> for &[T] {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self
      .iter()
      .map(|x| x.clone().to_v8(scope).unwrap())
      .collect()
  }
}

impl<'a, const N: usize, T: ToV8<'a> + Clone> ToArgs<'a> for &[T; N] {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self
      .iter()
      .map(|x| x.clone().to_v8(scope).unwrap())
      .collect()
  }
}

impl<'a, T: ToV8<'a>> ToArgs<'a> for Vec<T> {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self.into_iter().map(|x| x.to_v8(scope).unwrap()).collect()
  }
}

impl<'a> ToArgs<'a> for () {
  fn to_args(
    self,
    _scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    vec![]
  }
}

impl_to_args_for_tuples!(
  (A),
  (A, B),
  (A, B, C),
  (A, B, C, D),
  (A, B, C, D, E),
  (A, B, C, D, E, F),
  (A, B, C, D, E, F, G),
  (A, B, C, D, E, F, G, H),
  (A, B, C, D, E, F, G, H, I),
  (A, B, C, D, E, F, G, H, I, J),
  (A, B, C, D, E, F, G, H, I, J, K),
  (A, B, C, D, E, F, G, H, I, J, K, L)
);

// impl ToArgs for {

struct CallbackData<T> {
  data: RefCell<T>,
  callback: RefCell<
    Box<
      dyn for<'a> FnMut(
        &mut v8::PinScope<'a, '_>,
        &mut T,
        v8::FunctionCallbackArguments<'a>,
        v8::ReturnValue<'a>,
      ),
    >,
  >,
}

pub fn js_callback<
  's,
  T: 'static,
  F: for<'a> FnMut(
      &mut v8::PinScope<'a, '_>,
      &mut T,
      v8::FunctionCallbackArguments<'a>,
      v8::ReturnValue<'a>,
    ) + 'static,
>(
  scope: &mut v8::PinScope<'s, '_>,
  data: T,
  f: F,
) -> v8::Local<'s, v8::Function> {
  v8::FunctionBuilder::<v8::Function>::new(cast_fn(|scope, args, rv| {
    let data = get_extra_data::<CallbackData<T>>(scope, args.data());
    let mut callback = data.callback.borrow_mut();
    callback(scope, &mut *data.data.borrow_mut(), args, rv);
  }))
  .data(with_extra_data(
    scope,
    CallbackData {
      data: RefCell::new(data),
      callback: RefCell::new(Box::new(f)),
    },
  ))
  .build(scope)
  .unwrap()
}

#[derive(Clone)]
pub struct JsObject {
  obj: GlobalHandle<v8::Object>,
}

impl JsObject {
  pub fn construct<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    constructor: v8::Local<'s, v8::Function>,
    args: impl ToArgs<'s>,
  ) -> Self {
    let args = args.to_args(scope);
    JsObject::new(scope, constructor.new_instance(scope, &args).unwrap())
  }

  pub fn new(scope: &v8::PinScope, obj: v8::Local<v8::Object>) -> Self {
    Self {
      obj: GlobalHandle::new(v8::Global::new(scope, obj)),
    }
  }

  #[allow(dead_code)]
  pub fn get<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
    name: &str,
  ) -> v8::Local<'s, v8::Value> {
    self.obj.get(scope)
      .get(scope, internalized(scope, name).into())
      .unwrap()
  }

  pub fn call<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    args: impl ToArgs<'s>,
  ) -> v8::Local<'s, v8::Value> {
    let local_obj = self.obj.get(scope);
    let args = args.to_args(scope);
    local_obj
      .get(scope, internalized(scope, name).into())
      .unwrap()
      .cast::<v8::Function>()
      .call(scope, local_obj.into(), &args)
      .unwrap()
  }
}

fn get_extra_data<'s, T: 'static>(
  scope: &mut v8::PinScope<'s, '_>,
  value: v8::Local<v8::Value>,
) -> Rc<T> {
  let extra_data =
    deno_core::cppgc::try_unwrap_cppgc_object::<ExtraData<T>>(scope, value)
      .unwrap();
  unsafe { extra_data.as_ref() }.data.clone()
}

fn with_extra_data<'s, T: 'static>(
  scope: &mut v8::PinScope<'s, '_>,
  data: T,
) -> v8::Local<'s, v8::Value> {
  let extra_data = ExtraData {
    data: Rc::new(data),
  };
  let obj = deno_core::cppgc::make_cppgc_object(scope, extra_data);
  obj.into()
}

struct ExtraData<T> {
  data: Rc<T>,
}

unsafe impl<T> GarbageCollected for ExtraData<T> {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"ExtraData"
  }
}

/// Helper for creating HTTP test servers from Rust
pub struct HttpTestServer {
  pub port: u16,
}

impl HttpTestServer {
  /// Create an HTTP server and wait for it to start listening.
  /// The `handler` closure receives (scope, req, res) and should handle the request.
  pub async fn start<F>(runtime: &mut JsRuntime, handler: F) -> Self
  where
    F: for<'a> FnMut(
        &mut v8::PinScope<'a, '_>,
        v8::Local<'a, v8::Object>,
        v8::Local<'a, v8::Object>,
      ) + 'static,
  {
    use deno_core::PollEventLoopOptions;
    use tokio::sync::oneshot;

    // Find an available port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let create_server =
      import_from(runtime, "node:http", "createServer").unwrap();
    let (listening_tx, listening_rx) = oneshot::channel::<()>();

    runtime.with_scope(|scope| {
      let create_server_fn =
        v8::Local::new(scope, &create_server).cast::<v8::Function>();

      // Wrap handler to extract req/res from args
      let request_handler =
        js_callback(scope, RefCell::new(handler), |scope, handler, args, _| {
          let req = args.get(0).cast::<v8::Object>();
          let res = args.get(1).cast::<v8::Object>();
          (handler.borrow_mut())(scope, req, res);
        });

      let server_val = create_server_fn
        .call(
          scope,
          v8::undefined(scope).into(),
          &[request_handler.into()],
        )
        .unwrap();
      let server = JsObject::new(scope, server_val.cast::<v8::Object>());

      let listen_cb =
        js_callback(scope, Some(listening_tx), |_scope, tx, _, _| {
          tx.take().unwrap().send(()).unwrap();
        });

      server.call(scope, "listen", (port as i32, "127.0.0.1", listen_cb));
    });

    // Run event loop until server is listening
    tokio::select! {
      _ = runtime.run_event_loop(PollEventLoopOptions::default()) => {}
      _ = listening_rx => {}
    }

    HttpTestServer { port }
  }

  /// Create an HTTP server with custom state and wait for it to start listening.
  /// The `handler` closure receives (scope, state, req, res).
  pub async fn start_with_state<S, F>(
    runtime: &mut JsRuntime,
    state: S,
    handler: F,
  ) -> Self
  where
    S: 'static,
    F: for<'a> FnMut(
        &mut v8::PinScope<'a, '_>,
        &mut S,
        v8::Local<'a, v8::Object>,
        v8::Local<'a, v8::Object>,
      ) + 'static,
  {
    use deno_core::PollEventLoopOptions;
    use tokio::sync::oneshot;

    // Find an available port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let create_server =
      import_from(runtime, "node:http", "createServer").unwrap();
    let (listening_tx, listening_rx) = oneshot::channel::<()>();

    runtime.with_scope(|scope| {
      let create_server_fn =
        v8::Local::new(scope, &create_server).cast::<v8::Function>();

      // Wrap handler with state
      let request_handler = js_callback(
        scope,
        (RefCell::new(state), RefCell::new(handler)),
        |scope, (state, handler), args, _| {
          let req = args.get(0).cast::<v8::Object>();
          let res = args.get(1).cast::<v8::Object>();
          (handler.borrow_mut())(scope, &mut *state.borrow_mut(), req, res);
        },
      );

      let server_val = create_server_fn
        .call(
          scope,
          v8::undefined(scope).into(),
          &[request_handler.into()],
        )
        .unwrap();
      let server = JsObject::new(scope, server_val.cast::<v8::Object>());

      let listen_cb =
        js_callback(scope, Some(listening_tx), |_scope, tx, _, _| {
          tx.take().unwrap().send(()).unwrap();
        });

      server.call(scope, "listen", (port as i32, "127.0.0.1", listen_cb));
    });

    // Run event loop until server is listening
    tokio::select! {
      _ = runtime.run_event_loop(PollEventLoopOptions::default()) => {}
      _ = listening_rx => {}
    }

    HttpTestServer { port }
  }

  /// Make an HTTP GET request and return response body chunks as they arrive.
  /// Returns a receiver that yields each chunk of data received.
  pub fn get(&self) -> tokio::sync::mpsc::Receiver<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let port = self.port;

    tokio::task::spawn(async move {
      let mut stream =
        match tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
          .await
        {
          Ok(s) => s,
          Err(_) => return,
        };

      let request =
        "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
      if stream.write_all(request.as_bytes()).await.is_err() {
        return;
      }

      let mut buf = [0u8; 1024];
      loop {
        let read_timeout = tokio::time::Duration::from_millis(100);
        match tokio::time::timeout(read_timeout, stream.read(&mut buf)).await {
          Ok(Ok(0)) => break,
          Ok(Ok(n)) => {
            if tx.send(buf[..n].to_vec()).await.is_err() {
              break;
            }
          }
          Ok(Err(_)) => break,
          Err(_) => continue, // Timeout, keep trying
        }
      }
    });

    rx
  }
}

/// Helper to run the event loop until a receiver gets a value or timeout.
/// Returns Some(value) if received, None if timed out.
pub async fn run_until<T>(
  runtime: &mut deno_core::JsRuntime,
  rx: tokio::sync::oneshot::Receiver<T>,
  timeout: std::time::Duration,
) -> Option<T> {
  use deno_core::PollEventLoopOptions;
  use std::pin::pin;

  let mut event_loop =
    pin!(runtime.run_event_loop(PollEventLoopOptions::default()));
  let mut rx = pin!(rx);

  loop {
    tokio::select! {
      _ = &mut event_loop => {}
      result = &mut rx => {
        return result.ok();
      }
      _ = tokio::time::sleep(timeout) => {
        return None;
      }
    }
  }
}

/// Creates a callback that signals a oneshot channel when called.
pub fn signal_cb<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  tx: tokio::sync::oneshot::Sender<()>,
) -> v8::Local<'s, v8::Function> {
  js_callback(scope, Some(tx), |_scope, tx, _, _| {
    if let Some(tx) = tx.take() {
      let _ = tx.send(());
    }
  })
}

/// Creates a callback that sends a value on a oneshot channel when called.
pub fn send_cb<'s, T: Send + 'static>(
  scope: &mut v8::PinScope<'s, '_>,
  tx: tokio::sync::oneshot::Sender<T>,
  f: impl FnMut(&mut v8::PinScope<'_, '_>, v8::FunctionCallbackArguments<'_>) -> T
  + 'static,
) -> v8::Local<'s, v8::Function> {
  js_callback(scope, (Some(tx), f), |scope, (tx, f), args, _| {
    if let Some(tx) = tx.take() {
      let _ = tx.send(f(scope, args));
    }
  })
}

/// Test helper for net module tests
pub struct NetTest {
  pub runtime: JsRuntime,
  socket_cons: v8::Global<v8::Value>,
}

impl NetTest {
  pub fn new() -> Self {
    let (mut runtime, _) =
      crate::checkin::runner::create_runtime_without_snapshot(
        false,
        None,
        vec![],
        deno_core::RuntimeOptions::default(),
      );
    let socket_cons = import_from(&mut runtime, "node:net", "Socket").unwrap();
    Self {
      runtime,
      socket_cons,
    }
  }

  /// Create a new Socket and run setup code in a scope
  pub fn with_socket<R>(
    &mut self,
    f: impl FnOnce(&mut v8::PinScope, JsObject) -> R,
  ) -> R {
    let socket_cons = self.socket_cons.clone();
    self.runtime.with_scope(|scope| {
      let cons = v8::Local::new(scope, &socket_cons).cast::<v8::Function>();
      let socket = JsObject::construct(scope, cons, ());
      f(scope, socket)
    })
  }

  /// Run event loop until receiver gets a value or timeout (5 seconds default)
  pub async fn run_until<T>(
    &mut self,
    rx: tokio::sync::oneshot::Receiver<T>,
  ) -> Option<T> {
    run_until(&mut self.runtime, rx, std::time::Duration::from_secs(5)).await
  }
}
