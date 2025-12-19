import { SocketCb } from "checkin:net";

const socket = new SocketCb("localhost", 8080);
await socket.connect((data) => {
  console.log(data);
});

await socket.write(new Uint8Array([1, 2, 3, 4, 5]));
