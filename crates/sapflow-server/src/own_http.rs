//! The viewer HTTP path on our own event loop (mio), without tokio, hyper or
//! axum: `SAPFLOW_HTTP_IMPL=own`. Ingest, callbacks and recording still run
//! on tokio. On the M4 at 750 viewers it used 43% less CPU and 88% less
//! memory than the hyper path (`docs/benchmarks.md`).
//!
//! Each worker thread owns a poller and the connections it accepts from a
//! shared listener. Requests that must wait (blocking playlist reload, the
//! preload-hinted part) park on their connection and are re-checked when a
//! stream changes ([`notify`]). Request framing is `http1.rs`; the answers
//! make the same decisions as `http.rs`.
//!
//! Limits: at most [`MAX_CONNECTIONS`] connections (past it, new ones wait
//! in the kernel's accept queue); one response in flight per connection, so
//! a client that sends without reading stops being read; a connection with
//! nothing parked that makes no progress for [`IDLE_TIMEOUT`] is closed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, IoSlice, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Token, Waker};
use sapflow_hls::playlist::{self, Chunk};
use sapflow_hls::{PartLookup, Readiness};

use crate::App;
use crate::http1::{self, Method, Parsed};
use crate::stream::LiveStream;

const LISTENER: Token = Token(0);
const WAKE: Token = Token(1);
const PLAYLIST_TYPE: &str = "application/vnd.apple.mpegurl";
const CHUNK_CACHE: &str = "public, max-age=3600";
/// Concurrent connections across all workers, as in `http.rs`.
pub const MAX_CONNECTIONS: usize = 20_000;
/// A connection with nothing parked is closed after this long without
/// reading a byte or writing one. As in `http.rs`, it must outlast clients'
/// idle keep-alive reuse.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How often each worker checks deadlines and retries a paused accept.
const SWEEP: Duration = Duration::from_millis(250);

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

/// Starts `threads` workers serving `listener`.
pub fn spawn(app: Arc<App>, listener: std::net::TcpListener, threads: usize) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    for i in 0..threads.max(1) {
        let mut worker = Worker::new(app.clone(), listener.try_clone()?)?;
        std::thread::Builder::new()
            .name(format!("http-{i}"))
            .spawn(move || {
                if let Err(e) = worker.run() {
                    tracing::error!(error = %e, "HTTP worker stopped");
                }
            })?;
    }
    Ok(())
}

struct Response {
    status: &'static str,
    headers: String,
    body: Bytes,
}

impl Response {
    fn empty(status: &'static str) -> Self {
        Self {
            status,
            headers: String::new(),
            body: Bytes::new(),
        }
    }
    fn ok(body: Bytes, content_type: &str, cache: &str) -> Self {
        Self {
            status: "200 OK",
            headers: format!("content-type: {content_type}\r\ncache-control: {cache}\r\n"),
            body,
        }
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
}

struct Conn {
    sock: TcpStream,
    input: Vec<u8>,
    /// Response bytes still to write, and how much of the front is written.
    output: VecDeque<Bytes>,
    written: usize,
    parked: Option<Parked>,
    /// Close once the output is written.
    close_after: bool,
    writable_armed: bool,
    last_progress: Instant,
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

struct Worker {
    app: Arc<App>,
    poll: Poll,
    listener: TcpListener,
    conns: HashMap<usize, (Conn, Slot)>,
    parked: HashSet<usize>,
    next: usize,
    /// Connections are waiting in the accept queue because of the cap.
    accept_paused: bool,
    buf: Vec<u8>,
}

impl Worker {
    fn new(app: Arc<App>, listener: std::net::TcpListener) -> io::Result<Self> {
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
            conns: HashMap::new(),
            parked: HashSet::new(),
            next: 2,
            accept_paused: false,
            buf: vec![0; 16 * 1024],
        })
    }

    fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(1024);
        let mut next_sweep = Instant::now() + SWEEP;
        loop {
            let timeout = next_sweep.saturating_duration_since(Instant::now());
            match self.poll.poll(&mut events, Some(timeout)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            let mut woke = false;
            for ev in events.iter() {
                match ev.token() {
                    LISTENER => self.accept(),
                    WAKE => woke = true,
                    Token(id) => self.service(id, ev.is_readable()),
                }
            }
            if woke {
                let ids: Vec<usize> = self.parked.iter().copied().collect();
                for id in ids {
                    self.service(id, false);
                }
            }
            let now = Instant::now();
            if now >= next_sweep {
                next_sweep = now + SWEEP;
                self.sweep(now);
            }
        }
    }

    fn accept(&mut self) {
        loop {
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
                        output: VecDeque::new(),
                        written: 0,
                        parked: None,
                        close_after: false,
                        writable_armed: false,
                        last_progress: Instant::now(),
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
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    // Out of file descriptors and the like: retry at the
                    // next sweep.
                    tracing::warn!(error = %e, "HTTP accept failed");
                    self.accept_paused = true;
                    return;
                }
            }
        }
    }

    /// Times out parked requests and idle connections, and retries a
    /// paused accept.
    fn sweep(&mut self, now: Instant) {
        let due: Vec<usize> = self
            .conns
            .iter()
            .filter(|(_, (c, _))| match &c.parked {
                Some(p) => p.deadline <= now,
                None => now.duration_since(c.last_progress) >= IDLE_TIMEOUT,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let parked = self.conns.get(&id).is_some_and(|(c, _)| c.parked.is_some());
            if parked {
                self.service(id, false);
            } else {
                self.close(id);
            }
        }
        if self.accept_paused {
            self.accept();
        }
    }

    fn close(&mut self, id: usize) {
        self.parked.remove(&id);
        if let Some((mut c, _slot)) = self.conns.remove(&id) {
            let _ = self.poll.registry().deregister(&mut c.sock);
        }
    }

    /// Reads (if asked), answers what it can, writes what it can, and drops
    /// the connection when it is finished or broken.
    fn service(&mut self, id: usize, read: bool) {
        let Some((conn, _)) = self.conns.get_mut(&id) else {
            return;
        };
        let alive =
            drive(&self.app, &self.poll, conn, Token(id), &mut self.buf, read).unwrap_or(false);
        let parked = conn.parked.is_some();
        if !alive {
            self.close(id);
        } else if parked {
            self.parked.insert(id);
        } else {
            self.parked.remove(&id);
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
) -> io::Result<bool> {
    let now = Instant::now();
    loop {
        if read || conn.unread {
            read = false;
            if !fill(conn, buf, now) {
                return Ok(false);
            }
        }
        if let Some(p) = conn.parked.take() {
            match answer(app, &p.request) {
                Some(r) => respond(conn, r, p.head_only),
                None if now >= p.deadline => {
                    respond(conn, Response::empty("503 Service Unavailable"), false)
                }
                None => conn.parked = Some(p),
            }
        }
        // One response in flight: take the next request only once the last
        // is written.
        if conn.parked.is_none() && conn.output.is_empty() && !conn.close_after {
            take_request(app, conn, now);
        }
        flush(conn, now)?;
        // Go round again while there is another request to take: one already
        // buffered, or bytes left in the socket when the buffer was full.
        let ready_for_more = conn.parked.is_none() && conn.output.is_empty() && !conn.close_after;
        let more = conn.unread || !matches!(http1::parse(&conn.input), Parsed::Partial);
        if !(ready_for_more && more) {
            break;
        }
    }
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
    Ok(!(conn.close_after && conn.output.is_empty() && conn.parked.is_none()))
}

/// Reads until the socket is empty or one request head is buffered. False
/// when the peer closed or the socket failed.
fn fill(conn: &mut Conn, buf: &mut [u8], now: Instant) -> bool {
    conn.unread = false;
    loop {
        // Stop past one request head: the rest waits in the socket, which
        // pushes back on the client.
        if conn.input.len() > http1::MAX_HEAD {
            conn.unread = true;
            return true;
        }
        match conn.sock.read(buf) {
            Ok(0) => return false,
            Ok(n) => {
                conn.input
                    .extend_from_slice(buf.get(..n).unwrap_or_default());
                conn.last_progress = now;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

/// Parses and answers the next buffered request, or parks it.
fn take_request(app: &App, conn: &mut Conn, now: Instant) {
    let (req, used) = match http1::parse(&conn.input) {
        Parsed::Request(r, n) => (r, n),
        Parsed::Partial => return,
        Parsed::Invalid => {
            conn.close_after = true;
            conn.input.clear();
            conn.unread = false;
            respond(conn, Response::empty("400 Bad Request"), false);
            return;
        }
    };
    conn.input.drain(..used);
    conn.close_after = req.close;
    match req.method {
        Method::Get | Method::Head => {
            let head_only = req.method == Method::Head;
            match route(app, &req.target) {
                Ok(request) => match answer(app, &request) {
                    Some(r) => respond(conn, r, head_only),
                    None => {
                        let deadline = now + block_limit(stream_of(&request));
                        conn.parked = Some(Parked {
                            request,
                            head_only,
                            deadline,
                        });
                    }
                },
                Err(r) => respond(conn, r, head_only),
            }
        }
        Method::Options => {
            let mut r = Response::empty("204 No Content");
            r.headers = "access-control-allow-methods: GET, OPTIONS\r\naccess-control-allow-headers: *\r\naccess-control-max-age: 86400\r\n".into();
            respond(conn, r, false);
        }
        Method::Other => respond(conn, Response::empty("405 Method Not Allowed"), false),
    }
}

fn respond(conn: &mut Conn, r: Response, head_only: bool) {
    let head = http1::head(r.status, &r.headers, r.body.len(), conn.close_after);
    conn.output.push_back(Bytes::from(head));
    if !head_only && !r.body.is_empty() {
        conn.output.push_back(r.body);
    }
}

fn flush(conn: &mut Conn, now: Instant) -> io::Result<()> {
    while !conn.output.is_empty() {
        let slices: Vec<IoSlice<'_>> = conn
            .output
            .iter()
            .take(4)
            .enumerate()
            .map(|(i, b)| {
                let from = if i == 0 { conn.written } else { 0 };
                IoSlice::new(b.get(from..).unwrap_or_default())
            })
            .collect();
        match conn.sock.write_vectored(&slices) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(mut n) => {
                conn.last_progress = now;
                while n > 0 {
                    let Some(front) = conn.output.front() else {
                        break;
                    };
                    let left = front.len().saturating_sub(conn.written);
                    if n >= left {
                        n -= left;
                        conn.output.pop_front();
                        conn.written = 0;
                    } else {
                        conn.written += n;
                        n = 0;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
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

fn route(app: &App, target: &str) -> Result<Request, Response> {
    let not_found = || Response::empty("404 Not Found");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let rest = path.strip_prefix("/live/").ok_or_else(not_found)?;
    let (event, file) = rest.split_once('/').ok_or_else(not_found)?;
    if file.contains('/') {
        return Err(not_found());
    }
    let stream = app.streams.get(event).ok_or_else(not_found)?;
    if file == "index.m3u8" {
        return Ok(Request::Multivariant(stream));
    }
    if file == playlist::MEDIA_PLAYLIST {
        let (msn, part) = playlist::parse_blocking_query(query)
            .map_err(|_| Response::empty("400 Bad Request"))?;
        // Without parts or CAN-BLOCK-RELOAD, the client reloads completed
        // segments; ignore a blocking request from before a mode change.
        let (msn, part) = if app.config.low_latency {
            (msn, part)
        } else {
            (None, None)
        };
        return Ok(Request::Media { stream, msn, part });
    }
    let prefix = stream.lock().prefix.clone();
    let chunk = playlist::parse_chunk(&prefix, file).ok_or_else(not_found)?;
    Ok(Request::Chunk {
        stream,
        prefix,
        chunk,
    })
}

/// The response if it can be given now, `None` if the request must wait.
fn answer(app: &App, request: &Request) -> Option<Response> {
    let not_found = || Some(Response::empty("404 Not Found"));
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
    use crate::http::epoch_tests::{LANDSCAPE, PORTRAIT, app, begin};
    use crate::stream::Source;
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream as StdStream;

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
    async fn serves_parks_and_wakes_like_the_hyper_path() {
        let (app, _scratch) = app(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        spawn(app.clone(), listener, 2).expect("spawn");

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
        spawn(app, listener, 1).expect("spawn");

        let (mut s, mut r) = connect(addr);
        s.write_all(b"POST /live/x/y HTTP/1.1\r\n\r\n")
            .expect("send");
        assert_eq!(response(&mut r).0, "HTTP/1.1 405 Method Not Allowed");

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
}
