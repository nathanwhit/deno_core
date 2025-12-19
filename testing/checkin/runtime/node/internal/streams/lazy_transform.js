// deno-lint-ignore-file
// Copyright 2018-2025 the Deno authors. MIT license.

import { primordials } from "ext:core/mod.js";
import Transform from "node:_stream_transform";
// LazyTransform is a special type of Transform stream that is lazily loaded.
// This is used for performance with bi-API-ship: when two APIs are available
// for the stream, one conventional and one non-conventional.
"use strict";

const {
  ObjectDefineProperties,
  ObjectDefineProperty,
  ObjectSetPrototypeOf,
} = primordials;

function LazyTransform(options) {
  this._options = options;
}
ObjectSetPrototypeOf(LazyTransform.prototype, Transform.prototype);
ObjectSetPrototypeOf(LazyTransform, Transform);

function makeGetter(name) {
  return function () {
    Transform.call(this, this._options);
    this._writableState.decodeStrings = false;
    return this[name];
  };
}

function makeSetter(name) {
  return function (val) {
    ObjectDefineProperty(this, name, {
      __proto__: null,
      value: val,
      enumerable: true,
      configurable: true,
      writable: true,
    });
  };
}

ObjectDefineProperties(LazyTransform.prototype, {
  _readableState: {
    __proto__: null,
    get: makeGetter("_readableState"),
    set: makeSetter("_readableState"),
    configurable: true,
    enumerable: true,
  },
  _writableState: {
    __proto__: null,
    get: makeGetter("_writableState"),
    set: makeSetter("_writableState"),
    configurable: true,
    enumerable: true,
  },
});
export default LazyTransform;
export { LazyTransform };
