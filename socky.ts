import { SocketCb } from "checkin:net";
import { Duplex, Writable } from "node:stream";

// await new Promise((resolve) => setTimeout(resolve, 10000));

const socket = new SocketCb("localhost", 8080);

const prom = Promise.withResolvers<void>();

const iters = 1000000;
let got = 0;
const expected = iters * 5;
const lengths: Record<number, number> = {};

await socket.connect((data: Uint8Array) => data);
const start = performance.now();
// socket.on("data", (data) => {
//   got += data.length;
//   lengths[data.length] = (lengths[data.length] ?? 0) + 1;
//   if (got === expected) {
//     prom.resolve();
//   }
// });
const writeStart = performance.now();
const writeProm = Promise.withResolvers<void>();
let flushed = 0;
for (let i = 0; i < iters; i++) {
  const buf = new Uint8Array([1, 2, 3, 4, 5]);
  socket.write(buf, undefined, () => {
    flushed++;
    if (flushed === iters) {
      writeProm.resolve();
    }
  });
}
await writeProm.promise;
const writeEnd = performance.now();
console.log(`write time: ${writeEnd - writeStart}`);
console.log(`flushed: ${flushed}`);

console.log("connected");
// await prom.promise;
socket.unref();
console.log("done");
console.log(got, performance.now() - start);
console.log(lengths);
console.log(Object.values(lengths).reduce((a, b) => a + b, 0));

// await new Promise((resolve) => setTimeout(resolve, 100000));
