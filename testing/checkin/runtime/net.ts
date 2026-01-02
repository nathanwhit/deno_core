import { op_set_duplex_constructor, SocketCb } from "ext:core/ops";
import { Duplex } from "node:stream";
import { kState } from "ext:checkin_node/internal/streams/utils.js";

Object.setPrototypeOf(SocketCb, Duplex);
Object.setPrototypeOf(SocketCb.prototype, Duplex.prototype);

export { SocketCb };
