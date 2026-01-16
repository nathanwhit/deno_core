import * as net from "node:net";

// 1. Define the payload
const body = JSON.stringify({ message: "Hello from raw TCP!" });

// 2. Construct the raw HTTP string
// Note: Each line MUST end with \r\n (CRLF)
const httpRequest = "POST /post HTTP/1.1\r\n" +
  "Host: httpbin.org\r\n" +
  "Content-Type: application/json\r\n" +
  `Content-Length: ${Buffer.byteLength(body)}\r\n` +
  "Connection: close\r\n" +
  "\r\n" + // <--- The mandatory "Empty Line" separating headers from body
  body;

// 3. Open the TCP Socket
const client = net.createConnection({ host: "localhost", port: 3000 }, () => {
  console.log("Connected to server!");

  // 4. Write the raw string to the socket
  client.write(httpRequest);
});

// 5. Listen for the raw response
client.on("data", (data) => {
  console.log("--- RAW RESPONSE START ---");
  console.log(data.toString());
  console.log("--- RAW RESPONSE END ---");
  client.end();
});

client.on("end", () => {
  console.log("Disconnected from server");
});
