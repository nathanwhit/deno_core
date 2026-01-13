mod http;
mod net;

use deno_core::OpState;
use deno_core::op2;
use deno_core::v8;
use std::rc::Rc;

#[derive(Clone)]
pub struct Constructors {
  duplex: Rc<v8::Global<v8::Function>>,
  event_emitter: Rc<v8::Global<v8::Function>>,
  stream: Rc<v8::Global<v8::Function>>,
  readable: Rc<v8::Global<v8::Function>>,
}

impl Constructors {
  fn event_emitter<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.event_emitter)
  }

  fn duplex<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.duplex)
  }

  fn stream<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.stream)
  }

  fn readable<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
  ) -> v8::Local<'s, v8::Function> {
    v8::Local::new(scope, &*self.readable)
  }
}

#[op2]
pub fn op_set_constructors(
  op_state: &mut OpState,
  #[global] duplex_constructor: v8::Global<v8::Function>,
  #[global] event_emitter_constructor: v8::Global<v8::Function>,
  #[global] stream_constructor: v8::Global<v8::Function>,
  #[global] readable_constructor: v8::Global<v8::Function>,
) {
  op_state.put(Constructors {
    duplex: Rc::new(duplex_constructor),
    event_emitter: Rc::new(event_emitter_constructor),
    stream: Rc::new(stream_constructor),
    readable: Rc::new(readable_constructor),
  });
}

deno_core::extension!(
  checkin_node,
  ops = [
    op_exit,
    op_set_constructors,
    net::op_is_ipv4,
    net::op_is_ipv6,
    net::op_is_ip,
  ],
  objects = [
    net::SocketCb,
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
  ]
);

#[op2(fast)]
pub fn op_exit(#[smi] code: i32) {
  std::process::exit(code);
}
