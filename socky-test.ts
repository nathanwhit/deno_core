// Copyright 2018-2025 the Deno authors. MIT license.
import { Server, Socket } from "checkin:net";
// import { Server , Socket } from "node:net";

function logOnCall(obj: any, method: string) {
  const original = obj[method];
  obj[method] = (...args: any[]) => {
    console.log(
      `${method} called with args: ${args}, from ${new Error().stack}`,
    );
    return original.apply(obj, args);
  };
}

async function makeConnection() {
  const socket = new Socket();
  logOnCall(socket, "end");
  logOnCall(socket, "destroy");
  const closeProm = Promise.withResolvers();

  socket.on("data", (data) => {
    console.log("data", data);
  });
  socket.on("close", () => {
    console.log("close");
    closeProm.resolve();
  });
  socket.connect(3001, "localhost");
  await closeProm.promise;
}

console.log("making connections");
await Promise.all([
  makeConnection(),
  makeConnection(),
  makeConnection(),
]);
