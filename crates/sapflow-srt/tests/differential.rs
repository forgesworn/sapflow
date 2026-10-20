//! Differential test: each recorded publish (`fixtures/*.trace`, the
//! caller's datagrams with their timing) replayed through our listener must
//! deliver the payloads `srt-protocol` delivers for the same trace
//! (`fixtures/*.expected`, made by `crates/sapflow-server/tests/
//! srt_protocol_reference.rs`): same payloads, same order, same lengths.
//! Where they differ under loss, libsrt's own delivery of the recorded
//! session decides (`*.libsrt-expected`).
//!
//! The traces, recorded through a UDP relay on 2026-10-20 (ffmpeg lavfi
//! test signal, AAC tone, 4 s):
//! - `srt-live-transmit-publish`: libsrt 1.5.4's `srt-live-transmit`,
//!   160x90, latency 2000 ms, no loss.
//! - `ffmpeg-publish`: ffmpeg 8.1.2 with libsrt 1.5.3, 160x90, latency
//!   2000 ms, no loss.
//! - `srt-live-transmit-lossy-200ms`: 320x180, latency 200 ms, through
//!   `srt-loss-proxy` (20% random loss each way, 30 ms each way, seed 7):
//!   what reached the receiver, retransmissions included.

use std::hash::{BuildHasherDefault, DefaultHasher};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sapflow_srt::packet::{Control, ControlPacket, HandshakeKind, Packet};
use sapflow_srt::{Config, Event, Listener, Stats};

type Fixed = BuildHasherDefault<DefaultHasher>;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Records: microseconds since the first (u32), length (u16), datagram,
/// all big-endian.
fn read_trace(name: &str) -> Vec<(u64, Vec<u8>)> {
    let b = std::fs::read(fixtures().join(format!("{name}.trace"))).expect("trace");
    let mut out = Vec::new();
    let mut rest = b.as_slice();
    while rest.len() >= 6 {
        let t = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let len = usize::from(u16::from_be_bytes([rest[4], rest[5]]));
        out.push((u64::from(t), rest[6..6 + len].to_vec()));
        rest = &rest[6 + len..];
    }
    out
}

fn summarise(payloads: &[Vec<u8>]) -> String {
    payloads
        .iter()
        .map(|p| {
            let h = p.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01B3)
            });
            format!("{} {h:016x}\n", p.len())
        })
        .collect()
}

/// What our listener delivers for the trace. Like the caller did, the
/// conclusion carries the cookie we handed out and later packets the socket
/// id we gave.
fn we_deliver(trace: &[(u64, Vec<u8>)]) -> (Vec<Vec<u8>>, Stats) {
    let start = Instant::now();
    let peer: SocketAddr = "127.0.0.1:50000".parse().expect("addr");
    let mut l = Listener::with_key(Config::default(), start, Fixed::default());
    let (mut cookie, mut socket) = (None, None);
    let mut delivered = Vec::new();
    let mut stats = None;
    let end = start + Duration::from_micros(trace.last().map_or(0, |(t, _)| *t) + 5_000_000);
    let mut i = 0;
    loop {
        let next_packet = trace.get(i).map(|(t, _)| start + Duration::from_micros(*t));
        let now = match (next_packet, l.poll_timeout()) {
            (Some(p), Some(t)) => p.min(t),
            (Some(p), None) => p,
            (None, Some(t)) => t,
            (None, None) => break,
        };
        if now > end {
            break;
        }
        if next_packet == Some(now) {
            let mut d = trace[i].1.clone();
            i += 1;
            let handshake = d[..2] == [0x80, 0x00];
            if handshake {
                if d[36..40] == HandshakeKind::Conclusion.to_u32().to_be_bytes()
                    && let Some(c) = cookie
                {
                    d[44..48].copy_from_slice(&u32::to_be_bytes(c));
                }
            } else if let Some(s) = socket {
                d[12..16].copy_from_slice(&u32::to_be_bytes(s));
            }
            l.handle_datagram(now, peer, &d);
        } else {
            l.handle_timeout(now);
        }
        while let Some(ev) = l.poll_event() {
            match ev {
                Event::Connect { id, stream_id, .. } => {
                    assert!(stream_id.starts_with(b"publish:live/event-"));
                    socket = Some(id.0);
                    assert!(l.accept(id, now));
                }
                Event::Data { payload, .. } => delivered.push(payload),
                Event::Closed { stats: s, .. } => stats = Some(s),
            }
        }
        while let Some((_, out)) = l.poll_transmit() {
            if let Ok(Packet::Control(ControlPacket {
                body: Control::Handshake(h),
                ..
            })) = Packet::parse(&out)
                && h.kind == HandshakeKind::Induction
            {
                cookie = Some(h.cookie);
            }
        }
    }
    (delivered, stats.expect("the caller shut down"))
}

fn check(name: &str) -> Stats {
    let (delivered, stats) = we_deliver(&read_trace(name));
    let expected =
        std::fs::read_to_string(fixtures().join(format!("{name}.expected"))).expect("expected");
    assert_eq!(summarise(&delivered), expected, "{name}");
    stats
}

#[test]
fn srt_live_transmit_publish() {
    let s = check("srt-live-transmit-publish");
    assert_eq!((s.delivered, s.lost, s.dropped), (44, 0, 0));
}

#[test]
fn ffmpeg_publish() {
    let s = check("ffmpeg-publish");
    assert_eq!((s.delivered, s.lost, s.dropped), (90, 0, 0));
}

/// Under loss the two differ, and libsrt is the judge: the recorded
/// session's receiver was libsrt 1.5.4's `srt-live-transmit`, and what it
/// wrote out is `*.libsrt-expected`. We deliver exactly that. srt-protocol
/// delivers that less two packets: each one that arrived in time right
/// after a packet skipped as too late (sequence offsets 9 and 29 here; 8
/// never arrived and the retransmission of 28 came 13 ms past its
/// deadline). It discards one packet too many when it skips.
#[test]
fn srt_live_transmit_lossy_200ms() {
    let name = "srt-live-transmit-lossy-200ms";
    let trace = read_trace(name);
    let (delivered, stats) = we_deliver(&trace);
    let ours = summarise(&delivered);
    let libsrt = std::fs::read_to_string(fixtures().join(format!("{name}.libsrt-expected")))
        .expect("libsrt's delivery");
    assert_eq!(ours, libsrt, "the same as libsrt");
    assert_eq!(stats.delivered, 59);
    assert!(stats.retransmitted > 0);
    assert_eq!(stats.dropped, 2, "8 never arrived, 28 too late");

    let reference =
        std::fs::read_to_string(fixtures().join(format!("{name}.expected"))).expect("expected");
    // The sequence offset of each payload we delivered.
    let first = u32::from_be_bytes(
        trace
            .iter()
            .find(|(_, d)| d[0] & 0x80 == 0)
            .expect("data")
            .1[..4]
            .try_into()
            .expect("4"),
    );
    let offset_of = |p: &Vec<u8>| {
        trace
            .iter()
            .find(|(_, d)| d[0] & 0x80 == 0 && d[16..] == p[..])
            .map(|(_, d)| u32::from_be_bytes(d[..4].try_into().expect("4")) - first)
            .expect("from the trace")
    };
    let without: Vec<Vec<u8>> = delivered
        .iter()
        .filter(|p| ![9, 29].contains(&offset_of(p)))
        .cloned()
        .collect();
    assert_eq!(
        summarise(&without),
        reference,
        "srt-protocol: ours less 9 and 29"
    );
}
