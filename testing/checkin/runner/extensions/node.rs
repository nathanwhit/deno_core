// Copyright 2018-2025 the Deno authors. MIT license.
mod http;
mod net;
#[cfg(test)]
mod test_utils;

use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;
use std::rc::Rc;

#[derive(Clone)]
pub struct Constructors {
  duplex: GlobalHandle<v8::Function>,
  event_emitter: GlobalHandle<v8::Function>,
  readable: GlobalHandle<v8::Function>,
  writable: GlobalHandle<v8::Function>,
}

impl Constructors {
  fn event_emitter<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.event_emitter.get(scope)
  }

  // fn duplex<'s>(
  //   &self,
  //   scope: &v8::PinScope<'s, '_>,
  // ) -> v8::Local<'s, v8::Function> {
  //   v8::Local::new(scope, &*self.duplex)
  // }

  fn readable<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.readable.get(scope)
  }

  fn writable<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.writable.get(scope)
  }
}

#[op2]
pub fn op_set_constructors(
  op_state: &mut OpState,
  #[global] duplex_constructor: v8::Global<v8::Function>,
  #[global] event_emitter_constructor: v8::Global<v8::Function>,
  #[global] readable_constructor: v8::Global<v8::Function>,
  #[global] writable_constructor: v8::Global<v8::Function>,
) {
  op_state.put(Constructors {
    duplex: GlobalHandle::new(duplex_constructor),
    event_emitter: GlobalHandle::new(event_emitter_constructor),
    readable: GlobalHandle::new(readable_constructor),
    writable: GlobalHandle::new(writable_constructor),
  });
}

#[op2]
pub fn op_set_next_tick_func(
  op_state: &mut OpState,
  #[global] func: v8::Global<v8::Function>,
) {
  op_state.put(NextTickFunc {
    func: GlobalHandle::new(func),
  });
}

#[derive(Clone)]
pub struct NextTickFunc {
  func: GlobalHandle<v8::Function>,
}

impl NextTickFunc {
  pub fn get<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.func.get(scope)
  }
}

pub struct ScopeHolder {
  spawner: deno_core::V8TaskSpawner,
  isolate_ptr: v8::UnsafeRawIsolatePtr,
  context: GlobalHandle<v8::Context>,
}

impl ScopeHolder {
  pub fn new(
    spawner: deno_core::V8TaskSpawner,
    isolate_ptr: v8::UnsafeRawIsolatePtr,
    context: GlobalHandle<v8::Context>,
  ) -> Self {
    ScopeHolder {
      spawner,
      isolate_ptr,
      context,
    }
  }

  pub fn new_from_scope(
    spawner: deno_core::V8TaskSpawner,
    scope: &mut v8::PinScope,
  ) -> Self {
    let isolate_ptr = unsafe { scope.as_raw_isolate_ptr() };
    let context =
      GlobalHandle::new(v8::Global::new(scope, scope.get_current_context()));
    Self::new(spawner, isolate_ptr, context)
  }

  pub fn with_scope(&self, f: impl FnOnce(&mut v8::PinScope) + 'static) {
    self.spawner.spawn(move |scope| {
      v8::tc_scope!(let scope, scope);
      f(scope);
    })
  }

  pub fn with_scope_immediately<R>(
    &self,
    f: impl FnOnce(&mut v8::PinScope) -> R,
  ) -> R {
    let mut isolate =
      unsafe { v8::Isolate::from_raw_isolate_ptr(self.isolate_ptr) };
    v8::scope!(let scope, &mut isolate);
    let context = self.context.get(scope);
    let scope = &mut v8::ContextScope::new(scope, context);
    f(scope)
  }
}

pub struct JsMethod {
  function: GlobalHandle<v8::Function>,
}

impl JsMethod {
  #[allow(unused)]
  pub fn new(function: v8::Global<v8::Function>) -> Self {
    JsMethod {
      function: GlobalHandle::new(function),
    }
  }

  pub fn capture(
    scope: &v8::PinScope,
    object: v8::Local<v8::Object>,
    name: &str,
  ) -> Self {
    JsMethod {
      function: GlobalHandle::new(v8::Global::new(
        scope,
        object
          .get(scope, internalized(scope, name).into())
          .unwrap()
          .cast::<v8::Function>(),
      )),
    }
  }

  pub fn get<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    self.function.get(scope)
  }
}

pub fn internalized<'a>(
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

pub struct GlobalHandle<T> {
  handle: Rc<v8::Global<T>>,
}

impl<T> Clone for GlobalHandle<T> {
  fn clone(&self) -> Self {
    Self {
      handle: self.handle.clone(),
    }
  }
}

impl<T> GlobalHandle<T> {
  pub fn new(handle: v8::Global<T>) -> Self {
    GlobalHandle {
      handle: Rc::new(handle),
    }
  }
}

impl<T> From<v8::Global<T>> for GlobalHandle<T> {
  fn from(handle: v8::Global<T>) -> Self {
    GlobalHandle::new(handle)
  }
}

impl<T> GlobalHandle<T>
where
  v8::Global<T>: v8::Handle<Data = T>,
{
  pub fn get<'s>(&self, scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, T> {
    v8::Local::new(scope, &*self.handle)
  }
}

deno_core::extension!(
  checkin_node,
  ops = [
    op_exit,
    op_set_constructors,
    op_set_next_tick_func,
    net::op_is_ipv4,
    net::op_is_ipv6,
    net::op_is_ip,
    net::op_net_connect,
  ],
  objects = [
    net::Socket,
    net::Server,
    http::HttpServer,
    http::IncomingMessage,
    http::OutgoingMessage,
    http::ServerResponse,
  ],
  esm = [
    dir "checkin/runtime/node",
    "next_tick.ts",
    "fixed_queue.ts",
    "__bootstrap.js",
    "internal/streams/add-abort-signal.js",
    "internal/streams/compose.js",
    "internal/streams/destroy.js",
    "internal/streams/duplexify.js",
    "internal/streams/duplexpair.js",
    "internal/streams/end-of-stream.js",
    "internal/streams/from.js",
    "internal/streams/lazy_transform.js",
    "internal/streams/legacy.js",
    "internal/streams/operators.js",
    "internal/streams/pipeline.js",
    "internal/streams/state.js",
    "internal/streams/utils.js",
    "internal/errors.ts",
    "internal/validators.mjs",
    "node:_stream_duplex" = "internal/streams/duplex.js",
    "node:_stream_passthrough" = "internal/streams/passthrough.js",
    "node:_stream_readable" = "internal/streams/readable.js",
    "node:_stream_transform" = "internal/streams/transform.js",
    "node:_stream_writable" = "internal/streams/writable.js",
    "node:stream" = "stream.ts",
    "node:process" = "process.ts",
    "node:events" = "internal/events.mjs",
    "node:buffer" = "internal/buffer.mjs",
    "node:http" = "http.ts",
    "node:net" = "net.ts",
    "node:assert" = "assert.ts"
  ]
);

#[op2(fast)]
pub fn op_exit(#[smi] code: i32) {
  std::process::exit(code);
}
