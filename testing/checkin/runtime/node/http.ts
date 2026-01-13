import {
  HttpServer,
  IncomingMessage,
  OutgoingMessage,
  ServerResponse,
} from "ext:core/ops";
import { Stream } from "ext:checkin_node/internal/streams/legacy.js";
import { Readable, Writable } from "node:stream";
import { Server } from "node:net";

Object.setPrototypeOf(OutgoingMessage, Writable);
Object.setPrototypeOf(OutgoingMessage.prototype, Writable.prototype);
Object.setPrototypeOf(IncomingMessage, Readable);
Object.setPrototypeOf(IncomingMessage.prototype, Readable.prototype);
Object.setPrototypeOf(ServerResponse, OutgoingMessage);
Object.setPrototypeOf(ServerResponse.prototype, OutgoingMessage.prototype);
// Object.setPrototypeOf(HttpServer, Server);
// Object.setPrototypeOf(HttpServer.prototype, Server.prototype);

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

export { createServer, HttpServer as Server };
