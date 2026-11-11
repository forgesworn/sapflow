//! WHIP in process: the signalling listener and the UDP owner on loopback,
//! driven by `sapflow-whip`'s scripted publisher (STUN client, the
//! `sapflow-dtls` test client and SRTP) over a real UDP socket.

use std::path::PathBuf;
use std::time::Duration;

use sapflow_whip::testing::{PPS, Publisher, StunClient, VIDEO_SSRC, idr_slice, server_identity};
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
    let app = app(env);
    let sockets = Sockets {
        http: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        udp: UdpSocket::bind("127.0.0.1:0").await.expect("bind"),
    };
    let (http, udp) = (
        sockets.http.local_addr().expect("addr"),
        sockets.udp.local_addr().expect("addr"),
    );
    let (whip, router, rx) = prepare(&app, server_identity())
        .expect("router")
        .expect("WHIP on");
    tokio::spawn(run(app.clone(), whip.clone(), router, rx, sockets));
    Server {
        app,
        whip,
        http,
        udp,
    }
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
    // At least 3 s, and on until a part is out (at most 10 s). Each packet
    // is sent after the last one's 20 ms wait, so any time a step takes
    // beyond that makes the packets later than their RTP clock.
    let started = std::time::Instant::now();
    let mut packets = 0u64;
    for n in 0..500u64 {
        let packet = phone.p.audio((n % 251) as u8);
        if !skip.contains(&n) {
            phone.send(&packet).await;
        }
        packets += 1;
        phone.receive(Duration::from_millis(20)).await;
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "whip-loss-test-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).expect("owned scratch");
    let dir = root.to_str().expect("utf-8 path").to_owned();
    let server = start(&[
        ("SAPFLOW_RECORDINGS", &dir),
        ("SAPFLOW_RECORD_MIN_FREE_GB", "0"),
    ])
    .await;
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
    let mut feed = Feed::new(tx, publication.loss_notice());
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert_eq!(status.reason(), None);
    assert_eq!(feed.push(access_unit(false)), Ok(()), "full: dropped");
    assert_eq!(status.reason(), Some(IncompleteReason::IngestQueueFull));
    assert!(feed.take_keyframe_request(), "a keyframe is asked for");
    assert!(!feed.take_keyframe_request(), "once");
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
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
    let mut feed = Feed::bounded(tx, publication.loss_notice(), 12);
    let queued = feed.queued();
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert_eq!(queued.load(Ordering::Acquire), 10);
    assert_eq!(status.reason(), None);
    // A third would be 15 bytes: dropped as a full queue.
    assert_eq!(feed.push(access_unit(false)), Ok(()));
    assert_eq!(status.reason(), Some(IncompleteReason::IngestQueueFull));
    assert!(feed.take_keyframe_request());
    assert_eq!(queued.load(Ordering::Acquire), 10);
    // The ingest task takes both; the next IDR fits.
    while let Ok(item) = rx.try_recv() {
        queued.fetch_sub(item.size(), Ordering::AcqRel);
    }
    assert_eq!(feed.push(access_unit(true)), Ok(()));
    assert!(matches!(rx.try_recv(), Ok(Item::Video { au, .. }) if au.idr));
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
};

/// What a recording says after a session's events: through a `Feed`, as
/// the server runs them, or, with `feed` false, with only the access units
/// and audio given to the ingest and no loss marked by the server, which
/// leaves the muxer's own view.
async fn recorded(events: Vec<Event>, setup: Setup, feed: bool) -> Option<IncompleteReason> {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let mut publication = fixture.begin_kind(SourceKind::Es);
    publication
        .push(|source, now| configure(source, setup, now))
        .expect("setup");
    let status = status(&publication);
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let mut server = Feed::new(tx, publication.loss_notice());
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
                Media::Loss { .. } | Media::Other(_) => {}
            },
            _ => {}
        }
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }
        for item in items {
            publication
                .push(|source, now| feed_item(source, item, now))
                .expect("ingest");
        }
    }
    status.reason()
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
        (R::HandshakeTimeout, None),
        (R::Closed, None),
        (R::PeerClosed, None),
    ] {
        let fixture = crate::publish::loss_tests::Fixture::new();
        let publication = fixture.begin();
        let status = status(&publication);
        let (tx, _rx) = mpsc::channel(4);
        let mut feed = Feed::new(tx, publication.loss_notice());
        feed.ended(reason);
        assert_eq!(status.reason(), mark, "{reason:?}");
    }
}

/// Each `Lost` is counted and marks the recording incomplete; a dropped
/// frame marks it too.
#[tokio::test]
async fn receiver_loss_is_counted_and_marks_the_recording() {
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let (tx, _rx) = mpsc::channel(4);
    let mut feed = Feed::new(tx, publication.loss_notice());
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
    let fixture = crate::publish::loss_tests::Fixture::new();
    let publication = fixture.begin();
    let (tx, _rx) = mpsc::channel(4);
    let mut feeds = HashMap::from([(id, Feed::new(tx, publication.loss_notice()))]);
    let junk = [0x80, 0x6f, 0, 9, 0, 0, 0, 1, 0, 0, 0x10, 0x01, 1, 2, 3, 4];
    receive(&mut router, &mut feeds, p.tuple, &junk);
    assert_eq!(feeds[&id].received, 0, "spoofed bytes count");
    let audio = p.audio(1);
    receive(&mut router, &mut feeds, p.tuple, &audio);
    assert_eq!(feeds[&id].received, audio.len());
}
