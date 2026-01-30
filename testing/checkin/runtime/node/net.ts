// Copyright 2018-2025 the Deno authors. MIT license.
import { op_net_connect, Server, Socket } from "ext:core/ops";
import { Duplex } from "node:stream";
import { EventEmitter } from "node:events";

Object.setPrototypeOf(Socket, Duplex);
Object.setPrototypeOf(Socket.prototype, Duplex.prototype);
Object.setPrototypeOf(Server, EventEmitter);
Object.setPrototypeOf(Server.prototype, EventEmitter.prototype);

export {
  op_net_connect as connect,
  op_net_connect as createConnection,
  Server,
  Socket as Socket,
};
