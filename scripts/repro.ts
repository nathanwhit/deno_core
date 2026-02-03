// Copyright 2018-2025 the Deno authors. MIT license.
import $ from "dax";

// Instrumented server script
const serverScript = `
import { Server } from "node:net";

const port = 3001;
const server = new Server();

let dataCount = 0;
let pauseCount = 0;
let drainCount = 0;

setInterval(() => {
  console.log(\`STATS: data: \${dataCount}, pause: \${pauseCount}, drain: \${drainCount}\`);
}, 2000);

server.on("listening", () => {
  console.log("Server is listening on port " + port);
});

server.on("connection", (socket) => {
  socket.on("data", (data) => {
    dataCount++;
    if (!socket.write(data)) {
      pauseCount++;
      socket.pause();
    }
  });
  socket.on("drain", () => {
    drainCount++;
    socket.resume();
  });
});

server.listen(port, "0.0.0.0");
`;

await Deno.writeTextFile("/tmp/instrumented_server.ts", serverScript);

const server = $`./target/release-with-debug/dcore /tmp/instrumented_server.ts`
  .stdout("piped")
  .spawn();

// Stream stdout to console (after we see "listening")
let streamingStarted = false;
function startStreaming(reader: ReadableStreamDefaultReader<Uint8Array>) {
  const decoder = new TextDecoder();
  (async () => {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      console.log(decoder.decode(value));
    }
  })();
}

async function cleanup() {
  try {
    server.kill("SIGTERM");
  } catch {
    // ignore
  }
  try {
    await server;
  } catch {
    // ignore
  }
}

Deno.addSignalListener("SIGINT", async () => {
  await cleanup();
  Deno.exit(130);
});

// Wait for server to be ready, then continue streaming
const reader = server.stdout().getReader();
const decoder = new TextDecoder();
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  const text = decoder.decode(value);
  console.log(text);
  if (text.includes("Server is listening")) {
    startStreaming(reader);
    break;
  }
}

try {
  const result =
    await $`socky benchmark -H 127.0.0.1 -p 3001 -c 1000 -j 50 -m 50 --read`
      .timeout("20s")
      .noThrow();

  if (result.code === 124) {
    console.error("TIMEOUT: Benchmark took longer than 20 seconds");
    Deno.exit(1);
  } else if (result.code !== 0) {
    console.error(`Benchmark failed with code ${result.code}`);
    Deno.exit(1);
  } else {
    console.log("Benchmark completed successfully");
  }
} finally {
  await cleanup();
}
