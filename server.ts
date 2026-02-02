// Copyright 2018-2025 the Deno authors. MIT license.
import { Server } from "node:net";

const port = 3001;

const server = new Server();

server.on("listening", () => {
  console.log(`Server is listening on port ${port}`);
});

server.on("connection", (socket) => {
  socket.on("data", (data) => {
    if (!socket.write(data)) {
      socket.pause();
    }
  });
  socket.on("drain", () => {
    socket.resume();
  });
});

server.listen(port, "0.0.0.0");
