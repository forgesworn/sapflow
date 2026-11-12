//! The passive ICE-TCP listener (RFC 6544) on `SAPFLOW_WHIP_TCP_ADDR`,
//! with RFC 4571 framing and, behind a front proxy, PROXY protocol v2
//! (`docs/whip-ingest.md`, Connectivity). Bound only when configured.
//!
//! - **One connection is one tuple.** Each accepted connection gets a
//!   number that is never reused, and its frames reach the router as
//!   `Tuple::tcp(publisher address, number)`. The publisher's address is the
//!   TCP peer's, or, with `SAPFLOW_WHIP_TCP_PROXY_PROTOCOL=v2`, the source in
//!   the PROXY header from `SAPFLOW_WHIP_TRUSTED_PROXY` (`whip_tcp_front.rs`
//!   has the rules, LOCAL included). It keys the per-address cap and is what
//!   a check's XOR-MAPPED-ADDRESS reports; it is never logged. Admission and
//!   the failed-attempt throttle happen at the `POST`, on the signalling
//!   listener's address rules, not here.
//! - **Tasks and queues.** One task accepts; one task per connection reads
//!   and writes it. Frames go to the UDP owner, which owns every session,
//!   over one queue shared by every connection ([`INBOUND`] frames); a full
//!   queue makes the readers wait, which TCP's own flow control carries back
//!   to the publishers. What a session sends to a TCP tuple goes into that
//!   connection's queue ([`Limits::queue_frames`] and
//!   [`Limits::queue_bytes`]) without waiting, framed: a full queue drops the
//!   packet, as a lost datagram would be, and never holds up the owner.
//! - **Bounds.** At most [`Limits::connections`] connections (more are
//!   closed as they are accepted), and [`Limits::per_address`] from one
//!   publisher address (judged once the address is known, after any PROXY
//!   header). A PROXY header must be whole within [`Limits::header_timeout`].
//!   A connection must have had a frame sent back to it within
//!   [`Limits::handshake_timeout`] of its start, and since the last one
//!   within [`Limits::idle_timeout`]: sapflow sends nothing to a tuple whose
//!   check has not passed MESSAGE-INTEGRITY, so a connection that never
//!   authenticates, or whose session has gone, closes. One write may take
//!   at most [`Limits::write_timeout`]. Reads are 4 KiB at a time, and at
//!   most one partial frame is held besides.
//! - **Failures.** A failed accept is logged and retried after 100 ms, as
//!   on the signalling listener. A closed connection is not reported to its
//!   session: the session sees it as a path that went quiet, and ends by its
//!   consent or media timeout unless another path carries on.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sapflow_whip::framing::{self, Framing};
use sapflow_whip::{Transmit, Transport, Tuple};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use crate::config::WhipConfig;
use crate::whip_tcp_front::{Close, Front};

/// Frames queued from every connection to the UDP owner.
pub(crate) const INBOUND: usize = 1024;
/// Bytes read at a time.
const READ: usize = 4096;
/// Most bytes written in one go, coalesced from the queue.
const WRITE_BATCH: usize = 16 * 1024;
/// RFC 4571 frames: 1500 bytes each, as for a UDP datagram, and room for one
/// read and a partial frame.
pub(crate) const FRAMING: Framing = Framing {
    max_frame: 1500,
    max_buffered: 8192,
};

/// The listener's bounds. Each has a refusal path and a test.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Connections at once, in every state. Chrome 157 opened one per
    /// local candidate, 5 on the test host, so the default 128 is 8 for
    /// each of the default 16 publishers.
    pub connections: usize,
    /// Connections at once from one publisher address. Publishers behind
    /// one NAT share an address, so the default 32 leaves room for several
    /// browsers there while one address takes at most a quarter of the
    /// slots.
    pub per_address: usize,
    /// For a whole PROXY header, from accept.
    pub header_timeout: Duration,
    /// For the first frame sent back, from the end of the header (or
    /// accept).
    pub handshake_timeout: Duration,
    /// Between frames sent back.
    pub idle_timeout: Duration,
    /// For one write to finish.
    pub write_timeout: Duration,
    /// Frames queued to send on one connection.
    pub queue_frames: usize,
    /// Bytes queued to send on one connection, length prefixes included.
    pub queue_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 128,
            per_address: 32,
            header_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(5),
            queue_frames: 256,
            queue_bytes: 256 * 1024,
        }
    }
}

/// Counters. None holds an address.
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub accepted: AtomicU64,
    /// Closed at accept: [`Limits::connections`] were open.
    pub over_total: AtomicU64,
    /// Closed once the address was known: [`Limits::per_address`] from it.
    pub over_address: AtomicU64,
    /// PROXY protocol on, and the peer not the trusted proxy.
    pub untrusted: AtomicU64,
    /// PROXY headers refused.
    pub bad_header: AtomicU64,
    /// PROXY headers not whole in time.
    pub header_timeout: AtomicU64,
    /// LOCAL headers.
    pub local: AtomicU64,
    /// Frames over the cap, or too much buffered.
    pub framing: AtomicU64,
    /// No frame sent back in time.
    pub handshake_timeout: AtomicU64,
    pub idle_timeout: AtomicU64,
    pub write_timeout: AtomicU64,
    /// Frames from connections.
    pub frames_in: AtomicU64,
    /// Frames written.
    pub frames_out: AtomicU64,
    /// Packets dropped for a full send queue.
    pub queue_full: AtomicU64,
    /// Packets for a connection that has closed.
    pub no_connection: AtomicU64,
    /// Packets dropped because queueing them panicked (A3.4).
    pub queue_panics: AtomicU64,
}

/// The listener, bound at start so a bad address stops the server.
pub(crate) struct Bound {
    pub listener: TcpListener,
    /// The trusted proxy, when PROXY protocol v2 is on.
    pub proxy: Option<IpAddr>,
    pub limits: Limits,
    pub stats: Arc<Stats>,
}

impl Bound {
    /// Binds `SAPFLOW_WHIP_TCP_ADDR`, if set.
    pub(crate) async fn bind(config: &WhipConfig) -> std::io::Result<Option<Self>> {
        let Some(addr) = config.tcp_addr else {
            return Ok(None);
        };
        let proxy = if config.tcp_proxy_protocol {
            config.trusted_proxy
        } else {
            None
        };
        Ok(Some(Self::new(
            TcpListener::bind(addr).await?,
            proxy,
            Limits::default(),
        )))
    }

    pub(crate) fn new(listener: TcpListener, proxy: Option<IpAddr>, limits: Limits) -> Self {
        Self {
            listener,
            proxy,
            limits,
            stats: Arc::new(Stats::default()),
        }
    }
}

/// A frame from a connection, for the router.
pub(crate) struct Frame {
    pub tuple: Tuple,
    pub bytes: Vec<u8>,
}

/// A connection's send queue.
struct Writer {
    tx: mpsc::Sender<Vec<u8>>,
    /// Bytes queued, which the connection subtracts as it writes.
    queued: Arc<AtomicUsize>,
}

type Writers = Arc<Mutex<HashMap<u64, Writer>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The UDP owner's end: frames in, packets out.
pub(crate) struct Link {
    inbound: mpsc::Receiver<Frame>,
    /// Held while TCP is off, so [`Link::recv`] waits for ever.
    _idle: Option<mpsc::Sender<Frame>>,
    writers: Writers,
    limits: Limits,
    stats: Arc<Stats>,
}

impl Link {
    /// The next frame from any connection.
    pub(crate) async fn recv(&mut self) -> Option<Frame> {
        self.inbound.recv().await
    }

    /// Sends what the router gave out: UDP to the socket, TCP to the
    /// connection's queue. A TCP tuple never falls through to UDP.
    pub(crate) async fn transmit(&self, socket: &UdpSocket, t: Transmit) {
        match t.tuple.transport {
            Transport::Udp => {
                // Logged under the UDP owner's target, as before W3d. An
                // I/O error's text names no address.
                if let Err(error) = socket.send_to(&t.bytes, t.tuple.remote).await {
                    tracing::debug!(target: "sapflow::whip", %error, "WHIP send failed");
                }
            }
            // A3.4: this runs in the UDP owner, so queueing is guarded as
            // the owner's code for a session is: a panic drops the packet,
            // never the owner.
            Transport::Tcp => {
                let queued =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.queue(&t)));
                if queued.is_err() {
                    self.stats.queue_panics.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(target: "sapflow::whip", "WHIP TCP queue panicked; packet dropped");
                }
            }
        }
    }

    /// Frames `t` into its connection's queue, or drops and counts it.
    fn queue(&self, t: &Transmit) {
        let writers = lock(&self.writers);
        let Some(w) = writers.get(&t.tuple.connection) else {
            self.stats.no_connection.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut frame = Vec::with_capacity(t.bytes.len() + 2);
        if framing::encode(&FRAMING, &t.bytes, &mut frame).is_err() {
            self.stats.queue_full.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let len = frame.len();
        let queued = w.queued.load(Ordering::Acquire);
        if queued.saturating_add(len) > self.limits.queue_bytes {
            self.stats.queue_full.fetch_add(1, Ordering::Relaxed);
            return;
        }
        w.queued.fetch_add(len, Ordering::AcqRel);
        if w.tx.try_send(frame).is_err() {
            w.queued.fetch_sub(len, Ordering::AcqRel);
            self.stats.queue_full.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The owner's link, and the listener to run beside it (which waits for
/// ever when TCP is off).
pub(crate) fn link(
    bound: Option<Bound>,
) -> (
    Link,
    impl std::future::Future<Output = std::io::Result<()>> + Send,
) {
    let (tx, inbound) = mpsc::channel(INBOUND);
    let writers: Writers = Arc::default();
    let (limits, stats) = bound.as_ref().map_or_else(
        || (Limits::default(), Arc::default()),
        |b| (b.limits, b.stats.clone()),
    );
    let idle = bound.is_none().then(|| tx.clone());
    let shared = Arc::new(Shared {
        inbound: tx,
        writers: writers.clone(),
        addresses: Mutex::new(HashMap::new()),
        limits,
        stats: stats.clone(),
        proxy: bound.as_ref().and_then(|b| b.proxy),
        next: AtomicU64::new(1),
    });
    let link = Link {
        inbound,
        _idle: idle,
        writers,
        limits,
        stats,
    };
    let listener = async move {
        match bound {
            Some(b) => listen(b.listener, shared).await,
            None => std::future::pending().await,
        }
    };
    (link, listener)
}

/// What every connection task shares.
struct Shared {
    inbound: mpsc::Sender<Frame>,
    writers: Writers,
    /// Open connections per publisher address, canonical.
    addresses: Mutex<HashMap<IpAddr, usize>>,
    limits: Limits,
    stats: Arc<Stats>,
    proxy: Option<IpAddr>,
    next: AtomicU64,
}

async fn listen(listener: TcpListener, shared: Arc<Shared>) -> std::io::Result<()> {
    if let Ok(addr) = listener.local_addr() {
        tracing::info!(tcp = %addr, proxy_protocol = shared.proxy.is_some(), "WHIP ICE-TCP listening");
    }
    let slots = Arc::new(Semaphore::new(shared.limits.connections));
    let mut warnings = crate::accept::Warnings::default();
    loop {
        let (socket, peer) = crate::accept::next(&listener, "WHIP ICE-TCP", &mut warnings).await;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            shared.stats.over_total.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        shared.stats.accepted.fetch_add(1, Ordering::Relaxed);
        let n = shared.next.fetch_add(1, Ordering::Relaxed);
        let shared = shared.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let ended = connection(socket, peer, n, &shared).await;
            count(&shared.stats, ended);
            tracing::debug!(connection = n, reason = %ended, "WHIP ICE-TCP connection closed");
        });
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, Copy)]
enum Ended {
    Front(Close),
    HeaderTimeout,
    OverAddress,
    HandshakeTimeout,
    IdleTimeout,
    WriteTimeout,
    /// The peer closed, or a read or write failed.
    Peer,
    /// The UDP owner has gone.
    Shutdown,
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Front(c) => write!(f, "{c}"),
            Self::HeaderTimeout => f.write_str("PROXY header timeout"),
            Self::OverAddress => f.write_str("too many connections from one address"),
            Self::HandshakeTimeout => f.write_str("nothing sent back in time"),
            Self::IdleTimeout => f.write_str("idle"),
            Self::WriteTimeout => f.write_str("write stalled"),
            Self::Peer => f.write_str("closed"),
            Self::Shutdown => f.write_str("shutting down"),
        }
    }
}

fn count(stats: &Stats, ended: Ended) {
    let c = match ended {
        Ended::Front(Close::Untrusted) => &stats.untrusted,
        Ended::Front(Close::Header(_)) => &stats.bad_header,
        Ended::Front(Close::Local) => &stats.local,
        Ended::Front(Close::Framing(_)) => &stats.framing,
        Ended::HeaderTimeout => &stats.header_timeout,
        Ended::OverAddress => &stats.over_address,
        Ended::HandshakeTimeout => &stats.handshake_timeout,
        Ended::IdleTimeout => &stats.idle_timeout,
        Ended::WriteTimeout => &stats.write_timeout,
        Ended::Peer | Ended::Shutdown => return,
    };
    c.fetch_add(1, Ordering::Relaxed);
}

/// Holds one of an address's connection slots until dropped.
struct AddressSlot<'a> {
    shared: &'a Shared,
    ip: IpAddr,
}

impl Drop for AddressSlot<'_> {
    fn drop(&mut self) {
        let mut map = lock(&self.shared.addresses);
        if let Some(n) = map.get_mut(&self.ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

fn admit(shared: &Shared, ip: IpAddr) -> Option<AddressSlot<'_>> {
    let mut map = lock(&shared.addresses);
    let n = map.entry(ip).or_insert(0);
    if *n >= shared.limits.per_address {
        return None;
    }
    *n += 1;
    Some(AddressSlot { shared, ip })
}

/// Removes a connection's send queue when it ends.
struct Registration<'a> {
    writers: &'a Writers,
    n: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        lock(self.writers).remove(&self.n);
    }
}

async fn connection(mut stream: TcpStream, peer: SocketAddr, n: u64, shared: &Shared) -> Ended {
    let mut front = match shared.proxy {
        Some(trusted) => match Front::proxied(peer, trusted, FRAMING) {
            Ok(f) => f,
            // Dropped unread.
            Err(c) => return Ended::Front(c),
        },
        None => Front::direct(peer, FRAMING),
    };
    let mut buf = vec![0u8; READ];
    let mut frames = Vec::new();
    let client = match front.client() {
        Some(c) => c,
        None => {
            let deadline = Instant::now() + shared.limits.header_timeout;
            loop {
                match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
                    Err(_) => return Ended::HeaderTimeout,
                    Ok(Ok(0) | Err(_)) => return Ended::Peer,
                    Ok(Ok(k)) => match front.push(buf.get(..k).unwrap_or_default(), &mut frames) {
                        Err(c) => return Ended::Front(c),
                        Ok(Some(client)) => break client,
                        Ok(None) => {}
                    },
                }
            }
        }
    };
    let Some(_address) = admit(shared, client.ip()) else {
        return Ended::OverAddress;
    };
    let tuple = Tuple::tcp(client, n);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(shared.limits.queue_frames.max(1));
    let queued = Arc::new(AtomicUsize::new(0));
    lock(&shared.writers).insert(
        n,
        Writer {
            tx,
            queued: queued.clone(),
        },
    );
    let _registration = Registration {
        writers: &shared.writers,
        n,
    };
    let (mut rd, mut wr) = stream.split();
    let started = Instant::now();
    let mut last_sent: Option<Instant> = None;
    let mut batch = Vec::with_capacity(WRITE_BATCH);
    loop {
        for bytes in frames.drain(..) {
            shared.stats.frames_in.fetch_add(1, Ordering::Relaxed);
            if shared.inbound.send(Frame { tuple, bytes }).await.is_err() {
                return Ended::Shutdown;
            }
        }
        let deadline = match last_sent {
            None => started + shared.limits.handshake_timeout,
            Some(t) => t + shared.limits.idle_timeout,
        };
        tokio::select! {
            r = rd.read(&mut buf) => match r {
                Ok(0) | Err(_) => return Ended::Peer,
                Ok(k) => {
                    if let Err(c) = front.push(buf.get(..k).unwrap_or_default(), &mut frames) {
                        return Ended::Front(c);
                    }
                }
            },
            Some(first) = rx.recv() => {
                batch.clear();
                batch.extend_from_slice(&first);
                let mut count = 1u64;
                while batch.len() < WRITE_BATCH {
                    match rx.try_recv() {
                        Ok(more) => {
                            batch.extend_from_slice(&more);
                            count += 1;
                        }
                        Err(_) => break,
                    }
                }
                let wrote = tokio::time::timeout(shared.limits.write_timeout, wr.write_all(&batch)).await;
                queued.fetch_sub(batch.len(), Ordering::AcqRel);
                match wrote {
                    Ok(Ok(())) => {
                        shared.stats.frames_out.fetch_add(count, Ordering::Relaxed);
                        last_sent = Some(Instant::now());
                    }
                    Ok(Err(_)) => return Ended::Peer,
                    Err(_) => return Ended::WriteTimeout,
                }
            }
            () = tokio::time::sleep_until(deadline) => {
                return if last_sent.is_none() {
                    Ended::HandshakeTimeout
                } else {
                    Ended::IdleTimeout
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A send queue past its bytes, or its frames, drops what comes next
    /// and counts it; a closed connection's packets are counted too.
    #[test]
    fn the_send_queue_is_bounded_in_bytes_and_frames() {
        let limits = Limits {
            queue_frames: 4,
            queue_bytes: 3 * 1002,
            ..Limits::default()
        };
        let (_, inbound) = mpsc::channel(1);
        let link = Link {
            inbound,
            _idle: None,
            writers: Arc::default(),
            limits,
            stats: Arc::default(),
        };
        let (tx, mut rx) = mpsc::channel(limits.queue_frames);
        let queued = Arc::new(AtomicUsize::new(0));
        lock(&link.writers).insert(
            9,
            Writer {
                tx,
                queued: queued.clone(),
            },
        );
        let remote: SocketAddr = "198.51.100.3:4000".parse().expect("addr");
        let t = |len: usize, n: u64| Transmit {
            tuple: Tuple::tcp(remote, n),
            bytes: vec![0x80; len],
        };
        for _ in 0..4 {
            link.queue(&t(1000, 9));
        }
        assert_eq!(queued.load(Ordering::Acquire), 3 * 1002, "three fit");
        assert_eq!(link.stats.queue_full.load(Ordering::Relaxed), 1);
        // Frames, not bytes: four small ones fill it.
        let first = rx.try_recv().expect("framed");
        assert_eq!(&first[..2], &1000u16.to_be_bytes());
        queued.store(0, Ordering::Release);
        while rx.try_recv().is_ok() {}
        for _ in 0..5 {
            link.queue(&t(10, 9));
        }
        assert_eq!(link.stats.queue_full.load(Ordering::Relaxed), 2);
        // Over the frame cap, or to a connection that has gone.
        link.queue(&t(1501, 9));
        assert_eq!(link.stats.queue_full.load(Ordering::Relaxed), 3);
        link.queue(&t(10, 10));
        assert_eq!(link.stats.no_connection.load(Ordering::Relaxed), 1);
    }
}
