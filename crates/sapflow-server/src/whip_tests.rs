//! WHIP in process: the signalling listener and the UDP owner on loopback,
//! driven by `sapflow-whip`'s scripted publisher (STUN client, the
//! `sapflow-dtls` test client and SRTP) over a real UDP socket.

use std::path::PathBuf;
use std::time::Duration;

use sapflow_whip::testing::{
    PPS, Publisher, Read, StunClient, VIDEO_SSRC, arm, idr_slice, server_identity,
};
use sapflow_whip::{Event, SessionConfig, Transmit, Tuple};
use std::time::Instant;
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
    whip: Arc<Whip>,
    http: SocketAddr,
    udp: SocketAddr,
}

impl Server {
    /// The per-path loss counts of the session at `location`.
    fn losses(&self, location: &str) -> String {
        let id = location
            .rsplit('/')
            .next()
            .and_then(SessionId::parse)
            .expect("a session Location");
        self.whip
            .resources()
            .get(&id)
            .map_or_else(|| "session gone".to_owned(), |r| r.losses.to_string())
    }
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
    start_with(env, |_| {}, None).await
}

/// A server whose sessions `tweak` adjusts, and whose callbacks go to
/// `callbacks` if given.
async fn start_with(
    env: &[(&str, &str)],
    tweak: impl FnOnce(&mut SessionConfig),
    callbacks: Option<Callbacks>,
) -> Server {
    serve_test(env, tweak, callbacks, false).await
}

/// As [`start_with`]; with `hooks`, the panic hooks are on (A3.4), as only
/// tests turn them on.
async fn serve_test(
    env: &[(&str, &str)],
    tweak: impl FnOnce(&mut SessionConfig),
    callbacks: Option<Callbacks>,
    hooks: bool,
) -> Server {
    let mut app = app(env);
    if let Some(callbacks) = callbacks {
        Arc::get_mut(&mut app).expect("unshared").callbacks = callbacks;
    }
    let sockets = Sockets {
        http: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        udp: UdpSocket::bind("127.0.0.1:0").await.expect("bind"),
        tcp: None,
    };
    let (http, udp) = (
        sockets.http.local_addr().expect("addr"),
        sockets.udp.local_addr().expect("addr"),
    );
    let config = app.config.whip.clone().expect("WHIP on");
    let mut session = session_config(&config);
    tweak(&mut session);
    let (whip, mut router, rx) =
        prepare_with(&app, server_identity(), config, session).expect("router");
    if hooks {
        router.enable_panic_hooks();
        whip.panic_hooks.store(true, Ordering::Relaxed);
    }
    tokio::spawn(run(app.clone(), whip.clone(), router, rx, sockets));
    Server {
        app,
        whip,
        http,
        udp,
    }
}

/// A server whose signalling listener has `limits`.
async fn start_limited(limits: Limits) -> Server {
    start_limited_with(&[], limits).await
}

/// As [`start_limited`], with `env` too.
async fn start_limited_with(env: &[(&str, &str)], limits: Limits) -> Server {
    let app = app(env);
    let sockets = Sockets {
        http: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        udp: UdpSocket::bind("127.0.0.1:0").await.expect("bind"),
        tcp: None,
    };
    let (http, udp) = (
        sockets.http.local_addr().expect("addr"),
        sockets.udp.local_addr().expect("addr"),
    );
    let config = app.config.whip.clone().expect("WHIP on");
    let session = session_config(&config);
    let (mut whip, router, rx) =
        prepare_with(&app, server_identity(), config, session).expect("router");
    Arc::get_mut(&mut whip).expect("unshared").limits = limits;
    tokio::spawn(run(app.clone(), whip.clone(), router, rx, sockets));
    Server {
        app,
        whip,
        http,
        udp,
    }
}

/// Whether `raw` is answered `200` within 5 s, trying again while the
/// server has yet to free a slot (it does once it sees a client's close).
async fn served_again(server: &Server, raw: &[u8]) -> bool {
    for _ in 0..100 {
        if send(server.http, raw).await.status == 200 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Whether the server closes `s` within `wait` without answering.
async fn closed_unanswered(s: &mut TcpStream, wait: Duration) -> bool {
    let mut b = [0u8; 64];
    matches!(
        tokio::time::timeout(wait, s.read(&mut b)).await,
        Ok(Ok(0) | Err(_))
    )
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
    /// The media clock: when its first 20 ms tick fell due, and the ticks
    /// taken since. Packets go out on it, as a real encoder's do, so time a
    /// step takes is not added to every later packet.
    clock: Option<(tokio::time::Instant, u64)>,
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
            clock: None,
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
        self.receive_until(tokio::time::Instant::now() + wait).await;
    }

    /// Waits for the media clock's next 20 ms tick, answering what the
    /// server sends meanwhile; returns the tick's number. A tick already
    /// past returns at once, so a stall delays packets without slowing the
    /// clock.
    async fn tick(&mut self) -> u64 {
        let (start, n) = *self.clock.get_or_insert((tokio::time::Instant::now(), 0));
        self.receive_until(start + Duration::from_millis(20 * n))
            .await;
        self.clock = Some((start, n + 1));
        n
    }

    /// Takes what the server sends until `until`, answering DTLS.
    async fn receive_until(&mut self, until: tokio::time::Instant) {
        let mut buf = vec![0u8; 2048];
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
    /// on the media clock.
    async fn publish(&mut self, seconds: u64) {
        for _ in 0..seconds * 50 {
            let tick = self.tick().await;
            if tick % 2 == 0 {
                let frame = tick / 2;
                let packets = if frame % 25 == 0 {
                    let idr = idr_slice(1200, 0x11);
                    self.p.video(&[SPS, PPS, &idr], VIDEO_SSRC)
                } else {
                    self.p.delta(300).0
                };
                for d in packets {
                    self.send(&d).await;
                }
            }
            let a = self.p.audio((tick % 251) as u8);
            self.send(&a).await;
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
    let stream = server.app.streams.get("event-whip-one").expect("claimed");
    // A second at a time, up to ten, until a part is out.
    for _ in 0..10 {
        phone.publish(1).await;
        if stream.has_content() {
            break;
        }
    }
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
    // Its claim is released: nothing holds event-b.
    for _ in 0..50 {
        if server.app.streams.get("event-b").is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(server.app.streams.get("event-b").is_none());
}

/// The test offer without its video section: audio-only Opus, the case
/// where the muxer cannot see a lost packet.
fn audio_only(offer: &str) -> String {
    let audio = offer.split("m=video").next().expect("audio first");
    audio.replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0")
}

/// Publishes audio-only Opus, 20 ms apart, leaving out the sequence
/// numbers in `skip`; returns the recording's incomplete reason and the
/// session's per-path loss counts, which say what set it.
async fn audio_run(
    server: &Server,
    seed: u8,
    event: &str,
    skip: &[u64],
) -> (Option<IncompleteReason>, String) {
    let mut phone = Phone::new(seed, server).await;
    let offer = audio_only(&phone.p.offer());
    let a = send(
        server.http,
        &sdp_post(&format!("/whip/live/{event}"), TOKEN, &offer),
    )
    .await;
    assert_eq!(a.status, 201, "{}", a.body);
    assert!(!a.body.contains("m=video"));
    phone.answered(&a.body);
    phone.connect().await;
    let stream = server.app.streams.get(event).expect("claimed");
    // At least 3 s, and on until a part is out (at most 10 s), on the media
    // clock: packet n goes at the start plus n times 20 ms.
    let started = std::time::Instant::now();
    let mut packets = 0u64;
    for n in 0..500u64 {
        phone.tick().await;
        let packet = phone.p.audio((n % 251) as u8);
        if !skip.contains(&n) {
            phone.send(&packet).await;
        }
        packets += 1;
        if n >= 150 && stream.has_content() {
            break;
        }
    }
    let wall = started.elapsed();
    assert!(stream.has_content(), "audio-only Opus reached the playlist");
    let reason = {
        let session = stream.session();
        let recorder = session.recorder.as_ref().expect("recording");
        recorder.status().reason()
    };
    let location = a.header("location").expect("Location");
    let playlist = stream
        .lock()
        .media
        .as_ref()
        .map(|m| String::from_utf8_lossy(m).into_owned())
        .unwrap_or_default();
    let shown = format!(
        "{}; {packets} packets ({} ms of RTP time) sent over {} ms; playlist: {} EXT-X-DISCONTINUITY, {} EXT-X-GAP",
        server.losses(location),
        packets * 20,
        wall.as_millis(),
        playlist.matches("#EXT-X-DISCONTINUITY\n").count(),
        playlist.matches("#EXT-X-GAP").count()
    );
    (reason, shown)
}

async fn udp_server() -> (Server, PathBuf) {
    udp_server_with(false).await
}

/// As [`udp_server`], with the panic hooks on if `hooks`.
async fn udp_server_with(hooks: bool) -> (Server, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "whip-loss-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).expect("owned scratch");
    let dir = root.to_str().expect("utf-8 path").to_owned();
    let env = [
        ("SAPFLOW_RECORDINGS", dir.as_str()),
        ("SAPFLOW_RECORD_MIN_FREE_GB", "0"),
    ];
    let server = serve_test(&env, |_| {}, None, hooks).await;
    (server, root)
}

static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Over real UDP and real time, a run that loses nothing leaves the
/// recording complete. If it is ever marked, the message says which path
/// marked it: a receiver path, a drop, the queue, or the muxer's own
/// report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_run_over_udp_stays_complete() {
    let (server, root) = udp_server().await;
    let (reason, losses) = audio_run(&server, 7, "event-whip-clean", &[]).await;
    let _ = std::fs::remove_dir_all(&root);
    println!("clean UDP run: {losses}");
    assert_eq!(reason, None, "marked by: {losses}");
}

/// Over real UDP: audio-only Opus with one packet left out marks the
/// recording, from the receiver.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_packet_marks_the_recording_from_the_receiver() {
    let (server, root) = udp_server().await;
    let (reason, losses) = audio_run(&server, 8, "event-whip-lossy", &[80]).await;
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(reason, Some(IncompleteReason::MediaLoss), "{losses}");
    assert!(losses.contains("lost_packets=1"), "{losses}");
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

/// What the ingest task does with the marks the feed queued: applies them
/// in order, then any the feed held when it went. Other items are dropped.
fn apply_marks(rx: &mut mpsc::Receiver<Item>, publication: &Publication, counts: &LossCounts) {
    let loss = publication.loss_notice();
    while let Ok(item) = rx.try_recv() {
        if let Item::Marks(marks) = item {
            marks.into_iter().for_each(|r| loss.mark_incomplete(r));
        }
    }
    std::mem::take(&mut *counts.trailing.lock().expect("trailing"))
        .into_iter()
        .for_each(|r| loss.mark_incomplete(r));
}

fn status(publication: &Publication) -> Arc<crate::recorder::RecordingStatus> {
    publication
        .stream
        .session()
        .recorder
        .as_ref()
        .expect("recording started")
        .status()
}

/// A full queue is loss for that publisher: the recording is marked, a
/// keyframe is asked for, and video waits for the next IDR rather than
/// feeding undecodable frames.
#[tokio::test]
async fn a_full_queue_skips_video_to_the_next_idr() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let status = status(&publication);
    let (tx, mut rx) = mpsc::channel(1);
    let mut feed = Feed::new(tx);
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert_eq!(feed.push(access_unit(false)), Ok(()), "full: dropped");
    assert!(feed.take_keyframe_request(), "a keyframe is asked for");
    assert!(!feed.take_keyframe_request(), "once");
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
    // The feed never marks the recording itself: the full queue's mark
    // waited for room, and goes ahead of whatever comes next.
    assert_eq!(status.reason(), None);
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert!(
        matches!(rx.try_recv(), Ok(Item::Marks(m)) if m == [IncompleteReason::IngestQueueFull])
    );
    assert!(rx.try_recv().is_err(), "skipped until an IDR");
    assert!(!feed.take_keyframe_request(), "already waiting");
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if !au.idr));
    drop(rx);
    assert_eq!(feed.push(access_unit(true)), Err(Gone));
}

/// The queue is bounded in bytes as well as items: past the bound is a
/// full queue, and bytes the ingest task takes make room again.
#[tokio::test]
async fn the_queue_is_bounded_in_bytes() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let status = status(&publication);
    let (tx, mut rx) = mpsc::channel(64);
    let mut feed = Feed::bounded(tx, 12);
    let queued = feed.queued();
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert_eq!(queued.load(Ordering::Acquire), 10);
    // A third would be 15 bytes: dropped as a full queue, and marked in
    // the queue behind the two (a mark holds no media bytes).
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert!(feed.take_keyframe_request());
    assert_eq!(queued.load(Ordering::Acquire), 10);
    // The ingest task takes all three; the next IDR fits.
    let mut marks = Vec::new();
    while let Ok(item) = rx.try_recv() {
        queued.fetch_sub(item.size(), Ordering::AcqRel);
        if let Item::Marks(m) = item {
            marks.extend(m);
        }
    }
    assert_eq!(marks, [IncompleteReason::IngestQueueFull]);
    assert_eq!(status.reason(), None, "until the ingest applies it");
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
}

/// Two SPS at different sizes and their PPS, from `sapflow-hls`'s
/// `resolution-change.ts` fixture.
const SPS_WIDE: &[u8] = &[
    0x67, 0x42, 0xc0, 0x0b, 0xda, 0x0a, 0x37, 0xe4, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
    0x00, 0x03, 0x00, 0xf2, 0x3c, 0x50, 0xaa, 0x80,
];
const SPS_NARROW: &[u8] = &[
    0x67, 0x42, 0xc0, 0x0a, 0xda, 0x08, 0x2f, 0xe5, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
    0x00, 0x03, 0x00, 0xf2, 0x3c, 0x48, 0x9a, 0x80,
];
const PPS_FIXTURE: &[u8] = &[0x68, 0xce, 0x0f, 0xc8];

/// Frame `n` of 25 fps video, a keyframe each second carrying `sps`.
fn frame(n: u64, sps: &[u8]) -> Media {
    let idr = n.is_multiple_of(25);
    let nal: &[u8] = if idr {
        &[0x65, 0x88, 0x84]
    } else {
        &[0x41, 0x9a, 0x02]
    };
    let mut data = (nal.len() as u32).to_be_bytes().to_vec();
    data.extend_from_slice(nal);
    Media::Video {
        pts: n * 3600,
        au: AccessUnit {
            data,
            idr,
            sps: idr.then(|| sps.to_vec()),
            pps: idr.then(|| PPS_FIXTURE.to_vec()),
        },
    }
}

/// A mark the owner raises while the ingest queue still holds the item
/// that rolls the recording (a codec epoch change, #77) lands on the new
/// recording, the one the loss is in, and not on the old one.
#[tokio::test]
async fn a_mark_raised_behind_a_queued_roll_lands_on_the_new_recording() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let mut publication = fixture.begin_kind(SourceKind::Es);
    publication
        .push(|source, now| configure(source, VIDEO_ONLY, now))
        .expect("setup");
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let mut feed = Feed::new(tx);
    let (queued, counts) = (feed.queued(), feed.counts());
    let mut last_audio = None;
    // Two seconds at one size, taken by the ingest.
    for n in 0..50 {
        assert_eq!(feed.push(frame(n, SPS_WIDE)), Ok(()));
    }
    while let Ok(item) = rx.try_recv() {
        assert!(take(
            &mut publication,
            (&queued, &counts),
            VIDEO_ONLY,
            &mut last_audio,
            item
        ));
    }
    let first = status(&publication);
    // Then a keyframe at another size, still in the queue when loss after
    // it is reported, and more frames.
    for n in 50..60 {
        assert_eq!(feed.push(frame(n, SPS_NARROW)), Ok(()));
    }
    assert_eq!(
        feed.push(Media::Loss {
            path: LossPath::Lost,
            packets: 2
        }),
        Ok(())
    );
    for n in 60..100 {
        assert_eq!(feed.push(frame(n, SPS_NARROW)), Ok(()));
    }
    while let Ok(item) = rx.try_recv() {
        assert!(take(
            &mut publication,
            (&queued, &counts),
            VIDEO_ONLY,
            &mut last_audio,
            item
        ));
    }
    let second = status(&publication);
    assert!(
        !Arc::ptr_eq(&first, &second),
        "the keyframe rolled the recording"
    );
    assert_eq!(
        second.reason(),
        Some(IncompleteReason::MediaLoss),
        "the new recording"
    );
    assert_eq!(
        first.reason(),
        None,
        "the loss was not in the old recording"
    );
}

/// An in-process session run with the scripted publisher, collecting the
/// session's events, on a clock the test moves.
struct Run {
    p: Publisher,
    s: Session,
    now: Instant,
    events: Vec<Event>,
}

impl Run {
    fn new(seed: u8, offer: fn(&str) -> String, config: SessionConfig) -> Self {
        let t0 = Instant::now();
        let mut p = Publisher::new(seed, &[GCM]);
        let a = sapflow_whip::testing::accept(&offer(&p.offer()), config, t0, seed);
        p.answered(&a);
        let mut s = a.session;
        p.connect(&mut s, t0);
        let mut run = Self {
            p,
            s,
            now: t0,
            events: Vec::new(),
        };
        run.drain();
        run
    }

    fn drain(&mut self) {
        while self.s.poll_transmit().is_some() {}
        while let Some(e) = self.s.poll_event() {
            self.events.push(e);
        }
    }

    fn send(&mut self, d: &[u8]) {
        let tuple = self.p.tuple;
        self.s.handle_datagram(self.now, tuple, d);
        self.drain();
    }

    fn wait(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        self.s.handle_timeout(self.now);
        self.drain();
    }

    /// `n` Opus packets 20 ms apart, leaving out those numbered in `skip`.
    fn audio(&mut self, n: u64, skip: &[u64]) {
        for i in 0..n {
            let a = self.p.audio(i as u8);
            if !skip.contains(&i) {
                self.send(&a);
            }
            self.wait(20);
        }
    }

    /// `n` video frames 40 ms apart, a keyframe when `idr(i)`.
    fn video(&mut self, n: u64, idr: fn(u64) -> bool) {
        for i in 0..n {
            let packets = if idr(i) {
                let slice = idr_slice(1200, 0x11);
                self.p.video(&[SPS, PPS, &slice], VIDEO_SSRC)
            } else {
                self.p.delta(300).0
            };
            for d in packets {
                self.send(&d);
            }
            self.wait(40);
        }
    }

    fn has(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.events.iter().any(f)
    }

    fn lost(&self) -> bool {
        self.has(|e| matches!(e, Event::Media(sapflow_rtp::Event::Lost { .. })))
    }
}

fn video_only(offer: &str) -> String {
    let head = offer.split("m=audio").next().expect("session part");
    let video = offer.split("m=video").nth(1).expect("a video section");
    format!("{head}m=video{video}").replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 1")
}

const AUDIO_ONLY: Setup = Setup {
    video: false,
    audio: true,
    panic_hooks: false,
};

/// What a recording says after a session's events: through a `Feed`, as
/// the server runs them, or, with `feed` false, with only the access units
/// and audio given to the ingest and no loss marked by the server, which
/// leaves the muxer's own view.
async fn recorded(events: Vec<Event>, setup: Setup, feed: bool) -> Option<IncompleteReason> {
    recorded_in_full(events, setup, feed).await.reason
}

/// A recording's state after a run, with what the server counted.
struct Recorded {
    reason: Option<IncompleteReason>,
    /// `EXT-X-DISCONTINUITY` tags in the media playlist.
    discontinuities: usize,
    counts: Arc<LossCounts>,
}

async fn recorded_in_full(events: Vec<Event>, setup: Setup, feed: bool) -> Recorded {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let mut publication = fixture.begin_kind(SourceKind::Es);
    publication
        .push(|source, now| configure(source, setup, now))
        .expect("setup");
    let status = status(&publication);
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let mut server = Feed::new(tx);
    for e in events {
        let mut items = Vec::new();
        match e {
            Event::Media(m) if feed => server.offer(m).expect("ingest"),
            Event::MediaDropped { count } if feed => {
                server
                    .push(Media::Loss {
                        path: LossPath::Dropped,
                        packets: count,
                    })
                    .expect("ingest");
            }
            Event::Media(m) => match media(m) {
                Media::Video { pts, au } => items.push(Item::Video {
                    pts,
                    au,
                    received: 0,
                }),
                Media::Audio { pts, packet, .. } => items.push(Item::Audio {
                    pts,
                    packet,
                    received: 0,
                    trace: AudioTrace::default(),
                }),
                Media::Loss { .. } | Media::Reanchor | Media::Other(_) => {}
            },
            _ => {}
        }
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }
        for item in items {
            if let Item::Marks(marks) = item {
                let loss = publication.loss_notice();
                marks.into_iter().for_each(|r| loss.mark_incomplete(r));
                continue;
            }
            publication
                .push(|source, now| match item {
                    Item::Reanchor => reanchor(source, setup, now),
                    item => feed_item(source, item, now),
                })
                .expect("ingest");
        }
    }
    let playlist = publication
        .stream
        .lock()
        .media
        .as_ref()
        .map(|m| String::from_utf8_lossy(m).into_owned())
        .unwrap_or_default();
    Recorded {
        reason: status.reason(),
        discontinuities: playlist.matches("#EXT-X-DISCONTINUITY\n").count(),
        counts: server.counts(),
    }
}

fn arrival_reanchors(run: &Run) -> usize {
    run.events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::Media(sapflow_rtp::Event::Discontinuity {
                    cause: Discontinuity::Arrival { .. },
                    ..
                })
            )
        })
        .count()
}

/// Audio-only Opus whose delivery delay ramps past 500 ms with nothing
/// lost: each packet 24 ms after the last, 20 ms of RTP apart, for 4 s.
fn delay_ramp() -> Run {
    let mut run = Run::new(48, audio_only, SessionConfig::default());
    for i in 0..200u64 {
        let a = run.p.audio(i as u8);
        run.send(&a);
        run.wait(24);
    }
    run
}

/// A timeline re-anchor with no loss is not a mark: the server ends the
/// muxer's run deliberately before the jumped packet, so the recording
/// stays complete, the playlist gets one discontinuity (keeping live
/// latency bounded), and the re-anchor is counted. The muxer alone, given
/// the same media, would have marked it.
#[tokio::test]
async fn a_delay_ramp_with_no_loss_reanchors_without_marking() {
    let run = delay_ramp();
    assert_eq!(arrival_reanchors(&run), 1, "one re-anchor on arrival time");
    assert!(!run.lost());
    let r = recorded_in_full(run.events, AUDIO_ONLY, true).await;
    assert_eq!(r.reason, None, "{}", r.counts);
    assert_eq!(r.counts.reanchors.load(Ordering::Relaxed), 1);
    assert_eq!(r.discontinuities, 1, "one HLS discontinuity");
    let alone = recorded(delay_ramp().events, AUDIO_ONLY, false).await;
    assert_eq!(
        alone,
        Some(IncompleteReason::MediaLoss),
        "the muxer alone marks it"
    );
}

/// An outage that loses packets and then re-anchors is marked as before:
/// the lost packets mark it, whatever the re-anchor does.
#[tokio::test]
async fn an_outage_that_loses_packets_and_reanchors_is_marked() {
    let mut run = Run::new(49, audio_only, SessionConfig::default());
    run.audio(100, &[]);
    // Ten packets lost, and the next arrives 900 ms after the last one,
    // 220 ms of RTP time later.
    for _ in 0..10 {
        let _lost = run.p.audio(0);
    }
    run.wait(880);
    run.audio(100, &[]);
    assert!(run.lost(), "the receiver reports the gap");
    assert_eq!(arrival_reanchors(&run), 1, "and re-anchors after it");
    let r = recorded_in_full(run.events, AUDIO_ONLY, true).await;
    assert_eq!(r.reason, Some(IncompleteReason::MediaLoss), "{}", r.counts);
    assert_eq!(r.counts.lost_packets.load(Ordering::Relaxed), 10);
    assert_eq!(r.counts.reanchors.load(Ordering::Relaxed), 1);
}

/// The lost packet: a sequence gap the receiver declares lost after the
/// jitter wait, and fills with silence. The recording is marked from the
/// receiver. The muxer alone sees nothing (the committed form of taking the
/// receiver's mark out), and a run without loss is not marked.
#[tokio::test]
async fn a_lost_packet_is_marked_from_the_receiver_not_the_muxer() {
    let run = |seed, skip: &'static [u64]| {
        let mut r = Run::new(seed, audio_only, SessionConfig::default());
        r.audio(150, skip);
        r
    };
    let clean = run(40, &[]);
    assert!(!clean.lost());
    assert_eq!(recorded(clean.events, AUDIO_ONLY, true).await, None);
    let lossy = run(41, &[80]);
    assert!(lossy.lost());
    assert_eq!(
        recorded(lossy.events, AUDIO_ONLY, true).await,
        Some(IncompleteReason::MediaLoss)
    );
    let lossy = run(41, &[80]);
    assert_eq!(recorded(lossy.events, AUDIO_ONLY, false).await, None);
}

/// Path (a): a sequence jump of more than 3000 that the next packet
/// confirms is a restart, reported as a discontinuity and with no `Lost`;
/// the frames until the next IDR are skipped.
fn sequence_restart() -> Run {
    let mut run = Run::new(42, video_only, SessionConfig::default());
    run.video(50, |i| i % 25 == 0);
    run.p.video_seq = run.p.video_seq.wrapping_add(30_000);
    run.video(50, |i| i % 25 == 10);
    run
}

/// Path (b): a refused Opus payload sets the gap flag, and the next packet
/// is filled with silence for loss, with no sequence gap and no `Lost`.
fn refused_opus() -> Run {
    let mut run = Run::new(43, audio_only, SessionConfig::default());
    run.audio(100, &[]);
    let bad = run.p.audio_payload(vec![0xff]);
    run.send(&bad);
    run.wait(20);
    run.audio(20, &[]);
    run
}

/// Path (c): the session ends while packets wait behind a gap, inside the
/// jitter wait. They never come out and no `Lost` is reported.
fn held_at_the_end() -> Run {
    let mut run = Run::new(44, audio_only, SessionConfig::default());
    run.audio(100, &[]);
    let _missing = run.p.audio(0);
    for i in 0..3 {
        let a = run.p.audio(i);
        run.send(&a);
    }
    run.s.close(run.now);
    run.drain();
    run
}

/// Another path: events the receiver's queue had no room for are dropped
/// and counted there. A small queue makes one gap's release overflow it.
fn dropped_for_room() -> Run {
    let mut config = SessionConfig::default();
    config.receiver.max_events = 4;
    let mut run = Run::new(45, audio_only, config);
    run.audio(100, &[]);
    let missing = run.p.audio(0);
    for i in 0..10 {
        let a = run.p.audio(i);
        run.send(&a);
    }
    run.send(&missing);
    run
}

const VIDEO_ONLY: Setup = Setup {
    video: true,
    audio: false,
    panic_hooks: false,
};

/// Each path marks the recording through the server, and the muxer alone,
/// given the same access units and audio, does not.
async fn marked_only_by_the_server(path: fn() -> Run, setup: Setup) -> Run {
    let alone = path();
    assert_eq!(
        recorded(alone.events, setup, false).await,
        None,
        "the muxer alone"
    );
    let run = path();
    let check = path();
    assert_eq!(
        recorded(run.events, setup, true).await,
        Some(IncompleteReason::MediaLoss)
    );
    check
}

#[tokio::test]
async fn a_sequence_restart_marks_the_recording() {
    let run = marked_only_by_the_server(sequence_restart, VIDEO_ONLY).await;
    assert!(run.has(|e| matches!(
        e,
        Event::Media(sapflow_rtp::Event::Discontinuity {
            cause: Discontinuity::Sequence,
            ..
        })
    )));
    assert!(!run.lost(), "no Lost on this path");
}

#[tokio::test]
async fn a_refused_opus_payload_marks_the_recording() {
    let run = marked_only_by_the_server(refused_opus, AUDIO_ONLY).await;
    assert!(run.has(|e| matches!(e, Event::Media(sapflow_rtp::Event::Refused { .. }))));
    assert!(run.has(|e| matches!(
        e,
        Event::Media(sapflow_rtp::Event::Audio { frame, .. }) if frame.kind == FrameKind::Loss
    )));
    assert!(!run.lost(), "no Lost on this path");
}

#[tokio::test]
async fn media_held_when_the_session_ends_marks_the_recording() {
    let run = marked_only_by_the_server(held_at_the_end, AUDIO_ONLY).await;
    assert!(run.has(|e| matches!(e, Event::MediaDropped { count } if *count >= 3)));
    assert!(matches!(run.events.last(), Some(Event::Ended(_))));
    assert!(!run.lost(), "no Lost on this path");
}

#[tokio::test]
async fn receiver_events_dropped_for_room_mark_the_recording() {
    let run = marked_only_by_the_server(dropped_for_room, AUDIO_ONLY).await;
    assert!(run.has(|e| matches!(e, Event::MediaDropped { .. })));
}

/// A run that loses nothing and ends with `close` leaves the recording
/// complete: no `MediaDropped`, and nothing marked.
#[tokio::test]
async fn a_clean_close_leaves_the_recording_complete() {
    let mut run = Run::new(46, audio_only, SessionConfig::default());
    run.audio(150, &[]);
    run.s.close(run.now);
    run.drain();
    assert!(!run.has(|e| matches!(e, Event::MediaDropped { .. })));
    assert!(matches!(
        run.events.last(),
        Some(Event::Ended(sapflow_whip::EndReason::Closed))
    ));
    assert_eq!(recorded(run.events, AUDIO_ONLY, true).await, None);
}

/// A session that ends without an orderly close (consent or media timing
/// out, DTLS failing, keys spent) marks its recording, since its last
/// media may be missing with no later packet to show a gap; `DELETE`, our
/// close and the publisher's `close_notify` do not. Loss just before an
/// orderly end cannot be seen.
#[tokio::test]
async fn an_abnormal_end_marks_the_recording() {
    use sapflow_whip::EndReason as R;
    for (reason, mark) in [
        (R::ConsentExpired, Some(IncompleteReason::TransportTimeout)),
        (R::MediaTimeout, Some(IncompleteReason::TransportTimeout)),
        (
            R::Dtls(sapflow_dtls::Error::Timeout),
            Some(IncompleteReason::Protocol),
        ),
        (R::KeysExhausted, Some(IncompleteReason::Protocol)),
        (R::Panicked, Some(IncompleteReason::IngestError)),
        (R::HandshakeTimeout, None),
        (R::Closed, None),
        (R::PeerClosed, None),
    ] {
        let fixture = crate::publish::loss_tests::Fixture::new();
        let publication = fixture.begin();
        let status = status(&publication);
        let (tx, mut rx) = mpsc::channel(4);
        let mut feed = Feed::new(tx);
        let counts = feed.counts();
        feed.ended(reason);
        apply_marks(&mut rx, &publication, &counts);
        assert_eq!(status.reason(), mark, "{reason:?}");
    }
}

/// A mark held for want of room is never overtaken. Between `push`
/// finding no room for the held marks and queueing its item, the ingest
/// may free a slot: the item is then refused as the queue being full, so a
/// keyframe at a new size cannot roll the recording ahead of the mark and
/// leave the file it belongs to complete.
#[tokio::test]
async fn an_item_never_goes_ahead_of_a_held_mark() {
    let (tx, mut rx) = mpsc::channel(1);
    let mut feed = Feed::new(tx);
    assert_eq!(feed.push(access_unit(true)), Ok(()), "fills the queue");
    assert_eq!(
        feed.push(access_unit(false)),
        Ok(()),
        "refused: the mark is held"
    );
    // As at the top of the next push: still no room, so still held.
    assert_eq!(feed.queue_marks(), Ok(()));
    // The ingest takes the queued frame now, before the item goes in.
    assert!(matches!(rx.try_recv(), Ok(Item::Video { .. })));
    let roll = Item::Video {
        pts: 3600,
        au: AccessUnit {
            data: vec![0, 0, 0, 3, 0x65, 0x88, 0x84],
            idr: true,
            sps: Some(SPS_NARROW.to_vec()),
            pps: Some(PPS_FIXTURE.to_vec()),
        },
        received: 0,
    };
    assert_eq!(feed.enqueue(roll), Ok(()));
    // Refused as the queue being full: the held mark took the freed slot,
    // and the keyframe did not get ahead of it.
    assert!(
        matches!(rx.try_recv(), Ok(Item::Marks(m)) if m == [IncompleteReason::IngestQueueFull]),
        "the mark first"
    );
    assert!(rx.try_recv().is_err(), "the keyframe was refused");
    // Video waits for the next keyframe, which goes in behind the mark.
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
}

/// The real ingest task, as it ends: marks the feed could not queue are
/// applied after everything queued before them, and a pending place is
/// given back, as for a `POST` cancelled before its session reached the
/// router.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ingest_applies_held_marks_and_gives_back_the_place_as_it_ends() {
    let (server, root) = udp_server_with(false).await;
    let req = PublishRequest {
        path: StreamPath::parse("live/event-tail").expect("path"),
        user: Some("publisher".to_owned()),
        pass: Some("whip-test-secret".to_owned()),
        connection_attempt_id: None,
    };
    let from: IpAddr = "127.0.0.1".parse().expect("loopback");
    let publication = Publication::begin(&server.app, &req, SourceKind::Es, from).expect("claimed");
    let recording = status(&publication);
    let id = SessionId::generate_seeded([70; 32]).expect("id");
    PendingSlot::take(&server.whip, "198.51.100.7".parse().expect("address"))
        .expect("a place")
        .attach(id);
    assert_eq!(server.whip.pending().total, 1);
    let (tx, rx) = mpsc::channel(1);
    let mut feed = Feed::new(tx);
    let (queued, counts) = (feed.queued(), feed.counts());
    assert_eq!(feed.push(access_unit(true)), Ok(()), "fills the queue");
    // No room for its mark: the feed holds it, and keeps it as it goes.
    feed.ended(sapflow_whip::EndReason::MediaTimeout);
    drop(feed);
    let (released, _) = watch::channel(false);
    ingest(
        server.whip.clone(),
        id,
        publication,
        (rx, queued, counts),
        released,
        VIDEO_ONLY,
    )
    .await;
    assert_eq!(recording.reason(), Some(IncompleteReason::TransportTimeout));
    assert_eq!(server.whip.pending().total, 0, "the place is given back");
    let _ = std::fs::remove_dir_all(&root);
}

/// A mark that finds the queue full at the end, as an abnormal end can,
/// is kept for the ingest task, which applies it after everything queued
/// before it.
#[tokio::test]
async fn a_mark_with_no_room_at_the_end_still_reaches_the_recording() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let status = status(&publication);
    let (tx, mut rx) = mpsc::channel(1);
    let mut feed = Feed::new(tx);
    let counts = feed.counts();
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    feed.ended(sapflow_whip::EndReason::MediaTimeout);
    drop(feed);
    assert_eq!(
        *counts.trailing.lock().expect("trailing"),
        [IncompleteReason::TransportTimeout]
    );
    apply_marks(&mut rx, &publication, &counts);
    assert_eq!(status.reason(), Some(IncompleteReason::TransportTimeout));
}

/// Each `Lost` is counted and marks the recording incomplete; a dropped
/// frame marks it too.
#[tokio::test]
async fn receiver_loss_is_counted_and_marks_the_recording() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let (tx, mut rx) = mpsc::channel(4);
    let mut feed = Feed::new(tx);
    let counts = feed.counts();
    let status = publication
        .stream
        .session()
        .recorder
        .as_ref()
        .expect("recording started")
        .status();
    assert_eq!(status.reason(), None);
    assert_eq!(
        feed.push(Media::Loss {
            path: LossPath::Lost,
            packets: 3
        }),
        Ok(())
    );
    assert_eq!(feed.push(loss(LossPath::IncompleteFrame)), Ok(()));
    assert_eq!(feed.lost(), 3);
    apply_marks(&mut rx, &publication, &counts);
    assert_eq!(status.reason(), Some(IncompleteReason::MediaLoss));
}

/// Only SRTP and SRTCP the session authenticated count against the
/// bitrate cap: bytes spoofed from the publisher's tuple do not.
#[tokio::test]
async fn only_authenticated_media_counts_against_the_cap() {
    let t0 = Instant::now();
    let mut router = Router::new(RouterConfig::default()).expect("router");
    let mut p = Publisher::new(30, &[GCM]);
    let a = sapflow_whip::testing::accept(&p.offer(), SessionConfig::default(), t0, 30);
    p.answered(&a);
    let id = SessionId::generate_seeded([30; 32]).expect("id");
    router.insert(id, a.session).expect("room");
    router.handle_datagram(t0, p.tuple, &p.check(true));
    let mut pending = p.client.start();
    for _ in 0..16 {
        for d in std::mem::take(&mut pending) {
            router.handle_datagram(t0, p.tuple, &d);
        }
        while let Some(t) = router.poll_transmit() {
            if classify(&t.bytes) != Kind::Rtcp {
                pending.extend(p.take(t));
            }
        }
        if p.has_keys() && pending.is_empty() {
            break;
        }
    }
    assert!(p.has_keys());
    let (tx, _rx) = mpsc::channel(4);
    let mut feeds = HashMap::from([(id, Feed::new(tx))]);
    let junk = [0x80, 0x6f, 0, 9, 0, 0, 0, 1, 0, 0, 0x10, 0x01, 1, 2, 3, 4];
    receive(&mut router, &mut feeds, p.tuple, &junk);
    assert_eq!(feeds[&id].received, 0, "spoofed bytes count");
    let audio = p.audio(1);
    receive(&mut router, &mut feeds, p.tuple, &audio);
    assert_eq!(feeds[&id].received, audio.len());
}

/// Callbacks for `live/event-<event>` into a queue a test reads.
fn callback_queue(event: &str) -> (Callbacks, mpsc::Receiver<crate::callback::Queued>) {
    crate::callback::test_queue(crate::publish_observer::fixture_scope(event, 1, 1, 1), 64)
}

/// The callback events queued within `wait`, by name.
async fn callback_events(
    rx: &mut mpsc::Receiver<crate::callback::Queued>,
    wait: Duration,
) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok(Some(q)) = tokio::time::timeout(wait, rx.recv()).await {
        let body: serde_json::Value = serde_json::from_slice(&q.body).expect("JSON");
        names.push(body["event"].as_str().unwrap_or_default().to_owned());
    }
    names
}

fn delete(location: &str) -> Vec<u8> {
    let path = location
        .strip_prefix("https://whip.example")
        .expect("our Location");
    format!("DELETE {path} HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n").into_bytes()
}

/// A phone admitted to `event`, connected, publishing until a part is out.
async fn live(server: &Server, seed: u8, event: &str) -> (Phone, String) {
    let mut phone = Phone::new(seed, server).await;
    let a = admit(server, &mut phone, event).await;
    assert_eq!(a.status, 201, "{}", a.body);
    let location = a.header("location").expect("Location").to_owned();
    phone.connect().await;
    let stream = server.app.streams.get(event).expect("claimed");
    for _ in 0..10 {
        phone.publish(1).await;
        if stream.has_content() {
            break;
        }
    }
    assert!(stream.has_content());
    (phone, location)
}

/// Polls `f` every 50 ms for up to 5 s.
async fn eventually(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

/// A3.2: a DELETE after media sends `unpublish` at once and holds the
/// stream; a POST within the grace continues the same stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_after_media_unpublishes_and_a_post_in_the_grace_continues() {
    let (callbacks, mut rx) = callback_queue("grace");
    let server = start_with(&[], |_| {}, Some(callbacks)).await;
    let (_phone, location) = live(&server, 30, "event-grace").await;
    assert_eq!(
        callback_events(&mut rx, Duration::from_millis(200)).await,
        ["publish"]
    );
    let held = server.app.streams.get("event-grace").expect("live");
    assert_eq!(send(server.http, &delete(&location)).await.status, 200);
    assert_eq!(
        callback_events(&mut rx, Duration::from_millis(200)).await,
        ["unpublish"],
        "at once"
    );
    let still = server
        .app
        .streams
        .get("event-grace")
        .expect("held for the grace");
    assert!(Arc::ptr_eq(&held, &still) && still.has_content());
    assert_eq!(server.whip.ends.get("deleted"), 1);
    // A new POST within the grace takes the held stream over.
    let mut next = Phone::new(31, &server).await;
    assert_eq!(admit(&server, &mut next, "event-grace").await.status, 201);
    let resumed = server.app.streams.get("event-grace").expect("claimed");
    assert!(Arc::ptr_eq(&held, &resumed), "the same stream");
}

/// A3.2: a POST after the grace starts a new stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_after_the_grace_starts_a_new_stream() {
    let server = start(&[("SAPFLOW_RECONNECT_GRACE", "1")]).await;
    let (_phone, location) = live(&server, 32, "event-later").await;
    let held = server.app.streams.get("event-later").expect("live");
    assert_eq!(send(server.http, &delete(&location)).await.status, 200);
    assert!(
        eventually(|| server.app.streams.get("event-later").is_none()).await,
        "ended after the grace"
    );
    let mut next = Phone::new(33, &server).await;
    assert_eq!(admit(&server, &mut next, "event-later").await.status, 201);
    let fresh = server.app.streams.get("event-later").expect("claimed");
    assert!(!Arc::ptr_eq(&held, &fresh), "a new stream");
}

/// A3.2: a DELETE before any media releases the path with no callback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_before_media_releases_with_no_callback() {
    let (callbacks, mut rx) = callback_queue("early");
    let server = start_with(&[], |_| {}, Some(callbacks)).await;
    let mut phone = Phone::new(34, &server).await;
    let a = admit(&server, &mut phone, "event-early").await;
    assert_eq!(a.status, 201);
    let location = a.header("location").expect("Location").to_owned();
    assert_eq!(send(server.http, &delete(&location)).await.status, 200);
    assert!(server.app.streams.get("event-early").is_none(), "released");
    assert!(
        callback_events(&mut rx, Duration::from_millis(300))
            .await
            .is_empty()
    );
    assert_eq!(server.whip.ends.get("deleted"), 1);
}

/// A3.2: a session whose handshake never completes ends at its deadline
/// and releases the path with no callback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handshake_timeout_releases_with_no_callback() {
    let (callbacks, mut rx) = callback_queue("mute");
    let server = start_with(
        &[],
        |s| s.handshake_timeout = Duration::from_secs(1),
        Some(callbacks),
    )
    .await;
    let mut phone = Phone::new(35, &server).await;
    assert_eq!(admit(&server, &mut phone, "event-mute").await.status, 201);
    assert!(server.app.streams.get("event-mute").is_some());
    assert!(
        eventually(|| server.app.streams.get("event-mute").is_none()).await,
        "released at the deadline"
    );
    assert_eq!(server.whip.ends.get("handshake-timeout"), 1);
    assert_eq!(server.whip.ends.get("dtls-failed"), 0, "it went quiet");
    assert!(
        callback_events(&mut rx, Duration::from_millis(300))
            .await
            .is_empty()
    );
}

/// A publisher whose certificate does not match its offer: sapflow-dtls
/// refuses the certificate without ending the handshake (at epoch 0 a
/// forger could have sent it), so the session ends at its deadline, and is
/// counted as the DTLS failure it was, not as a plain timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fingerprint_mismatch_is_a_dtls_failure_at_the_deadline() {
    let server = start_with(&[], |s| s.handshake_timeout = Duration::from_secs(1), None).await;
    let mut phone = Phone::new(37, &server).await;
    let other = sapflow_dtls::Fingerprint::from_bytes([0xab; 32]);
    let offer = sapflow_whip::testing::synthetic_offer(&other, &phone.p.ufrag);
    let a = send(
        server.http,
        &sdp_post("/whip/live/event-mismatch", TOKEN, &offer),
    )
    .await;
    assert_eq!(a.status, 201, "{}", a.body);
    phone.answered(&a.body);
    let c = phone.p.check(true);
    phone.send(&c).await;
    for d in phone.p.client.start() {
        phone.send(&d).await;
    }
    phone.receive(Duration::from_millis(300)).await;
    assert!(!phone.p.has_keys(), "never connects");
    assert!(
        eventually(|| server.app.streams.get("event-mismatch").is_none()).await,
        "released at the deadline"
    );
    assert_eq!(server.whip.ends.get("dtls-failed"), 1);
    assert_eq!(server.whip.ends.get("handshake-timeout"), 0);
}

/// A3.2: every refusal in Resources, with its status and its reason, the
/// app's 401, 403, 429 and a 500 among them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_refusal_has_its_status_and_reason() {
    use crate::publish_auth::stub::{self, Answer as App};
    let _keep: Option<stub::Stub> = None;
    use resource::Refusal as R;
    let reason = |r: R| r.reason().to_owned();
    // Before admission, on the global publisher.
    let server = start(&[]).await;
    let phone = Phone::new(36, &server).await;
    let offer = phone.p.offer();
    let at = server.http;
    let check = |a: Answer, status: u16, why: String| {
        assert_eq!((a.status, a.body), (status, why));
    };
    let no_type = post(
        "/whip/live/event-r",
        &[("authorization", "Bearer publisher:whip-test-secret")],
        &offer,
    );
    check(
        send(at, &no_type).await,
        415,
        reason(R::UnsupportedMediaType),
    );
    let not_sdp = send(at, &sdp_post("/whip/live/event-r", TOKEN, "not sdp")).await;
    assert_eq!(not_sdp.status, 400);
    assert!(!not_sdp.body.is_empty());
    let unprofiled = offer
        .replace("H264/90000", "VP8/90000")
        .replace("opus/48000/2", "PCMU/8000");
    let outside = send(at, &sdp_post("/whip/live/event-r", TOKEN, &unprofiled)).await;
    assert_eq!(outside.status, 422);
    assert!(!outside.body.is_empty());
    let no_token = post(
        "/whip/live/event-r",
        &[("content-type", "application/sdp")],
        &offer,
    );
    check(send(at, &no_token).await, 401, reason(R::Unauthorised));
    // From the app that authorises publishers.
    for (answer, status, why) in [
        (App::Status(401), 401, R::Unauthorised),
        (App::Status(403), 403, R::Forbidden),
        (App::Status(429), 429, R::Throttled),
        (App::Status(500), 503, R::Unavailable),
    ] {
        let auth = stub::start(answer).await;
        let url = auth.url.clone();
        let server = start(&[
            ("SAPFLOW_PUBLISH_USER", ""),
            ("SAPFLOW_PUBLISH_PASS", ""),
            ("SAPFLOW_PUBLISH_AUTH_URL", &url),
        ])
        .await;
        let refused = send(server.http, &sdp_post("/whip/live/event-r", TOKEN, &offer)).await;
        check(refused, status, reason(why));
    }
}

/// A3.3: a second POST while the first session is still taking media gets
/// 409, with its reason and the CORS headers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_post_while_media_flows_gets_409() {
    let server = start(&[]).await;
    let (mut phone, _) = live(&server, 37, "event-busy").await;
    // The first goes on publishing while the second asks.
    let publishing = tokio::spawn(async move {
        phone.publish(2).await;
        phone
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let second = Phone::new(38, &server).await;
    let busy = send(
        server.http,
        &sdp_post("/whip/live/event-busy", TOKEN, &second.p.offer()),
    )
    .await;
    let _phone = publishing.await.expect("published");
    assert_eq!(busy.status, 409);
    assert_eq!(busy.body, resource::Refusal::Conflict.reason());
    assert_eq!(busy.header("access-control-allow-origin"), Some(ORIGIN));
    assert_eq!(server.whip.ends.get("superseded"), 0);
}

/// A3.3: once the first session has taken no media for SAPFLOW_WHIP_STALE,
/// a second POST takes over at once, as if it had left, without waiting
/// for consent to expire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_session_is_superseded() {
    let server = start(&[("SAPFLOW_WHIP_STALE", "1")]).await;
    let (_phone, _) = live(&server, 39, "event-stall").await;
    let held = server.app.streams.get("event-stall").expect("live");
    // Its last media is stamped as the owner reads it: ask again, until
    // the stale time has passed on the server's clock.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let mut next = Phone::new(40, &server).await;
    let mut a = admit(&server, &mut next, "event-stall").await;
    for _ in 0..40 {
        if a.status != 409 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        a = admit(&server, &mut next, "event-stall").await;
    }
    assert_eq!(a.status, 201, "{}", a.body);
    assert_eq!(server.whip.ends.get("superseded"), 1);
    let taken = server.app.streams.get("event-stall").expect("claimed");
    assert!(Arc::ptr_eq(&held, &taken), "the held stream, carried on");
}

/// A3.3: a second POST during the first session's handshake replaces it
/// at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_during_the_first_handshake_replaces_it() {
    let server = start(&[]).await;
    let mut first = Phone::new(41, &server).await;
    assert_eq!(admit(&server, &mut first, "event-race").await.status, 201);
    let mut second = Phone::new(42, &server).await;
    let a = admit(&server, &mut second, "event-race").await;
    assert_eq!(a.status, 201, "{}", a.body);
    assert_eq!(server.whip.ends.get("superseded"), 1);
    assert_eq!(server.whip.resources().len(), 1, "only the second is live");
    second.connect().await;
}

/// Kept from before WHIP: in global-publisher mode an SRT or RTMP publisher
/// with the publisher's credentials takes over any attached stream, a live
/// WHIP session's included, which then ends on its next packet. A WHIP
/// POST never takes over SRT or RTMP: it gets 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn srt_takes_over_a_live_whip_stream_and_whip_never_takes_over_srt() {
    let server = start(&[]).await;
    let (mut phone, _) = live(&server, 43, "event-mixed").await;
    let req = sapflow_core::PublishRequest::parse_srt_stream_id(
        "publish:live/event-mixed:publisher:whip-test-secret",
    )
    .expect("request");
    let srt = Publication::begin(
        &server.app,
        &req,
        SourceKind::Ts,
        "127.0.0.1".parse().expect("ip"),
    )
    .expect("SRT takes over");
    phone.publish(1).await;
    assert!(
        eventually(|| server.whip.ends.get("ingest-ended") == 1).await,
        "the WHIP session ended on its next packet"
    );
    let other = Phone::new(44, &server).await;
    let busy = send(
        server.http,
        &sdp_post("/whip/live/event-mixed", TOKEN, &other.p.offer()),
    )
    .await;
    assert_eq!(busy.status, 409, "SRT is never superseded by WHIP");
    srt.end();
}

/// How a test makes session A panic (A3.4).
enum Fault {
    /// A datagram to A's session.
    Datagram(Vec<u8>),
    /// A datagram to A's session, then audio, for which the owner reads
    /// A's session.
    DatagramThenAudio(Vec<u8>),
    /// An Opus payload from A's publisher.
    Audio(&'static [u8]),
    /// A datagram to A's session, then a `POST` for A's path, whose
    /// supersede reads A's session.
    DatagramThenPost(Vec<u8>),
}

fn session_id(location: &str) -> SessionId {
    location
        .rsplit('/')
        .next()
        .and_then(SessionId::parse)
        .expect("a session Location")
}

/// Every mark a session's counts hold (re-anchors are not marks).
fn marks(counts: &LossCounts) -> u64 {
    counts
        .fields()
        .iter()
        .filter(|(name, _)| *name != "reanchors")
        .map(|(_, n)| n)
        .sum()
}

/// A3.4: session A panics, by `fault`, while B publishes. A ends as
/// `panicked`, its recording is marked, and its path is freed, with the
/// listener's entries for it, for a new publisher; B loses nothing, and
/// its playlist keeps growing after A has gone.
async fn panic_isolation(seed: u8, fault: Fault) {
    let (server, root) = udp_server_with(true).await;
    let (a_event, b_event) = (
        format!("event-panic-a{seed}"),
        format!("event-panic-b{seed}"),
    );
    let (mut a, a_location) = live(&server, seed, &a_event).await;
    let (mut b, b_location) = live(&server, seed + 1, &b_event).await;
    let (a_id, b_id) = (session_id(&a_location), session_id(&b_location));
    let resource = |id: &SessionId| {
        server
            .whip
            .resources()
            .get(id)
            .map(|r| (r.losses.clone(), r.released.clone()))
    };
    let (a_counts, mut a_released) = resource(&a_id).expect("A is live");
    let (b_counts, _) = resource(&b_id).expect("B is live");
    let a_stream = server.app.streams.get(&a_event).expect("live");
    let b_stream = server.app.streams.get(&b_event).expect("live");
    let b_task = tokio::spawn(async move {
        b.publish(4).await;
        b
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    // With nothing marked before the fault, the panic's reason is the
    // recording's first.
    let clean = marks(&a_counts) == 0;
    // The recording A's media went to.
    let recording = a_stream
        .session()
        .recorder
        .as_ref()
        .expect("recording")
        .status();
    match fault {
        Fault::Datagram(d) => a.send(&d).await,
        Fault::DatagramThenAudio(d) => {
            a.send(&d).await;
            for n in 0..5 {
                let d = a.p.audio(n);
                a.send(&d).await;
            }
        }
        Fault::Audio(payload) => {
            let d = a.p.audio_payload(payload.to_vec());
            a.send(&d).await;
        }
        Fault::DatagramThenPost(d) => {
            a.send(&d).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            // 201 or 409: A's publication may not have gone yet.
            let mut c = Phone::new(seed + 100, &server).await;
            let _ = admit(&server, &mut c, &a_event).await;
        }
    }
    assert!(
        eventually(|| server.whip.ends.get("panicked") == 1).await,
        "A ended as panicked: {:?}; A {a_counts}",
        server.whip.ends
    );
    let after = b_stream.version();
    let released = tokio::time::timeout(Duration::from_secs(5), a_released.wait_for(|r| *r)).await;
    assert!(
        released.is_ok_and(|r| r.is_ok()),
        "A's publication released"
    );
    assert!(server.whip.resources().get(&a_id).is_none());
    assert_ne!(server.whip.paths().get(&a_event), Some(&a_id));
    assert_eq!(server.whip.ends.get("ingest-ended"), 0);
    assert!(a_counts.panics.load(Ordering::Relaxed) >= 1, "{a_counts}");
    let reason = recording.reason();
    if clean {
        assert_eq!(reason, Some(IncompleteReason::IngestError), "{a_counts}");
    } else {
        assert!(reason.is_some(), "{a_counts}");
    }
    let _b = b_task.await.expect("B published throughout");
    assert_eq!(marks(&b_counts), 0, "B lost nothing: {b_counts}");
    assert!(
        b_stream.version() > after,
        "B's playlist kept growing after A had gone"
    );
    // A's path takes a new publisher.
    let mut d = Phone::new(seed + 101, &server).await;
    assert_eq!(admit(&server, &mut d, &a_event).await.status, 201);
    let _ = std::fs::remove_dir_all(&root);
}

/// In a call into the session (the router's guard).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_one_session_ends_only_that_session() {
    panic_isolation(
        45,
        Fault::Datagram(sapflow_whip::testing::PANIC_DATAGRAM.to_vec()),
    )
    .await;
}

/// In the session's deadline, which the owner reads every turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_a_session_deadline_ends_only_that_session() {
    panic_isolation(47, Fault::Datagram(arm(Read::PollTimeout))).await;
}

/// In the owner's read of the session around an SRTP datagram, which only
/// `Router::inspect` guards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_the_owners_read_ends_only_that_session() {
    panic_isolation(49, Fault::DatagramThenAudio(arm(Read::Stats))).await;
}

/// In the owner's read of the session for supersede.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_the_supersede_read_ends_only_that_session() {
    panic_isolation(51, Fault::DatagramThenPost(arm(Read::LastMedia))).await;
}

/// In the owner's feed for the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_the_feed_ends_only_that_session() {
    panic_isolation(53, Fault::Audio(injected::FEED_PANIC)).await;
}

/// In the ingest, inside the publication with the stream's locks held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panic_in_the_ingest_ends_only_that_session() {
    panic_isolation(55, Fault::Audio(injected::INGEST_PANIC)).await;
}

/// A3.4's hooks with the switch off, as the server always runs: the panic
/// datagram, every armed read, both payloads and a supersede `POST` reach
/// a live session, and nothing panics. A build of the test targets
/// compiles the hooks into the server binary too, which never turns them
/// on, so this is that binary's case.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_panic_hooks_do_nothing_unless_a_test_turns_them_on() {
    let (server, root) = udp_server_with(false).await;
    let event = "event-panic-off";
    let (mut a, location) = live(&server, 57, event).await;
    let id = session_id(&location);
    let counts = server
        .whip
        .resources()
        .get(&id)
        .map(|r| r.losses.clone())
        .expect("A is live");
    let stream = server.app.streams.get(event).expect("live");
    let mut faults = vec![sapflow_whip::testing::PANIC_DATAGRAM.to_vec()];
    for read in [
        Read::PollTimeout,
        Read::MediaFrom,
        Read::LastMedia,
        Read::Receiver,
        Read::Stats,
    ] {
        faults.push(arm(read));
    }
    for d in faults {
        a.send(&d).await;
    }
    for payload in [injected::FEED_PANIC, injected::INGEST_PANIC] {
        let d = a.p.audio_payload(payload.to_vec());
        a.send(&d).await;
    }
    a.publish(1).await;
    // Supersede reads the session's last media: A is live, so 409.
    let mut c = Phone::new(157, &server).await;
    assert_eq!(admit(&server, &mut c, event).await.status, 409);
    let before = stream.version();
    a.publish(1).await;
    assert!(stream.version() > before, "A's playlist keeps growing");
    assert_eq!(
        server.whip.ends.get("panicked"),
        0,
        "{:?}",
        server.whip.ends
    );
    assert!(server.whip.resources().get(&id).is_some(), "A is live");
    assert_eq!(counts.panics.load(Ordering::Relaxed), 0, "{counts}");
    assert_eq!(counts.abnormal_ends.load(Ordering::Relaxed), 0, "{counts}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A3.7: past `Limits::connections` signalling connections, a new one is
/// closed at once; once one closes, the next is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn past_the_signalling_connection_cap_a_connection_is_closed() {
    let server = start_limited(Limits {
        connections: 3,
        per_address: 10,
        ..Limits::default()
    })
    .await;
    let mut idle = Vec::new();
    for _ in 0..3 {
        idle.push(TcpStream::connect(server.http).await.expect("connect"));
    }
    let mut fourth = TcpStream::connect(server.http).await.expect("connect");
    assert!(closed_unanswered(&mut fourth, Duration::from_secs(2)).await);
    assert_eq!(server.whip.signalling.over_total.load(Ordering::Relaxed), 1);
    drop(idle.pop());
    let preflight = b"OPTIONS /whip/live/event-cap HTTP/1.1\r\nhost: whip.example\r\n\r\n";
    assert!(served_again(&server, preflight).await, "served again");
}

/// A3.7: one address holds at most `Limits::per_address` signalling
/// connections, below the total; its own next connection is closed at
/// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_address_holds_at_most_its_share_of_the_signalling_slots() {
    let server = start_limited(Limits {
        connections: 8,
        per_address: 2,
        ..Limits::default()
    })
    .await;
    let mut idle = Vec::new();
    for _ in 0..2 {
        idle.push(TcpStream::connect(server.http).await.expect("connect"));
    }
    let mut third = TcpStream::connect(server.http).await.expect("connect");
    assert!(closed_unanswered(&mut third, Duration::from_secs(2)).await);
    assert_eq!(
        server.whip.signalling.over_address.load(Ordering::Relaxed),
        1
    );
    assert_eq!(server.whip.signalling.over_total.load(Ordering::Relaxed), 0);
    drop(idle);
    let preflight = b"OPTIONS /whip/live/event-share HTTP/1.1\r\nhost: whip.example\r\n\r\n";
    assert!(served_again(&server, preflight).await, "freed");
}

/// A body still arriving at `Limits::client_time` from the connection is
/// answered `408`, not left to the handler's timeout and `503`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_offer_that_arrives_too_slowly_gets_408() {
    let server = start_limited(Limits {
        client_time: Duration::from_secs(1),
        ..Limits::default()
    })
    .await;
    let mut s = TcpStream::connect(server.http).await.expect("connect");
    let head = format!(
        "POST /whip/live/event-slow HTTP/1.1\r\nhost: whip.example\r\ncontent-type: application/sdp\r\nauthorization: Bearer {TOKEN}\r\ncontent-length: 200\r\n\r\n"
    );
    s.write_all(head.as_bytes()).await.expect("head");
    let started = Instant::now();
    let (mut reader, mut writer) = s.into_split();
    let trickle = tokio::spawn(async move {
        for _ in 0..40 {
            if writer.write_all(b"v").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(15), reader.read_to_end(&mut out)).await;
    trickle.abort();
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        server
            .whip
            .signalling
            .client_timeouts
            .load(Ordering::Relaxed),
        1
    );
}

/// The failed-attempt throttle is a refusal path of its own: past 10
/// failed attempts a minute, an address gets `429` before its offer is
/// read, parsed or answered, whatever it sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_throttled_address_is_refused_before_its_offer_is_read() {
    let server = start(&[]).await;
    let offer = Publisher::new(60, &[GCM]).offer();
    for _ in 0..10 {
        let a = send(
            server.http,
            &sdp_post("/whip/live/event-thr", "publisher:wrong", &offer),
        )
        .await;
        assert_eq!(a.status, 401, "{}", a.body);
    }
    // The right token, but an offer that does not parse: refused as
    // throttled, not as malformed.
    let a = send(
        server.http,
        &sdp_post("/whip/live/event-thr", TOKEN, "not sdp"),
    )
    .await;
    assert_eq!(a.status, 429, "{}", a.body);
    let a = send(
        server.http,
        &sdp_post("/whip/live/event-thr", TOKEN, &offer),
    )
    .await;
    assert_eq!(a.status, 429, "{}", a.body);
}

/// A `POST` as the trusted proxy forwards it for `ip`.
fn forwarded_post(path: &str, ip: &str, body: &str) -> Vec<u8> {
    let auth = format!("Bearer {TOKEN}");
    post(
        path,
        &[
            ("authorization", &auth),
            ("content-type", "application/sdp"),
            ("x-forwarded-for", ip),
        ],
        body,
    )
}

/// A3.7: sessions that have not connected yet are capped in total and per
/// publisher address; a `POST` past either gets `503`, and a session that
/// ends gives its place back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_sessions_are_capped_in_total_and_per_address() {
    let server = start_limited_with(
        &[("SAPFLOW_WHIP_TRUSTED_PROXY", "127.0.0.1")],
        Limits {
            pending: 3,
            pending_per_address: 2,
            ..Limits::default()
        },
    )
    .await;
    let offer = Publisher::new(61, &[GCM]).offer();
    let post_as =
        |n: u8, ip: &str| forwarded_post(&format!("/whip/live/event-pend-{n}"), ip, &offer);
    let (a, b) = ("198.51.100.1", "198.51.100.2");
    let first = send(server.http, &post_as(1, a)).await;
    assert_eq!(first.status, 201, "{}", first.body);
    assert_eq!(send(server.http, &post_as(2, a)).await.status, 201);
    let refused = send(server.http, &post_as(3, a)).await;
    assert_eq!(refused.status, 503, "per address: {}", refused.body);
    assert_eq!(send(server.http, &post_as(4, b)).await.status, 201);
    let refused = send(server.http, &post_as(5, b)).await;
    assert_eq!(refused.status, 503, "in total: {}", refused.body);
    // One of A's ends: B may start another.
    let location = first.header("location").expect("Location").to_owned();
    assert_eq!(send(server.http, &delete(&location)).await.status, 200);
    assert_eq!(send(server.http, &post_as(5, b)).await.status, 201);
}

/// The per-address caps count an IPv6 host by its /64 and an IPv4 address
/// whole: a second address in the same /64 shares the first's places, one
/// in another /64 or another IPv4 address does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_per_address_caps_count_ipv6_by_its_64() {
    let server = start_limited_with(
        &[("SAPFLOW_WHIP_TRUSTED_PROXY", "127.0.0.1")],
        Limits {
            pending_per_address: 2,
            ..Limits::default()
        },
    )
    .await;
    let offer = Publisher::new(64, &[GCM]).offer();
    let mut n = 0u8;
    let mut post_as = |ip: &str| {
        n += 1;
        forwarded_post(&format!("/whip/live/event-six-{n}"), ip, &offer)
    };
    for ip in ["2001:db8:1::1", "2001:db8:1::2"] {
        assert_eq!(send(server.http, &post_as(ip)).await.status, 201, "{ip}");
    }
    let refused = send(server.http, &post_as("2001:db8:1:0:ffff::3")).await;
    assert_eq!(refused.status, 503, "the same /64: {}", refused.body);
    assert_eq!(
        send(server.http, &post_as("2001:db8:2::1")).await.status,
        201
    );
    for ip in ["198.51.100.1", "198.51.100.1"] {
        assert_eq!(send(server.http, &post_as(ip)).await.status, 201, "{ip}");
    }
    assert_eq!(
        send(server.http, &post_as("198.51.100.2")).await.status,
        201
    );
}

/// The signalling slots are keyed the same way.
#[test]
fn address_keys_are_an_ipv4_address_or_an_ipv6_64() {
    let key = |s: &str| address_key(s.parse().expect("address"));
    assert_eq!(key("2001:db8:1::1"), key("2001:db8:1:0:ffff::3"));
    assert_ne!(key("2001:db8:1::1"), key("2001:db8:2::1"));
    assert_ne!(key("198.51.100.1"), key("198.51.100.2"));
    assert_eq!(key("::ffff:198.51.100.1"), key("198.51.100.1"));
    let addresses = Addresses::default();
    let first = AddressSlot::take(&addresses, key("2001:db8:1::1"), 1).expect("first");
    assert!(AddressSlot::take(&addresses, key("2001:db8:1::9"), 1).is_none());
    drop(first);
    assert!(AddressSlot::take(&addresses, key("2001:db8:1::9"), 1).is_some());
}

/// A session that has connected no longer counts as pending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_session_no_longer_counts_as_pending() {
    let server = start_limited(Limits {
        pending: 1,
        ..Limits::default()
    })
    .await;
    let mut phone = Phone::new(62, &server).await;
    assert_eq!(admit(&server, &mut phone, "event-pc-1").await.status, 201);
    let other = Publisher::new(63, &[GCM]).offer();
    let refused = send(
        server.http,
        &sdp_post("/whip/live/event-pc-2", TOKEN, &other),
    )
    .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    phone.connect().await;
    let mut admitted = false;
    for _ in 0..40 {
        if send(
            server.http,
            &sdp_post("/whip/live/event-pc-2", TOKEN, &other),
        )
        .await
        .status
            == 201
        {
            admitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(admitted, "admitted once the first had connected");
}

/// A3.7 over real UDP: unauthenticated STUN, DTLS, RTP and random bytes
/// sent to the media port from a fresh address draw nothing, and so does a
/// live publisher's check re-sent from there once its original has
/// arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_address_draws_nothing() {
    let server = start(&[]).await;
    let (mut phone, _) = live(&server, 64, "event-fresh").await;
    let fresh = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let mut corrupted = phone.p.check(false);
    if let Some(last) = corrupted.last_mut() {
        *last ^= 0xff;
    }
    let mut unsigned = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
    unsigned.extend_from_slice(&[7; 12]);
    let mut hello = vec![0x16, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 40, 1];
    hello.resize(60, 0x5a);
    let rtp = [0x80, 0x6f, 0, 9, 0, 0, 0, 1, 0, 0, 0x10, 0x01, 1, 2, 3, 4];
    let random: Vec<u8> = (0..200u32).map(|i| (i * 37 % 251) as u8).collect();
    for d in [&corrupted, &unsigned, &hello, &rtp[..], &random] {
        fresh.send_to(d, server.udp).await.expect("send");
    }
    // A check the publisher sends, then the same bytes from the fresh
    // address, as a captured copy would arrive.
    let check = phone.p.check(false);
    phone.send(&check).await;
    phone.receive(Duration::from_millis(200)).await;
    fresh.send_to(&check, server.udp).await.expect("send");
    let mut buf = [0u8; 2048];
    let got = tokio::time::timeout(Duration::from_millis(700), fresh.recv_from(&mut buf)).await;
    assert!(got.is_err(), "the fresh address was sent {:?}", got.ok());
    // The publisher was not cut off.
    phone.publish(1).await;
}

#[test]
fn every_end_reason_has_a_name() {
    use sapflow_whip::EndReason as R;
    let names = [
        end_name(R::Closed, Some(CloseCause::Delete)),
        end_name(R::Closed, Some(CloseCause::Superseded)),
        end_name(R::Closed, Some(CloseCause::IngestEnded)),
        end_name(R::Closed, None),
        end_name(R::HandshakeTimeout, None),
        end_name(R::ConsentExpired, None),
        end_name(R::Dtls(sapflow_dtls::Error::Timeout), None),
        end_name(R::PeerClosed, None),
        end_name(R::MediaTimeout, None),
        end_name(R::KeysExhausted, None),
        end_name(R::Panicked, None),
    ];
    let distinct: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(distinct.len(), names.len());
    // A panic in the owner's code or the ingest counts with the session's.
    assert_eq!(
        end_name(R::Closed, Some(CloseCause::Panicked)),
        end_name(R::Panicked, None)
    );
    let counts = EndCounts::default();
    assert_eq!((counts.add("deleted"), counts.add("deleted")), (1, 2));
    assert_eq!((counts.get("deleted"), counts.get("panicked")), (2, 0));
}
