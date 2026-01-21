import { createServer } from "node:http";

const server = createServer((_req, res) => {
  res.end(
    new Uint8Array([
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
    ]),
  );
});

server.listen(3000, "127.0.0.1", () => {
  console.log("listening on port 3000");
});
