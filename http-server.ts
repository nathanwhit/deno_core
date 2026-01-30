// Copyright 2018-2025 the Deno authors. MIT license.
import { createServer } from "node:http";

const body = new Uint8Array([
  0x48,
  0x65,
  0x6c,
  0x6c,
  0x6f,
  0x2c,
  0x20,
  0x77,
  0x6f,
  0x72,
  0x6c,
  0x64,
  0x21,
]);

const server = createServer((_req, res) => {
  res.end(body);
});

server.listen(3000, "127.0.0.1", () => {
  console.log("listening on port 3000");
});
