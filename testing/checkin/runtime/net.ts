import { SocketCb } from "ext:core/ops";
import { Duplex } from "node:stream";

Object.setPrototypeOf(SocketCb, Duplex);
Object.setPrototypeOf(SocketCb.prototype, Duplex.prototype);

export { SocketCb };
