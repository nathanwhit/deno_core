import { Server } from "node:net";

const port = 3001;

const server = new Server();

server.on("listening", () => {
  console.log(`Server is listening on port ${port}`);
});

server.on("connection", (socket) => {
  socket.on("data", (data) => {
    socket.write(data);
  });
});

server.listen(port, "0.0.0.0");
