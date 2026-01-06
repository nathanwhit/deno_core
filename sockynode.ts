import { Socket } from "node:net";

const socket = new Socket();
const prom = Promise.withResolvers<void>();

const iters = 1000000;
let got = 0;
// const expected = iters * 5;
const expected = 4 * 1024 * 1024 * 1024;
socket.connect(8080, "localhost");
const start = performance.now();
const lengths: Record<number, number> = {};
socket.on("data", (data: Uint8Array) => {
  got += data.length;
  lengths[data.length] = (lengths[data.length] ?? 0) + 1;
  if (got === expected) {
    prom.resolve();
  }
});

socket.on("error", (error) => {
  // console.log("GOT ERROR", error);
  prom.reject(error);
});

// socket.end(new Uint8Array([1, 2, 3, 4, 5]));
// socket.destroy();

// const writeProm = Promise.withResolvers<void>();
// const writeStart = performance.now();
// let flushed = 0;
// for (let i = 0; i < iters; i++) {
//   const buf = new Uint8Array([1, 2, 3, 4, 5]);
//   socket.write(buf, undefined, () => {
//     flushed++;
//     if (flushed === iters) {
//       writeProm.resolve();
//     }
//   });
// }
// await writeProm.promise;
// const writeEnd = performance.now();
// console.log(`write time: ${writeEnd - writeStart}`);
// console.log(`flushed: ${flushed}`);
await prom.promise;
socket.unref();
console.log(got, performance.now() - start);
console.log(lengths);

console.log(Object.values(lengths).reduce((a, b) => a + b, 0));
