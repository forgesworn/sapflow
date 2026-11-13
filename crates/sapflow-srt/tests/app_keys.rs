//! A listener with `Config::app_keys` against our own encrypting `Caller`:
//! the application checks each caller's key with its own passphrase, then
//! accepts it strictly, and encrypted media comes through. Also replays the
//! `own_srt_app_keys` fuzz seeds, so they keep reaching a successful check.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::packet::{Control, ControlPacket, HandshakeKind, Packet};
use sapflow_srt::{Caller, CallerConfig, Config, Event, KeyCheckError, Listener};

/// The passphrase the `own_srt_app_keys` fuzz target checks callers with.
const FUZZ_APP: &[u8] = b"own_srt_app_keys fuzz passphrase";
/// That target's global secret in its second mode.
const FUZZ_GLOBAL: &[u8] = b"own_srt_app_keys global passphrase";

fn listener_addr() -> SocketAddr {
    "127.0.0.1:9000".parse().expect("addr")
}

fn caller_addr() -> SocketAddr {
    "192.0.2.1:5000".parse().expect("addr")
}

fn secret(passphrase: &[u8], size: KeySize) -> CryptoConfig {
    CryptoConfig::new(passphrase.to_vec(), size).expect("passphrase")
}

struct Session {
    connected: bool,
    delivered: Vec<Vec<u8>>,
    checks: Vec<Result<KeySize, KeyCheckError>>,
    /// The caller's datagrams as `own_srt_app_keys` records.
    records: Vec<u8>,
    /// No data packet on the wire carried a payload we sent.
    ciphertext_only: bool,
}

/// One caller sending `payloads` to a listener with app keys that checks
/// it twice with `app` and accepts it with `accept_with_key`.
fn session(caller_key: Option<CryptoConfig>, app: &CryptoConfig, payloads: &[Vec<u8>]) -> Session {
    let now = Instant::now();
    let mut c = Caller::new(
        CallerConfig {
            stream_id: "publish:live/gig".into(),
            crypto: caller_key,
            ..CallerConfig::default()
        },
        listener_addr(),
        now,
    )
    .expect("caller");
    let mut l = Listener::new(
        Config {
            app_keys: true,
            ..Config::default()
        },
        now,
    );
    let mut s = Session {
        connected: false,
        delivered: Vec::new(),
        checks: Vec::new(),
        records: Vec::new(),
        ciphertext_only: true,
    };
    let mut last = 0u64;
    let mut enqueued = false;
    for ms in 0..1500u64 {
        let at = now + Duration::from_millis(ms);
        c.handle_timeout(at);
        l.handle_timeout(at);
        while let Some((_, bytes)) = c.poll_transmit(at) {
            if let Ok(Packet::Data(p)) = Packet::parse(&bytes)
                && payloads.iter().any(|sent| sent.as_slice() == p.payload)
            {
                s.ciphertext_only = false;
            }
            let mut step = ms - last;
            while step > 255 {
                s.records.extend_from_slice(&[0x80, 255, 0, 0]);
                step -= 255;
            }
            last = ms;
            s.records.push(0);
            s.records.push(step as u8);
            s.records
                .extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            s.records.extend_from_slice(&bytes);
            l.handle_datagram(at, caller_addr(), &bytes);
        }
        while let Some(e) = l.poll_event() {
            match e {
                Event::Connect { id, .. } => {
                    s.checks.push(l.check_key(id, app));
                    s.checks.push(l.check_key(id, app));
                    l.accept_with_key(id, at, app);
                }
                Event::Data { payload, .. } => s.delivered.push(payload),
                Event::Closed { .. } => {}
            }
        }
        while let Some((_, bytes)) = l.poll_transmit() {
            c.handle_datagram(at, listener_addr(), &bytes);
        }
        if c.is_connected() && !enqueued {
            for p in payloads {
                c.enqueue(at, p.clone()).expect("enqueue");
            }
            enqueued = true;
        }
    }
    s.connected = c.is_connected();
    s
}

fn payloads() -> Vec<Vec<u8>> {
    (0..20u8).map(|n| vec![n; 188]).collect()
}

#[test]
fn a_checked_key_carries_encrypted_media_at_either_length() {
    let app = b"per-stream app passphrase";
    for size in [KeySize::Aes128, KeySize::Aes256] {
        // The caller picks the key length; the app's key_size is ignored.
        for app_size in [KeySize::Aes128, KeySize::Aes256] {
            let s = session(Some(secret(app, size)), &secret(app, app_size), &payloads());
            assert_eq!(s.checks, vec![Ok(size), Ok(size)]);
            assert!(s.connected, "{size:?} caller, {app_size:?} app");
            assert_eq!(s.delivered, payloads());
            assert!(s.ciphertext_only, "media was encrypted on the wire");
        }
    }
}

#[test]
fn a_wrong_or_missing_key_never_connects() {
    let app = secret(b"per-stream app passphrase", KeySize::Aes256);
    let wrong = session(
        Some(secret(b"some other passphrase", KeySize::Aes256)),
        &app,
        &payloads(),
    );
    assert_eq!(
        wrong.checks,
        vec![Err(KeyCheckError::BadSecret), Err(KeyCheckError::BadSecret)]
    );
    assert!(!wrong.connected);
    assert!(wrong.delivered.is_empty());
    let plain = session(None, &app, &payloads());
    assert_eq!(
        plain.checks,
        vec![
            Err(KeyCheckError::NoKeyMaterial),
            Err(KeyCheckError::NoKeyMaterial)
        ]
    );
    assert!(!plain.connected);
    assert!(plain.delivered.is_empty());
}

fn seeds() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/own_srt_app_keys")
}

/// Records the fuzz seeds again (each run makes fresh keys):
/// `cargo test -p sapflow-srt --test app_keys -- --ignored write_fuzz_seeds`.
#[test]
#[ignore]
fn write_fuzz_seeds() {
    std::fs::create_dir_all(seeds()).expect("seed dir");
    for (size, name) in [
        (KeySize::Aes128, "caller-aes128"),
        (KeySize::Aes256, "caller-aes256"),
    ] {
        let s = session(
            Some(secret(FUZZ_APP, size)),
            &secret(FUZZ_APP, KeySize::Aes256),
            &(0..4u8).map(|n| vec![n; 188]).collect::<Vec<_>>(),
        );
        assert!(s.connected);
        std::fs::write(seeds().join(name), &s.records).expect("write seed");
        // The same session in the target's second mode (a global secret
        // with plaintext allowed): the first record's peer bits set to 3.
        let mut optional = s.records;
        optional[0] |= 3;
        std::fs::write(seeds().join(format!("{name}-optional")), optional).expect("write seed");
    }
}

/// The `own_srt_app_keys` harness, cut down to its default path (peer 0,
/// cookie and socket id rewritten, strict accept) in either mode: what each
/// seed's checks returned and how many payloads were delivered.
fn replay(data: &[u8]) -> (Vec<Result<KeySize, KeyCheckError>>, usize) {
    let start = Instant::now();
    let app = secret(FUZZ_APP, KeySize::Aes256);
    let optional = data.first().is_some_and(|c| c & 3 == 3);
    let mut l = Listener::new(
        Config {
            max_connections: 4,
            buffer_packets: 256,
            app_keys: true,
            crypto: optional.then(|| secret(FUZZ_GLOBAL, KeySize::Aes128)),
            allow_plaintext: optional,
            ..Config::default()
        },
        start,
    );
    let peer = caller_addr();
    let mut cookies: HashMap<SocketAddr, u32> = HashMap::new();
    let mut socket = None;
    let (mut checks, mut delivered) = (Vec::new(), 0);
    let mut now = start;
    let mut rest = data;
    while let [ctl, step, hi, lo, tail @ ..] = rest {
        let len = usize::from(u16::from_be_bytes([*hi, *lo])).min(tail.len());
        let (datagram, after) = tail.split_at(len);
        rest = after;
        now += Duration::from_millis(u64::from(*step));
        if ctl & 0x80 != 0 {
            l.handle_timeout(now);
        } else {
            let mut d = datagram.to_vec();
            match Packet::parse(&d) {
                Ok(Packet::Control(ControlPacket {
                    body: Control::Handshake(h),
                    ..
                })) if h.kind == HandshakeKind::Conclusion => {
                    if let (Some(c), Some(f)) = (cookies.get(&peer), d.get_mut(44..48)) {
                        f.copy_from_slice(&c.to_be_bytes());
                    }
                }
                Ok(Packet::Control(ControlPacket {
                    body: Control::Handshake(_),
                    ..
                })) => {}
                _ => {
                    if let (Some(s), Some(f)) = (socket, d.get_mut(12..16)) {
                        f.copy_from_slice(&u32::to_be_bytes(s));
                    }
                }
            }
            l.handle_datagram(now, peer, &d);
        }
        l.handle_timeout(now);
        while let Some(e) = l.poll_event() {
            match e {
                Event::Connect { id, .. } => {
                    checks.push(l.check_key(id, &app));
                    if l.accept_with_key(id, now, &app) {
                        socket = Some(id.0);
                    }
                }
                Event::Data { .. } => delivered += 1,
                Event::Closed { .. } => socket = None,
            }
        }
        while let Some((to, out)) = l.poll_transmit() {
            if let Ok(Packet::Control(ControlPacket {
                body: Control::Handshake(h),
                ..
            })) = Packet::parse(&out)
                && h.kind == HandshakeKind::Induction
            {
                cookies.insert(to, h.cookie);
            }
        }
    }
    (checks, delivered)
}

#[test]
fn fuzz_seeds_reach_a_successful_check_and_encrypted_media() {
    let mut files: Vec<_> = std::fs::read_dir(seeds())
        .expect("seed dir")
        .map(|e| e.expect("entry").path())
        .collect();
    files.sort();
    assert_eq!(files.len(), 4, "{files:?}");
    let sizes = [
        KeySize::Aes128,
        KeySize::Aes128,
        KeySize::Aes256,
        KeySize::Aes256,
    ];
    for (file, size) in files.iter().zip(sizes) {
        let (checks, delivered) = replay(&std::fs::read(file).expect("seed"));
        assert_eq!(checks, vec![Ok(size)], "{file:?}");
        assert!(delivered > 0, "{file:?}");
    }
}
