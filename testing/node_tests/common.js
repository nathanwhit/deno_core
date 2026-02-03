// Copyright 2018-2025 the Deno authors. MIT license.
'use strict';

const noop = () => {};

module.exports = {
  mustCall: (fn = noop, exact = 1) => fn,
  mustNotCall: (msg = 'function should not be called') => {
    return () => { throw new Error(msg); };
  },
};
