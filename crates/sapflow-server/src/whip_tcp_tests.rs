//! ICE-TCP in process: the signalling listener, the UDP owner and the
//! ICE-TCP listener on loopback, driven by `sapflow-whip`'s scripted
//! publisher over a real TCP connection in RFC 4571 frames, with and without
//! a PROXY protocol v2 header, and each of the listener's bounds at a small
//! value.

use std::time::Duration;

use sapflow_whip::framing::{Decoder, encode};
use sapflow_whip::stun::Message;
use sapflow_whip::testing::{PPS, Publisher, StunClient, VIDEO_SSRC, idr_slice, server_identity};
use sapflow_whip::{Transmit, Tuple};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use super::*;
use crate::callback::Callbacks;
use crate::config::Config;
use crate::stream::Registry;
use crate::whip_tcp::{Bound, FRAMING, Limits, Stats};
use crate::whip_tcp_front::header_v4;

const GCM: u16 = 0x0007;
const SPS: &[u8] = &[
    0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08,
    0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
];
const TOKEN: &str = "publisher:whip-tcp-secret";

struct Server {
    app: Arc<App>,
    whip: Arc<Whip>,
    http: SocketAddr,
    tcp: SocketAddr,
    stats: Arc<Stats>,
}

fn app() -> Arc<App> {
    let all = [
        ("SAPFLOW_PUBLISH_USER", "publisher"),
        ("SAPFLOW_PUBLISH_PASS", "whip-tcp-secret"),
        ("SAPFLOW_WHIP_ADDR", "127.0.0.1:0"),
        ("SAPFLOW_WHIP_UDP_ADDR", "127.0.0.1:0"),
        ("SAPFLOW_WHIP_TCP_ADDR", "127.0.0.1:1"),
        ("SAPFLOW_WHIP_CANDIDATES", "tcp 192.0.2.1:47401"),
        ("SAPFLOW_WHIP_PUBLIC_URL", "https://whip.example/"),
        ("SAPFLOW_RECONNECT_GRACE", "30"),
    ];
    let config = Config::load(
        |key| {
            all.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        },
        Vec::new(),
    )
    .expect("a WHIP test configuration");
    Arc::new(App {
        recording_limits: Arc::new(crate::recorder::RecordingLimits::new(config.max_publishers)),
        publish_sessions: None,
        config,
        streams: Arc::new(Registry::default()),
        callbacks: Callbacks::start(None, None),
        auth_throttle: Mutex::new(sapflow_core::limits::AuthThrottle::new(
            10,
            Duration::from_secs(60),
        )),
        refusal_log: Mutex::new(sapflow_core::limits::LogLimiter::new(
            20,
            Duration::from_secs(60),
        )),
    })
}

/// A server whose ICE-TCP listener believes PROXY headers from `proxy`,
/// within `limits`.
async fn start(proxy: Option<IpAddr>, limits: Limits) -> Server {
    serve_tcp(proxy, limits, false).await
}

/// As [`start`]; with `hooks`, the panic hooks are on (A3.4).
async fn serve_tcp(proxy: Option<IpAddr>, limits: Limits, hooks: bool) -> Server {
    let app = app();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let tcp = listener.local_addr().expect("addr");
    let bound = Bound::new(listener, proxy, limits);
    let stats = bound.stats.clone();
    let sockets = Sockets {
        http: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        udp: UdpSocket::bind("127.0.0.1:0").await.expect("bind"),
        tcp: Some(bound),
    };
    let http = sockets.http.local_addr().expect("addr");
    let (whip, mut router, rx) = prepare(&app, server_identity())
        .expect("router")
        .expect("WHIP on");
    if hooks {
        router.enable_panic_hooks();
        whip.panic_hooks.store(true, Ordering::Relaxed);
    }
    tokio::spawn(run(app.clone(), whip.clone(), router, rx, sockets));
    Server {
        app,
        whip,
        http,
        tcp,
        stats,
    }
}

fn get(stats: &Stats, f: fn(&Stats) -> &std::sync::atomic::AtomicU64) -> u64 {
    f(stats).load(Ordering::Relaxed)
}

/// POSTs `offer`; returns the status, the `Location` and the answer.
async fn post(server: &Server, event: &str, offer: &str) -> (u16, String, String) {
    let raw = format!(
        "POST /whip/live/{event} HTTP/1.1\r\nhost: whip.example\r\nauthorization: Bearer {TOKEN}\r\ncontent-type: application/sdp\r\ncontent-length: {}\r\n\r\n{offer}",
        offer.len()
    );
    let mut s = TcpStream::connect(server.http).await.expect("connect");
    s.write_all(raw.as_bytes()).await.expect("write");
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut out)).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let location = head
        .split("\r\n")
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("location"))
        })
        .map(|(_, v)| v.trim().to_owned())
        .unwrap_or_default();
    (status, location, body.to_owned())
}

fn line<'a>(sdp: &'a str, prefix: &str) -> &'a str {
    sdp.lines()
        .find_map(|l| l.strip_prefix(prefix))
        .expect("answer line")
        .trim()
}

/// Whether the peer closes the connection within `wait`.
async fn closed_within(s: &mut TcpStream, wait: Duration) -> bool {
    let mut b = [0u8; 2048];
    loop {
        match tokio::time::timeout(wait, s.read(&mut b)).await {
            Err(_) => return false,
            Ok(Ok(0) | Err(_)) => return true,
            Ok(Ok(_)) => {}
        }
    }
}

/// A scripted publisher on one TCP connection.
struct Phone {
    p: Publisher,
    stream: TcpStream,
    decoder: Decoder,
    clock: Option<(tokio::time::Instant, u64)>,
}

impl Phone {
    /// Connects, sending `header` first if given.
    async fn new(seed: u8, server: &Server, header: Option<Vec<u8>>) -> Self {
        let mut p = Publisher::new(seed, &[GCM]);
        let mut stream = TcpStream::connect(server.tcp).await.expect("connect");
        p.tuple = Tuple::tcp(stream.local_addr().expect("addr"), 0);
        if let Some(h) = header {
            // A refused connection may already be closed.
            let _ = stream.write_all(&h).await;
        }
        Self {
            p,
            stream,
            decoder: Decoder::new(FRAMING),
            clock: None,
        }
    }

    async fn admit(&mut self, server: &Server, event: &str) -> String {
        let (status, location, answer) = post(server, event, &self.p.offer()).await;
        assert_eq!(status, 201, "{answer}");
        self.p.client.expect_server(server_identity().fingerprint());
        self.p.stun = Some(StunClient::new(
            line(&answer, "a=ice-ufrag:"),
            line(&answer, "a=ice-pwd:"),
            &self.p.ufrag,
        ));
        assert!(
            answer
                .lines()
                .any(|l| l.starts_with("a=candidate:") && l.ends_with("typ host tcptype passive")),
            "a passive TCP candidate: {answer}"
        );
        location
    }

    async fn send(&mut self, packets: &[Vec<u8>]) {
        assert!(self.try_send(packets).await, "send");
    }

    /// Sends `packets`, framed; `false` if the connection has gone.
    async fn try_send(&mut self, packets: &[Vec<u8>]) -> bool {
        let mut out = Vec::new();
        for d in packets {
            encode(&FRAMING, d, &mut out).expect("frame");
        }
        self.stream.write_all(&out).await.is_ok()
    }

    /// Takes what the server sends until `until`, answering DTLS.
    async fn receive_until(&mut self, until: tokio::time::Instant) {
        let mut buf = vec![0u8; 8192];
        while let Ok(Ok(n)) = tokio::time::timeout_at(until, self.stream.read(&mut buf)).await {
            assert!(n > 0, "the server kept the connection");
            self.decoder.push(&buf[..n]).expect("framing");
            let mut replies = Vec::new();
            while let Some(bytes) = self.decoder.pop().expect("framing") {
                if classify(&bytes) == Kind::Rtcp && !self.p.has_keys() {
                    continue;
                }
                replies.extend(self.p.take(Transmit {
                    tuple: self.p.tuple,
                    bytes,
                }));
            }
            self.send(&replies).await;
        }
    }

    async fn receive(&mut self, wait: Duration) {
        self.receive_until(tokio::time::Instant::now() + wait).await;
    }

    async fn connect(&mut self) {
        let mut first = vec![self.p.check(true)];
        first.extend(self.p.client.start());
        self.send(&first).await;
        for _ in 0..20 {
            self.receive(Duration::from_millis(100)).await;
            if self.p.has_keys() {
                break;
            }
        }
        assert!(self.p.has_keys(), "DTLS completes over ICE-TCP");
    }

    /// The XOR-MAPPED-ADDRESS of the first STUN answer.
    fn mapped(&self) -> Option<SocketAddr> {
        let first = self.p.stun_answers.first()?;
        Message::parse(first).ok()?.xor_mapped
    }

    async fn publish(&mut self, seconds: u64) {
        for _ in 0..seconds * 50 {
            let (start, n) = *self.clock.get_or_insert((tokio::time::Instant::now(), 0));
            self.receive_until(start + Duration::from_millis(20 * n))
                .await;
            self.clock = Some((start, n + 1));
            let mut packets = Vec::new();
            if n % 2 == 0 {
                let frame = n / 2;
                if frame % 25 == 0 {
                    let idr = idr_slice(1200, 0x11);
                    packets.extend(self.p.video(&[SPS, PPS, &idr], VIDEO_SSRC));
                } else {
                    packets.extend(self.p.delta(300).0);
                }
            }
            packets.push(self.p.audio((n % 251) as u8));
            self.send(&packets).await;
        }
    }
}

/// A3.1 in process over ICE-TCP: POST, then ICE, DTLS and SRTP in RFC 4571
/// frames on one connection, then media in the stream. Nothing goes out
/// over UDP to the publisher's address, and DELETE ends the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publisher_reaches_the_stream_over_ice_tcp() {
    let server = start(None, Limits::default()).await;
    let mut phone = Phone::new(31, &server, None).await;
    // A UDP socket on the TCP connection's own address and port: a send
    // that fell through to UDP would land here.
    let udp = UdpSocket::bind(phone.p.tuple.remote).await.expect("bind");
    let location = phone.admit(&server, "event-whip-tcp").await;
    phone.connect().await;
    assert_eq!(phone.mapped(), Some(phone.p.tuple.remote));
    let stream = server.app.streams.get("event-whip-tcp").expect("claimed");
    for _ in 0..10 {
        phone.publish(1).await;
        if stream.has_content() {
            break;
        }
    }
    assert!(stream.has_content(), "media reached the playlist over TCP");
    assert_eq!(phone.p.misdirected, 0);
    let mut b = [0u8; 64];
    assert!(
        tokio::time::timeout(Duration::from_millis(200), udp.recv_from(&mut b))
            .await
            .is_err(),
        "no UDP to a TCP publisher"
    );
    assert!(get(&server.stats, |s| &s.frames_in) > 100);
    assert!(get(&server.stats, |s| &s.frames_out) > 5);
    assert_eq!(get(&server.stats, |s| &s.queue_full), 0);
    let path = location
        .strip_prefix("https://whip.example")
        .expect("absolute");
    let raw = format!("DELETE {path} HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");
    let mut s = TcpStream::connect(server.http).await.expect("connect");
    s.write_all(raw.as_bytes()).await.expect("write");
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    assert!(out.starts_with(b"HTTP/1.1 200"), "DELETE 200");
}

/// PROXY v2 from the trusted proxy: the header's source is the publisher's
/// address, as the check's XOR-MAPPED-ADDRESS shows, and the session
/// connects behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proxy_v2_header_from_the_trusted_proxy_gives_the_publisher_address() {
    let server = start(Some("127.0.0.1".parse().expect("ip")), Limits::default()).await;
    let source: SocketAddr = "203.0.113.9:40123".parse().expect("addr");
    let mut phone = Phone::new(32, &server, Some(header_v4(1, source, &[]))).await;
    phone.admit(&server, "event-whip-proxied").await;
    phone.connect().await;
    assert_eq!(phone.mapped(), Some(source));
    assert_eq!(get(&server.stats, |s| &s.untrusted), 0);
}

/// PROXY v2 from any other peer: dropped before a byte is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn any_other_peer_is_dropped_unread() {
    let server = start(Some("192.0.2.99".parse().expect("ip")), Limits::default()).await;
    let source: SocketAddr = "203.0.113.9:40123".parse().expect("addr");
    let mut phone = Phone::new(33, &server, Some(header_v4(1, source, &[]))).await;
    phone.admit(&server, "event-whip-untrusted").await;
    let c = phone.p.check(true);
    let _ = phone.try_send(&[c]).await;
    assert!(closed_within(&mut phone.stream, Duration::from_secs(2)).await);
    assert!(phone.p.stun_answers.is_empty());
    assert_eq!(get(&server.stats, |s| &s.untrusted), 1);
}

/// A LOCAL header is the proxy's own connection: closed, no frame read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_header_is_closed_without_reading_frames() {
    let server = start(Some("127.0.0.1".parse().expect("ip")), Limits::default()).await;
    let mut local = sapflow_core::proxy_protocol::SIGNATURE.to_vec();
    local.extend_from_slice(&[0x20, 0x00, 0, 0]);
    let mut phone = Phone::new(34, &server, Some(local)).await;
    phone.admit(&server, "event-whip-local").await;
    let c = phone.p.check(true);
    let _ = phone.try_send(&[c]).await;
    assert!(closed_within(&mut phone.stream, Duration::from_secs(2)).await);
    assert_eq!(get(&server.stats, |s| &s.local), 1);
    assert_eq!(get(&server.stats, |s| &s.frames_in), 0);
}

/// A header still partial at the timeout closes the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partial_header_times_out() {
    let limits = Limits {
        header_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let server = start(Some("127.0.0.1".parse().expect("ip")), limits).await;
    let source: SocketAddr = "203.0.113.9:40123".parse().expect("addr");
    let h = header_v4(1, source, &[]);
    let mut s = TcpStream::connect(server.tcp).await.expect("connect");
    s.write_all(&h[..20]).await.expect("write");
    let t = tokio::time::Instant::now();
    assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(250), "{took:?}");
    assert_eq!(get(&server.stats, |s| &s.header_timeout), 1);
}

/// The connection cap: past it, a connection is closed as it is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_connection_cap_a_connection_is_closed() {
    let limits = Limits {
        connections: 2,
        ..Limits::default()
    };
    let server = start(None, limits).await;
    let mut a = TcpStream::connect(server.tcp).await.expect("connect");
    let mut b = TcpStream::connect(server.tcp).await.expect("connect");
    let mut c = TcpStream::connect(server.tcp).await.expect("connect");
    assert!(closed_within(&mut c, Duration::from_secs(2)).await, "third");
    assert!(!closed_within(&mut a, Duration::from_millis(200)).await);
    assert!(!closed_within(&mut b, Duration::from_millis(200)).await);
    assert_eq!(get(&server.stats, |s| &s.over_total), 1);
    // A slot is free again once one closes.
    drop(a);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut d = TcpStream::connect(server.tcp).await.expect("connect");
    assert!(!closed_within(&mut d, Duration::from_millis(200)).await);
    assert_eq!(get(&server.stats, |s| &s.over_total), 1);
}

/// The per-address cap, keyed on the PROXY header's source: a second
/// connection from that address is closed, one from another is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_per_address_cap_a_connection_is_closed() {
    let limits = Limits {
        per_address: 1,
        ..Limits::default()
    };
    let server = start(Some("127.0.0.1".parse().expect("ip")), limits).await;
    let connect = |source: &str| {
        let source: SocketAddr = source.parse().expect("addr");
        let tcp = server.tcp;
        async move {
            let mut s = TcpStream::connect(tcp).await.expect("connect");
            s.write_all(&header_v4(1, source, &[]))
                .await
                .expect("write");
            s
        }
    };
    let mut a = connect("203.0.113.1:1000").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut b = connect("203.0.113.1:2000").await;
    let mut c = connect("203.0.113.2:1000").await;
    assert!(
        closed_within(&mut b, Duration::from_secs(2)).await,
        "same address"
    );
    assert!(!closed_within(&mut a, Duration::from_millis(200)).await);
    assert!(!closed_within(&mut c, Duration::from_millis(200)).await);
    assert_eq!(get(&server.stats, |s| &s.over_address), 1);
}

/// A frame longer than 1500 bytes ends the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_over_the_cap_ends_the_connection() {
    let server = start(None, Limits::default()).await;
    let mut s = TcpStream::connect(server.tcp).await.expect("connect");
    let mut b = 1501u16.to_be_bytes().to_vec();
    b.extend_from_slice(&[0x80; 1501]);
    s.write_all(&b).await.expect("write");
    assert!(closed_within(&mut s, Duration::from_secs(2)).await);
    assert_eq!(get(&server.stats, |s| &s.framing), 1);
    assert_eq!(get(&server.stats, |s| &s.frames_in), 0);
}

/// A connection that is never answered closes at the handshake timeout,
/// whatever it sends; one answered and then left quiet, at the idle one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unanswered_and_idle_connections_close() {
    let limits = Limits {
        handshake_timeout: Duration::from_millis(300),
        idle_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let server = start(None, limits).await;
    let mut s = TcpStream::connect(server.tcp).await.expect("connect");
    let mut junk = Vec::new();
    encode(&FRAMING, &[0x00, 0x01, 0, 0], &mut junk).expect("frame");
    s.write_all(&junk).await.expect("write");
    let t = tokio::time::Instant::now();
    assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    assert!(t.elapsed() >= Duration::from_millis(250));
    assert_eq!(get(&server.stats, |s| &s.handshake_timeout), 1);

    let mut phone = Phone::new(35, &server, None).await;
    phone.admit(&server, "event-whip-idle").await;
    // One check without USE-CANDIDATE is answered; then nothing more is
    // sent to the connection.
    let c = phone.p.check(false);
    phone.send(&[c]).await;
    phone.receive(Duration::from_millis(150)).await;
    assert_eq!(phone.p.stun_answers.len(), 1);
    assert!(closed_within(&mut phone.stream, Duration::from_secs(3)).await);
    assert_eq!(get(&server.stats, |s| &s.idle_timeout), 1);
}

/// A3.4 over ICE-TCP: a panic in a session on a TCP tuple ends only that
/// session; another, on its own connection to the same listener, keeps
/// publishing with nothing marked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_a_tcp_session_ends_only_that_session() {
    let server = serve_tcp(None, Limits::default(), true).await;
    let mut a = Phone::new(41, &server, None).await;
    a.admit(&server, "event-tcp-panic-a").await;
    a.connect().await;
    let mut b = Phone::new(42, &server, None).await;
    let b_location = b.admit(&server, "event-tcp-panic-b").await;
    b.connect().await;
    let b_stream = server
        .app
        .streams
        .get("event-tcp-panic-b")
        .expect("claimed");
    for _ in 0..10 {
        b.publish(1).await;
        if b_stream.has_content() {
            break;
        }
    }
    assert!(b_stream.has_content(), "B is live over TCP");
    let b_id = b_location
        .rsplit('/')
        .next()
        .and_then(SessionId::parse)
        .expect("B's id");
    let b_counts = server
        .whip
        .resources()
        .get(&b_id)
        .map(|r| r.losses.clone())
        .expect("B is live");
    let b_task = tokio::spawn(async move {
        b.publish(3).await;
        b
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.send(&[sapflow_whip::testing::PANIC_DATAGRAM.to_vec()])
        .await;
    let mut ended = false;
    for _ in 0..100 {
        if server.whip.ends.get("panicked") == 1 {
            ended = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ended, "A ended as panicked: {:?}", server.whip.ends);
    let after = b_stream.version();
    let _b = b_task.await.expect("B published throughout");
    assert!(b_stream.version() > after, "B's playlist kept growing");
    let marks: u64 = b_counts
        .fields()
        .iter()
        .filter(|(name, _)| *name != "reanchors")
        .map(|(_, n)| n)
        .sum();
    assert_eq!(marks, 0, "B lost nothing: {b_counts}");
}
