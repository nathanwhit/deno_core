import * as net from "node:net";
const server = net.createServer((s) => {
  s.end();
});

server.listen(0, "127.0.0.1", () => {
  console.log("listening", server.address());

  const c = net.createConnection(server.address().port);
  c.on("close", () => {
    console.log("close");
    throw new Error("test");
  });
});
