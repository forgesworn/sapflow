//! `SAPFLOW_SRT_APP_KEYS` end to end: the real SRT loop (`run`) on a loopback
//! socket, our own encrypting `Caller` as the encoder, and a stub app that
//! answers 2xx with a per-stream passphrase. A wrong or missing key must be
//! refused before the path is claimed, so a live or held stage, its
//! recording and its callbacks stay as they were.
//!
//! Known gap, for the replay guard (S3): a captured conclusion handshake
//! replayed by another peer still passes the key check. Not tested here.

use std::time::Duration;

use sapflow_srt::crypto::CryptoConfig;
use sapflow_srt::{Caller, CallerCloseReason, CallerConfig, CallerEvent};
use tokio::net::UdpSocket as TokioUdp;

use super::*;
use crate::publish::loss_tests::Fixture;
use crate::publish_auth::stub::{self, Answer, Stub};

/// The stream's passphrase as the app mints it: 39 bytes.
const KEY: &str = "pw_srt_00112233445566778899aabbccddeeff";
const WRONG: &str = "pw_srt_ffeeddccbbaa99887766554433221100";
const MEDIA: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts");
const STREAM_ID: &str = "publish:live/event-a:encoder:pw_live_visible";

/// An app with app keys on, authorised by `stub`, holding a stage for
/// `grace` after its publisher leaves.
fn fixture(stub: &Stub, grace: Duration) -> Fixture {
    let mut fixture = Fixture::new();
    let app = Arc::get_mut(&mut fixture.app).expect("sole App");
    app.config.publish_auth = Some(stub.endpoint.clone());
    app.config.srt_app_keys = true;
    app.config.reconnect_grace = grace;
    fixture
}

/// Serves SRT for `app` on a loopback port, as `serve` does.
fn origin(app: &Arc<App>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let (socket, pending, got) =
        bind_udp(SocketAddr::from(([127, 0, 0, 1], 0)), RCVBUF_MIN).expect("bind");
    let addr = socket.local_addr().expect("addr");
    let app = app.clone();
    let task = tokio::spawn(async move {
        let _ = run(app, socket, pending, got, 0).await;
    });
    (addr, task)
}

fn key(passphrase: &str, size: KeySize) -> CryptoConfig {
    CryptoConfig::new(passphrase.as_bytes().to_vec(), size).expect("passphrase")
}

/// One encoder over a real UDP socket.
struct Encoder {
    caller: Caller,
    socket: TokioUdp,
    connected: bool,
    closed: Option<CallerCloseReason>,
}

impl Encoder {
    /// Calls `origin` and runs the handshake to its end: connected, or why not.
    async fn call(origin: SocketAddr, crypto: Option<CryptoConfig>) -> Self {
        let socket = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
        let caller = Caller::new(
            CallerConfig {
                stream_id: STREAM_ID.into(),
                crypto,
                ..CallerConfig::default()
            },
            origin,
            Instant::now(),
        )
        .expect("caller");
        let mut encoder = Self {
            caller,
            socket,
            connected: false,
            closed: None,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while !encoder.connected && encoder.closed.is_none() && Instant::now() < deadline {
            encoder.pump(Duration::from_millis(20)).await;
        }
        encoder
    }

    /// Runs the caller's I/O for `period`.
    async fn pump(&mut self, period: Duration) {
        let until = Instant::now() + period;
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            self.caller.handle_timeout(now);
            while let Some((to, bytes)) = self.caller.poll_transmit(now) {
                let _ = self.socket.send_to(&bytes, to).await;
            }
            while let Some(event) = self.caller.poll_event() {
                match event {
                    CallerEvent::Connected { .. } => self.connected = true,
                    CallerEvent::Closed { reason } => self.closed = Some(reason),
                }
            }
            if now >= until {
                return;
            }
            let wait = self
                .caller
                .poll_timeout()
                .map_or(Duration::from_millis(5), |t| {
                    t.saturating_duration_since(now)
                })
                .min(until - now)
                .max(Duration::from_millis(1));
            if let Ok(Ok((n, from))) =
                tokio::time::timeout(wait, self.socket.recv_from(&mut buf)).await
            {
                self.caller.handle_datagram(Instant::now(), from, &buf[..n]);
            }
        }
    }

    /// Sends the whole fixture, a datagram at a time.
    async fn send_media(&mut self) {
        for chunk in MEDIA.chunks(1316) {
            while self.caller.enqueue(Instant::now(), chunk.to_vec()).is_err() {
                self.pump(Duration::from_millis(2)).await;
            }
            self.pump(Duration::from_millis(1)).await;
        }
        self.pump(Duration::from_millis(200)).await;
    }

    async fn stop(&mut self) {
        self.caller.stop(Instant::now());
        self.pump(Duration::from_millis(50)).await;
    }

    fn rejected(&self) -> Option<u32> {
        match self.closed {
            Some(CallerCloseReason::Rejected(code)) => Some(code),
            _ => None,
        }
    }
}

/// The callback events queued so far.
fn events(fixture: &mut Fixture) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok(item) = fixture.callbacks.try_recv() {
        let body: serde_json::Value = serde_json::from_slice(&item.body).expect("JSON");
        names.push(body["event"].as_str().unwrap_or("").to_owned());
    }
    names
}

/// Waits for the callback `name`, keeping `encoder` serviced.
async fn wait_for(fixture: &mut Fixture, encoder: &mut Encoder, name: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..200 {
        seen.extend(events(fixture));
        if seen.iter().any(|e| e == name) {
            return seen;
        }
        encoder.pump(Duration::from_millis(20)).await;
    }
    panic!("no {name} callback: {seen:?}");
}

/// Who owns the stage, its recording's state, and whether it is mapped.
#[derive(Debug, PartialEq)]
struct Stage {
    owner: (u64, bool),
    recording: Option<Option<IncompleteReason>>,
}

fn stage(app: &Arc<App>) -> Stage {
    let stream = app.streams.get("event-a").expect("stage mapped");
    let session = stream.session();
    Stage {
        owner: session.owner(),
        recording: session.recorder.as_ref().map(|r| r.status().reason()),
    }
}

/// The two attacks: an encoder with no key material, and one with the wrong
/// passphrase. Each is refused, after the app was asked, with the code a
/// failed key check gives.
async fn attack(origin: SocketAddr, stub: &Stub) {
    for (crypto, code) in [
        (None, RejectCode::UNSECURE),
        (Some(key(WRONG, KeySize::Aes256)), RejectCode::BADSECRET),
    ] {
        let asked = stub.count();
        let attacker = Encoder::call(origin, crypto).await;
        assert!(!attacker.connected);
        assert_eq!(attacker.rejected(), Some(code.0));
        assert_eq!(stub.count(), asked + 1, "the app was asked first");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_wrong_or_missing_key_leaves_a_live_stage_untouched() {
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);

    let mut live = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(live.connected, "{:?}", live.closed);
    live.send_media().await;
    wait_for(&mut fixture, &mut live, "publish").await;
    let before = stage(&fixture.app);
    assert!(before.owner.1, "attached");
    assert_eq!(before.recording, Some(None), "recording, complete so far");

    attack(addr, &stub).await;
    live.pump(Duration::from_millis(100)).await;

    assert_eq!(stage(&fixture.app), before);
    assert!(live.connected && live.closed.is_none(), "{:?}", live.closed);
    let after = events(&mut fixture);
    for name in ["unpublish", "recordSegmentComplete", "publish"] {
        assert!(!after.iter().any(|e| e == name), "{after:?}");
    }
    let logs = captured.text();
    assert!(logs.contains("reason=BadSecret"), "{logs}");
    assert!(logs.contains("reason=NoKeyMaterial"), "{logs}");
    assert!(!logs.contains("publisher replaced"), "{logs}");
    live.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_wrong_or_missing_key_leaves_a_held_stage_held() {
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::from_secs(30));
    let (addr, server) = origin(&fixture.app);

    let mut live = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(live.connected, "{:?}", live.closed);
    live.send_media().await;
    wait_for(&mut fixture, &mut live, "publish").await;
    live.stop().await;
    wait_for(&mut fixture, &mut live, "unpublish").await;
    let held = stage(&fixture.app);
    assert!(!held.owner.1, "held for a reconnect");
    assert!(held.recording.is_some());

    attack(addr, &stub).await;

    assert_eq!(stage(&fixture.app), held);
    let after = events(&mut fixture);
    for name in ["unpublish", "recordSegmentComplete", "publish"] {
        assert!(!after.iter().any(|e| e == name), "{after:?}");
    }
    // The hold survived: the right key resumes it, with the same recording.
    let mut back = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(back.connected, "{:?}", back.closed);
    let resumed = stage(&fixture.app);
    assert_eq!(resumed.owner, (held.owner.0 + 1, true));
    assert!(resumed.recording.is_some());
    assert!(captured.text().contains("resumed=true"));
    back.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_right_key_publishes_at_either_length_and_is_logged_as_an_app_key() {
    for (size, len) in [(KeySize::Aes128, 16), (KeySize::Aes256, 32)] {
        let captured = crate::log_capture::Capture::start();
        let stub = stub::start(Answer::Passphrase(&[KEY])).await;
        let mut fixture = fixture(&stub, Duration::ZERO);
        let (addr, server) = origin(&fixture.app);
        let mut encoder = Encoder::call(addr, Some(key(KEY, size))).await;
        assert!(encoder.connected, "{len}: {:?}", encoder.closed);
        encoder.send_media().await;
        wait_for(&mut fixture, &mut encoder, "publish").await;
        assert_eq!(stub.last()["srtKey"], "accepted");
        let logs = captured.text();
        assert!(logs.contains("SRT publisher accepted"), "{logs}");
        assert!(
            logs.contains("encryption=\"app-key\"") && logs.contains(&format!("key_len={len}")),
            "{logs}"
        );
        encoder.stop().await;
        server.abort();
        let _ = server.await;
        drop(captured);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn required_encryption_refuses_plaintext_before_the_app_is_asked() {
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    Arc::get_mut(&mut fixture.app)
        .expect("sole App")
        .config
        .srt_require_encryption = true;
    let (addr, server) = origin(&fixture.app);
    let plain = Encoder::call(addr, None).await;
    assert_eq!(plain.rejected(), Some(RejectCode::UNSECURE.0));
    assert_eq!(stub.count(), 0);
    let mut keyed = Encoder::call(addr, Some(key(KEY, KeySize::Aes128))).await;
    assert!(keyed.connected, "{:?}", keyed.closed);
    assert_eq!(stub.count(), 1);
    keyed.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn without_a_passphrase_in_the_answer_admission_is_as_before() {
    // App keys on, but this gig has no passphrase: 2xx alone admits as it
    // always has, and the accept log says the caller does not encrypt.
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Status(200)).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut encoder = Encoder::call(addr, None).await;
    assert!(encoder.connected, "{:?}", encoder.closed);
    encoder.send_media().await;
    wait_for(&mut fixture, &mut encoder, "publish").await;
    assert_eq!(stub.last()["srtKey"], "accepted");
    assert!(captured.text().contains("encryption=\"none\""));
    encoder.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_wrong_keys_from_one_address_are_throttled() {
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    // The fixture's throttle allows ten failures a minute per address.
    for attempt in 0..10 {
        let attacker = Encoder::call(addr, Some(key(WRONG, KeySize::Aes128))).await;
        assert_eq!(
            attacker.rejected(),
            Some(RejectCode::BADSECRET.0),
            "{attempt}"
        );
    }
    assert_eq!(stub.count(), 10);
    // Now refused as throttled, without asking the app, even with the
    // right key.
    for crypto in [key(WRONG, KeySize::Aes128), key(KEY, KeySize::Aes256)] {
        let attacker = Encoder::call(addr, Some(crypto)).await;
        assert_eq!(attacker.rejected(), Some(RejectCode::http(402).0));
    }
    assert_eq!(stub.count(), 10);
    assert!(fixture.app.streams.get("event-a").is_none());
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_passphrase_never_reaches_logs_or_errors() {
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut texts = Vec::new();
    for crypto in [
        None,
        Some(key(WRONG, KeySize::Aes256)),
        Some(key(KEY, KeySize::Aes128)),
    ] {
        let mut encoder = Encoder::call(addr, crypto).await;
        texts.push(format!("{:?}", encoder.closed));
        encoder.stop().await;
    }
    for error in [
        KeyCheckError::NotPending,
        KeyCheckError::NoKeyMaterial,
        KeyCheckError::BadSecret,
        KeyCheckError::UnsupportedKey,
        KeyCheckError::InvalidConfig,
    ] {
        texts.push(format!("{error} {error:?}"));
    }
    // An admission holding the key prints the path only.
    let req = sapflow_core::PublishRequest::parse_srt_stream_id(STREAM_ID).expect("request");
    let admission = crate::srt::admit(&fixture.app, STREAM_ID, "127.0.0.1".parse().expect("ip"), 0)
        .await
        .expect("admitted");
    texts.push(format!("{admission:?}"));
    assert!(matches!(admission, Admission::Pending(_)));
    texts.push(format!("{:?}", fixture.app.config));
    drop((req, admission));
    let _ = events(&mut fixture);
    let logs = captured.text();
    assert!(logs.contains("publish refused"), "{logs}");
    let hex: String = KEY.bytes().map(|b| format!("{b:02x}")).collect();
    for text in texts.iter().chain([&logs]) {
        for secret in [KEY, WRONG, &hex, "pw_live_visible"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn at_cutover_a_global_optional_key_and_app_keys_are_told_apart() {
    // As the switch goes on in production: a global passphrase with
    // `optional`, and app keys. Legacy gigs (no header) admit the global
    // key or plaintext; a gig with a passphrase admits only its own key.
    const GLOBAL: &str = "deployment-wide-secret";
    let captured = crate::log_capture::Capture::start();
    for (answer, stream) in [
        (Answer::Status(200), "legacy"),
        (Answer::Passphrase(&[KEY]), "keyed"),
    ] {
        let stub = stub::start(answer).await;
        let mut fixture = fixture(&stub, Duration::ZERO);
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.srt_crypto = Some(key(GLOBAL, KeySize::Aes128));
        app.config.srt_allow_plaintext = true;
        let (addr, server) = origin(&fixture.app);
        let global = Encoder::call(addr, Some(key(GLOBAL, KeySize::Aes128))).await;
        let plain = Encoder::call(addr, None).await;
        let own = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
        if stream == "legacy" {
            assert!(global.connected && plain.connected, "{stream}");
            // The app key is no key here: only the global one unwraps.
            assert_eq!(own.rejected(), Some(RejectCode::BADSECRET.0));
        } else {
            assert_eq!(global.rejected(), Some(RejectCode::BADSECRET.0));
            assert_eq!(plain.rejected(), Some(RejectCode::UNSECURE.0));
            assert!(own.connected, "{:?}", own.closed);
        }
        for mut encoder in [global, plain, own] {
            encoder.stop().await;
        }
        server.abort();
        let _ = server.await;
    }
    let logs = captured.text();
    for line in [
        "encryption=\"deployment-key\" key_len=16",
        "encryption=\"none\"",
        "encryption=\"app-key\" key_len=32",
    ] {
        assert!(logs.contains(line), "{line}: {logs}");
    }
    assert!(!logs.contains(GLOBAL) && !logs.contains(KEY));
}

#[tokio::test]
async fn a_caller_gone_before_the_app_answered_claims_nothing_and_is_not_counted() {
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let fixture = fixture(&stub, Duration::ZERO);
    let peer: SocketAddr = "127.0.0.1:5000".parse().expect("peer");
    let mut listener = Listener::new(listener_config(&fixture.app.config), Instant::now());
    // More than the throttle's ten: none of them counts.
    for _ in 0..12 {
        let Ok(Admission::Pending(keyed)) =
            crate::srt::admit(&fixture.app, STREAM_ID, peer.ip(), 0).await
        else {
            panic!("expected a keyed admission")
        };
        assert!(accept_pending(&fixture.app, &mut listener, ConnId(7), peer, keyed).is_none());
    }
    assert!(fixture.app.streams.get("event-a").is_none());
    assert_eq!(stub.count(), 12);
    assert!(
        fixture
            .app
            .auth_throttle
            .lock()
            .expect("throttle")
            .allowed(peer.ip(), Instant::now())
    );
    assert_eq!(listener.stats().rejected, 0, "no one to answer");
}

/// Without the header, a caller the listener would refuse is refused before
/// the claim too: an SRT claim takes over the path, so claiming first would
/// knock a live publisher off for a caller that then fails to connect.
#[tokio::test(flavor = "current_thread")]
async fn without_the_header_a_caller_refused_at_accept_never_replaces_a_live_stage() {
    const GLOBAL: &str = "deployment-wide-secret";
    const JUNK: &str = "anything-the-attacker-likes";
    let captured = crate::log_capture::Capture::start();
    let from: std::net::IpAddr = "127.0.0.1".parse().expect("ip");
    /// The live publisher's protocol, required with no global key, and the
    /// attackers with the code each is refused with.
    type Case = (&'static str, bool, Vec<(CryptoConfig, RejectCode)>);
    let cases: [Case; 3] = [
        (
            "rtmp",
            true,
            vec![(key(JUNK, KeySize::Aes128), RejectCode::UNSECURE)],
        ),
        (
            "rtmp",
            false,
            vec![
                (key(JUNK, KeySize::Aes128), RejectCode::BADSECRET),
                (key(GLOBAL, KeySize::Aes256), RejectCode::BADSECRET),
            ],
        ),
        (
            "srt",
            false,
            vec![
                (key(JUNK, KeySize::Aes128), RejectCode::BADSECRET),
                (key(GLOBAL, KeySize::Aes256), RejectCode::BADSECRET),
            ],
        ),
    ];
    for (live_kind, required, attackers) in cases {
        let stub = stub::start(Answer::Status(200)).await;
        let mut fixture = fixture(&stub, Duration::ZERO);
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        if required {
            app.config.srt_require_encryption = true;
        } else {
            app.config.srt_crypto = Some(key(GLOBAL, KeySize::Aes128));
            app.config.srt_allow_plaintext = true;
        }
        let (addr, server) = origin(&fixture.app);
        let mut rtmp = None;
        let mut live = None;
        if live_kind == "rtmp" {
            let req = sapflow_core::PublishRequest::parse_rtmp(
                "live",
                "event-a?user=encoder&pass=pw_live_visible",
            )
            .expect("rtmp");
            rtmp = Some(
                Publication::begin_authorised(&fixture.app, &req, SourceKind::Flv, from)
                    .await
                    .expect("RTMP live"),
            );
        } else {
            let mut encoder = Encoder::call(addr, Some(key(GLOBAL, KeySize::Aes128))).await;
            assert!(encoder.connected, "{:?}", encoder.closed);
            encoder.send_media().await;
            wait_for(&mut fixture, &mut encoder, "publish").await;
            live = Some(encoder);
        }
        let _ = events(&mut fixture);
        let before = stage(&fixture.app);
        assert!(before.owner.1, "{live_kind}: attached");
        assert!(before.recording.is_some());
        for (crypto, code) in attackers {
            let asked = stub.count();
            let attacker = Encoder::call(addr, Some(crypto)).await;
            assert_eq!(attacker.rejected(), Some(code.0), "{live_kind} {required}");
            assert_eq!(stub.count(), asked + 1, "the app was asked first");
            if let Some(live) = &mut live {
                live.pump(Duration::from_millis(50)).await;
            }
        }
        assert_eq!(stage(&fixture.app), before, "{live_kind} {required}");
        let after = events(&mut fixture);
        for name in ["unpublish", "recordSegmentComplete", "publish"] {
            assert!(!after.iter().any(|e| e == name), "{after:?}");
        }
        if let Some(live) = &mut live {
            assert!(live.connected && live.closed.is_none(), "{:?}", live.closed);
            live.stop().await;
        }
        drop(rtmp);
        server.abort();
        let _ = server.await;
    }
    let logs = captured.text();
    assert!(!logs.contains("publisher replaced"), "{logs}");
    assert!(
        logs.contains("neither the app nor the server has a passphrase"),
        "{logs}"
    );
    assert!(logs.contains("key length differs"), "{logs}");
    assert!(!logs.contains(GLOBAL) && !logs.contains(JUNK));
}
