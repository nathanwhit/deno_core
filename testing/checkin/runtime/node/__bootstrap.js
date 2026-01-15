import { op_set_constructors } from "ext:core/ops";
import {
  processTicksAndRejections,
  runNextTicks,
} from "ext:checkin_node/next_tick.ts";
import process from "node:process";
import { core } from "ext:core/mod.js";
import * as addAbortSignal from "ext:checkin_node/internal/streams/add-abort-signal.js";
import * as duplexify from "ext:checkin_node/internal/streams/duplexify.js";
import * as streamTransform from "node:_stream_transform";
import * as utils from "ext:checkin_node/internal/streams/utils.js";
import * as streamDuplex from "node:_stream_duplex";
import * as events from "node:events";
import * as lazyTransform from "ext:checkin_node/internal/streams/lazy_transform.js";
import * as errors from "ext:checkin_node/internal/errors.ts";
import * as destroy from "ext:checkin_node/internal/streams/destroy.js";
import * as duplexpair from "ext:checkin_node/internal/streams/duplexpair.js";
import * as legacy from "ext:checkin_node/internal/streams/legacy.js";
import * as state from "ext:checkin_node/internal/streams/state.js";
import * as operators from "ext:checkin_node/internal/streams/operators.js";
import * as validators from "ext:checkin_node/internal/validators.mjs";
import * as pipeline from "ext:checkin_node/internal/streams/pipeline.js";
import * as endOfStream from "ext:checkin_node/internal/streams/end-of-stream.js";
import * as streamPassthrough from "node:_stream_passthrough";
import * as streamWritable from "node:_stream_writable";
import * as stream from "node:stream";
import * as streamReadable from "node:_stream_readable";
import * as from from "ext:checkin_node/internal/streams/from.js";
import * as buffer from "node:buffer";
import * as compose from "ext:checkin_node/internal/streams/compose.js";
import * as http from "node:http";
import * as net from "node:net";

addAbortSignal;
duplexify;
streamTransform;
utils;
streamDuplex;
events;
lazyTransform;
errors;
destroy;
duplexpair;
legacy;
state;
operators;
validators;
pipeline;
endOfStream;
streamPassthrough;
streamWritable;
stream;
streamReadable;
from;
buffer;
compose;
http;
net;

export function init(globalThis) {
  globalThis.process = process;

  core.setNextTickCallback(processTicksAndRejections);
  core.setMacrotaskCallback(runNextTicks);

  op_set_constructors(
    stream.Duplex,
    events.EventEmitter,
    stream.Readable,
    stream.Writable,
  );
}

export default init;
