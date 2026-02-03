// Copyright 2018-2025 the Deno authors. MIT license.
import {
  op_is_ip,
  op_is_ipv4,
  op_is_ipv6,
  op_net_connect,
  Server,
  Socket,
} from "ext:core/ops";
import { Duplex } from "node:stream";
import { EventEmitter } from "node:events";

Object.setPrototypeOf(Socket, Duplex);
Object.setPrototypeOf(Socket.prototype, Duplex.prototype);
Object.setPrototypeOf(Server, EventEmitter);
Object.setPrototypeOf(Server.prototype, EventEmitter.prototype);

export {
  op_net_connect as connect,
  op_net_connect as createConnection,
  op_is_ip as isIP,
  op_is_ipv4 as isIPv4,
  op_is_ipv6 as isIPv6,
  Server,
  Socket,
};
