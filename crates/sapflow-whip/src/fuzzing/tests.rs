use super::*;
use crate::testing::{StunClient, fixture};
use std::path::PathBuf;

fn seed_dir(target: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds")
        .join(target)
}

fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(seed_dir(target))
        .expect("seed directory")
        .map(|e| {
            let p = e.expect("entry").path();
            let name = p.file_name().expect("name").to_string_lossy().into_owned();
            (name, std::fs::read(&p).expect("seed"))
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "seeds for {target}");
    out
}

#[test]
fn stun_seeds_reach_the_agent() {
    for (name, bytes) in seeds("stun_message") {
        let answered = stun_message(&bytes);
        let want = name.starts_with("honest-");
        assert_eq!(answered, want, "{name}");
    }
}

#[test]
fn sdp_seeds_are_answered() {
    for (name, bytes) in seeds("sdp_offer") {
        let refused = name.contains("passive");
        assert_eq!(sdp_offer(&bytes), !refused, "{name}");
    }
}

#[test]
fn frame_seeds_decode() {
    for (name, bytes) in seeds("rfc4571_frames") {
        assert!(rfc4571_frames(&bytes) > 0, "{name}");
    }
}

/// The session seeds reach their stage: the `connect-` ones a connected
/// session, the `end-` ones an ended one.
#[test]
fn session_seeds_reach_their_stage() {
    for (name, bytes) in seeds("whip_session") {
        let stage = whip_session(&bytes);
        if name.starts_with("connect-") {
            assert!(stage >= 2, "{name}: {stage}");
        }
        if name.starts_with("end-") {
            assert_eq!(stage, 3, "{name}");
        }
    }
}

/// The forgotten-and-copied seed does what it says: forgotten checks
/// validate the third tuple, copies draw sends to it, and the budget holds.
#[test]
fn forgotten_and_copied_reaches_the_third_tuple() {
    let (_, bytes) = seeds("whip_session")
        .into_iter()
        .find(|(n, _)| n == "connect-forgotten-and-copied")
        .expect("seed");
    let run = whip_session_run(&bytes);
    assert_eq!(run.stage, 2);
    assert!(run.to_third > 0, "{run:?}");
    assert!(run.to_third <= 4 * run.from_third, "{run:?}");
}

/// The eviction seed evicts, under the eviction oracle.
#[test]
fn connect_evict_evicts() {
    let (_, bytes) = seeds("whip_session")
        .into_iter()
        .find(|(n, _)| n == "connect-evict")
        .expect("seed");
    let run = whip_session_run(&bytes);
    assert_eq!(run.stage, 2);
    assert!(run.evicted > 0, "{run:?}");
}

/// Scripts for [`whip_router`]: what the committed seeds hold.
fn router_scripts() -> Vec<(&'static str, Vec<u8>)> {
    use router_op as r;
    let connect = |s: &mut Vec<u8>, k: u8| {
        push_step(s, r::CHECK, &[k | 2]);
        for _ in 0..3 {
            push_step(s, r::DTLS, &[k]);
        }
    };
    let mut both = Vec::new();
    connect(&mut both, 0);
    connect(&mut both, 1);
    for i in 0u8..12 {
        push_step(&mut both, r::MEDIA, &[(i & 1) | ((i % 3) << 1)]);
    }
    push_step(&mut both, r::RAW, &[0, 1, 2, 3]);
    push_step(&mut both, r::RAW, &[3, 0x80, 0x6f, 0, 9, 0, 0, 0, 1]);
    push_step(&mut both, r::TIME, &[8]);
    let mut copies = both.clone();
    for (who, pick) in [(0u8, 0u8), (1, 3), (2, 7), (3, 11), (4, 5), (5, 20)] {
        push_step(&mut copies, r::COPY, &[who, pick]);
    }
    push_step(&mut copies, r::MEDIA, &[0]);
    push_step(&mut copies, r::MEDIA, &[1]);
    let mut cross = Vec::new();
    connect(&mut cross, 0);
    connect(&mut cross, 1);
    push_step(&mut cross, r::MEDIA, &[0]);
    // Publisher 0's check from publisher 1's tuple, then media from both.
    push_step(&mut cross, r::CHECK, &[2 | 4]);
    push_step(&mut cross, r::MEDIA, &[1]);
    push_step(&mut cross, r::MEDIA, &[0]);
    push_step(&mut cross, r::CLOSE, &[1]);
    push_step(&mut cross, r::MEDIA, &[0]);
    push_step(&mut cross, r::TIME, &[40]);
    let mut raw = Vec::new();
    // Checks for both sessions, unauthenticated, from strangers.
    for b in [0x08u8, 0x19, 0x2a, 0x3b] {
        push_step(&mut raw, r::CHECK, &[b]);
    }
    for b in 0u8..8 {
        push_step(
            &mut raw,
            r::RAW,
            &[b, 0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42],
        );
        push_step(&mut raw, r::RAW, &[b, 0x16, 0xfe, 0xfd, 0, 0, 0, 0, 0, 0]);
    }
    // Past both handshake deadlines: the sessions end and are removed.
    push_step(&mut raw, r::TIME, &[255]);
    push_step(&mut raw, r::TIME, &[255]);
    vec![
        ("connect-both-and-publish", both),
        ("connect-both-and-copies", copies),
        ("cross-check-and-close", cross),
        ("strangers-only", raw),
    ]
}

/// Unauthenticated checks from a tuple never validated draw nothing in
/// the session harness, before and after the publisher connects.
#[test]
fn unauthenticated_checks_draw_nothing() {
    let mut s = Vec::new();
    push_step(&mut s, op::CHECK, &[0x08 | 2]);
    connect(&mut s);
    push_step(&mut s, op::CHECK, &[0x38 | 2]);
    push_step(&mut s, op::RTP, &[2]);
    let run = whip_session_run(&s);
    assert_eq!(run.stage, 2);
}

/// The router scripts reach what they are for, with every invariant held.
#[test]
fn router_scripts_hold_the_invariants() {
    for (name, script) in router_scripts() {
        let run = whip_router_run(&script);
        match name {
            "strangers-only" => {
                assert_eq!(run.connected, 0, "{name}");
                assert_eq!(run.to_strangers, 0, "{name}");
            }
            _ => assert_eq!(run.connected, 2, "{name}: {run:?}"),
        }
    }
}

/// The committed router seeds are the scripts above.
#[test]
fn router_seeds_are_the_scripts() {
    let committed = seeds("whip_router");
    for (name, script) in router_scripts() {
        let seed = committed
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("seed {name}"));
        assert_eq!(seed.1, script, "{name}: run write_router_seeds");
    }
}

/// Writes the [`whip_router`] seeds. Run with
/// `cargo test -p sapflow-whip --features testing -- --ignored write_router_seeds`.
#[test]
#[ignore = "writes fuzz/seeds/whip_router"]
fn write_router_seeds() {
    let dir = seed_dir("whip_router");
    std::fs::create_dir_all(&dir).expect("dir");
    for (name, script) in router_scripts() {
        std::fs::write(dir.join(name), script).expect("write");
    }
}

/// A fixed-seed mutation pass over every seed, with the unit tests.
#[test]
fn seeded_mutations_hold_the_invariants() {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    type Target = (&'static str, fn(&[u8]));
    let targets: [Target; 5] = [
        ("stun_message", |d| {
            stun_message(d);
        }),
        ("sdp_offer", |d| {
            sdp_offer(d);
        }),
        ("rfc4571_frames", |d| {
            rfc4571_frames(d);
        }),
        ("whip_session", |d| {
            whip_session(d);
        }),
        ("whip_router", |d| {
            whip_router(d);
        }),
    ];
    for (target, run) in targets {
        let rounds = if target.starts_with("whip_") { 6 } else { 48 };
        for (_, seed) in seeds(target) {
            for _ in 0..rounds {
                let mut m = seed.clone();
                for _ in 0..1 + next() % 4 {
                    if m.is_empty() {
                        break;
                    }
                    let i = (next() as usize) % m.len();
                    m[i] ^= 1 << (next() % 8);
                }
                if next() % 4 == 0 {
                    m.truncate((next() as usize) % (m.len() + 1));
                }
                run(&m);
            }
        }
    }
}

#[test]
fn steps_stop_at_truncation() {
    let mut d = Vec::new();
    push_step(&mut d, 1, b"abc");
    push_step(&mut d, 2, b"");
    d.extend([3, 0, 9, 1]);
    let got: Vec<_> = steps(&d).collect();
    assert_eq!(got, [(1, &b"abc"[..]), (2, &b""[..])]);
}

fn honest_attributes(extra: &[(u16, &[u8])]) -> Vec<u8> {
    let mut b = Builder::new(stun::BINDING_REQUEST, &[0; 12]);
    for (t, v) in extra {
        b = b.attribute(*t, v);
    }
    b.finish()[stun::HEADER_LEN..].to_vec()
}

fn framed(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in packets {
        framing::encode(&Framing::default(), p, &mut out).expect("fits");
    }
    out
}

fn connect(s: &mut Vec<u8>) {
    push_step(s, op::CHECK, &[1]);
    push_step(s, op::DTLS, &[]);
    push_step(s, op::DTLS, &[]);
}

/// Writes the committed seeds from the RFC 5769 vectors, the W0 offers and
/// captures, the recorded Chrome run and scripted sessions. Run with
/// `cargo test -p sapflow-whip --features testing -- --ignored write_seeds`.
#[test]
#[ignore = "writes fuzz/seeds"]
fn write_seeds() {
    let write = |target: &str, name: &str, bytes: &[u8]| {
        let dir = seed_dir(target);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(name), bytes).expect("write");
    };
    // STUN: the RFC 5769 vectors raw, and honest checks.
    let hex = |s: &str| testing::unhex(&s.split_whitespace().collect::<String>());
    for (name, v) in [
        ("raw-rfc5769-request", crate::stun::tests_vectors::REQUEST),
        (
            "raw-rfc5769-response-v4",
            crate::stun::tests_vectors::RESPONSE_V4,
        ),
        (
            "raw-rfc5769-response-v6",
            crate::stun::tests_vectors::RESPONSE_V6,
        ),
        (
            "raw-rfc5769-long-term",
            crate::stun::tests_vectors::LONG_TERM,
        ),
    ] {
        let mut b = vec![0];
        b.extend(hex(v));
        write("stun_message", name, &b);
    }
    let chrome: Vec<(u16, &[u8])> = vec![
        (0xc057, &[0, 1, 0, 0x0a]),
        (stun::attr::PRIORITY, &[0x6e, 0x7f, 0x1e, 0xff]),
        (stun::attr::ICE_CONTROLLING, &[1, 2, 3, 4, 5, 6, 7, 8]),
        (stun::attr::USE_CANDIDATE, &[]),
    ];
    let mut b = vec![128];
    b.extend(honest_attributes(&chrome));
    write("stun_message", "honest-chrome-check", &b);
    b[0] = 129;
    write("stun_message", "honest-chrome-check-v6", &b);
    let mut b = vec![130];
    b.extend(honest_attributes(&[(stun::attr::ICE_CONTROLLED, &[9; 8])]));
    write("stun_message", "honest-controlled-487", &b);
    let mut b = vec![132];
    b.extend(honest_attributes(&[(0x0033, b"unknown-attr")]));
    write("stun_message", "honest-unknown-420", &b);
    write("stun_message", "honest-bare", &[134]);
    // A raw check whose integrity is wrong (signed with another key).
    let mut c = StunClient::new("abcd", "the-wrong-password-for-it", "efgh");
    let mut b = vec![0];
    b.extend(c.check(true));
    write("stun_message", "raw-wrong-key", &b);

    // SDP: every W0 offer, the recorded Chrome offer and the synthetic one.
    for (dir, file) in [
        ("chrome-157-desktop-macos", "offer.sdp"),
        ("chrome-157-desktop-macos", "offer-tcp-run.sdp"),
        ("firefox-160-desktop-macos", "offer.sdp"),
        ("safari-26.6.2-macos", "offer.sdp"),
        ("safari-26.6.2-macos", "offer-tcp-run.sdp"),
        ("safari-27-ios", "offer.sdp"),
        ("obs-32.0.4-macos", "offer.sdp"),
        ("obs-32.0.4-macos", "offer-tcp-run.sdp"),
        ("ffmpeg-8.1-reference", "offer-dtls-active.sdp"),
        ("ffmpeg-8.1-reference", "offer-default.sdp"),
        ("vanadium-157-android-grapheneos", "offer-tcp-run.sdp"),
        ("vanadium-157-android-grapheneos", "offer-udp-run.sdp"),
    ] {
        let name = format!("w0-{dir}-{file}").replace("offer-default", "offer-default-passive");
        write(
            "sdp_offer",
            &name,
            fixture(&format!("{dir}/{file}")).as_bytes(),
        );
    }
    let recorded = std::fs::read(testing::crate_fixture("chrome-157/offer.sdp")).expect("offer");
    write("sdp_offer", "w3a-chrome-157-harness-offer.sdp", &recorded);
    let synthetic = testing::synthetic_offer(&Fingerprint::from_bytes([1; 32]), "abcd");
    write("sdp_offer", "synthetic.sdp", synthetic.as_bytes());

    // RFC 4571: the W0 ClientHellos that came over TCP, framed, with a
    // check and an RTP packet around them.
    let mut c = StunClient::new("abcd", "0123456789abcdefghijklmn", "efgh");
    for (dir, chunk) in [
        ("chrome-157-desktop-macos", 1u8),
        ("safari-26.6.2-macos", 7),
        ("vanadium-157-android-grapheneos", 255),
    ] {
        let mut packets = vec![c.check(true), Vec::new()];
        packets.extend(sapflow_dtls::testing::capture(dir, "clienthello-over-tcp"));
        packets.push(vec![0x80; 60]);
        let mut b = vec![chunk];
        b.extend(framed(&packets));
        write(
            "rfc4571_frames",
            &format!("w0-{dir}-clienthello-over-tcp"),
            &b,
        );
    }

    // Sessions.
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::RTCP, &[0]);
    push_step(&mut s, op::RTP, &[1]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::TIME, &[0, 40]);
    push_step(&mut s, op::RTP, &[0x00, 0x41, 0x9a, 1, 2, 3]);
    push_step(&mut s, op::RTP, &[6]);
    push_step(&mut s, op::TIME, &[0x01, 0xf4]);
    push_step(&mut s, op::RTP, &[3, 0x41, 0x9a, 7]);
    push_step(&mut s, op::RTCP, &[1, 0x81, 0xc9, 0, 1, 0, 0, 0, 1]);
    push_step(&mut s, op::TIME, &[0x07, 0xd0]);
    write("whip_session", "connect-and-publish", &s);
    let mut s = Vec::new();
    push_step(&mut s, op::CHECK, &[0]);
    push_step(&mut s, op::DTLS, &[]);
    push_step(&mut s, op::CHECK, &[1]);
    push_step(&mut s, op::DTLS, &[]);
    push_step(&mut s, op::RTP, &[1]);
    push_step(&mut s, op::TIME, &[4]);
    write("whip_session", "connect-early-dtls", &s);
    let mut s = Vec::new();
    push_step(&mut s, op::CONTROLLED, &[1]);
    connect(&mut s);
    write("whip_session", "connect-after-role-conflict", &s);
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::CHECK, &[2]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::CHECK, &[3]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::RAW_B, &[0x80, 0x6f, 0, 1]);
    push_step(&mut s, op::TIME, &[40]);
    write("whip_session", "connect-move-tuple", &s);
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::REPLAY, &[1]);
    push_step(&mut s, op::REPLAY, &[2]);
    push_step(&mut s, op::RAW_A, &[0x17, 0xfe, 0xfd, 0, 1]);
    write("whip_session", "connect-replays", &s);
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::TIME, &[0x75, 0x30]);
    write("whip_session", "end-consent-expiry", &s);
    let mut s = Vec::new();
    push_step(&mut s, op::CHECK, &[1]);
    push_step(&mut s, op::DTLS, &[]);
    push_step(&mut s, op::TIME, &[0x27, 0x10]);
    write("whip_session", "end-handshake-timeout", &s);
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::CLOSE, &[]);
    push_step(&mut s, op::RTP, &[2]);
    write("whip_session", "end-close", &s);
    let mut s = Vec::new();
    push_step(&mut s, op::RAW_A, &[0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42]);
    push_step(&mut s, op::RAW_B, &[0x16, 0xfe, 0xff, 0, 0, 0, 0]);
    push_step(&mut s, op::CHECK, &[1]);
    push_step(&mut s, op::DTLS, &[0x03, 0xff, 0x00, 0x10]);
    push_step(&mut s, op::DTLS, &[]);
    write("whip_session", "stranger-then-mutated-dtls", &s);
    // Captured checks re-sent from a third tuple, before and after SRTP.
    let mut s = Vec::new();
    push_step(&mut s, op::CHECK, &[1]);
    push_step(&mut s, op::ELSEWHERE, &[0]);
    push_step(&mut s, op::DTLS, &[]);
    push_step(&mut s, op::DTLS, &[]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::CHECK, &[1]);
    push_step(&mut s, op::ELSEWHERE, &[1]);
    push_step(&mut s, op::TIME, &[40]);
    push_step(&mut s, op::RTP, &[2]);
    write("whip_session", "connect-replays-elsewhere", &s);
    // More checks than the ID cache holds, then old ones re-sent from the
    // third tuple (forgotten, so they validate it), and copies of fresh
    // SRTP and SRTCP from it ahead of and after their originals.
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::CHECK, &[0xf0]);
    push_step(&mut s, op::CHECK, &[0x31]);
    push_step(&mut s, op::ELSEWHERE, &[0, 0]);
    push_step(&mut s, op::ELSEWHERE, &[0, 1]);
    push_step(&mut s, op::ELSEWHERE, &[1]);
    push_step(&mut s, op::TIME, &[2]);
    push_step(&mut s, op::ELSEWHERE, &[2]);
    push_step(&mut s, op::ELSEWHERE, &[3]);
    push_step(&mut s, op::ELSEWHERE, &[7]);
    push_step(&mut s, op::TIME, &[0, 200]);
    push_step(&mut s, op::ELSEWHERE, &[0x81]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::TIME, &[0x0b, 0xb8]);
    write("whip_session", "connect-forgotten-and-copied", &s);
    // The eviction attack: forgotten checks, a forgotten nominating copy
    // from B, a copy of fresh SRTP from B ahead of its original, then
    // forgotten copies from new tuples into a full table. The publisher's
    // tuple, which delivered media more recently than the copiers, must
    // stay, and its next packet must count.
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::CHECK, &[0xf0]);
    push_step(&mut s, op::CHECK, &[0xf1]);
    push_step(&mut s, op::ELSEWHERE, &[0x80, 0]);
    push_step(&mut s, op::ELSEWHERE, &[0x81]);
    push_step(&mut s, op::ELSEWHERE, &[0x00, 1]);
    push_step(&mut s, op::ELSEWHERE, &[0x20, 2]);
    push_step(&mut s, op::ELSEWHERE, &[0x40, 3]);
    push_step(&mut s, op::RTP, &[2]);
    push_step(&mut s, op::TIME, &[4]);
    write("whip_session", "connect-evict", &s);
    // RTCP too short to protect, which the harness once unwrapped.
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::RTCP, &[1, 0x81, 0xc9, 0]);
    push_step(&mut s, op::RTP, &[2]);
    write("whip_session", "connect-short-rtcp", &s);
}

/// A longer mutation pass than the unit tests run: bit flips, byte sets,
/// inserts, deletes, truncation and splices between seeds. For hosts where
/// libFuzzer cannot run. `WHIP_SOAK_ROUNDS` sets the rounds per seed
/// (default 2000; a tenth of that for the session).
#[test]
#[ignore = "a long mutation run"]
fn mutation_soak() {
    let rounds: usize = std::env::var("WHIP_SOAK_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(2000);
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    type Target = (&'static str, fn(&[u8]));
    let targets: [Target; 5] = [
        ("stun_message", |d| {
            stun_message(d);
        }),
        ("sdp_offer", |d| {
            sdp_offer(d);
        }),
        ("rfc4571_frames", |d| {
            rfc4571_frames(d);
        }),
        ("whip_session", |d| {
            whip_session(d);
        }),
        ("whip_router", |d| {
            whip_router(d);
        }),
    ];
    let mut runs = 0usize;
    for (target, run) in targets {
        let all = seeds(target);
        let n = if target.starts_with("whip_") {
            rounds / 10
        } else {
            rounds
        };
        for (_, seed) in &all {
            for _ in 0..n {
                let mut m = seed.clone();
                for _ in 0..1 + next() % 8 {
                    let len = m.len();
                    let at = if len == 0 { 0 } else { (next() as usize) % len };
                    match next() % 7 {
                        0 if len > 0 => m[at] ^= 1 << (next() % 8),
                        1 if len > 0 => m[at] = next() as u8,
                        2 => m.insert(at, next() as u8),
                        3 if len > 0 => {
                            m.remove(at);
                        }
                        4 => m.truncate(at),
                        5 if len > 0 => {
                            let end = (at + 1 + (next() as usize) % 16).min(len);
                            let chunk = m[at..end].to_vec();
                            m.splice(at..at, chunk);
                        }
                        6 => {
                            let (_, other) = &all[(next() as usize) % all.len()];
                            if !other.is_empty() {
                                let from = (next() as usize) % other.len();
                                m.truncate(at);
                                m.extend_from_slice(&other[from..]);
                            }
                        }
                        _ => {}
                    }
                }
                run(&m);
                runs += 1;
            }
        }
    }
    println!("mutation soak: {runs} runs");
}

/// RTCP of fewer than 8 bytes cannot be protected (no SSRC): the harness
/// must skip it, not panic. One byte away from `connect-and-publish`.
#[test]
fn short_rtcp_after_connect_is_safe() {
    let mut s = Vec::new();
    connect(&mut s);
    push_step(&mut s, op::RTCP, &[1, 0x81, 0xc9, 0]);
    push_step(&mut s, op::RTCP, &[1, 0x81, 0xc9, 0, 1, 2, 3]);
    assert_eq!(whip_session(&s), 2);
}
