// Copyright 2018-2025 the Deno authors. MIT license.
import { nextTick } from "ext:checkin_node/next_tick.ts";
import { op_exit } from "ext:core/ops";
import { EventEmitter } from "node:events";

export const process = {
  nextTick,
  exit: (code: number) => {
    process.emit("exit", code);
    op_exit(code);
  },
  _exiting: false,
  exitCode: undefined,
};
Object.setPrototypeOf(process, EventEmitter.prototype);

export function dispatchProcessExitEvent() {
  if (!process._exiting) {
    process._exiting = true;
    process.emit("exit", process.exitCode || 0);
  }
}

export default process;
