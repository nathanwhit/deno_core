import { bench } from "checkin:bench";
import { nop, propAccessLazy, propAccessStatic } from "checkin:object";

const fn = propAccessStatic;

const obj = { foo: "foo" };
const obj2 = { bar: "bar" };
const args2 = {
  [0]() {
    return obj;
  },
  [1]() {
    return obj2;
  },
};
const start = performance.now();
for (let i = 0; i < 10_000_000_0; i++) {
  nop(fn(args2[0]()));
  nop(fn(args2[1]()));
}
const end = performance.now();
console.log(`Time taken: ${end - start} milliseconds`);
