import { SocketCb } from "ext:core/ops";
import { op_set_duplex_constructor } from "ext:core/ops";
import { Duplex } from "node:stream";

Object.setPrototypeOf(SocketCb, Duplex);
Object.setPrototypeOf(SocketCb.prototype, Duplex.prototype);

const orig = SocketCb.prototype.connect;
SocketCb.prototype.connect = function (cb) {
  return orig.call(this, (buf) => {
    return cb(new Uint8Array(buf));
  });
};

export { SocketCb };
