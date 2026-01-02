use deno_core::op2;

deno_core::extension!(
  checkin_node,
  ops = [
    op_exit,
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
  ]
);

#[op2(fast)]
pub fn op_exit(#[smi] code: i32) {
  std::process::exit(code);
}
