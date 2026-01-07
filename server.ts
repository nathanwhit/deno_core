import { Server } from "checkin:net";

const server = new Server();

server.on("listening", () => {
  console.log("Server is listening on port 3000");
});

const expected = 10000;

let count = 0;

server.on("connection", (socket) => {
  // socket.destroy();
  socket.write(new Uint8Array([1, 2, 3, 4, 5]));
  count++;
  socket.on("data", (data) => {
    console.log("data", data);
  });
  socket.on("close", () => {
    console.log("close");
  });
});

server.listen(8080, "0.0.0.0");
