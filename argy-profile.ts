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

for (let i = 0; i < 100000000; i++) {
  nop(
    validateArgsRust(args[0]()[0], args[0]()[1], args[0]()[2]),
  );
}
