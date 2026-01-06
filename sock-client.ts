import { SocketCb } from "checkin:net";

const socket = new SocketCb("localhost", 3000);

socket.on("data", (data) => {
  console.log("Received data:", Deno.core.decode(data));
  socket.write(Deno.core.encode("hello world"));
});

await socket.connect();
