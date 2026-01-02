import { Socket } from "node:net";

const socket = new Socket();
const prom = Promise.withResolvers<void>();

const iters = 100000;
let got = 0;
const expected = iters * 5;
const start = performance.now();
socket.connect(8080, "localhost");
const lengths: Record<number, number> = {};
socket.on("data", (data: Uint8Array) => {
  got += data.length;
  lengths[data.length] = (lengths[data.length] ?? 0) + 1;
  if (got === expected) {
    prom.resolve();
  }
});

const writeStart = performance.now();
// for (let i = 0; i < iters; i++) {
//   socket.write(new Uint8Array([1, 2, 3, 4, 5]));
// }
const writeEnd = performance.now();
console.log(`write time: ${writeEnd - writeStart}`);
await prom.promise;
socket.unref();
console.log(got, performance.now() - start);
console.log(lengths);

console.log(Object.values(lengths).reduce((a, b) => a + b, 0));
