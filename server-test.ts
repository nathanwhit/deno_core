import { Server, SocketCb } from "checkin:net";

const server = new Server();

const listeningProm = Promise.withResolvers();
const connectionProm = Promise.withResolvers();

let connectionCount = 0;
const expectedConnections = 3;

server.on("listening", () => {
  console.log("listening");
  listeningProm.resolve();
});

server.on("connection", (socket) => {
  connectionCount++;
  console.log("connection", connectionCount);
  socket.write(new Uint8Array([1, 2, 3, 4, 5]), undefined, () => {
    console.log("wrote");
    socket.destroy();
  });
  if (connectionCount === expectedConnections) {
    console.log("resolving connection prom");
    connectionProm.resolve();
  }
});

server.listen(3001, "127.0.0.1");
console.log("listening");
await listeningProm.promise;
console.log("listening prom resolved");

await connectionProm.promise;
console.log("connection prom resolved");

if (connectionCount !== expectedConnections) {
  throw new Error(
    `Expected ${expectedConnections} connections, got ${connectionCount}`,
  );
}

console.log("unrefing server");
server.unref();
export const success = true;
