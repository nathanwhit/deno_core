// Copyright 2018-2025 the Deno authors. MIT license.

import {
  DOMPoint,
  DOMPointReadOnly,
  op_callme as callme,
  op_get_foo_from_js as getFooFromJs,
  op_map as mapRust,
  op_map2 as map2Rust,
  op_nop as nop,
  op_object_assign as rustObjectAssign,
  op_prop_access_internalized_uncached as propAccessInternalizedUncached,
  op_prop_access_lazy as propAccessLazy,
  op_prop_access_static as propAccessStatic,
  op_prop_access_static_uncached as propAccessStaticUncached,
  op_set_foo_getter as setFooGetter,
  op_thing_is_string as thingIsStringRust,
  op_validate_args as validateArgsRust,
  Socket,
  TestEnumWrap,
  TestObjectWrap,
} from "ext:core/ops";

export { DOMPoint, DOMPointReadOnly, TestEnumWrap, TestObjectWrap };

export function map<T, U>(array: T[], func: (value: T) => U): U[] {
  const result = new Array<U>(array.length);
  for (let i = 0; i < array.length; i++) {
    const value = array[i];
    result[i] = func(value);
  }
  return result;
}

export function thingIsString(thing: unknown): thing is string {
  return typeof thing === "string";
}

export class Stream {
  constructor() {}
}

Object.setPrototypeOf(Socket.prototype, Stream.prototype);
Object.setPrototypeOf(Socket, Stream);

interface ThingOptions {
  foo?: string;
  bar?: boolean;
  baz?: number;
  required: number;
}

export function validateArgs(
  arg1: string,
  callbackOrOptions?: ((arg: string) => void) | ThingOptions,
  options?: ThingOptions,
) {
  if (typeof arg1 !== "string") {
    throw new Error("Arg1 must be a string");
  }
  const opts = typeof callbackOrOptions === "function"
    ? options
    : callbackOrOptions;
  if (typeof opts !== "object" || opts === null) {
    throw new Error("Options must be an object");
  }

  if (!opts) {
    if (typeof callbackOrOptions !== "function") {
      throw new Error("Callback or options is required");
    }
    return;
  }

  if ("bar" in opts && typeof opts.bar !== "boolean") {
    throw new Error("Bar must be a boolean");
  }
  if ("baz" in opts && typeof opts.baz !== "number") {
    throw new Error("Baz must be a number");
  }
  if (!("required" in opts)) {
    throw new Error("Required is required");
  }
  if (typeof opts.required !== "number") {
    throw new Error("Required must be a number");
  }
  if ("foo" in opts && typeof opts.foo !== "string") {
    throw new Error("Foo must be a string");
  }
}

export {
  callme,
  getFooFromJs,
  map2Rust,
  mapRust,
  nop,
  propAccessInternalizedUncached,
  propAccessLazy,
  propAccessStatic,
  propAccessStaticUncached,
  rustObjectAssign,
  setFooGetter,
  Socket,
  thingIsStringRust,
  validateArgsRust,
};
