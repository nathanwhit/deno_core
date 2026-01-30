// Copyright 2018-2025 the Deno authors. MIT license.
import {
  HttpServer,
  IncomingMessage,
  OutgoingMessage,
  ServerResponse,
} from "ext:core/ops";
import { Readable, Writable } from "node:stream";

Object.setPrototypeOf(OutgoingMessage, Writable);
Object.setPrototypeOf(OutgoingMessage.prototype, Writable.prototype);
Object.setPrototypeOf(IncomingMessage, Readable);
Object.setPrototypeOf(IncomingMessage.prototype, Readable.prototype);
Object.setPrototypeOf(ServerResponse, OutgoingMessage);
Object.setPrototypeOf(ServerResponse.prototype, OutgoingMessage.prototype);

function createServer(options?: unknown, requestListener?: Function) {
  if (typeof options === "function") {
    requestListener = options;
    options = undefined;
  }
  const server = new HttpServer();
  if (requestListener) {
    server.on("request", requestListener);
  }
  return server;
}

export {
  createServer,
  HttpServer as Server,
  IncomingMessage,
  OutgoingMessage,
  ServerResponse,
};
