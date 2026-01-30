// Copyright 2018-2025 the Deno authors. MIT license.
import {
  callme,
  getFooFromJs,
  nop,
  propAccessInternalizedUncached,
  propAccessLazy,
  propAccessStatic,
  propAccessStaticUncached,
  rustObjectAssign,
  setFooGetter,
} from "checkin:object";
import { bench } from "checkin:bench";

// deno-lint-ignore no-explicit-any
export function do_not_optimize(v: any) {
  $._ = v;
  return v;
}
export const $ = {
  _: null,
  __() {
    return print($._);
  },
};

export const print = (() => {
  if (globalThis.console?.log) return globalThis.console.log;
  if (globalThis.print && !globalThis.document) return globalThis.print;

  return () => {
    throw new Error("no print function available");
  };
})();

const args = {
  [0]() {
    return { a: 1 };
  },
  [1]() {
    return { b: 2 };
  },
};

const results: number[][] = [[], []];
for (let i = 0; i < 10; i++) {
  {
    const start = performance.now();
    for (let i = 0; i < 1000000; i++) {
      rustObjectAssign(args[0](), args[1]());
    }
    const end = performance.now();
    // console.log(end - start);
    results[0].push(end - start);
  }

  {
    const start = performance.now();
    for (let i = 0; i < 1000000; i++) {
      Object.assign(args[0](), args[1]());
    }
    const end = performance.now();
    // console.log(end - start);
    results[1].push(end - start);
  }
}

console.log(results[0].reduce((a, b) => a + b, 0) / results[0].length);
console.log(results[1].reduce((a, b) => a + b, 0) / results[1].length);

const argy = {
  foo() {
    return () => {
      return;
    };
  },
};

let tot = 0;
for (let i = 0; i < 10; i++) {
  {
    const start = performance.now();
    const nnoop = argy.foo();
    for (let i = 0; i < 1000000; i++) {
      do_not_optimize(nnoop)();
    }
    const end = performance.now();
    tot += end - start;
  }
}
console.log(tot / 10);

tot = 0;
for (let i = 0; i < 10; i++) {
  {
    const start = performance.now();
    const noop = nop;
    for (let i = 0; i < 1000000; i++) {
      do_not_optimize(noop)();
    }
    const end = performance.now();
    tot += end - start;
  }
}
console.log(tot / 10);

const input = () => {
  return nop(0);
};
const id = (a: any) => a;
bench("id", () => nop(id)(input()), {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});
bench("rust id", () => nop(nop)(input()), {
  warmup: 10,
  iterations: 10000000,
  innerIterations: 10,
});
bench("rustObjectAssign", () => {
  rustObjectAssign(args[0](), args[1]());
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("Object.assign", () => {
  Object.assign(args[0](), args[1]());
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("rustCallMe", () => {
  nop(callme)(() => do_not_optimize(nop(input)(0)));
}, {
  warmup: 10,
  iterations: 10_000_000,
  innerIterations: 10,
});

function callMeJs(f: () => void) {
  return nop(f)();
}

bench("callMeJs", () => {
  nop(callMeJs)(() => do_not_optimize(nop(nop)(0)));
}, {
  warmup: 10,
  iterations: 10_000_000,
  innerIterations: 10,
});

const args2 = {
  [0]() {
    return { foo: "foo" };
  },
  [1]() {
    return { bar: "bar" };
  },
};

function getFoo(obj: any) {
  return do_not_optimize(nop(obj.foo));
}

setFooGetter(nop(getFoo));

bench("propAccessLazy", () => {
  do_not_optimize(nop(propAccessLazy)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("propAccessStatic", () => {
  do_not_optimize(nop(propAccessStatic)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("propAccessLazyUndefined", () => {
  do_not_optimize(nop(propAccessLazy)(args2[1]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("propAccessStaticUndefined", () => {
  do_not_optimize(nop(propAccessStatic)(args2[1]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("getFoo", () => {
  do_not_optimize(nop(getFoo)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("getFooFromJs", () => {
  do_not_optimize(nop(getFooFromJs)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("propAccessInternalizedUncached", () => {
  do_not_optimize(nop(propAccessInternalizedUncached)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

bench("propAccessStaticUncached", () => {
  do_not_optimize(nop(propAccessStaticUncached)(args2[0]()));
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});
