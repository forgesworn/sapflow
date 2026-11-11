use super::*;
use std::path::{Path, PathBuf};

fn seed_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds")
        .join(target)
}

fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = seed_dir(target);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("entry").path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    assert!(!files.is_empty(), "seeds for {target}");
    files
        .into_iter()
        .map(|p| {
            let name = p.file_name().expect("name").to_string_lossy().into_owned();
            (name, std::fs::read(&p).expect("seed"))
        })
        .collect()
}

#[test]
fn record_seeds_frame() {
    for (name, bytes) in seeds("dtls_record") {
        assert!(dtls_record(&bytes) > 0, "{name} frames a record");
    }
}

#[test]
fn message_seeds_parse() {
    for (name, bytes) in seeds("dtls_handshake_messages") {
        assert!(dtls_handshake_messages(&bytes) > 0, "{name} is accepted");
    }
}

/// The server seeds reach past the hello, and the `connect-` ones all the
/// way, so the fuzzer starts deep in the state machine.
#[test]
fn server_seeds_reach_their_stage() {
    for (name, bytes) in seeds("dtls_server") {
        let reached = dtls_server(&bytes);
        let want = if name.starts_with("connect-") {
            Reached::BothConnected
        } else if name.starts_with("openssl-") {
            Reached::Connected
        } else {
            Reached::Handshaking
        };
        assert!(
            reached >= want,
            "{name}: reached {reached:?}, want {want:?}"
        );
    }
}

/// Not a substitute for coverage-guided fuzzing (CI runs cargo-fuzz): a
/// fixed-seed mutation pass over every seed, so the invariants are
/// exercised off the happy path on every test run.
#[test]
fn seeded_mutations_hold_the_invariants() {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for (target, rounds) in [
        ("dtls_record", 64),
        ("dtls_handshake_messages", 64),
        ("dtls_server", 12),
    ] {
        for (_, seed) in seeds(target) {
            for _ in 0..rounds {
                let mut m = seed.clone();
                for _ in 0..1 + next() % 4 {
                    if m.is_empty() {
                        break;
                    }
                    let at = (next() as usize) % m.len();
                    match next() % 4 {
                        0 => m[at] ^= 1 << (next() % 8),
                        1 => m[at] = next() as u8,
                        2 => m.truncate(at),
                        _ => m.insert(at, next() as u8),
                    }
                }
                match target {
                    "dtls_record" => {
                        dtls_record(&m);
                    }
                    "dtls_handshake_messages" => {
                        dtls_handshake_messages(&m);
                    }
                    _ => {
                        dtls_server(&m);
                    }
                }
            }
        }
    }
}

#[test]
fn steps_stop_at_truncation() {
    let mut data = Vec::new();
    push_step(&mut data, 1, b"ab");
    push_step(&mut data, 4, b"");
    data.extend_from_slice(&[0, 0, 9, 1]);
    assert_eq!(
        steps(&data).collect::<Vec<_>>(),
        [(1, &b"ab"[..]), (4, &b""[..])]
    );
    assert_eq!(dtls_server(&[]), Reached::Nothing);
    assert_eq!(dtls_handshake_messages(&[]), 0);
    assert_eq!(dtls_record(&[]), 0);
}

fn honest(rounds: usize) -> Vec<u8> {
    let mut s = Vec::new();
    for _ in 0..rounds {
        for _ in 0..8 {
            push_step(&mut s, 1, &[]);
        }
        push_step(&mut s, 4, &[]);
    }
    s
}

/// The OpenSSL run's datagrams in one direction.
fn openssl_datagrams(run: &str, direction: char) -> Vec<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/openssl")
        .join(format!("{run}.record"));
    std::fs::read_to_string(path)
        .expect("record")
        .lines()
        .filter(|l| l.starts_with(direction))
        .map(|l| testing::unhex(l.split(' ').nth(2).unwrap_or("")))
        .filter(|d| !d.is_empty())
        .collect()
}

/// The handshake messages (type, body) in some datagrams.
fn messages_of(grams: &[Vec<u8>]) -> Vec<(u8, Vec<u8>)> {
    use crate::reassembly::{Limits, Reassembler};
    let mut r = Reassembler::new(Limits {
        max_message: 1 << 16,
        max_fragments: 64,
        max_buffered: 1 << 17,
        ahead: 16,
    });
    let mut out = Vec::new();
    for g in grams {
        for rec in record::records(g) {
            if rec.epoch != 0 || rec.content_type != record::HANDSHAKE {
                continue;
            }
            for f in message::fragments(rec.body) {
                if r.push(0, &f).is_ok() {
                    while let Some(m) = r.pop() {
                        out.push((m.msg_type, m.body));
                    }
                }
            }
        }
    }
    out
}

/// Writes the committed seeds from the W0 captures, the OpenSSL runs and
/// scripted handshakes. Run on demand: `cargo test -p sapflow-dtls --lib
/// -- --ignored write_seeds`.
#[test]
#[ignore = "writes fuzz/seeds"]
fn write_seeds() {
    let write = |target: &str, name: &str, bytes: &[u8]| {
        let d = seed_dir(target);
        std::fs::create_dir_all(&d).expect("dir");
        assert!(bytes.len() <= 65_536, "{name} within CI's -max_len");
        std::fs::write(d.join(name), bytes).expect("write");
    };
    for &(dir, stem, _) in testing::CAPTURES {
        // "chrome-157", "safari-27", "safari-26.6.2" and so on.
        let short = dir.splitn(3, '-').take(2).collect::<Vec<_>>().join("-");
        let tag = if stem.contains("tcp") { "-tcp" } else { "" };
        for (i, g) in testing::capture(dir, stem).iter().enumerate() {
            write("dtls_record", &format!("w0-{short}{tag}-{i}"), g);
        }
        let mut m = vec![0];
        m.extend(testing::capture_hello(dir, stem));
        write(
            "dtls_handshake_messages",
            &format!("hello-{short}{tag}"),
            &m,
        );
        let mut s = vec![0];
        for g in testing::capture(dir, stem) {
            push_step(&mut s, 0, &g);
        }
        for _ in 0..12 {
            push_step(&mut s, 3, &[20]);
        }
        write("dtls_server", &format!("w0-{short}{tag}"), &s);
    }
    for run in ["gcm", "cm80-mtu300-drop"] {
        for (dir, tag) in [('C', "client"), ('S', "server")] {
            for (i, g) in openssl_datagrams(run, dir).iter().enumerate() {
                write("dtls_record", &format!("openssl-{run}-{tag}-{i}"), g);
            }
        }
    }
    let client = messages_of(&openssl_datagrams("gcm", 'C'));
    let ours = messages_of(&openssl_datagrams("gcm", 'S'));
    for (sel, kind, name, from) in [
        (1u8, message::CERTIFICATE, "certificate-openssl", &client),
        (1, message::CERTIFICATE, "certificate-sapflow", &ours),
        (
            2,
            message::CLIENT_KEY_EXCHANGE,
            "client-key-exchange-openssl",
            &client,
        ),
        (
            3,
            message::CERTIFICATE_VERIFY,
            "certificate-verify-openssl",
            &client,
        ),
        (0, message::CLIENT_HELLO, "hello-openssl", &client),
    ] {
        let (_, body) = from.iter().find(|m| m.0 == kind).expect("message");
        let mut m = vec![sel];
        m.extend_from_slice(body);
        write("dtls_handshake_messages", name, &m);
    }
    write("dtls_handshake_messages", "finished", &[4; 13]);
    let mut fp = vec![5];
    fp.extend_from_slice(testing::identity(1).fingerprint().to_sdp().as_bytes());
    write("dtls_handshake_messages", "fingerprint", &fp);

    let mut s = vec![32];
    for g in openssl_datagrams("gcm", 'C') {
        push_step(&mut s, 0, &g);
    }
    write("dtls_server", "openssl-gcm", &s);
    for (name, mode) in [
        ("connect-default", 0u8),
        ("connect-cookie", 1),
        ("connect-mtu256-fragmented", 2 | 8),
        ("connect-cm80", 4),
    ] {
        let mut s = vec![mode];
        s.extend(honest(4));
        write("dtls_server", name, &s);
    }
    let mut s = vec![16];
    s.extend(honest(3));
    write("dtls_server", "mismatch", &s);
    // Loss, timers, replays and a mutation, then the rest honestly.
    let mut s = vec![0];
    push_step(&mut s, 2, &[]);
    push_step(&mut s, 3, &[3]);
    push_step(&mut s, 3, &[3]);
    push_step(&mut s, 4, &[]);
    push_step(&mut s, 1, &[]);
    push_step(&mut s, 5, &[0]);
    push_step(&mut s, 3, &[30]);
    push_step(&mut s, 4, &[]);
    push_step(&mut s, 7, &[]);
    s.extend(honest(4));
    write("dtls_server", "connect-lossy", &s);
    // A full handshake, then records after it: data, a renegotiation
    // attempt and close_notify.
    let mut s = vec![0];
    s.extend(honest(4));
    push_step(&mut s, 6, &[record::APPLICATION_DATA, 1, 2, 3]);
    push_step(
        &mut s,
        6,
        &[
            record::HANDSHAKE,
            1,
            0,
            0,
            2,
            0,
            9,
            0,
            0,
            0,
            0,
            0,
            2,
            0xfe,
            0xfd,
        ],
    );
    push_step(&mut s, 6, &[record::ALERT, 1, 0]);
    push_step(&mut s, 4, &[]);
    write("dtls_server", "connect-then-close", &s);
}
