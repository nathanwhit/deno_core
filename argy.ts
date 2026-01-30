// Copyright 2018-2025 the Deno authors. MIT license.
import { bench } from "checkin:bench";
import { nop, validateArgs, validateArgsRust } from "checkin:object";
let counter = 0;
const args = {
  [0]() {
    return ["a", () => {}, {
      foo: "foo" + counter++,
      bar: counter % 2 === 0,
      baz: 1 + counter++,
      required: 1 + counter++,
    }];
  },
};
bench("validateArgs", () => {
  nop(
    nop(validateArgs)(nop(args[0]()[0]), nop(args[0]()[1]), nop(args[0]()[2])),
  );
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});

for (let i = 0; i < 10000000; i++) {
  nop(
    nop(validateArgsRust)(
      nop(args[0]()[0]),
      nop(args[0]()[1]),
      nop(args[0]()[2]),
    ),
  );
}

bench("validateArgsRust", () => {
  nop(
    nop(validateArgsRust)(
      nop(args[0]()[0]),
      nop(args[0]()[1]),
      nop(args[0]()[2]),
    ),
  );
}, {
  warmup: 10,
  iterations: 1000000,
  innerIterations: 10,
});
