# node:http API Surface and Behaviors (main HEAD)

Scope: `node:http` public/user-visible API sufficient for clean-room reimplementation.

## Top-Level Exports (`lib/http.js`)
- Constructors/classes: `Server`, `ServerResponse`, `IncomingMessage`, `OutgoingMessage`, `ClientRequest`, `Agent`.
- Functions: `createServer(opts?, requestListener?)`, `request(url|opts, options?, cb?)`, `get(url|opts, options?, cb?)`, `_connectionListener`.
- Constants: `METHODS` (sorted), `STATUS_CODES`.
- Utilities: `validateHeaderName`, `validateHeaderValue`, `setMaxIdleHTTPParsers(max)`.
- Accessors: `maxHeaderSize` getter (reads `--max-http-header-size`), `globalAgent` getter/setter.
- Lazy undici exports: `WebSocket`, `CloseEvent`, `MessageEvent`.

## Server
Constructor: `new Server(options?, requestListener?)`.
Options: `IncomingMessage`, `ServerResponse`, `insecureHTTPParser`, `maxHeaderSize`, `requireHostHeader` (default true), `joinDuplicateHeaders`, `highWaterMark`, `rejectNonStandardBodyWrites` (default false), `optimizeEmptyRequests`, `uniqueHeaders`, `shouldUpgradeCallback`, timeouts (`requestTimeout` default 300000ms, `headersTimeout` default min(60000, requestTimeout), `keepAliveTimeout` default 5000ms, `keepAliveTimeoutBuffer` default 1000ms), `connectionsCheckingInterval` default 30000ms, `noDelay`, `keepAlive`, `keepAliveInitialDelay`.
Prototype methods: `close(cb)`, `closeAllConnections()`, `closeIdleConnections()`, `setTimeout(msecs, cb)`.
Properties configurable: `timeout` (socket-level inactivity), `maxHeadersCount` (null unlimited), `maxRequestsPerSocket` (0 unlimited), `keepAliveTimeout`, `keepAliveTimeoutBuffer`, `requestTimeout`, `headersTimeout`, `requireHostHeader`, `joinDuplicateHeaders`, `rejectNonStandardBodyWrites`.
Events: `request` (req,res), `checkContinue` (Expect: 100-continue), `checkExpectation` (other Expect), `connect` (CONNECT), `upgrade` (Upgrade), `dropRequest` (maxRequestsPerSocket exceeded; server replies 503), `clientError` (parser errors; default 400/408/413/431 depending), `timeout` (socket idle), `connection`, `close`, `listening`.
Lifecycle/order: connection → parser → on headers complete build `IncomingMessage` → if Expect handling then `checkContinue`/`checkExpectation` else auto 100 or 417 → `request` emitted. Response finish triggers detach, `close` on response next tick. Keep-alive timeout applied after response if keep-alive; otherwise socket ended/destroyed. Host header required for HTTP/1.1 when `requireHostHeader` true; missing yields 400 + close.
Upgrades: if request has upgrade/connect and listener exists, emit with `req`, `socket`/`UpgradeStream`, `head` (remaining bytes). If body not finished, wraps socket until body done. No listener → socket destroyed. PRI method closes.
Keep-alive and limits: `maxRequestsPerSocket` counts per connection; when limit exceeded emit `dropRequest`, send 503, keep connection? Response forced. If `maxRequestsPerSocket` is null, unlimited. Keep-alive headers: `Connection: keep-alive` plus `Keep-Alive: timeout=<seconds>[, max=<n>]` when applicable; otherwise `Connection: close`.
Timeout enforcement: periodic check for `headersTimeout` and `requestTimeout` via connection list; socket timeout events propagate to req/res/server; if unhandled destroy.

## ServerResponse (extends OutgoingMessage)
Defaults: `statusCode` 200, `statusMessage` undefined (filled from STATUS_CODES in writeHead), `sendDate` true. `_hasBody` false for HEAD or 1xx/204/304, disables chunked/length.
Methods: `writeContinue()`, `writeProcessing()`, `writeEarlyHints(hints)`, `writeHead(statusCode[, reason|headers])`, `setHeader/getHeader/getHeaders/getHeaderNames/getRawHeaderNames/hasHeader/appendHeader/removeHeader`, `flushHeaders()`, `addTrailers(headers)`, `setTimeout`, `end`, `write`, `destroy`. Emits `finish`, `close`, `drain`, `timeout`, `error`. `headersSent` getter. Body writes after end throw; body writes when not allowed are ignored unless `rejectNonStandardBodyWrites` true (then throw). `writeHead` validates status code 100-999; throwing ERR_HTTP_HEADERS_SENT on duplicates.
Connection logic: if chunked but status 204/304, chunked disabled and connection closed. If Expect: 100 not satisfied before final status, `shouldKeepAlive` cleared. Keep-alive header generation depends on content-length/chunked flags and removed headers.

## IncomingMessage (Readable)
Fields: `socket`/`client`, `method`, `url`, `statusCode`, `statusMessage`, `httpVersion`, `httpVersionMajor/Minor`, `headers`, `rawHeaders`, `headersDistinct`, `trailers`, `rawTrailers`, `trailersDistinct`, `complete`, `aborted`, `upgrade`, `joinDuplicateHeaders`.
Methods: `setTimeout`, `_read` (resumes socket), `_destroy` (emits `aborted` if not complete, may destroy socket), `_dump` (drains unread data), `_dumpAndCloseReadable` (optimization for empty requests). `connection` getter alias.
Header handling: known headers optimized casing; joining rules—comma-join by default, cookie joins with `; `, set-cookie as array, authorization can duplicate when `joinDuplicateHeaders` true, otherwise first wins.
Events: standard Readable (`data`, `end`, `close`, `aborted`, `error`, `timeout`).

## OutgoingMessage (base for ServerResponse/ClientRequest)
State: `_header`, `_headerSent`, `finished`, `chunkedEncoding`, `_contentLength`, `shouldKeepAlive`, `_last`, `_trailer`, `kOutHeaders` map, `strictContentLength`, `joinDuplicateHeaders`, highWaterMark, `_closed`, `writable`, `destroyed`, `writableLength`, `writableFinished`, `writableNeedDrain`.
Methods: `_storeHeader(firstLine, headers)` (adds Date if needed; sets Connection/Keep-Alive/chunked/Content-Length), `_send`, `write`, `end`, `cork`/`uncork`, `flushHeaders`, `setHeader(s)/appendHeader/removeHeader/getHeader*`, `hasHeader`, `destroy`, `setTimeout`, `_flush` to drain buffered output. `pipe` always errors. Trailers only allowed with chunked; otherwise ERR_HTTP_TRAILER_INVALID.
Content-Length enforcement: if `strictContentLength` true and writes exceed/undershoot, throws ERR_HTTP_CONTENT_LENGTH_MISMATCH.
Backpressure: `write` returns false when buffered; `drain` emitted when ready. Cork/uncork batches chunked frames.

## ClientRequest (extends OutgoingMessage)
Construction: `new ClientRequest(input, options, cb)` supports URL string/URL object or options; merges options with defaults; `method` uppercased token (default GET); `path` default `/`. Validates unescaped chars (throws on INVALID_PATH_REGEX). `Host` header auto-set unless `setHost:false` or `setDefaultHeaders:false`. `auth` sets `Authorization` if not provided. `setDefaultHeaders:false` also marks connection/content-length/TE removed.
Options: `agent` (Agent|false|null|undefined), `_defaultAgent`, `createConnection`, `defaultPort`, `family`, `hints`, `localAddress`, `localPort`, `lookup`, `insecureHTTPParser`, `joinDuplicateHeaders`, `maxHeaderSize`, `setHost`, `setDefaultHeaders`, `signal`, `socketPath`, `timeout`, `uniqueHeaders`, proxy support via agent `kProxyConfig`. `maxHeadersCount` per-request propagates to parser (pairs).
Lifecycle: if agent provided, `agent.addRequest`; agent false creates new agent instance (non-keepalive by default). Without agent, sets `Connection: close` and uses provided createConnection or `net.createConnection`. Emits `socket` when assigned. If `Expect` header present, `_storeHeader` is called immediately before body; otherwise stored later.
Events: `response` (final response), `information` for all 1xx except 101, `continue` on 100, `upgrade`/`connect` on 101/CONNECT with `res`, `socket`, `head`, `timeout`, `error`, `abort`, `close`, `drain`. Aborting: `abort()` sets `aborted`, emits `abort` next tick, destroys; `destroy(err)` dumps res, destroys socket. Timeout: `setTimeout` attaches socket listener to emit `timeout` on request; response timeout triggers `res.emit('timeout')`.
Response handling: parser builds `IncomingMessage`; informational responses clear `req.res` and emit `information`; final attaches `res.req=req`, sets keep-alive based on headers/status, emits `response` (or dumps if no listener/aborted). For HEAD/304 bodies skipped. Upgrade/CONNECT detaches socket from agent and emits event with remaining bytes; no listener destroys socket. Double responses destroy socket and keep first response.
Backpressure: `write` uses OutgoingMessage logic; `drain` from socket forwarded.

## Agent
Defaults: protocol `http:`, defaultPort 80, `keepAlive` false (globalAgent true), `keepAliveMsecs` 1000, `maxSockets` Infinity, `maxFreeSockets` 256, `maxTotalSockets` Infinity, `scheduling` lifo, `timeout` optional, `agentKeepAliveTimeoutBuffer` 1000.
Pooling: keyed by host:port:localAddress[:family][:socketPath]. Free sockets reused (FIFO/LIFO by scheduling). Free socket kept only if writable, under limits, and `keepSocketAlive()` returns true (respects server Keep-Alive timeout hint minus buffer). Otherwise destroyed.
Request queue: if at limits, request enqueued; when socket freed or removed, next request gets socket or new one created. Uses async context via AsyncResource for queued requests.
Events: `free` (socket reusable), `timeout` on free socket destroys, `keylog` forwarded if listener added. Socket listeners installed for `free`, `close` (removes from pools, updates total count), `timeout` (destroys if free), `agentRemove` (upgrade/remove).
Proxy support: respects `proxyEnv` / `--use-env-proxy`; `createConnection` routes via proxy config (http/https) or direct; CONNECT tunneling handled in https layer; errors with `ERR_PROXY_TUNNEL` passed to request.
`destroy()` closes all sockets/freeSockets.

## Parser and common utilities
- `setMaxIdleHTTPParsers(max)` controls FreeList pool size (default 1000).
- `maxHeaderPairs` default 2000; negative/0 disables limit. `maxHeaderSize` optional per server/client.
- `insecureHTTPParser` toggle allows lenient parsing; warning emitted on first use.
- `checkInvalidHeaderChar` rejects non VCHAR/obs-text. `checkIsHttpToken` validates token chars.
- `IncomingMessage` construction uses server-provided class for requests (allows customization).
- `joinDuplicateHeaders` toggles merging behavior for incoming headers.

## Events and ordering (typical)
Server:
1) `connection` (net)  
2) headers parsed → `checkContinue`/`checkExpectation` if Expect present  
3) `request` (req,res)  
4) optional `upgrade`/`connect` before body consumed when upgrade flag  
5) `response` finish → `close` on response async  
6) `dropRequest` when maxRequestsPerSocket exceeded on new request  
7) `timeout` on req/res/server from socket timeout  
8) `clientError` on parse/other socket errors before headers sent  
Client:
1) `socket` assigned  
2) `continue` on 100  
3) `information` on other 1xx  
4) `response` on final (or `upgrade`/`connect` if 101/CONNECT)  
5) `end` on res → `close` on req; keep-alive returns socket to agent unless `Connection: close`/not drained/timeout  
6) `timeout` on request/res as applicable  
7) `error`/`abort` as triggered

## Notable behaviors and edge cases
- Host header required for HTTP/1.1 when `requireHostHeader` true; missing → 400, close.
- `maxRequestsPerSocket` >0: when exceeded, server emits `dropRequest`, replies 503, does not serve new request.
- `optimizeEmptyRequests`: if enabled and request lacks body headers, server dumps request body handling (no data/end/close on req).
- Informational responses: for client, 1xx (except 101) emit `information` and parser continues; for server, 1xx sent via `res.writeProcessing`/`writeEarlyHints`/`writeContinue`; 100 triggers `continue` on req.
- Keep-alive hint from server `Keep-Alive: timeout=` influences agent socket reuse timeout (may disable reuse if too small).
- Trailers require chunked; otherwise ERR_HTTP_TRAILER_INVALID.
- HEAD/204/304/1xx mark `_hasBody=false`; body writes ignored or rejected based on `rejectNonStandardBodyWrites`.
- Upgrade/CONNECT: server destroys socket if no listener; client destroys if no listener for upgrade/connect; bytes after upgrade handed as `head`.
- Errors: write after end → ERR_STREAM_WRITE_AFTER_END; write with strictContentLength mismatch throws; header validation throws ERR_INVALID_*; parse errors trigger `clientError` (server) or `error` on request (client).

## Documentation/test touchpoints (selected)
- `doc/api/http.md`: `server.maxRequestsPerSocket`, `server.keepAliveTimeout`, `server.keepAliveTimeoutBuffer`, Expect handling (`'checkContinue'`, `'checkExpectation'`), keep-alive options.
- Tests: `test/parallel/test-http-keep-alive-drop-requests.js` / `test-https-keep-alive-drop-requests.js` cover `dropRequest`; keep-alive limits in `test-http-keep-alive-pipeline-max-requests.js`; defaults in `test-http-server-keep-alive-defaults.js`; maxRequestsPerSocket null in `test-http-server-keep-alive-max-requests-null.js`.

