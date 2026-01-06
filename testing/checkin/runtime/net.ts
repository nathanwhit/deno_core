import { Server, SocketCb } from "ext:core/ops";
import { Duplex } from "node:stream";
import { EventEmitter } from "node:events";

Object.setPrototypeOf(SocketCb, Duplex);
Object.setPrototypeOf(SocketCb.prototype, Duplex.prototype);
Object.setPrototypeOf(Server, EventEmitter);
Object.setPrototypeOf(Server.prototype, EventEmitter.prototype);

export { Server, SocketCb };
