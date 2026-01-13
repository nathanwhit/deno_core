import {
  HttpServer,
  IncomingMessage,
  OutgoingMessage,
  ServerResponse,
} from "ext:core/ops";
import { Stream } from "ext:checkin_node/internal/streams/legacy.js";
import { Readable } from "node:stream";

Object.setPrototypeOf(OutgoingMessage, Stream);
Object.setPrototypeOf(OutgoingMessage.prototype, Stream.prototype);
Object.setPrototypeOf(IncomingMessage, Readable);
Object.setPrototypeOf(IncomingMessage.prototype, Readable.prototype);
Object.setPrototypeOf(ServerResponse, OutgoingMessage);
Object.setPrototypeOf(ServerResponse.prototype, OutgoingMessage.prototype);
export { HttpServer as Server };
