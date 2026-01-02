import { op_set_duplex_constructor, SocketCb } from "ext:core/ops";
import { Duplex } from "node:stream";
import { kState } from "ext:checkin_node/internal/streams/utils.js";

Object.setPrototypeOf(SocketCb, Duplex);
Object.setPrototypeOf(SocketCb.prototype, Duplex.prototype);

// Helper for Rust to push data from async callbacks
// When Rust calls push() from async tasks, JavaScript's _read() may still
// have kSync set from a concurrent call, preventing data events from emitting.
// This wrapper temporarily clears kSync to allow proper event emission.
(SocketCb.prototype as any)._pushFromAsync = function (
  chunk: Uint8Array | null,
) {
  const state = (this as any)._readableState;
  if (!state) return this.push(chunk);

  // kSync flag from Node.js readable stream internals
  const kSync = 1 << 12;

  // Temporarily clear kSync to allow direct data event emission
  const wasSync = (state[kState] & kSync) !== 0;
  if (wasSync) {
    state[kState] &= ~kSync;
  }

  return this.push(chunk);
};

export { SocketCb };
