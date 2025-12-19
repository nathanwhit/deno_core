// Copyright 2018-2025 the Deno authors. MIT license.
// Copyright Joyent, Inc. and other Node contributors.

import { core } from "ext:core/mod.js";

import { FixedQueue } from "ext:checkin_node/fixed_queue.ts";

interface Tock {
  callback: (...args: Array<unknown>) => void;
  args: Array<unknown>;
}

const queue = new FixedQueue();

export function processTicksAndRejections() {
  let tock: Tock;
  do {
    // deno-lint-ignore no-cond-assign
    while (tock = queue.shift()) {
      try {
        const callback = tock.callback;
        if (tock.args === undefined) {
          callback();
        } else {
          const args = (tock as Tock).args;
          switch (args.length) {
            case 1:
              callback(args[0]);
              break;
            case 2:
              callback(args[0], args[1]);
              break;
            case 3:
              callback(args[0], args[1], args[2]);
              break;
            case 4:
              callback(args[0], args[1], args[2], args[3]);
              break;
            default:
              callback(...args);
          }
        }
      } catch (e) {
        reportError(e);
      }
    }
    core.runMicrotasks();
  } while (!queue.isEmpty());
  core.setHasTickScheduled(false);
}

export function runNextTicks() {
  if (queue.isEmpty() || !core.hasTickScheduled()) {
    return true;
  }

  processTicksAndRejections();
  return true;
}

// `nextTick()` will not enqueue any callback when the process is about to
// exit since the callback would not have a chance to be executed.
export function nextTick(this: unknown, callback: () => void): void;
export function nextTick<T extends Array<unknown>>(
  this: unknown,
  callback: (...args: T) => void,
  ...args: T
): void;
export function nextTick<T extends Array<unknown>>(
  this: unknown,
  callback: (...args: T) => void,
  ...args: T
) {
  let args_;
  switch (args.length) {
    case 0:
      break;
    case 1:
      args_ = [args[0]];
      break;
    case 2:
      args_ = [args[0], args[1]];
      break;
    case 3:
      args_ = [args[0], args[1], args[2]];
      break;
    default:
      args_ = new Array(args.length);
      for (let i = 0; i < args.length; i++) {
        args_[i] = args[i];
      }
  }

  if (queue.isEmpty()) {
    core.setHasTickScheduled(true);
  }
  queue.push({ callback, args: args_ });
}
