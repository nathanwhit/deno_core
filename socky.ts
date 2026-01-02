import { SocketCb } from "checkin:net";

const timeout = setTimeout(() => {
  console.log("exiting due to timeout");
  process.exit(1);
}, 5_000);
const socket = new SocketCb("localhost", 8080);

const prom = Promise.withResolvers<void>();

const iters = 100000;
let got = 0;
const start = performance.now();
const expected = iters * 5;
const lengths = {};

await socket.connect((data: Uint8Array) => data);
socket.on("data", (data) => {
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
clearTimeout(timeout);
console.log("done");
console.log(got, performance.now() - start);
console.log(lengths);
console.log(Object.values(lengths).reduce((a, b) => a + b, 0));
