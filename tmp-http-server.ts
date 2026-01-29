import * as http from "node:http";
import * as net from "node:net";
import assert from "node:assert";
const body = "hello world\n";

function test(handler, request_generator, response_validator) {
  const server = http.createServer(handler);

  let client_got_eof = false;
  let server_response = "";

  server.listen(0);
  server.on("listening", function () {
    console.log(this.address());
    const c = net.createConnection(this.address().port);

    console.log("created connection");
    c.setEncoding("utf8");
    console.log("set encoding");

    c.on("connect", function () {
      console.log("connect");
      c.write(request_generator());
    });

    c.on("data", function (chunk) {
      console.log("data", chunk);
      server_response += chunk;
    });

    c.on("end", function () {
      console.log("end");
      client_got_eof = true;
      c.end();
      server.close();
      console.log("calling response validator");
      response_validator(server_response, client_got_eof, false);
      console.log("response validator called");
    });
  });
}

{
  function handler(req, res) {
    console.log("handler 1");
    assert.strictEqual(req.httpVersion, "1.0");
    assert.strictEqual(req.httpVersionMajor, 1);
    assert.strictEqual(req.httpVersionMinor, 0);
    res.writeHead(200, { "Content-Type": "text/plain" });
    console.log("wrote head");
    res.end(body);
    console.log("ended");
  }

  function request_generator() {
    return "GET / HTTP/1.0\r\n\r\n";
  }

  function response_validator(server_response, client_got_eof, timed_out) {
    const m = server_response.split("\r\n\r\n");
    console.log("response validator 1", m[1], body);
    assert.strictEqual(m[1], body);
    assert.strictEqual(client_got_eof, true);
    assert.strictEqual(timed_out, false);
    console.log("response validator 1 done");
  }

  console.log("test 1");
  test(handler, request_generator, response_validator);
}

//
// Don't send HTTP/1.1 status lines to HTTP/1.0 clients.
//
// https://github.com/joyent/node/issues/1234
//
{
  function handler(req, res) {
    console.log("handler 2");
    assert.strictEqual(req.httpVersion, "1.0");
    assert.strictEqual(req.httpVersionMajor, 1);
    assert.strictEqual(req.httpVersionMinor, 0);
    res.sendDate = false;
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.write("Hello, ");
    res._send("");
    res.write("world!");
    res._send("");
    res.end();
  }

  function request_generator() {
    return ("GET / HTTP/1.0\r\n" +
      "User-Agent: curl/7.19.7 (x86_64-pc-linux-gnu) libcurl/7.19.7 " +
      "OpenSSL/0.9.8k zlib/1.2.3.3 libidn/1.15\r\n" +
      "Host: 127.0.0.1:1337\r\n" +
      "Accept: */*\r\n" +
      "\r\n");
  }

  function response_validator(server_response, client_got_eof, timed_out) {
    // Accept either HTTP/1.0 or HTTP/1.1 in response (hyper matches request version)
    const expected_response_1_1 = "HTTP/1.1 200 OK\r\n" +
      "Content-Type: text/plain\r\n" +
      "Connection: close\r\n" +
      "\r\n" +
      "Hello, world!";
    const expected_response_1_0 = "HTTP/1.0 200 OK\r\n" +
      "Content-Type: text/plain\r\n" +
      "Connection: close\r\n" +
      "\r\n" +
      "Hello, world!";

    assert.ok(
      server_response === expected_response_1_1 ||
        server_response === expected_response_1_0,
      `Expected HTTP/1.0 or HTTP/1.1 response, got: ${
        JSON.stringify(server_response)
      }`,
    );
    assert.strictEqual(client_got_eof, true);
    assert.strictEqual(timed_out, false);
  }

  test(handler, request_generator, response_validator);
}

{
  function handler(req, res) {
    console.log("handler 3");
    assert.strictEqual(req.httpVersion, "1.1");
    assert.strictEqual(req.httpVersionMajor, 1);
    assert.strictEqual(req.httpVersionMinor, 1);
    res.sendDate = false;
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.write("Hello, ");
    res._send("");
    res.write("world!");
    res._send("");
    res.end();
  }

  function request_generator() {
    return "GET / HTTP/1.1\r\n" +
      "User-Agent: curl/7.19.7 (x86_64-pc-linux-gnu) libcurl/7.19.7 " +
      "OpenSSL/0.9.8k zlib/1.2.3.3 libidn/1.15\r\n" +
      "Connection: close\r\n" +
      "Host: 127.0.0.1:1337\r\n" +
      "Accept: */*\r\n" +
      "\r\n";
  }

  function response_validator(server_response, client_got_eof, timed_out) {
    console.log("response validator 3", server_response);
    const expected_response = "HTTP/1.1 200 OK\r\n" +
      "Content-Type: text/plain\r\n" +
      "Connection: close\r\n" +
      "Transfer-Encoding: chunked\r\n" +
      "\r\n" +
      "7\r\n" +
      "Hello, \r\n" +
      "6\r\n" +
      "world!\r\n" +
      "0\r\n" +
      "\r\n";

    console.log("response validator 3", server_response, expected_response);
    assert.strictEqual(server_response, expected_response);
    assert.strictEqual(client_got_eof, true);
    assert.strictEqual(timed_out, false);
  }

  test(handler, request_generator, response_validator);
}
