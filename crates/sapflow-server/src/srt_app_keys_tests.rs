//! `SAPFLOW_SRT_APP_KEYS` end to end: the real SRT loop (`run`) on a loopback
//! socket, our own encrypting `Caller` as the encoder, and a stub app that
//! answers 2xx with a per-stream passphrase. A wrong or missing key must be
//! refused before the path is claimed, so a live or held stage, its
//! recording and its callbacks stay as they were.
//!
//! A captured conclusion handshake replayed from another address or caller
//! socket passes the key check too: the replay guard (`crate::srt_replay`)
//! refuses it before the claim, under the app's passphrase and under the
//! deployment's alike.

use std::time::Duration;

use sapflow_srt::crypto::CryptoConfig;
use sapflow_srt::packet::{Control, ControlPacket, Extension, Handshake, HandshakeKind, Packet};
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
pub(crate) fn origin(app: &Arc<App>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let (socket, pending, got) =
        bind_udp(SocketAddr::from(([127, 0, 0, 1], 0)), RCVBUF_MIN).expect("bind");
    let addr = socket.local_addr().expect("addr");
    let app = app.clone();
    let task = tokio::spawn(async move {
        let _ = run(app, socket, pending, got, 0).await;
    });
    (addr, task)
}

pub(crate) fn key(passphrase: &str, size: KeySize) -> CryptoConfig {
    CryptoConfig::new(passphrase.as_bytes().to_vec(), size).expect("passphrase")
}

/// One encoder over a real UDP socket.
pub(crate) struct Encoder {
    caller: Caller,
    pub(crate) socket: TokioUdp,
    pub(crate) connected: bool,
    pub(crate) closed: Option<CallerCloseReason>,
    /// The last conclusion handshake with key material it sent, as an
    /// eavesdropper captures it.
    pub(crate) conclusion: Option<Vec<u8>>,
    /// Conclusion responses still to drop, as if lost on the way.
    drop_responses: u32,
}

/// Whether `d` is a conclusion handshake (caller's or listener's).
fn is_conclusion(d: &[u8]) -> bool {
    d.len() >= 48 && d.get(..2) == Some(&[0x80, 0x00]) && d.get(36..40) == Some(&[0xFF; 4])
}

impl Encoder {
    /// Calls `origin` and runs the handshake to its end: connected, or why not.
    async fn call(origin: SocketAddr, crypto: Option<CryptoConfig>) -> Self {
        Self::call_dropping(origin, crypto, 0).await
    }

    /// [`call`](Self::call), losing the first `drops` conclusion responses:
    /// the caller resends its conclusion.
    async fn call_dropping(origin: SocketAddr, crypto: Option<CryptoConfig>, drops: u32) -> Self {
        let mut encoder = Self::start(origin, crypto, drops).await;
        encoder
            .run_until(|e| e.connected || e.closed.is_some())
            .await;
        encoder
    }

    /// [`call`](Self::call) with another stream id.
    pub(crate) async fn call_as(
        origin: SocketAddr,
        stream_id: &str,
        crypto: Option<CryptoConfig>,
    ) -> Self {
        let mut encoder = Self::start_as(origin, stream_id, crypto, 0).await;
        encoder
            .run_until(|e| e.connected || e.closed.is_some())
            .await;
        encoder
    }

    /// An encoder for `origin` that has sent nothing yet.
    async fn start(origin: SocketAddr, crypto: Option<CryptoConfig>, drops: u32) -> Self {
        Self::start_as(origin, STREAM_ID, crypto, drops).await
    }

    async fn start_as(
        origin: SocketAddr,
        stream_id: &str,
        crypto: Option<CryptoConfig>,
        drops: u32,
    ) -> Self {
        let socket = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
        let caller = Caller::new(
            CallerConfig {
                stream_id: stream_id.into(),
                crypto,
                ..CallerConfig::default()
            },
            origin,
            Instant::now(),
        )
        .expect("caller");
        Self {
            caller,
            socket,
            connected: false,
            closed: None,
            conclusion: None,
            drop_responses: drops,
        }
    }

    /// Runs the caller's I/O until `done`, for 8 s at most.
    async fn run_until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !done(self) && Instant::now() < deadline {
            self.pump(Duration::from_millis(20)).await;
        }
    }

    /// Runs the caller's I/O for `period`.
    pub(crate) async fn pump(&mut self, period: Duration) {
        let until = Instant::now() + period;
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            self.caller.handle_timeout(now);
            while let Some((to, bytes)) = self.caller.poll_transmit(now) {
                if is_conclusion(&bytes) {
                    self.conclusion = Some(bytes.clone());
                }
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
                if self.drop_responses > 0 && is_conclusion(&buf[..n]) {
                    self.drop_responses -= 1;
                    continue;
                }
                self.caller.handle_datagram(Instant::now(), from, &buf[..n]);
            }
        }
    }

    /// Sends the whole fixture, a datagram at a time.
    pub(crate) async fn send_media(&mut self) {
        for chunk in MEDIA.chunks(1316) {
            while self.caller.enqueue(Instant::now(), chunk.to_vec()).is_err() {
                self.pump(Duration::from_millis(2)).await;
            }
            self.pump(Duration::from_millis(1)).await;
        }
        self.pump(Duration::from_millis(200)).await;
    }

    pub(crate) async fn stop(&mut self) {
        self.caller.stop(Instant::now());
        self.pump(Duration::from_millis(50)).await;
    }

    pub(crate) fn rejected(&self) -> Option<u32> {
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
    let captured = crate::log_capture::Capture::start();
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
    // Refused by the listener before any event, yet still logged.
    let text = captured.text();
    assert!(
        text.contains("publish refused")
            && text.contains("no key material, and encryption is required"),
        "{text}"
    );
    let mut keyed = Encoder::call(addr, Some(key(KEY, KeySize::Aes128))).await;
    assert!(keyed.connected, "{:?}", keyed.closed);
    assert_eq!(stub.count(), 1);
    keyed.stop().await;
    server.abort();
    let _ = server.await;
    drop(captured);
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
    let admission = crate::srt::admit(
        &fixture.app,
        STREAM_ID,
        "127.0.0.1".parse().expect("ip"),
        0,
        None,
        None,
    )
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
            crate::srt::admit(&fixture.app, STREAM_ID, peer.ip(), 0, None, None).await
        else {
            panic!("expected a keyed admission")
        };
        assert!(
            accept_pending(
                &fixture.app,
                &mut listener,
                &mut ReplayGuard::default(),
                ConnId(7),
                peer,
                keyed
            )
            .is_none()
        );
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

/// How the origin answered a replayed conclusion.
#[derive(Debug, PartialEq)]
pub(crate) enum Answered {
    Rejected(u32),
    Accepted,
    Nothing,
}

/// The next handshake the origin sends `socket` for caller socket `id`,
/// within a second; anything else arriving there is dropped.
async fn handshake_for(socket: &TokioUdp, id: u32) -> Option<Handshake> {
    let mut buf = vec![0u8; 2048];
    let until = Instant::now() + Duration::from_secs(1);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        let Ok(Ok(n)) = tokio::time::timeout(left, socket.recv(&mut buf)).await else {
            return None;
        };
        if let Ok(Packet::Control(ControlPacket {
            dest,
            body: Control::Handshake(h),
            ..
        })) = Packet::parse(&buf[..n])
            && dest == id
        {
            return Some(h);
        }
    }
    None
}

/// Replays `captured` from `socket`, as a replayer does: with `caller_socket`
/// as its caller socket id (the captured one if `None`) and, when `induct`,
/// a cookie for its own address from an induction first (the captured
/// cookie stays valid only from the address it was issued to).
pub(crate) async fn replay(
    socket: &TokioUdp,
    origin: SocketAddr,
    captured: &[u8],
    caller_socket: Option<u32>,
    induct: bool,
) -> Answered {
    let mut d = captured.to_vec();
    let id = caller_socket
        .unwrap_or_else(|| u32::from_be_bytes(d[40..44].try_into().expect("socket id field")));
    d[40..44].copy_from_slice(&id.to_be_bytes());
    if induct {
        let mut out = Vec::new();
        ControlPacket::new(
            0,
            0,
            Control::Handshake(Handshake {
                version: 4,
                encryption: 0,
                extension: 2,
                initial_seq: 1000,
                mtu: 1500,
                flow_window: 8192,
                kind: HandshakeKind::Induction,
                socket_id: id,
                cookie: 0,
                peer_ip: [0; 16],
                extensions: vec![],
            }),
        )
        .encode(&mut out);
        socket.send_to(&out, origin).await.expect("send");
        let h = handshake_for(socket, id).await.expect("induction answer");
        d[44..48].copy_from_slice(&h.cookie.to_be_bytes());
    }
    socket.send_to(&d, origin).await.expect("send");
    match handshake_for(socket, id).await.map(|h| h.kind) {
        Some(HandshakeKind::Rejection(code)) => Answered::Rejected(code),
        Some(HandshakeKind::Conclusion) => Answered::Accepted,
        _ => Answered::Nothing,
    }
}

/// The two ways a stage is keyed: the app's passphrase for the stream, and,
/// with no passphrase in the answer, the deployment's own.
fn keyed_fixtures() -> [(&'static str, Answer, CryptoConfig, Option<CryptoConfig>); 2] {
    const GLOBAL: &str = "deployment-wide-secret";
    [
        (
            "app key",
            Answer::Passphrase(&[KEY]),
            key(KEY, KeySize::Aes256),
            None,
        ),
        (
            "deployment key",
            Answer::Status(200),
            key(GLOBAL, KeySize::Aes128),
            Some(key(GLOBAL, KeySize::Aes128)),
        ),
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn a_replayed_conclusion_never_takes_a_live_stage() {
    for (mode, answer, encoder_key, global) in keyed_fixtures() {
        let captured = crate::log_capture::Capture::start();
        let stub = stub::start(answer).await;
        let mut fixture = fixture(&stub, Duration::ZERO);
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.srt_crypto = global;
        let (addr, server) = origin(&fixture.app);

        let mut live = Encoder::call(addr, Some(encoder_key)).await;
        assert!(live.connected, "{mode}: {:?}", live.closed);
        live.send_media().await;
        wait_for(&mut fixture, &mut live, "publish").await;
        let before = stage(&fixture.app);
        assert!(before.owner.1, "attached");
        let conclusion = live.conclusion.clone().expect("captured");

        let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
        for (same_address, caller_socket, what) in [
            (false, None, "another address, same caller socket"),
            (false, Some(0x1234_5678), "another address and socket"),
            (true, Some(0x0765_4321), "same address, another socket"),
        ] {
            // From another address a replayer needs a cookie of its own.
            let (from, induct) = if same_address {
                (&live.socket, false)
            } else {
                (&elsewhere, true)
            };
            let asked = stub.count();
            let answered = replay(from, addr, &conclusion, caller_socket, induct).await;
            assert_eq!(
                answered,
                Answered::Rejected(RejectCode::BADSECRET.0),
                "{mode}: {what}"
            );
            assert_eq!(
                stub.count(),
                asked,
                "{mode}: refused before the app is asked"
            );
            live.pump(Duration::from_millis(50)).await;
        }

        assert_eq!(stage(&fixture.app), before, "{mode}");
        assert!(live.connected && live.closed.is_none(), "{:?}", live.closed);
        let after = events(&mut fixture);
        for name in ["unpublish", "recordSegmentComplete", "publish"] {
            assert!(!after.iter().any(|e| e == name), "{mode}: {after:?}");
        }
        let logs = captured.text();
        assert!(
            logs.contains("key material belongs to another caller"),
            "{logs}"
        );
        assert!(!logs.contains("publisher replaced"), "{logs}");
        // Neither the key material nor its digest is logged.
        let Ok(Packet::Control(ControlPacket {
            body: Control::Handshake(h),
            ..
        })) = Packet::parse(&conclusion)
        else {
            panic!("a conclusion");
        };
        let km = h
            .extensions
            .iter()
            .find_map(|e| match e {
                Extension::KmReq(km) => Some(km.clone()),
                _ => None,
            })
            .expect("key material");
        let bound = sapflow_srt::crypto::bound_key_material(&km).expect("bound part");
        let digest = <sha2::Sha256 as sha2::Digest>::digest(bound);
        let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
        for secret in [hex(&km[16..24]), hex(&bound[..8]), hex(&digest[..8])] {
            assert!(!logs.contains(&secret), "{logs}");
        }
        live.stop().await;
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_replay_never_takes_a_held_stage_and_a_real_reconnect_resumes_it() {
    for (mode, answer, encoder_key, global) in keyed_fixtures() {
        let captured = crate::log_capture::Capture::start();
        let stub = stub::start(answer).await;
        let mut fixture = fixture(&stub, Duration::from_secs(30));
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.srt_crypto = global;
        let (addr, server) = origin(&fixture.app);

        let mut live = Encoder::call(addr, Some(encoder_key.clone())).await;
        assert!(live.connected, "{mode}: {:?}", live.closed);
        live.send_media().await;
        wait_for(&mut fixture, &mut live, "publish").await;
        live.stop().await;
        wait_for(&mut fixture, &mut live, "unpublish").await;
        let held = stage(&fixture.app);
        assert!(!held.owner.1, "held for a reconnect");
        let conclusion = live.conclusion.clone().expect("captured");

        // The encoder's own address and caller socket, after its connection
        // ended, and another address: both refused.
        let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
        for (from, induct, what) in [
            (
                &live.socket,
                false,
                "same address and socket, connection ended",
            ),
            (&elsewhere, true, "another address"),
        ] {
            let answered = replay(from, addr, &conclusion, None, induct).await;
            assert_eq!(
                answered,
                Answered::Rejected(RejectCode::BADSECRET.0),
                "{mode}: {what}"
            );
        }
        assert_eq!(stage(&fixture.app), held, "{mode}");
        let after = events(&mut fixture);
        for name in ["unpublish", "recordSegmentComplete", "publish"] {
            assert!(!after.iter().any(|e| e == name), "{mode}: {after:?}");
        }

        // The encoder reconnects: fresh key material, the hold resumes.
        let mut back = Encoder::call(addr, Some(encoder_key)).await;
        assert!(back.connected, "{mode}: {:?}", back.closed);
        let resumed = stage(&fixture.app);
        assert_eq!(resumed.owner, (held.owner.0 + 1, true), "{mode}");
        assert!(captured.text().contains("resumed=true"));
        back.stop().await;
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_lost_response_resend_is_answered_not_refused() {
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut encoder = Encoder::call_dropping(addr, Some(key(KEY, KeySize::Aes256)), 2).await;
    assert!(encoder.connected, "{:?}", encoder.closed);
    assert_eq!(encoder.drop_responses, 0, "two responses were lost");
    assert_eq!(stub.count(), 1, "one admission");
    encoder.send_media().await;
    wait_for(&mut fixture, &mut encoder, "publish").await;
    assert!(!captured.text().contains("publish refused"));
    let ip = "127.0.0.1".parse().expect("ip");
    {
        let mut throttle = fixture.app.auth_throttle.lock().expect("throttle");
        // The fixture allows ten failures a minute: had a resend counted as
        // one, nine more would reach the limit.
        for _ in 0..9 {
            throttle.failed(ip, Instant::now());
        }
        assert!(throttle.allowed(ip, Instant::now()));
    }
    encoder.stop().await;
    server.abort();
    let _ = server.await;
}

/// Whether `ip` has no failed attempts on the fixture's throttle (ten a
/// minute): with none, nine more still leave it allowed. Uses up nine, so
/// call it last.
pub(crate) fn uncharged(app: &Arc<App>, ip: &str) -> bool {
    let ip = ip.parse().expect("ip");
    let mut throttle = app.auth_throttle.lock().expect("throttle");
    for _ in 0..9 {
        throttle.failed(ip, Instant::now());
    }
    throttle.allowed(ip, Instant::now())
}

#[tokio::test(flavor = "current_thread")]
async fn replays_are_refused_without_asking_the_app_or_charging_the_throttle() {
    // A replayer behind the encoder's own address (one NAT at a venue) must
    // not be able to lock the encoder out by replaying.
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut live = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(live.connected, "{:?}", live.closed);
    let conclusion = live.conclusion.clone().expect("captured");
    let replayer = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
    for attempt in 0..20u32 {
        let answered = replay(&replayer, addr, &conclusion, Some(0x100 + attempt), true).await;
        assert_eq!(
            answered,
            Answered::Rejected(RejectCode::BADSECRET.0),
            "{attempt}"
        );
        live.pump(Duration::from_millis(10)).await;
    }
    assert_eq!(stub.count(), 1, "only the live encoder's admission");
    assert!(uncharged(&fixture.app, "127.0.0.1"));
    let _ = events(&mut fixture);
    live.stop().await;
    server.abort();
    let _ = server.await;
}

/// The captured conclusion with its key material edited where the key
/// check does not look: the first half of the salt, and the key flags
/// (even key to odd).
fn edited(conclusion: &[u8]) -> Vec<u8> {
    let Ok(Packet::Control(ControlPacket {
        timestamp,
        dest,
        body: Control::Handshake(mut h),
    })) = Packet::parse(conclusion)
    else {
        panic!("a conclusion");
    };
    for e in &mut h.extensions {
        if let Extension::KmReq(km) = e {
            km[16] ^= 0x01;
            km[23] ^= 0x80;
            assert!(matches!(km[3], 1 | 2), "one key");
            km[3] = 3 - km[3];
        }
    }
    let mut out = Vec::new();
    ControlPacket::new(timestamp, dest, Control::Handshake(h)).encode(&mut out);
    out
}

#[tokio::test(flavor = "current_thread")]
async fn an_edited_copy_of_the_key_material_is_still_a_replay() {
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut live = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(live.connected, "{:?}", live.closed);
    live.send_media().await;
    wait_for(&mut fixture, &mut live, "publish").await;
    let before = stage(&fixture.app);
    let copy = edited(&live.conclusion.clone().expect("captured"));
    let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
    let answered = replay(&elsewhere, addr, &copy, Some(0x4242), true).await;
    assert_eq!(answered, Answered::Rejected(RejectCode::BADSECRET.0));
    assert_eq!(stub.count(), 1, "refused before the app is asked");
    assert_eq!(stage(&fixture.app), before);
    live.pump(Duration::from_millis(50)).await;
    assert!(live.connected && live.closed.is_none());
    live.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_replay_while_the_original_waits_for_the_app_is_refused_and_the_original_admitted() {
    // The app takes its time, so the original's admission is still in
    // flight while copies of its conclusion arrive from elsewhere.
    let stub = stub::start(Answer::SlowPassphrase(600, &[KEY])).await;
    let mut fixture = fixture(&stub, Duration::ZERO);
    let (addr, server) = origin(&fixture.app);
    let mut original = Encoder::start(addr, Some(key(KEY, KeySize::Aes256)), 0).await;
    original.run_until(|e| e.conclusion.is_some()).await;
    let conclusion = original.conclusion.clone().expect("captured");
    let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
    for socket in [None, Some(0x0bad_cafe), Some(0x0bad_cafd)] {
        let answered = replay(&elsewhere, addr, &conclusion, socket, true).await;
        assert_eq!(
            answered,
            Answered::Rejected(RejectCode::BADSECRET.0),
            "{socket:?}"
        );
    }
    assert!(!original.connected, "still waiting for the app");
    original
        .run_until(|e| e.connected || e.closed.is_some())
        .await;
    assert!(original.connected, "{:?}", original.closed);
    assert_eq!(stub.count(), 1, "only the original asked the app");
    original.send_media().await;
    wait_for(&mut fixture, &mut original, "publish").await;
    assert!(uncharged(&fixture.app, "127.0.0.1"));
    original.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn key_material_from_a_refused_attempt_stays_its_owners() {
    // The encoder's attempt is refused before its key is checked: throttled
    // (someone behind its address used up the allowance) or by the app
    // (the stage is not armed yet). Its key material was never admitted,
    // but a copy of it from elsewhere is still refused, before the app.
    for answer in [Answer::Passphrase(&[KEY]), Answer::Status(401)] {
        let stub = stub::start(answer).await;
        let mut fixture = fixture(&stub, Duration::ZERO);
        let throttled = matches!(answer, Answer::Passphrase(_));
        if throttled {
            let mut throttle = fixture.app.auth_throttle.lock().expect("throttle");
            for _ in 0..10 {
                throttle.failed("127.0.0.1".parse().expect("ip"), Instant::now());
            }
        }
        let (addr, server) = origin(&fixture.app);
        let refused = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
        let code = if throttled { 402 } else { 401 };
        assert_eq!(refused.rejected(), Some(RejectCode::http(code).0));
        let asked = stub.count();
        assert_eq!(asked, usize::from(!throttled));
        let conclusion = refused.conclusion.clone().expect("captured");
        // The window passes, or the stage is armed: the throttle is clear.
        *fixture.app.auth_throttle.lock().expect("throttle") =
            sapflow_core::limits::AuthThrottle::new(10, Duration::from_secs(60));
        let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
        let answered = replay(&elsewhere, addr, &conclusion, None, true).await;
        assert_eq!(
            answered,
            Answered::Rejected(RejectCode::BADSECRET.0),
            "{code}"
        );
        assert_eq!(
            stub.count(),
            asked,
            "{code}: refused before the app is asked"
        );
        assert!(fixture.app.streams.get("event-a").is_none(), "{code}");
        if throttled {
            // The encoder's next attempt, with fresh key material, publishes.
            let mut back = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
            assert!(back.connected, "{:?}", back.closed);
            back.send_media().await;
            wait_for(&mut fixture, &mut back, "publish").await;
            back.stop().await;
        }
        server.abort();
        let _ = server.await;
    }
}

/// An app whose replay guard keeps its admitted tier in the store at `path`.
fn with_store(fixture: &Fixture, path: &std::path::Path) {
    let (store, records) = crate::srt_replay_store::ReplayStore::open(path).expect("store");
    *fixture.app.srt_replay.lock().expect("guard") =
        crate::srt_replay::ReplayGuard::with_store(store, records);
}

#[tokio::test(flavor = "current_thread")]
async fn a_recorded_connection_replayed_after_a_restart_is_refused() {
    // Without the store a restart would forget the admission, and a replay
    // of the recorded handshake would restore its keys: its recorded media
    // would decrypt onto the stage again.
    let scratch = crate::srt_replay_store::tests::Scratch::new();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let conclusion = {
        let mut fixture = fixture(&stub, Duration::ZERO);
        with_store(&fixture, &scratch.path());
        let (addr, server) = origin(&fixture.app);
        let mut live = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
        assert!(live.connected, "{:?}", live.closed);
        live.send_media().await;
        wait_for(&mut fixture, &mut live, "publish").await;
        live.stop().await;
        server.abort();
        let _ = server.await;
        // The process ends: its guard, and the store's lock, go with it.
        *fixture.app.srt_replay.lock().expect("guard") = Default::default();
        live.conclusion.clone().expect("captured")
    };
    let asked = stub.count();
    let fixture = fixture(&stub, Duration::ZERO);
    with_store(&fixture, &scratch.path());
    let (addr, server) = origin(&fixture.app);
    let elsewhere = TokioUdp::bind("127.0.0.1:0").await.expect("bind");
    let answered = replay(&elsewhere, addr, &conclusion, None, true).await;
    assert_eq!(answered, Answered::Rejected(RejectCode::BADSECRET.0));
    assert_eq!(stub.count(), asked, "refused before the app is asked");
    assert!(fixture.app.streams.get("event-a").is_none(), "no stage");
    // A real encoder, with fresh key material, publishes as before.
    let mut back = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert!(back.connected, "{:?}", back.closed);
    back.stop().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_caller_is_refused_when_the_store_cannot_be_written() {
    let scratch = crate::srt_replay_store::tests::Scratch::new();
    let stub = stub::start(Answer::Passphrase(&[KEY])).await;
    let fixture = fixture(&stub, Duration::ZERO);
    let (mut store, records) =
        crate::srt_replay_store::ReplayStore::open(&scratch.path()).expect("store");
    store.fail_before_rename = true;
    *fixture.app.srt_replay.lock().expect("guard") =
        crate::srt_replay::ReplayGuard::with_store(store, records);
    let (addr, server) = origin(&fixture.app);
    let refused = Encoder::call(addr, Some(key(KEY, KeySize::Aes256))).await;
    assert_eq!(refused.rejected(), Some(RejectCode::http(503).0));
    assert!(
        fixture.app.streams.get("event-a").is_none(),
        "nothing claimed"
    );
    server.abort();
    let _ = server.await;
}

/// Without app keys, in global-key mode: an app that admits on the 2xx, and
/// a deployment passphrase with `required` or `optional`.
fn global_fixture(stub: &Stub, global: &str, required: bool) -> Fixture {
    let mut fixture = Fixture::new();
    let app = Arc::get_mut(&mut fixture.app).expect("sole App");
    app.config.publish_auth = Some(stub.endpoint.clone());
    app.config.reconnect_grace = Duration::ZERO;
    app.config.srt_crypto = Some(key(global, KeySize::Aes128));
    app.config.srt_allow_plaintext = !required;
    app.config.srt_require_encryption = required;
    assert!(!app.config.srt_app_keys);
    fixture
}

/// With app keys off and a global passphrase, a caller with valid stream-id
/// credentials whose key the listener would refuse is refused before the
/// claim: the wrong passphrase or key length used to take a live RTMP or SRT
/// stage over first, then fail to connect. The right key still takes over.
#[tokio::test(flavor = "current_thread")]
async fn without_app_keys_a_wrong_global_key_never_replaces_a_live_stage() {
    const GLOBAL: &str = "deployment-wide-secret";
    const WRONG_GLOBAL: &str = "not-the-deployment-secret";
    let from: std::net::IpAddr = "127.0.0.1".parse().expect("ip");
    for live_kind in ["rtmp", "srt"] {
        for required in [true, false] {
            let captured = crate::log_capture::Capture::start();
            let stub = stub::start(Answer::Status(200)).await;
            let mut fixture = global_fixture(&stub, GLOBAL, required);
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
            assert!(before.owner.1 && before.recording.is_some(), "{live_kind}");
            let label = format!("{live_kind} required={required}");
            // (a) the wrong passphrase, (b) the wrong key length: the app is
            // asked, then the key refuses the caller. Under required the
            // listener announces its key length, so a caller like ours (and
            // libsrt) with another one gives up before it is asked about;
            // under optional it is not announced and the key check refuses.
            for (crypto, wrong_length) in [
                (key(WRONG_GLOBAL, KeySize::Aes128), false),
                (key(GLOBAL, KeySize::Aes256), true),
            ] {
                let asked = stub.count();
                let attacker = Encoder::call(addr, Some(crypto)).await;
                if wrong_length && required {
                    assert_eq!(attacker.closed, Some(CallerCloseReason::UnsupportedPeer));
                    assert_eq!(stub.count(), asked, "{label}");
                } else {
                    assert_eq!(
                        attacker.rejected(),
                        Some(RejectCode::BADSECRET.0),
                        "{label}: {:?}",
                        attacker.closed
                    );
                    assert_eq!(stub.count(), asked + 1, "{label}: the app was asked first");
                }
                if let Some(live) = &mut live {
                    live.pump(Duration::from_millis(50)).await;
                }
            }
            // (c) no key material under required: the listener announces
            // encryption, so our caller gives up at once, and one that sent
            // its conclusion anyway is refused in the handshake (UNSECURE),
            // before the app is asked, as on main.
            if required {
                let asked = stub.count();
                let plain = Encoder::call(addr, None).await;
                assert!(!plain.connected, "{label}");
                assert!(
                    matches!(
                        plain.closed,
                        Some(
                            CallerCloseReason::UnsupportedPeer | CallerCloseReason::Rejected(1011)
                        )
                    ),
                    "{label}: {:?}",
                    plain.closed
                );
                assert_eq!(stub.count(), asked, "{label}");
            }
            assert_eq!(stage(&fixture.app), before, "{label}");
            let after = events(&mut fixture);
            for name in ["unpublish", "recordSegmentComplete", "publish"] {
                assert!(!after.iter().any(|e| e == name), "{label}: {after:?}");
            }
            if let Some(live) = &mut live {
                assert!(live.connected && live.closed.is_none(), "{:?}", live.closed);
            }
            let logs = captured.text();
            assert!(!logs.contains("publisher replaced"), "{label}: {logs}");
            assert!(logs.contains("reason=BadSecret"), "{label}: {logs}");
            assert_eq!(
                logs.contains("key length differs"),
                !required,
                "{label}: {logs}"
            );
            // The right key still takes the stage over, as today.
            let mut successor = Encoder::call(addr, Some(key(GLOBAL, KeySize::Aes128))).await;
            assert!(successor.connected, "{label}: {:?}", successor.closed);
            assert_eq!(
                stage(&fixture.app).owner,
                (before.owner.0 + 1, true),
                "{label}"
            );
            let logs = captured.text();
            assert!(logs.contains("publisher replaced"), "{label}");
            assert!(
                logs.contains("encryption=\"deployment-key\" key_len=16"),
                "{label}"
            );
            assert!(!logs.contains(GLOBAL) && !logs.contains(WRONG_GLOBAL));
            successor.stop().await;
            if let Some(live) = &mut live {
                live.stop().await;
            }
            drop(rtmp);
            server.abort();
            let _ = server.await;
            drop(captured);
        }
    }
}

/// Plaintext under `optional`, without app keys: admitted after the check as
/// before, and logged as such.
#[tokio::test(flavor = "current_thread")]
async fn without_app_keys_optional_still_admits_plaintext() {
    let captured = crate::log_capture::Capture::start();
    let stub = stub::start(Answer::Status(200)).await;
    let mut fixture = global_fixture(&stub, "deployment-wide-secret", false);
    let (addr, server) = origin(&fixture.app);
    let mut plain = Encoder::call(addr, None).await;
    assert!(plain.connected, "{:?}", plain.closed);
    plain.send_media().await;
    wait_for(&mut fixture, &mut plain, "publish").await;
    assert_eq!(stub.last().get("srtKey"), None, "no capability field");
    assert!(captured.text().contains("encryption=\"none\""));
    plain.stop().await;
    server.abort();
    let _ = server.await;
}
