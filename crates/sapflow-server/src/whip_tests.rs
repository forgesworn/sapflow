//! WHIP in process: the signalling listener and the UDP owner on loopback,
//! driven by `sapflow-whip`'s scripted publisher (STUN client, the
//! `sapflow-dtls` test client and SRTP) over a real UDP socket.

use std::time::Duration;

use sapflow_whip::testing::{PPS, Publisher, StunClient, VIDEO_SSRC, idr_slice, server_identity};
use sapflow_whip::{Transmit, Tuple};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use super::*;
use crate::callback::Callbacks;
use crate::config::Config;
use crate::stream::Registry;

const GCM: u16 = 0x0007;
/// A Main profile SPS that `sapflow-hls` reads (its codec tests' own).
const SPS: &[u8] = &[
    0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08,
    0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
];
const TOKEN: &str = "publisher:whip-test-secret";
const ORIGIN: &str = "https://stage.example";

struct Server {
    app: Arc<App>,
    http: SocketAddr,
    udp: SocketAddr,
}

fn app(env: &[(&str, &str)]) -> Arc<App> {
    let mut all = vec![
        ("SAPFLOW_PUBLISH_USER", "publisher"),
        ("SAPFLOW_PUBLISH_PASS", "whip-test-secret"),
        ("SAPFLOW_WHIP_ADDR", "127.0.0.1:0"),
        ("SAPFLOW_WHIP_UDP_ADDR", "127.0.0.1:0"),
        ("SAPFLOW_WHIP_CANDIDATES", "udp 192.0.2.1:47300"),
        ("SAPFLOW_WHIP_PUBLIC_URL", "https://whip.example/"),
        ("SAPFLOW_WHIP_ALLOW_ORIGINS", ORIGIN),
        ("SAPFLOW_RECONNECT_GRACE", "30"),
    ];
    all.extend_from_slice(env);
    let config = Config::load(
        |key| {
            all.iter()
                .rev()
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

async fn start(env: &[(&str, &str)]) -> Server {
    let app = app(env);
    let sockets = Sockets {
        http: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        udp: UdpSocket::bind("127.0.0.1:0").await.expect("bind"),
    };
    let (http, udp) = (
        sockets.http.local_addr().expect("addr"),
        sockets.udp.local_addr().expect("addr"),
    );
    tokio::spawn(serve_with(app.clone(), sockets, server_identity()));
    Server { app, http, udp }
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One raw request; the connection closes after the answer.
async fn send(addr: SocketAddr, raw: &[u8]) -> Answer {
    let mut s = TcpStream::connect(addr).await.expect("connect");
    s.write_all(raw).await.expect("write");
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut out)).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Answer {
        status,
        headers,
        body: body.to_owned(),
    }
}

fn post(path: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut raw = format!("POST {path} HTTP/1.1\r\nhost: whip.example\r\n");
    for (k, v) in headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    raw.push_str(&format!("content-length: {}\r\n\r\n{body}", body.len()));
    raw.into_bytes()
}

fn sdp_post(path: &str, token: &str, body: &str) -> Vec<u8> {
    let auth = format!("Bearer {token}");
    post(
        path,
        &[
            ("authorization", &auth),
            ("content-type", "application/sdp"),
            ("origin", ORIGIN),
        ],
        body,
    )
}

fn line<'a>(sdp: &'a str, prefix: &str) -> &'a str {
    sdp.lines()
        .find_map(|l| l.strip_prefix(prefix))
        .expect("answer line")
        .trim()
}

/// A scripted publisher with its own UDP socket.
struct Phone {
    p: Publisher,
    socket: UdpSocket,
    server: SocketAddr,
}

impl Phone {
    async fn new(seed: u8, server: &Server) -> Self {
        let mut p = Publisher::new(seed, &[GCM]);
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        p.tuple = Tuple::udp(socket.local_addr().expect("addr"));
        Self {
            p,
            socket,
            server: server.udp,
        }
    }

    fn answered(&mut self, answer: &str) {
        self.p.client.expect_server(server_identity().fingerprint());
        self.p.stun = Some(StunClient::new(
            line(answer, "a=ice-ufrag:"),
            line(answer, "a=ice-pwd:"),
            &self.p.ufrag,
        ));
    }

    async fn send(&self, d: &[u8]) {
        self.socket.send_to(d, self.server).await.expect("send");
    }

    /// Takes what the server sent within `wait`, answering DTLS.
    async fn receive(&mut self, wait: Duration) {
        let mut buf = vec![0u8; 2048];
        let until = tokio::time::Instant::now() + wait;
        while let Ok(Ok((n, _))) =
            tokio::time::timeout_at(until, self.socket.recv_from(&mut buf)).await
        {
            let t = Transmit {
                tuple: self.p.tuple,
                bytes: buf[..n].to_vec(),
            };
            if classify(&t.bytes) == Kind::Rtcp && !self.p.has_keys() {
                continue;
            }
            for reply in self.p.take(t) {
                self.send(&reply).await;
            }
        }
    }

    async fn connect(&mut self) {
        let c = self.p.check(true);
        self.send(&c).await;
        for d in self.p.client.start() {
            self.send(&d).await;
        }
        for _ in 0..20 {
            self.receive(Duration::from_millis(100)).await;
            if self.p.has_keys() {
                break;
            }
        }
        assert!(self.p.has_keys(), "DTLS completes over UDP");
    }

    /// `seconds` of 25 fps video, a keyframe each second, and 20 ms audio,
    /// paced in real time.
    async fn publish(&mut self, seconds: u64) {
        for frame in 0..seconds * 25 {
            let packets = if frame % 25 == 0 {
                let idr = idr_slice(1200, 0x11);
                self.p.video(&[SPS, PPS, &idr], VIDEO_SSRC)
            } else {
                self.p.delta(300).0
            };
            for d in packets {
                self.send(&d).await;
            }
            for _ in 0..2 {
                let a = self.p.audio((frame % 251) as u8);
                self.send(&a).await;
            }
            self.receive(Duration::from_millis(40)).await;
        }
    }
}

async fn admit(server: &Server, phone: &mut Phone, event: &str) -> Answer {
    let a = send(
        server.http,
        &sdp_post(&format!("/whip/live/{event}"), TOKEN, &phone.p.offer()),
    )
    .await;
    if a.status == 201 {
        phone.answered(&a.body);
    }
    a
}

/// A3.1 in process: POST, then ICE, DTLS and SRTP over UDP, then media in
/// the stream; DELETE releases the session and a POST that follows at once
/// takes the held stream over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publisher_reaches_the_stream_through_post_and_udp() {
    let server = start(&[]).await;
    let mut phone = Phone::new(1, &server).await;
    let a = admit(&server, &mut phone, "event-whip-one").await;
    assert_eq!(a.status, 201, "{}", a.body);
    assert_eq!(a.header("content-type"), Some("application/sdp"));
    let location = a.header("location").expect("Location").to_owned();
    let id = location
        .strip_prefix("https://whip.example/whip/session/")
        .expect("an absolute Location");
    assert_eq!(id.len(), 32, "128 random bits in hex");
    assert_eq!(a.header("access-control-allow-origin"), Some(ORIGIN));
    assert_eq!(a.header("access-control-expose-headers"), Some("location"));
    assert!(a.body.contains("a=ice-lite"));
    phone.connect().await;
    phone.publish(3).await;
    let stream = server.app.streams.get("event-whip-one").expect("claimed");
    assert!(stream.has_content(), "media reached the playlist");

    let path = format!("/whip/session/{id}");
    let wrong = send(
        server.http,
        format!("DELETE {path} HTTP/1.1\r\nauthorization: Bearer publisher:other\r\n\r\n")
            .as_bytes(),
    )
    .await;
    assert_eq!(wrong.status, 401);
    let gone = send(
        server.http,
        format!("DELETE {path} HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n").as_bytes(),
    )
    .await;
    assert_eq!(gone.status, 200);
    let again = send(
        server.http,
        format!("DELETE {path} HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n").as_bytes(),
    )
    .await;
    assert_eq!(again.status, 404, "the session is gone");
    // The stream is held for a reconnect; a POST at once takes it over.
    let mut next = Phone::new(2, &server).await;
    let b = admit(&server, &mut next, "event-whip-one").await;
    assert_eq!(b.status, 201, "{}", b.body);
}

/// Every refusal the listener makes before admission, and 409 and 401
/// from admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_carry_their_statuses() {
    let server = start(&[]).await;
    let phone = Phone::new(3, &server).await;
    let offer = phone.p.offer();
    let status = |a: Answer| a.status;
    let at = server.http;
    assert_eq!(
        status(send(at, &sdp_post("/elsewhere", TOKEN, &offer)).await),
        404
    );
    let patch = send(
        at,
        b"PATCH /whip/session/00112233445566778899aabbccddeeff HTTP/1.1\r\ncontent-length: 0\r\n\r\n",
    )
    .await;
    assert_eq!(patch.status, 405);
    assert_eq!(patch.header("allow"), Some("DELETE, OPTIONS"));
    let no_token = post(
        "/whip/live/event-a",
        &[("content-type", "application/sdp")],
        &offer,
    );
    assert_eq!(status(send(at, &no_token).await), 401);
    let no_type = post(
        "/whip/live/event-a",
        &[("authorization", "Bearer publisher:whip-test-secret")],
        &offer,
    );
    assert_eq!(status(send(at, &no_type).await), 415);
    // Over 16 KiB by its content-length: refused before the body is sent.
    let big = format!(
        "POST /whip/live/event-a HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\ncontent-type: application/sdp\r\ncontent-length: 16385\r\n\r\n"
    );
    assert_eq!(status(send(at, big.as_bytes()).await), 413);
    assert_eq!(
        status(send(at, &sdp_post("/whip/live/event-a", TOKEN, "not sdp")).await),
        400
    );
    let no_h264 = offer
        .replace("H264/90000", "VP8/90000")
        .replace("opus/48000/2", "PCMU/8000");
    assert_eq!(
        status(send(at, &sdp_post("/whip/live/event-a", TOKEN, &no_h264)).await),
        422
    );
    assert_eq!(
        status(
            send(
                at,
                &sdp_post("/whip/live/event-a", "publisher:wrong", &offer)
            )
            .await
        ),
        401
    );
    assert_eq!(
        status(send(at, &sdp_post("/whip/live/not-a-stage", TOKEN, &offer)).await),
        404
    );
    assert_eq!(
        status(send(at, &sdp_post("/whip/live/event-a", TOKEN, &offer)).await),
        201
    );
    let second = Phone::new(4, &server).await;
    let busy = send(
        at,
        &sdp_post("/whip/live/event-a", TOKEN, &second.p.offer()),
    )
    .await;
    assert_eq!(busy.status, 409, "the path was claimed at POST");
    assert_eq!(busy.header("access-control-allow-origin"), Some(ORIGIN));
    let unknown = send(
        at,
        format!("DELETE /whip/session/00112233445566778899aabbccddeeff HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n").as_bytes(),
    )
    .await;
    assert_eq!(unknown.status, 404);
}

/// CORS headers go only to a configured origin, refusals included; a
/// preflight names the methods and headers, and says what POST takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_only_for_configured_origins() {
    let server = start(&[]).await;
    let preflight = |origin: &str| {
        format!(
            "OPTIONS /whip/live/event-a HTTP/1.1\r\norigin: {origin}\r\naccess-control-request-method: POST\r\n\r\n"
        )
    };
    let ok = send(server.http, preflight(ORIGIN).as_bytes()).await;
    assert_eq!(ok.status, 200);
    assert_eq!(ok.header("access-control-allow-origin"), Some(ORIGIN));
    assert_eq!(
        ok.header("access-control-allow-methods"),
        Some("POST, DELETE, OPTIONS")
    );
    assert_eq!(
        ok.header("access-control-allow-headers"),
        Some("authorization, content-type")
    );
    assert_eq!(ok.header("accept-post"), Some("application/sdp"));
    let other = send(server.http, preflight("https://other.example").as_bytes()).await;
    assert_eq!(other.status, 200);
    assert!(
        other
            .headers
            .iter()
            .all(|(k, _)| !k.to_ascii_lowercase().starts_with("access-control-")),
        "{:?}",
        other.headers
    );
    let refused = send(
        server.http,
        &post(
            "/whip/live/event-a",
            &[("content-type", "application/sdp"), ("origin", ORIGIN)],
            "v=0",
        ),
    )
    .await;
    assert_eq!(refused.status, 401);
    assert_eq!(refused.header("access-control-allow-origin"), Some(ORIGIN));
}

/// The router holds at most `SAPFLOW_MAX_PUBLISHERS` sessions; past that a
/// POST is refused 503 and its path released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_session_cap_a_post_gets_503() {
    let server = start(&[("SAPFLOW_MAX_PUBLISHERS", "1")]).await;
    let one = Phone::new(5, &server).await;
    let two = Phone::new(6, &server).await;
    let first = send(
        server.http,
        &sdp_post("/whip/live/event-a", TOKEN, &one.p.offer()),
    )
    .await;
    assert_eq!(first.status, 201);
    let full = send(
        server.http,
        &sdp_post("/whip/live/event-b", TOKEN, &two.p.offer()),
    )
    .await;
    assert_eq!(full.status, 503);
    // Its claim was released: nothing holds event-b.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(server.app.streams.get("event-b").is_none());
}

fn access_unit(idr: bool) -> Media {
    Media::Video {
        pts: 0,
        au: AccessUnit {
            data: vec![0, 0, 0, 1, if idr { 0x65 } else { 0x41 }],
            idr,
            sps: None,
            pps: None,
        },
    }
}

/// A full queue is loss for that publisher: the recording is marked, and
/// video waits for the next IDR rather than feeding undecodable frames.
#[test]
fn a_full_queue_skips_video_to_the_next_idr() {
    let (app, _scratch) = crate::test_support::app(false);
    let publication = crate::test_support::begin(&app);
    let (tx, mut rx) = mpsc::channel(1);
    let mut feed = Feed::new(tx, publication.loss_notice());
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert_eq!(feed.push(access_unit(false)), Ok(()), "full: dropped");
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert!(rx.try_recv().is_err(), "skipped until an IDR");
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if !au.idr));
    drop(rx);
    assert_eq!(feed.push(access_unit(true)), Err(Gone));
}
