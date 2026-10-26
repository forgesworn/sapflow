# Replacing tokio: a plan

The initial plan below sets out what replacing the async runtime would take
and what measurements decide whether to continue. An optional viewer prototype
has since landed and is now the only viewer backend: Sapflow's HTTP event loop
serves viewers, and the Hyper viewer path was removed (`SAPFLOW_HTTP_IMPL=hyper`
stops startup). Ingest, callbacks, private publish control and recording still
run on tokio. The control API and the callback client are our own small
HTTP/1.1 code on tokio sockets and `httparse` (`http_lite.rs`); axum, hyper and
hyper-util are no longer dependencies of the server. The inventory below is the
original 2026-10-22 snapshot. See the measurements in
[benchmarks](benchmarks.md) and the implementation gates in
[readiness](readiness.md). This does not replace the whole server runtime or
establish a mobile performance improvement.

## Where tokio is today

Only the server (`crates/sapflow-server`) and the bench tool use tokio. The
media libraries (`sapflow-srt`, `sapflow-rtmp`, `sapflow-hls`, `sapflow-core`, `sapflow-client`,
`sapflow-ffi`) are sans-IO and runtime-free already, so nothing here touches
them or the mobile apps.

In the server (about 10,000 lines), tokio does:

| Job | Uses | Where |
|-----|------|-------|
| Sockets: SRT (UDP), RTMP (TCP), HTTP (TCP) | `tokio::net` (11) | `srt_own.rs`, `rtmp.rs`, `http.rs` (since removed; viewers are on `own_http.rs`) |
| Timers and timeouts | `tokio::time` (38) | almost everywhere |
| Tasks: one per connection, viewer and stream | `tokio::spawn` (29), `tokio::task` (8) | `stream.rs`, `publish.rs`, `recorder.rs` |
| Channels and notification (waking blocked playlist requests) | `tokio::sync` (16), `select!` (6) | `stream.rs`, `http.rs` (since removed; `own_http::notify`) |
| Recording files | `tokio::fs` (13), `tokio::io` (6) | `recorder.rs` |
| Shutdown on SIGTERM | `tokio::signal` (1) | `main.rs` |

tokio is not alone. `axum` (the HTTP routes) and `hyper` / `hyper-util` (the
HTTP/1.1 server and the callback client) are built on it. Replacing tokio
means replacing the HTTP server and client too, and that is most of the
work. (Since then the control API and the callback client no longer use axum or
hyper: see `http_lite.rs`, `publish_control.rs` and `callback.rs`.)

## What it could gain

- **CPU.** `docs/profiling.md` puts "tokio runtime and unattributed" at 13 to
  28% of samples, but says plainly that this is a residue (kevent polling,
  thread wake-ups, frames it could not attribute), not a measured runtime
  cost. Per viewer it is about 13% of 0.040% of a core. At 750 viewers the
  server uses 62% of a core in total; a runtime that cost nothing would save
  a few points of that, not a multiple. The larger per-viewer cost is kernel
  socket I/O (about half) and hyper, axum and tower (about a quarter).
- **Memory.** Unmeasured. Per-task stacks are small in tokio; any saving
  would come from dropping hyper and axum's per-connection state.
- **Dependencies and audit surface.** axum and hyper-util, with tokio,
  hyper, tower and everything under them, are 61 of the 117 crates in the
  server's dependency tree (`cargo tree -p sapflow -e normal`, 2026-10-22),
  though some of those (`bytes`, `libc`) would stay. Removing them makes the binary
  smaller and the whole server readable end to end, in the same spirit as
  writing our own SRT and RTMP.
- **One model throughout.** The protocol code is sans-IO already. An event
  loop of our own would drive it directly, with no async state machines in
  between.

## What it costs

- **An HTTP/1.1 server of our own.** Request parsing, keep-alive, pipelining
  limits, slow-client timeouts, header limits, `Range` if needed, and the LL-HLS
  blocking playlist reload (thousands of requests parked until the next part).
  This is outside input and becomes a fuzz target. hyper's years of hardening
  are what we would be giving up.
- **An HTTP client** for the stream-health callback (plain HTTP/1.1 POST, no
  TLS today).
- **Timers**, a timer wheel or heap keyed by deadline.
- **Two pollers.** epoll on Linux (production) and kqueue on macOS (where
  every measurement so far was made), unless we take `mio` or `polling` as
  the one dependency that hides the difference.
- **Files.** Recording writes move to a blocking writer thread fed by a
  bounded queue, which is simple.
- **Time.** The largest piece of work since the SRT library, and it lands
  while the production trial is still ahead.

## The options

| Option | Our own? | Work | Notes |
|--------|----------|------|-------|
| A. Keep tokio, tune it | no | small | Current-thread runtime per core, fewer wake-ups, `recvmmsg` for SRT. Measures what the runtime really costs |
| B. Our own event loop on `mio` (or `rustix` epoll/kqueue directly) | yes, apart from the poller | large | One thread per core, each owning its sockets; the sans-IO libraries plug straight in. Our own HTTP/1.1 server and client |
| C. Another runtime (smol, monoio, glommio) | no | medium to large | Swaps one dependency for another; io_uring runtimes are Linux only. Does not meet "our own" |

B is the option that matches the rest of the project. C is not worth doing.

## Step 1: measure before building

A spike, kept off main, that answers whether B is worth it:

1. Build the viewer path alone on B: an HTTP/1.1 server on `mio` that serves
   a playlist and parts from a `sapflow-hls` muxer fed by a recorded MPEG-TS
   file, with blocking playlist reload. No SRT, RTMP, callbacks or recording.
2. Run `sapflow-bench` against it and against the current server serving the
   same stream, at 250 and 750 viewers, three runs each, as in
   `docs/benchmarks.md`.
3. Compare CPU, peak memory, fan-out time and failed requests.

**Go on** if the spike uses clearly less CPU or memory (say a quarter less at
750 viewers) with no failed requests, or if fewer dependencies alone is
reason enough. **Stop** and do option A if the difference is within noise.

### Result (2026-10-22)

It passed the bar. On the M4 at 750 viewers our own loop used 43% less CPU
(34% of a core against 60%) and 88% less memory (27 MB against 219 MB), with
no failed requests and faster median part fan-out. Three alternating runs
agreed to within a point. Details in `docs/benchmarks.md`, "Viewer HTTP on
our own event loop".

## Step 2, if it goes ahead

In order, each one landing behind a setting so the tokio path stays the
default until the new one matches it in `scripts/smoke.sh` and the benchmarks:

1. The HTTP server, with fuzz targets for request parsing. Done
   (2026-10-22): `SAPFLOW_HTTP_IMPL=own` (`own_http.rs`, framing in
   `http1.rs`, fuzz target `http1_request`), with a connection cap, an idle
   timeout and one response in flight per connection. It became the default,
   and the Hyper viewer path was then removed; CI runs the smoke test on the
   own loop.
2. SRT ingest (UDP; the listener is sans-IO already).
3. RTMP ingest (TCP; `sapflow_rtmp::Connection` is sans-IO already).
4. The callback client. Done for the HTTP part: a small HTTP/1.1 client on
   tokio sockets replaced hyper-util, and the control API likewise replaced
   axum (`http_lite.rs`, fuzz target `http_lite`). Both still run on tokio.
5. Recording on a writer thread.
6. Signals and shutdown.
7. Remove tokio; regenerate `THIRD_PARTY_LICENSES.txt` (axum and hyper are
   already out).

## Not before the trial

The production trial against mediamtx runs on the server as it is. Starting
this first would put an untested HTTP server in front of the first real
audience. Run the trial first, then the spike.
