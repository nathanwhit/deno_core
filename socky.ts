import { SocketCb } from "checkin:net";
import { Duplex, Writable } from "node:stream";

// const timeout = setTimeout(() => {
//   console.log("exiting due to timeout");
//   process.exit(1);
// }, 10_000);
const socket = new SocketCb("localhost", 8080);

const prom = Promise.withResolvers<void>();

const iters = 100000;
let got = 0;
const start = performance.now();
const expected = iters * 5;
const lengths = {};
console.log(Object.getOwnPropertyNames(SocketCb.prototype));
console.log(
  Object.getOwnPropertyNames(Object.getPrototypeOf(SocketCb.prototype)),
);
console.log(
  Object.getOwnPropertyNames(
    Object.getPrototypeOf(Object.getPrototypeOf(SocketCb.prototype)),
  ),
);

await socket.connect((data: Uint8Array) => data);

const writeStart = performance.now();
for (let i = 0; i < iters; i++) {
  socket.write(new Uint8Array([1, 2, 3, 4, 5]), null);
}
const writeEnd = performance.now();
console.log(`write time: ${writeEnd - writeStart}`);
socket.on("data", (data) => {
  got += data.length;
  console.log("got", got);
  lengths[data.length] = (lengths[data.length] ?? 0) + 1;
  if (got === expected) {
    prom.resolve();
  }
});
console.log("connected");
await prom.promise;
socket.unref();
console.log("done");
console.log(got, performance.now() - start);
console.log(lengths);
console.log(Object.values(lengths).reduce((a, b) => a + b, 0));
