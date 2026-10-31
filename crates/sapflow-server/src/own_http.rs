//! The viewer HTTP path on our own event loop (mio), without tokio, hyper or
//! axum. It is the only viewer backend: it replaced a Hyper one that used
//! about twice the CPU and several times the memory (`docs/benchmarks.md`).
//! Ingest, callbacks and recording still run on tokio.
//!
//! Every viewer gets the same bytes: no sessions, no cookies, no query strings
//! in URIs. Viewer addresses are not logged.
//!
//! Each worker thread owns a poller and the connections it accepts from a
//! shared listener. Requests that must wait (blocking playlist reload, the
//! preload-hinted part) park on their connection. A stream change wakes
//! every worker ([`notify`]), and each re-checks the parked requests whose
//! stream changed. Request framing is `http1.rs`.
//!
//! Limits:
//! - At most [`MAX_CONNECTIONS`] connections; past it, new ones wait in the
//!   kernel's accept queue.
//! - A request head must arrive within [`HEADER_TIMEOUT`] of its first byte.
//! - A connection with nothing parked that makes no progress for
//!   [`IDLE_TIMEOUT`] is closed.
//! - A ready response has an absolute [`BODY_TIMEOUT`], independent of progress.
//! - Closing a response at either deadline is logged, without the viewer's
//!   address and at most [`SLOW_CLOSE_LOGS`] a minute per worker.
//! - One response in flight per connection, so a client that sends without
//!   reading stops being read, and at most [`REQUESTS_PER_TURN`] requests per
//!   connection before the worker serves the others.
//! - A panic while serving a connection closes that connection only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, IoSlice, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use sapflow_hls::playlist::{self, Chunk};
use sapflow_hls::{PartLookup, Readiness};

use crate::App;
use crate::http1::{self, Method, Parsed, Persist};
use crate::media_body::MediaBody;
use crate::stream::LiveStream;

const LISTENER: Token = Token(0);
const WAKE: Token = Token(1);
const PLAYLIST_TYPE: &str = "application/vnd.apple.mpegurl";
const CHUNK_CACHE: &str = "public, max-age=3600";
/// Concurrent connections across all workers.
pub const MAX_CONNECTIONS: usize = 20_000;
/// Time allowed for a request head to arrive once its first byte has
/// (slow-loris).
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(60);
/// A connection with nothing parked is closed after this long without
/// reading a byte or writing one. It must outlast clients' idle keep-alive
/// reuse: at 10 s, viewers that reused a connection just as it closed saw
/// failed requests.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Absolute time allowed to transfer one ready framed response, including
/// headers. Parked LL-HLS waiting has its own deadline and is excluded.
pub const BODY_TIMEOUT: Duration = Duration::from_secs(120);
/// Requests one connection may have answered before the worker turns to the
/// others; the rest wait their turn on the ready queue.
pub const REQUESTS_PER_TURN: usize = 16;
/// Write attempts per connection turn. A continuously writable large response
/// must yield so the worker can sweep other connections' deadlines.
const WRITES_PER_TURN: usize = 16;
/// Connections accepted per turn, for the same reason.
const ACCEPTS_PER_TURN: usize = 64;
/// How often each worker checks deadlines and retries a paused accept.
const SWEEP: Duration = Duration::from_millis(250);
/// Slow-viewer close lines each worker logs a minute; the rest are counted.
pub const SLOW_CLOSE_LOGS: u32 = 10;

static OPEN: AtomicUsize = AtomicUsize::new(0);
/// Every worker's waker, so a stream change reaches every parked request.
static WAKERS: Mutex<Vec<Arc<Waker>>> = Mutex::new(Vec::new());

/// Wakes every worker to re-check its parked requests. Called after every
/// stream change; a no-op unless this event loop is running.
pub fn notify() {
    if let Ok(wakers) = WAKERS.lock() {
        for w in wakers.iter() {
            let _ = w.wake();
        }
    }
}

/// The timeouts, separate so tests can shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub header: Duration,
    pub idle: Duration,
    pub body: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            header: HEADER_TIMEOUT,
            idle: IDLE_TIMEOUT,
            body: BODY_TIMEOUT,
        }
    }
}

/// Starts `threads` workers serving `listener`. A worker that fails ends the
/// process, so a supervisor restarts it rather than leaving viewers queued on
/// a listener nobody serves.
pub fn spawn(
    app: Arc<App>,
    listener: std::net::TcpListener,
    threads: usize,
    timeouts: Timeouts,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    for i in 0..threads.max(1) {
        let mut worker = Worker::new(app.clone(), listener.try_clone()?, timeouts)?;
        std::thread::Builder::new()
            .name(format!("http-{i}"))
            .spawn(move || {
                match catch_unwind(AssertUnwindSafe(|| worker.run())) {
                    Ok(Err(e)) => tracing::error!(error = %e, "HTTP worker failed"),
                    _ => tracing::error!("HTTP worker panicked"),
                }
                std::process::exit(1);
            })?;
    }
    Ok(())
}

struct Response {
    status: &'static str,
    headers: String,
    body: MediaBody,
    /// `false` for a 204, which carries no content-length.
    has_length: bool,
}

impl Response {
    fn empty(status: &'static str) -> Self {
        Self {
            status,
            headers: String::new(),
            body: MediaBody::default(),
            has_length: true,
        }
    }
    fn ok(body: impl Into<MediaBody>, content_type: &str, cache: &str) -> Self {
        Self {
            status: "200 OK",
            headers: format!("content-type: {content_type}\r\ncache-control: {cache}\r\n"),
            body: body.into(),
            has_length: true,
        }
    }
    fn not_found() -> Self {
        Self::empty("404 Not Found")
    }
}

/// One framed response. Chunk handles stay in their shared segment metadata;
/// advancing a slow viewer changes only these cursors.
#[derive(Default)]
struct Output {
    head: Bytes,
    written: usize,
    body: MediaBody,
    response_started: Option<Instant>,
}

/// Bound stack usage independently of the number of parts in a segment.
const WRITE_SLICES: usize = 8;

/// Why the server ended a response before the viewer had read it all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlowClose {
    /// The absolute [`BODY_TIMEOUT`] passed.
    Deadline,
    /// Nothing written for [`IDLE_TIMEOUT`] with bytes still queued.
    Stalled,
}

impl SlowClose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deadline => "body-deadline",
            Self::Stalled => "no-progress",
        }
    }
}

impl Output {
    fn is_empty(&self) -> bool {
        self.head.is_empty() && self.body.is_empty()
    }

    fn remaining(&self) -> usize {
        self.head.len() - self.written + self.body.len()
    }

    fn expired(&self, now: Instant, timeout: Duration) -> bool {
        !self.is_empty()
            && self
                .response_started
                .is_some_and(|started| now.saturating_duration_since(started) >= timeout)
    }

    /// Writes a finite turn. The clock is injectable to check progress and
    /// expiry without sleeping; real sockets use the monotonic wall clock.
    fn flush_to(
        &mut self,
        writer: &mut impl Write,
        timeout: Duration,
        mut now: impl FnMut() -> Instant,
    ) -> io::Result<(Option<Instant>, bool)> {
        let mut progress = None;
        for _ in 0..WRITES_PER_TURN {
            if self.is_empty() {
                return Ok((progress, false));
            }
            if self.expired(now(), timeout) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.write_to(writer) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(_) => progress = Some(now()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok((progress, false));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok((progress, !self.is_empty()))
    }

    fn write_to(&mut self, writer: &mut impl Write) -> io::Result<usize> {
        let mut slices = [IoSlice::new(&[]); WRITE_SLICES];
        let header = &self.head[self.written..];
        let mut count = 0;
        if !header.is_empty() {
            slices[0] = IoSlice::new(header);
            count = 1;
        }
        count += self.body.slices(&mut slices[count..]);
        if count == 0 {
            return Ok(0);
        }
        let offered: usize = slices[..count].iter().map(|slice| slice.len()).sum();
        let written = writer.write_vectored(&slices[..count])?;
        if written > offered {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write exceeded offered response bytes",
            ));
        }
        let header_bytes = written.min(header.len());
        self.written += header_bytes;
        if self.written == self.head.len() {
            self.head = Bytes::new();
            self.written = 0;
        }
        self.body.advance(written - header_bytes)?;
        Ok(written)
    }
}

/// A request that may have to wait.
enum Request {
    Multivariant(Arc<LiveStream>),
    Media {
        stream: Arc<LiveStream>,
        msn: Option<u64>,
        part: Option<u64>,
    },
    Chunk {
        stream: Arc<LiveStream>,
        prefix: String,
        chunk: Chunk,
    },
}

struct Parked {
    request: Request,
    head_only: bool,
    deadline: Instant,
    /// The stream's version when last checked: re-check only after it moves.
    seen: u64,
}

struct Conn {
    sock: TcpStream,
    input: Vec<u8>,
    /// One response still to write, sharing its media chunks with the muxer.
    output: Output,
    parked: Option<Parked>,
    /// Close once the output is written.
    close_after: bool,
    /// The request being answered was HTTP/1.0 asking to keep alive.
    keep_alive_10: bool,
    writable_armed: bool,
    last_progress: Instant,
    /// When the first byte of the request head now arriving came.
    head_started: Option<Instant>,
    /// The socket may hold bytes not yet read, because the buffer was full.
    unread: bool,
}

/// Counts a connection in [`OPEN`] for as long as it lives.
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        OPEN.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_CONNECTIONS).then_some(n + 1)
        })
        .ok()
        .map(|_| Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a turn on one connection left behind.
enum Turn {
    /// Waiting for the socket or a stream change.
    Idle,
    /// Out of budget with requests still to answer: queue another turn.
    Again,
    /// Finished or broken: close it.
    Close,
}

struct Worker {
    app: Arc<App>,
    poll: Poll,
    listener: TcpListener,
    timeouts: Timeouts,
    conns: HashMap<usize, (Conn, Slot)>,
    parked: HashSet<usize>,
    /// Connections with work left over from their last turn.
    ready: VecDeque<usize>,
    next: usize,
    /// Connections may be waiting in the accept queue.
    accept_more: bool,
    /// Accepting stopped on the connection cap or an error: retry at sweeps.
    accept_paused: bool,
    buf: Vec<u8>,
    slow_log: sapflow_core::limits::LogLimiter,
    /// Responses this worker closed at the body deadline and for no progress.
    slow_closes: (u64, u64),
}

impl Worker {
    fn new(app: Arc<App>, listener: std::net::TcpListener, timeouts: Timeouts) -> io::Result<Self> {
        let poll = Poll::new()?;
        let mut listener = TcpListener::from_std(listener);
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        let waker = Arc::new(Waker::new(poll.registry(), WAKE)?);
        if let Ok(mut w) = WAKERS.lock() {
            w.push(waker);
        }
        Ok(Self {
            app,
            poll,
            listener,
            timeouts,
            conns: HashMap::new(),
            parked: HashSet::new(),
            ready: VecDeque::new(),
            next: 2,
            accept_more: false,
            accept_paused: false,
            buf: vec![0; 16 * 1024],
            slow_log: sapflow_core::limits::LogLimiter::new(
                SLOW_CLOSE_LOGS,
                Duration::from_secs(60),
            ),
            slow_closes: (0, 0),
        })
    }

    fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);
        let mut next_sweep = Instant::now() + SWEEP;
        loop {
            let timeout = if self.ready.is_empty() && !self.accept_more {
                next_sweep.saturating_duration_since(Instant::now())
            } else {
                Duration::ZERO
            };
            match self.poll.poll(&mut events, Some(timeout)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            let mut woke = false;
            for ev in events.iter() {
                match ev.token() {
                    LISTENER => self.accept_more = true,
                    WAKE => woke = true,
                    Token(id) => {
                        let read = ev.is_readable() || ev.is_read_closed() || ev.is_error();
                        self.service(id, read);
                    }
                }
            }
            // One more turn for each connection left with work, oldest first;
            // anything they queue again waits for the next round.
            for _ in 0..self.ready.len() {
                if let Some(id) = self.ready.pop_front() {
                    self.service(id, false);
                }
            }
            if self.accept_more {
                self.accept();
            }
            if woke {
                self.wake_parked();
            }
            let now = Instant::now();
            if now >= next_sweep {
                next_sweep = now + SWEEP;
                self.sweep(now);
            }
        }
    }

    /// Re-checks the parked requests whose stream has changed.
    fn wake_parked(&mut self) {
        let ids: Vec<usize> = self
            .parked
            .iter()
            .copied()
            .filter(|id| {
                self.conns
                    .get(id)
                    .and_then(|(c, _)| c.parked.as_ref())
                    .is_some_and(|p| stream_of(&p.request).version() != p.seen)
            })
            .collect();
        for id in ids {
            self.service(id, false);
        }
    }

    fn accept(&mut self) {
        self.accept_more = false;
        for _ in 0..ACCEPTS_PER_TURN {
            let Some(slot) = Slot::take() else {
                self.accept_paused = true;
                return;
            };
            match self.listener.accept() {
                Ok((mut sock, _)) => {
                    let _ = sock.set_nodelay(true);
                    let id = self.next;
                    self.next += 1;
                    if self
                        .poll
                        .registry()
                        .register(&mut sock, Token(id), Interest::READABLE)
                        .is_err()
                    {
                        continue;
                    }
                    let conn = Conn {
                        sock,
                        input: Vec::new(),
                        output: Output::default(),
                        parked: None,
                        close_after: false,
                        keep_alive_10: false,
                        writable_armed: false,
                        last_progress: Instant::now(),
                        head_started: None,
                        unread: false,
                    };
                    self.conns.insert(id, (conn, slot));
                    // Edge-triggered: a request may already be waiting.
                    self.service(id, true);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.accept_paused = false;
                    return;
                }
                // One connection reset before it was accepted: carry on.
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                    ) => {}
                Err(e) => {
                    // Out of file descriptors and the like: retry at the
                    // next sweep.
                    tracing::warn!(error = %e, "HTTP accept failed");
                    self.accept_paused = true;
                    return;
                }
            }
        }
        // Budget spent with connections possibly still queued.
        self.accept_more = true;
    }

    /// Times out parked requests, slow request heads and idle connections,
    /// and retries a paused accept.
    fn sweep(&mut self, now: Instant) {
        let t = self.timeouts;
        let mut expired = Vec::new();
        let mut closing = Vec::new();
        for (id, (c, _)) in &self.conns {
            match &c.parked {
                Some(p) if p.deadline <= now => expired.push(*id),
                Some(_) => {}
                None => {
                    let slow_head = c
                        .head_started
                        .is_some_and(|s| now.duration_since(s) >= t.header);
                    let idle = now.duration_since(c.last_progress) >= t.idle;
                    if c.output.expired(now, t.body) {
                        closing.push((*id, Some(SlowClose::Deadline)));
                    } else if idle && !c.output.is_empty() {
                        closing.push((*id, Some(SlowClose::Stalled)));
                    } else if slow_head || idle {
                        closing.push((*id, None));
                    }
                }
            }
        }
        for id in expired {
            self.service(id, false);
        }
        for (id, slow) in closing {
            if let Some(reason) = slow {
                self.record_slow_close(id, reason, now);
            }
            self.close(id);
        }
        if self.accept_paused {
            self.accept();
        }
    }

    /// Counts and, within the log budget, reports a response ended early.
    fn record_slow_close(&mut self, id: usize, reason: SlowClose, now: Instant) {
        let Some((conn, _)) = self.conns.get(&id) else {
            return;
        };
        match reason {
            SlowClose::Deadline => self.slow_closes.0 += 1,
            SlowClose::Stalled => self.slow_closes.1 += 1,
        }
        if let Some(suppressed) = self.slow_log.allow(now) {
            tracing::info!(
                reason = reason.as_str(),
                remaining_bytes = conn.output.remaining(),
                response_ms = conn
                    .output
                    .response_started
                    .map_or(0, |at| now.saturating_duration_since(at).as_millis() as u64),
                worker_body_deadline_closes = self.slow_closes.0,
                worker_no_progress_closes = self.slow_closes.1,
                suppressed,
                "closed a slow viewer's response"
            );
        }
    }

    fn close(&mut self, id: usize) {
        self.parked.remove(&id);
        if let Some((mut c, _slot)) = self.conns.remove(&id) {
            let _ = self.poll.registry().deregister(&mut c.sock);
        }
    }

    /// Gives one connection a turn and files it by the result.
    fn service(&mut self, id: usize, read: bool) {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        let app = &self.app;
        let poll = &self.poll;
        let buf = &mut self.buf;
        let body_timeout = self.timeouts.body;
        let turn = catch_unwind(AssertUnwindSafe(|| {
            drive(app, poll, conn, Token(id), buf, read, body_timeout)
        }))
        .unwrap_or_else(|_| {
            tracing::error!("panic while serving an HTTP connection; closed it");
            Ok(Turn::Close)
        })
        .unwrap_or(Turn::Close);
        let parked = conn.parked.is_some();
        let deadline = conn.output.expired(Instant::now(), body_timeout);
        match turn {
            Turn::Close => {
                if deadline {
                    self.record_slow_close(id, SlowClose::Deadline, Instant::now());
                }
                self.close(id);
            }
            Turn::Again => {
                self.parked.remove(&id);
                self.ready.push_back(id);
            }
            Turn::Idle if parked => {
                self.parked.insert(id);
            }
            Turn::Idle => {
                self.parked.remove(&id);
            }
        }
    }
}

fn drive(
    app: &App,
    poll: &Poll,
    conn: &mut Conn,
    token: Token,
    buf: &mut [u8],
    mut read: bool,
    body_timeout: Duration,
) -> io::Result<Turn> {
    let mut taken = 0;
    let turn = loop {
        let now = Instant::now();
        if conn.output.expired(now, body_timeout) {
            return Ok(Turn::Close);
        }
        if read || conn.unread {
            read = false;
            if !fill(conn, buf, now) {
                return Ok(Turn::Close);
            }
        }
        if let Some(mut p) = conn.parked.take() {
            let seen = stream_of(&p.request).version();
            match answer(app, &p.request) {
                Some(r) => respond(conn, r, p.head_only, now),
                None if now >= p.deadline => {
                    respond(conn, Response::empty("503 Service Unavailable"), false, now)
                }
                None => {
                    p.seen = seen;
                    conn.parked = Some(p);
                }
            }
        }
        // One response in flight: take the next request only once the last
        // is written.
        if conn.parked.is_none() && conn.output.is_empty() && !conn.close_after {
            if taken == REQUESTS_PER_TURN {
                break Turn::Again;
            }
            if take_request(app, conn, now) {
                taken += 1;
            }
        }
        if flush(conn, body_timeout)? {
            break Turn::Again;
        }
        // Go round again while there is another request to take: one already
        // buffered, or bytes left in the socket when the buffer was full.
        let ready_for_more = conn.parked.is_none() && conn.output.is_empty() && !conn.close_after;
        let more = conn.unread || !matches!(http1::parse(&conn.input), Parsed::Partial);
        if !(ready_for_more && more) {
            break Turn::Idle;
        }
    };
    let want_write = !conn.output.is_empty();
    if want_write != conn.writable_armed {
        let interest = if want_write {
            Interest::READABLE | Interest::WRITABLE
        } else {
            Interest::READABLE
        };
        poll.registry()
            .reregister(&mut conn.sock, token, interest)?;
        conn.writable_armed = want_write;
    }
    if conn.close_after && conn.output.is_empty() && conn.parked.is_none() {
        return Ok(Turn::Close);
    }
    Ok(turn)
}

/// Reads until the socket is empty or one request head is buffered. False
/// when the peer closed or the socket failed.
fn fill(conn: &mut Conn, buf: &mut [u8], now: Instant) -> bool {
    conn.unread = false;
    loop {
        // Stop past one request head: the rest waits in the socket, which
        // pushes back on the client.
        let room = (http1::MAX_HEAD + 1).saturating_sub(conn.input.len());
        if room == 0 {
            conn.unread = true;
            return true;
        }
        let Some(into) = buf.get_mut(..room.min(buf.len())) else {
            return false;
        };
        match conn.sock.read(into) {
            Ok(0) => return false,
            Ok(n) => {
                if conn.input.is_empty() && conn.head_started.is_none() {
                    conn.head_started = Some(now);
                }
                conn.input
                    .extend_from_slice(into.get(..n).unwrap_or_default());
                conn.last_progress = now;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

/// Parses and answers the next buffered request, or parks it. True if a
/// request was taken.
fn take_request(app: &App, conn: &mut Conn, now: Instant) -> bool {
    let (req, used) = match http1::parse(&conn.input) {
        Parsed::Request(r, n) => (r, n),
        Parsed::Partial => return false,
        Parsed::Invalid => {
            conn.close_after = true;
            conn.input = Vec::new();
            conn.unread = false;
            conn.keep_alive_10 = false;
            respond(conn, Response::empty("400 Bad Request"), false, now);
            return true;
        }
    };
    conn.input.drain(..used);
    // Pipelined bytes already here start the next head now.
    conn.head_started = (!conn.input.is_empty()).then_some(now);
    if conn.input.is_empty() && conn.input.capacity() > 4096 {
        conn.input = Vec::new();
    }
    conn.close_after = req.close;
    conn.keep_alive_10 = req.keep_alive_10;
    let head_only = req.method == Method::Head;
    match route(app, req.method, &req.target) {
        Ok(request) => {
            let seen = stream_of(&request).version();
            match answer(app, &request) {
                Some(r) => respond(conn, r, head_only, now),
                None => {
                    let deadline = now + block_limit(stream_of(&request));
                    conn.parked = Some(Parked {
                        request,
                        head_only,
                        deadline,
                        seen,
                    });
                }
            }
        }
        Err(r) => respond(conn, r, head_only, now),
    }
    true
}

fn respond(conn: &mut Conn, r: Response, head_only: bool, now: Instant) {
    let persist = if conn.close_after {
        Persist::Close
    } else if conn.keep_alive_10 {
        Persist::KeepAlive
    } else {
        Persist::Default
    };
    let length = r.has_length.then_some(r.body.len());
    let head = http1::head(r.status, &r.headers, length, persist);
    conn.output = Output {
        head: Bytes::from(head),
        written: 0,
        response_started: Some(now),
        body: if head_only {
            MediaBody::default()
        } else {
            r.body
        },
    };
}

fn flush(conn: &mut Conn, body_timeout: Duration) -> io::Result<bool> {
    let (progress, again) = conn
        .output
        .flush_to(&mut conn.sock, body_timeout, Instant::now)?;
    if let Some(now) = progress {
        conn.last_progress = now;
    }
    Ok(again)
}

fn stream_of(r: &Request) -> &LiveStream {
    match r {
        Request::Multivariant(s) => s,
        Request::Media { stream, .. } | Request::Chunk { stream, .. } => stream,
    }
}

/// Apple: hold a blocking request at most three target durations.
fn block_limit(stream: &LiveStream) -> Duration {
    let target = stream
        .lock()
        .ingest
        .muxer()
        .map_or(1, |m| m.target_duration());
    Duration::from_secs(3 * target)
}

/// The routes: `/live/{event}/index.m3u8`, the media playlist and
/// `/live/{event}/{file}`, each answering GET, HEAD and OPTIONS. Anything else
/// is 404 whatever the method; another method on a route is 405.
fn route(app: &App, method: Method, target: &str) -> Result<Request, Response> {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let (event, file) = path
        .strip_prefix("/live/")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(e, f)| !e.is_empty() && !f.is_empty() && !f.contains('/'))
        .ok_or_else(Response::not_found)?;
    match method {
        Method::Get | Method::Head => {}
        Method::Options => {
            let mut r = Response::empty("204 No Content");
            r.has_length = false;
            r.headers = "access-control-allow-methods: GET, OPTIONS\r\naccess-control-allow-headers: *\r\naccess-control-max-age: 86400\r\n".into();
            return Err(r);
        }
        Method::Other => {
            let mut r = Response::empty("405 Method Not Allowed");
            r.headers = "allow: GET,HEAD,OPTIONS\r\n".into();
            return Err(r);
        }
    }
    if file == "index.m3u8" {
        let stream = app.streams.get(event).ok_or_else(Response::not_found)?;
        return Ok(Request::Multivariant(stream));
    }
    if file == playlist::MEDIA_PLAYLIST {
        // The query is checked before the stream is looked up.
        let (msn, part) = playlist::parse_blocking_query(query)
            .map_err(|_| Response::empty("400 Bad Request"))?;
        // Without parts or CAN-BLOCK-RELOAD, the client reloads completed
        // segments; ignore a blocking request from before a mode change.
        let (msn, part) = if app.config.low_latency {
            (msn, part)
        } else {
            (None, None)
        };
        let stream = app.streams.get(event).ok_or_else(Response::not_found)?;
        return Ok(Request::Media { stream, msn, part });
    }
    let stream = app.streams.get(event).ok_or_else(Response::not_found)?;
    let prefix = stream.lock().prefix.clone();
    let chunk = playlist::parse_chunk(&prefix, file).ok_or_else(Response::not_found)?;
    Ok(Request::Chunk {
        stream,
        prefix,
        chunk,
    })
}

/// The response if it can be given now, `None` if the request must wait.
fn answer(app: &App, request: &Request) -> Option<Response> {
    let not_found = || Some(Response::not_found());
    match request {
        Request::Multivariant(stream) => match stream.lock().multivariant.clone() {
            Some(b) => Some(Response::ok(b, PLAYLIST_TYPE, "no-cache")),
            None => not_found(),
        },
        Request::Media { stream, msn, part } => {
            let st = stream.lock();
            if st.closed {
                return not_found();
            }
            let readiness = st
                .ingest
                .muxer()
                .map_or(Readiness::Wait, |m| m.readiness(*msn, *part));
            match readiness {
                Readiness::BadRequest => Some(Response::empty("400 Bad Request")),
                Readiness::Ready => {
                    if !app.config.low_latency
                        && let Some(muxer) = st.ingest.muxer()
                    {
                        return Some(Response::ok(
                            Bytes::from(playlist::media_with_mode(muxer, &st.prefix, false)),
                            PLAYLIST_TYPE,
                            "no-cache",
                        ));
                    }
                    st.media
                        .clone()
                        .map(|b| Response::ok(b, PLAYLIST_TYPE, "no-cache"))
                }
                Readiness::Wait => None,
            }
        }
        Request::Chunk {
            stream,
            prefix,
            chunk,
        } => {
            let st = stream.lock();
            // Reconnect codec epochs keep this prefix; closed presentations
            // do not.
            if st.closed || &st.prefix != prefix {
                return not_found();
            }
            let Some(m) = st.ingest.muxer() else {
                return not_found();
            };
            match chunk {
                Chunk::Init | Chunk::InitEpoch(_) => {
                    let epoch = match chunk {
                        Chunk::InitEpoch(id) => *id,
                        _ => 0,
                    };
                    match m.initialization(epoch) {
                        Some(init) => Some(Response::ok(init.clone(), "video/mp4", CHUNK_CACHE)),
                        None => not_found(),
                    }
                }
                Chunk::Segment(id) => match m.segment(*id) {
                    Some(s) => Some(Response::ok(s.data.clone(), "video/mp4", CHUNK_CACHE)),
                    None => not_found(),
                },
                Chunk::Part(id) => match m.part(*id) {
                    PartLookup::Found(p) => {
                        Some(Response::ok(p.data.clone(), "video/mp4", CHUNK_CACHE))
                    }
                    PartLookup::Missing => not_found(),
                    // The preload hint: hold the request until the part exists.
                    PartLookup::Next => None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::Source;
    use crate::test_support::{LANDSCAPE, PORTRAIT, app, begin};
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream as StdStream;

    #[test]
    fn scatter_output_keeps_partial_offsets_across_would_block_and_interrupts() {
        enum Action {
            Write(usize),
            Block,
            Interrupt,
        }
        struct Writer {
            actions: VecDeque<Action>,
            bytes: Vec<u8>,
            largest_batch: usize,
        }
        impl Write for Writer {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                panic!("the output must use scatter writes");
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }

            fn write_vectored(&mut self, slices: &[IoSlice<'_>]) -> io::Result<usize> {
                assert!(slices.len() <= WRITE_SLICES);
                assert!(slices.iter().all(|slice| !slice.is_empty()));
                self.largest_batch = self.largest_batch.max(slices.len());
                let limit = match self.actions.pop_front() {
                    Some(Action::Block) => return Err(io::ErrorKind::WouldBlock.into()),
                    Some(Action::Interrupt) => return Err(io::ErrorKind::Interrupted.into()),
                    Some(Action::Write(limit)) => limit,
                    None => usize::MAX,
                };
                let written = limit.min(slices.iter().map(|slice| slice.len()).sum());
                let mut remaining = written;
                for slice in slices {
                    let take = remaining.min(slice.len());
                    self.bytes.extend_from_slice(&slice[..take]);
                    remaining -= take;
                }
                Ok(written)
            }
        }

        let chunks: Vec<_> = (0..24)
            .flat_map(|index| {
                [
                    Bytes::new(),
                    Bytes::from(vec![b'A' + index as u8; index + 1]),
                ]
            })
            .collect();
        let data = sapflow_hls::muxer::SegmentData::from_chunks(chunks).expect("bounded fixture");
        let head = Bytes::from_static(b"head\r\n\r\n");
        let mut expected = head.to_vec();
        for chunk in data.chunks() {
            expected.extend_from_slice(chunk);
        }
        let mut output = Output {
            head,
            written: 0,
            response_started: None,
            body: data.into(),
        };
        let mut writer = Writer {
            actions: VecDeque::from([
                Action::Write(3),
                Action::Block,
                Action::Interrupt,
                Action::Write(7),
                Action::Block,
                Action::Write(0),
                Action::Write(1),
                Action::Write(23),
            ]),
            bytes: Vec::new(),
            largest_batch: 0,
        };
        assert_eq!(output.write_to(&mut writer).expect("short header write"), 3);
        assert_eq!(output.written, 3);
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted] {
            assert_eq!(
                output.write_to(&mut writer).expect_err("retry").kind(),
                kind
            );
            assert_eq!(output.written, 3);
            assert_eq!(output.body.len(), expected.len() - 8);
        }
        assert_eq!(output.write_to(&mut writer).expect("header/body split"), 7);
        assert!(output.head.is_empty());
        assert_eq!(output.body.len(), expected.len() - 10);
        assert_eq!(
            output
                .write_to(&mut writer)
                .expect_err("body blocked")
                .kind(),
            io::ErrorKind::WouldBlock
        );
        let left = output.body.len();
        assert_eq!(output.write_to(&mut writer).expect("zero write"), 0);
        assert_eq!(output.body.len(), left);
        for _ in 0..128 {
            if output.is_empty() {
                break;
            }
            output
                .write_to(&mut writer)
                .expect("remaining scatter write");
        }
        assert!(output.is_empty());
        assert_eq!(writer.bytes, expected);
        assert_eq!(writer.largest_batch, WRITE_SLICES);
    }

    /// One response off a blocking socket: status, headers, body.
    fn response(r: &mut BufReader<StdStream>) -> (String, Vec<String>, Vec<u8>) {
        let mut status = String::new();
        r.read_line(&mut status).expect("status line");
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).expect("header line");
            if line == "\r\n" || line.is_empty() {
                break;
            }
            headers.push(line.trim_end().to_ascii_lowercase());
        }
        let len: usize = headers
            .iter()
            .find_map(|h| h.strip_prefix("content-length: "))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        r.read_exact(&mut body).expect("body");
        (status.trim_end().to_string(), headers, body)
    }

    fn connect(addr: std::net::SocketAddr) -> (StdStream, BufReader<StdStream>) {
        let s = StdStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let r = BufReader::new(s.try_clone().expect("clone"));
        (s, r)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_parks_and_wakes() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        spawn(app.clone(), listener, 2, Timeouts::default()).expect("spawn");

        let mut publisher = begin(&app);
        publisher
            .push(|source, now| match source {
                Source::Ts(ts) => ts.push(PORTRAIT, now),
                _ => unreachable!(),
            })
            .expect("fixture");
        let stream = publisher.stream.clone();
        let (prefix, msn, hint) = {
            let st = stream.lock();
            let m = st.ingest.muxer().expect("muxer");
            (
                st.prefix.clone(),
                m.media_sequence() + m.entries().len() as u64 + 1,
                m.next_part_id(),
            )
        };

        // Pipelined: a playlist, a HEAD of it, a 404, an OPTIONS.
        let (mut s, mut r) = connect(addr);
        s.write_all(
            b"GET /live/event-epochs/index.m3u8 HTTP/1.1\r\n\r\n\
              HEAD /live/event-epochs/stream.m3u8 HTTP/1.1\r\n\r\n\
              GET /live/event-nothing/stream.m3u8 HTTP/1.1\r\n\r\n\
              OPTIONS /live/event-epochs/stream.m3u8 HTTP/1.1\r\n\r\n",
        )
        .expect("send");
        let (status, headers, body) = response(&mut r);
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(headers.contains(&"access-control-allow-origin: *".to_string()));
        assert!(headers.contains(&format!("content-type: {PLAYLIST_TYPE}")));
        assert!(
            std::str::from_utf8(&body)
                .expect("utf-8")
                .contains("#EXTM3U")
        );
        // HEAD: the length of the body, and no body.
        let mut line = String::new();
        r.read_line(&mut line).expect("HEAD status");
        assert_eq!(line, "HTTP/1.1 200 OK\r\n");
        let mut head_len = None;
        loop {
            let mut h = String::new();
            r.read_line(&mut h).expect("HEAD header");
            if h == "\r\n" {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length: ") {
                head_len = v.trim().parse::<usize>().ok();
            }
        }
        assert!(head_len.is_some_and(|n| n > 0));
        assert_eq!(response(&mut r).0, "HTTP/1.1 404 Not Found");
        let (status, headers, _) = response(&mut r);
        assert_eq!(status, "HTTP/1.1 204 No Content");
        assert!(headers.contains(&"access-control-allow-methods: get, options".to_string()));
        assert!(!headers.iter().any(|h| h.starts_with("content-length")));

        // A blocking playlist reload and the preload-hinted part both park.
        let blocking = std::thread::spawn(move || {
            let (mut s, mut r) = connect(addr);
            s.write_all(
                format!(
                    "GET /live/event-epochs/stream.m3u8?_HLS_msn={msn}&_HLS_part=0 HTTP/1.1\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("send");
            response(&mut r)
        });
        let part_name = playlist::part_name(&prefix, hint);
        let preload = std::thread::spawn(move || {
            let (mut s, mut r) = connect(addr);
            s.write_all(format!("GET /live/event-epochs/{part_name} HTTP/1.1\r\n\r\n").as_bytes())
                .expect("send");
            response(&mut r)
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!blocking.is_finished() && !preload.is_finished());

        // A reconnecting encoder's next epoch wakes both.
        publisher.end();
        let mut publisher = begin(&app);
        publisher
            .push(|source, now| match source {
                Source::Ts(ts) => ts.push(LANDSCAPE, now),
                _ => unreachable!(),
            })
            .expect("fixture");
        let (status, _, body) = blocking.join().expect("blocking reload");
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(std::str::from_utf8(&body).expect("utf-8").contains(&prefix));
        let (status, headers, body) = preload.join().expect("preload hint");
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(headers.contains(&"content-type: video/mp4".to_string()));
        assert!(!body.is_empty());
        publisher.end();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bad_requests_get_400_and_a_close_and_others_405() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        spawn(app, listener, 1, Timeouts::default()).expect("spawn");

        let (mut s, mut r) = connect(addr);
        s.write_all(b"POST /live/x/y HTTP/1.1\r\n\r\n")
            .expect("send");
        let (status, headers, _) = response(&mut r);
        assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
        assert!(headers.contains(&"allow: get,head,options".to_string()));
        // Off the routes, every method is 404.
        for raw in [
            "POST /elsewhere HTTP/1.1\r\n\r\n",
            "OPTIONS /elsewhere HTTP/1.1\r\n\r\n",
            "OPTIONS * HTTP/1.1\r\n\r\n",
            "GET http://example/live/event-nothing/index.m3u8 HTTP/1.1\r\n\r\n",
        ] {
            s.write_all(raw.as_bytes()).expect("send");
            assert_eq!(response(&mut r).0, "HTTP/1.1 404 Not Found", "{raw}");
        }
        // A bad blocking query is 400 before the stream is looked up.
        s.write_all(b"GET /live/event-nothing/stream.m3u8?_HLS_msn=x HTTP/1.1\r\n\r\n")
            .expect("send");
        assert_eq!(response(&mut r).0, "HTTP/1.1 400 Bad Request");

        s.write_all(b"GET / HTTP/1.1\r\ncontent-length: 4\r\n\r\nGET ")
            .expect("send");
        let (status, headers, _) = response(&mut r);
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(headers.contains(&"connection: close".to_string()));
        let mut rest = Vec::new();
        assert_eq!(r.read_to_end(&mut rest).expect("closed"), 0);

        // HTTP/1.0 closes after one answer.
        let (mut s, mut r) = connect(addr);
        s.write_all(b"GET /nothing HTTP/1.0\r\n\r\n").expect("send");
        assert_eq!(response(&mut r).0, "HTTP/1.1 404 Not Found");
        let mut rest = Vec::new();
        assert_eq!(r.read_to_end(&mut rest).expect("closed"), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_1_0_keep_alive_is_answered_in_kind() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        spawn(app, listener, 1, Timeouts::default()).expect("spawn");
        let (mut s, mut r) = connect(addr);
        for _ in 0..2 {
            s.write_all(b"GET /nothing HTTP/1.0\r\nconnection: keep-alive\r\n\r\n")
                .expect("send");
            let (status, headers, _) = response(&mut r);
            assert_eq!(status, "HTTP/1.1 404 Not Found");
            assert!(headers.contains(&"connection: keep-alive".to_string()));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_trickled_request_head_is_cut_off_at_the_header_timeout() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let timeouts = Timeouts {
            header: Duration::from_millis(400),
            idle: Duration::from_secs(60),
            body: BODY_TIMEOUT,
        };
        spawn(app, listener, 1, timeouts).expect("spawn");
        let (mut s, mut r) = connect(addr);
        r.get_ref()
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("timeout");
        let started = Instant::now();
        let mut closed = false;
        // A byte every 100 ms keeps the idle timer fresh but not the head's.
        for b in b"GET /nothing HTTP/1.1\r\nx: ".iter().cycle().take(40) {
            if s.write_all(&[*b]).is_err() {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            let mut probe = [0u8; 1];
            if let Ok(0) = r.get_mut().read(&mut probe) {
                closed = true;
                break;
            }
        }
        assert!(closed, "the slow head was never cut off");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pipelining_client_does_not_hold_up_others_on_its_worker() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        // One worker, so the hog and the other viewer share it.
        spawn(app, listener, 1, Timeouts::default()).expect("spawn");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hog = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let (mut s, r) = connect(addr);
                let reader = std::thread::spawn(move || {
                    let mut r = r;
                    let mut sink = [0u8; 64 * 1024];
                    while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
                });
                let burst = b"GET /nothing HTTP/1.1\r\n\r\n".repeat(512);
                while !stop.load(Ordering::Relaxed) {
                    if s.write_all(&burst).is_err() {
                        break;
                    }
                }
                let _ = s.shutdown(std::net::Shutdown::Both);
                let _ = reader.join();
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..5 {
            let (mut s, mut r) = connect(addr);
            let t = Instant::now();
            s.write_all(b"GET /nothing HTTP/1.1\r\n\r\n").expect("send");
            assert_eq!(response(&mut r).0, "HTTP/1.1 404 Not Found");
            assert!(
                t.elapsed() < Duration::from_millis(500),
                "{:?}",
                t.elapsed()
            );
        }
        stop.store(true, Ordering::Relaxed);
        let _ = hog.join();
    }
}

#[cfg(test)]
#[path = "own_http_deadline_tests.rs"]
mod deadline_tests;
