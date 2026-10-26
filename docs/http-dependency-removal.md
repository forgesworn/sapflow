# Server HTTP dependency removal

The viewer path already uses the Mio loop. The remaining private control API
and callback client now use Tokio sockets and a shared, runtime-free
`http_lite.rs` parser/writer built on the existing `httparse` dependency.
This removes Axum, Hyper, hyper-util and http-body-util from the **server**
dependency tree. Benchmark tooling still uses Hyper; Tokio is still the server
runtime. This change does not add TLS, HTTP/2 or certificate management.

## Control boundary

The listener remains loopback-only, with 16 concurrent connections. A request
head is limited to 16 KiB, 32 headers, a 2 KiB target and five seconds from
accept. Bearer tokens use the existing constant-time matcher before route
handling or a deliberate body read. Duplicate authorisation headers grant
nothing. The declared body must fit the existing 16 KiB limit; body reading and
handler execution share the existing ten-second timeout. Refusals never echo
credentials. Every response has `cache-control: no-store`.

The same provision/status/revoke handlers, authority, durable mutation policy,
success receipts and domain-error mapping remain. `HEAD` retains GET status
semantics without a response body. Connections serve one request, as before.
Chunked bodies, duplicate lengths, bare LF, folded headers and absolute targets
are refused. Session IDs must be sent as canonical literal IDs, without percent
encoding. Parser, routing and framing errors now use static JSON codes described
in [scoped publish sessions](scoped-publish-sessions.md).

After an early refusal the server sends and shuts down its write side, then
drains at most 64 KiB for at most 250 ms. This preserves the refusal response
when a caller is still sending bytes, without accepting a second request.

Real socket tests cover auth before bodies/routes/framing, oversized declared
bodies, conflicting lengths, transfer encodings, pipelining, header floods,
slow heads/bodies, expectation handling, connection admission and non-loopback
refusal. Original authority-boundary test assertions remain intact.

## Callback boundary

The client sends the existing frozen JSON body, content type, callback URL and
secret encoding. Queue bounds, event ordering, four-attempt policy and backoff
are unchanged. Each plain-HTTP connection has a five-second connect/write/read
budget; only a bounded response head is read. At most four interim responses
are accepted before a final status. Redirects are never followed. Neither raw
URLs nor transport errors that might contain credentials are logged.

Tests check actual request bytes, sequential delivery, 2xx acknowledgement,
terminal 4xx, four attempts for server failures/redirects, no redirect follow,
invalid URLs, interim limits, unbounded heads, silent peers and closed ports.

## Validation, 26 October 2026

- `cargo test --workspace`: 957 passed, six ignored, zero failures after the
  response-framing review correction below.
- Workspace clippy with warnings denied, format check and the full SRT/RTMP,
  LL-HLS, reconnect, callback, encryption and recording smoke test pass.
- All 221 pre-existing Python regressions pass. The updated mobile gate
  regressions also pass: 71 Android and 22 iOS tests.
- The server's unique normal dependency count falls from 125 to 98 using
  `cargo tree -p sapflow -e normal --prefix none --no-dedupe --format '{p}'`
  followed by `sort -u`. No Axum, Hyper, Tower or http-body package remains in
  that tree. The distribution licence inventory was regenerated.
- The production parser and callback URL writer have a new `http_lite` fuzz
  target with generated HTTP/URL seeds, automatically discovered by CI.
  The initial AddressSanitizer run with debug assertions and overflow checks completed
  11,942,254 executions in 61 seconds with no finding, at maximum input length
  65,536 bytes. This is a finite parser campaign, not internet-exposure or
  independent security approval. CI reruns the target on the final source.

Independent review found that malformed successful callback responses could
be acknowledged despite conflicting or invalid Content-Length headers. The
correction restores the prior client's framing precedence and numeric checks,
while preserving valid duplicate lengths, chunked/close-delimited responses and
no-body statuses. A raw receiver regression requires four attempts, zero
deliveries and one failure. The reviewer re-read the focused correction and
reported no remaining merge-blocking finding; broader production acceptance is
still separate.

The mobile gate fixes are a separate preceding commit. The generated iOS
matrix and full Android SDK/app gate were run in their own checkout; they are
host checks, not physical-device or release acceptance. No live host changed.
