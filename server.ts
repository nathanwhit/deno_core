import { Server } from "checkin:net";

const server = new Server();

server.on("listening", () => {
  console.log("Server is listening on port 3000");
});

const expected = 10000;

let count = 0;

server.on("connection", (socket) => {
  socket.destroy();
  count++;
});

server.listen(3000, "0.0.0.0");
