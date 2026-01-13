import { createServer } from "node:http";
import { Writable } from "node:stream";

const server = createServer((req, res) => {
  console.log(
    "request",
    typeof res.end,
    typeof res.write,
    typeof res._write,
    res.end === Writable.prototype.end,
    !!res._writableState,
    res._writableState?.constructed,
  );
  try {
    const ok = res.write("Hello, ");
    console.log("write returned", ok);
  } catch (err) {
    console.log("write error", err?.message);
  }
  try {
    res.end("world!");
    console.log("end returned");
  } catch (err) {
    console.log("end error", err?.message);
  }
});

server.listen(3001, "127.0.0.1", () => {
  console.log("listening 3001");
});
