//! Passive ICE-TCP in process (RFC 6544, RFC 4571): a whole session
//! through the router over one framed connection, both ways split at awkward
//! offsets, for both SRTP profiles; and a TCP tuple kept apart from a UDP
//! one and from another connection with the same address.

use crate::framing::{Decoder, Framing, encode};
use crate::ice::{Kind, Transport, Tuple, classify};
use crate::resource::SessionId;
use crate::router::{Router, RouterConfig};
use crate::session::{Event, SessionConfig, State, Transmit};
use crate::testing::*;
use sapflow_dtls::SrtpProfile;
use sapflow_rtp::opus::FrameKind;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const GCM: u16 = 0x0007;
const CM80: u16 = 0x0001;

/// Chunk sizes a stream is cut into, in turn: single bytes, a split length
/// prefix, and chunks longer than a frame.
const CUTS: [usize; 7] = [1, 2, 3, 5, 700, 1501, 4000];

/// One direction of a connection: packets framed, the stream cut by
/// [`CUTS`], and frames taken back out as a reader would.
struct Direction {
    decoder: Decoder,
    cut: usize,
    /// Stream bytes carried, length prefixes included.
    bytes: usize,
}

impl Direction {
    fn new() -> Self {
        Self {
            decoder: Decoder::new(Framing::default()),
            cut: 0,
            bytes: 0,
        }
    }

    fn carry(&mut self, packets: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut stream = Vec::new();
        for p in packets {
            encode(&Framing::default(), p, &mut stream).expect("within a frame");
        }
        self.bytes += stream.len();
        let mut frames = Vec::new();
        let mut at = 0;
        while at < stream.len() {
            let n = CUTS[self.cut % CUTS.len()].min(stream.len() - at);
            self.cut += 1;
            self.decoder.push(&stream[at..at + n]).expect("framing");
            at += n;
            while let Some(f) = self.decoder.pop().expect("framing") {
                frames.push(f);
            }
        }
        assert_eq!(self.decoder.buffered(), 0, "nothing left over");
        frames
    }
}

/// The publisher's end of one TCP connection, and the router's.
struct Connection {
    tuple: Tuple,
    up: Direction,
    down: Direction,
}

impl Connection {
    fn new(remote: SocketAddr, number: u64) -> Self {
        Self {
            tuple: Tuple::tcp(remote, number),
            up: Direction::new(),
            down: Direction::new(),
        }
    }

    /// Publisher to router.
    fn send(&mut self, r: &mut Router, now: Instant, packets: &[Vec<u8>]) {
        for f in self.up.carry(packets) {
            r.handle_datagram(now, self.tuple, &f);
        }
    }

    /// Router to publisher: everything queued must be for this connection.
    /// Returns the DTLS the publisher answers with.
    fn receive(&mut self, r: &mut Router, p: &mut Publisher) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(t) = r.poll_transmit() {
            assert_eq!(t.tuple, self.tuple, "only to the connection");
            out.push(t.bytes);
        }
        let mut replies = Vec::new();
        for bytes in self.down.carry(&out) {
            if classify(&bytes) == Kind::Rtcp && !p.has_keys() {
                continue;
            }
            replies.extend(p.take(Transmit {
                tuple: self.tuple,
                bytes,
            }));
        }
        replies
    }
}

#[derive(Default)]
struct Seen {
    units: Vec<(Vec<u8>, bool)>,
    audio: Vec<Vec<u8>>,
    lost: u64,
    connected: Option<SrtpProfile>,
}

fn collect(r: &mut Router, seen: &mut Seen) {
    while let Some((_, e)) = r.poll_event() {
        match e {
            Event::Connected(p) => seen.connected = Some(p),
            Event::Media(sapflow_rtp::Event::AccessUnit { au, .. }) => {
                seen.units.push((au.data, au.idr));
            }
            Event::Media(sapflow_rtp::Event::Audio { frame, .. })
                if frame.kind == FrameKind::Media =>
            {
                seen.audio.push(frame.data);
            }
            Event::Media(sapflow_rtp::Event::Lost { count, .. }) => seen.lost += count,
            _ => {}
        }
    }
}

fn timers(r: &mut Router, until: Instant) {
    for _ in 0..10_000 {
        match r.poll_timeout() {
            Some(t) if t <= until => r.handle_timeout(t),
            _ => break,
        }
    }
    r.handle_timeout(until);
}

/// A publisher over one framed connection: ICE, DTLS, then a second of
/// H.264 (STAP-A and FU-A) and Opus, through the router.
fn publish_over_tcp(profile: u16, seed: u8) {
    let t0 = Instant::now();
    let mut r = Router::new(RouterConfig::default()).expect("defaults");
    let mut p = Publisher::new(seed, &[profile]);
    let a = accept(&p.offer(), SessionConfig::default(), t0, seed);
    p.answered(&a);
    let id = SessionId::generate_seeded([seed; 32]).expect("id");
    r.insert(id, a.session).expect("room");
    let remote: SocketAddr = "198.51.100.20:51000".parse().expect("address");
    let mut c = Connection::new(remote, 7);
    p.tuple = c.tuple;
    assert_eq!(p.tuple.transport, Transport::Tcp);

    let mut pending = vec![p.check(true)];
    pending.extend(p.client.start());
    for _ in 0..16 {
        c.send(&mut r, t0, &pending);
        pending = c.receive(&mut r, &mut p);
        if p.has_keys() && pending.is_empty() {
            break;
        }
    }
    assert!(p.has_keys(), "DTLS completes over framed TCP");
    // The check's XOR-MAPPED-ADDRESS is the connection's address.
    let first = p.stun_answers.first().expect("a STUN answer");
    let m = crate::stun::Message::parse(first).expect("parses");
    assert_eq!(m.xor_mapped, Some(remote));
    let s = r.session(&id).expect("live");
    assert_eq!(s.state(), State::Connected);
    assert_eq!(
        (s.selected(), s.send_tuple()),
        (Some(c.tuple), Some(c.tuple))
    );
    let want = if profile == GCM {
        SrtpProfile::AeadAes128Gcm
    } else {
        SrtpProfile::Aes128CmHmacSha1_80
    };
    let mut seen = Seen::default();
    collect(&mut r, &mut seen);
    assert_eq!(seen.connected, Some(want));
    assert!(
        p.rtcp.iter().any(|x| rtcp_types(x).contains(&(206, 1))),
        "PLI at once, over the connection"
    );

    let mut t = t0;
    let reports = p.sender_reports(0xe8f0_0000_0000_0000);
    c.send(&mut r, t, &reports);
    let (mut want_units, mut want_audio) = (Vec::new(), Vec::new());
    for step in 0..50u64 {
        t = t0 + Duration::from_millis(20 * step);
        let mut packets = Vec::new();
        if step % 2 == 0 {
            let (v, data) = if step % 20 == 0 {
                p.keyframe(4000)
            } else {
                p.delta(if step % 4 == 0 { 600 } else { 2500 })
            };
            want_units.push((data, step % 20 == 0));
            packets.extend(v);
        }
        let fill = step as u8;
        want_audio.push(opus_packet(fill));
        packets.push(p.audio(fill));
        c.send(&mut r, t, &packets);
        timers(&mut r, t);
        collect(&mut r, &mut seen);
        c.receive(&mut r, &mut p);
    }
    timers(&mut r, t + Duration::from_millis(600));
    collect(&mut r, &mut seen);
    c.receive(&mut r, &mut p);

    assert_eq!(seen.units, want_units, "access units");
    assert_eq!(seen.audio, want_audio, "audio");
    assert_eq!(seen.lost, 0);
    let types: Vec<(u8, u8)> = p.rtcp.iter().flat_map(|x| rtcp_types(x)).collect();
    assert!(types.contains(&(201, 0)), "receiver reports");
    assert!(types.contains(&(205, 15)), "transport-cc");
    let s = r.session(&id).expect("live");
    let stats = s.stats();
    assert_eq!(stats.budget_dropped, 0, "the budget holds TCP to 4x too");
    assert_eq!((stats.srtp_refused, stats.oversized), (0, 0));
    assert_eq!(p.misdirected, 0);
    // The budget counts packets, not their two-byte length prefixes.
    let (received, sent) = s.tuple_bytes(&c.tuple).expect("a path");
    assert!(sent <= 4 * received, "{sent} for {received}");
    assert!(c.up.bytes as u64 > received && c.down.bytes as u64 > sent);

    // The same address over UDP, or on another connection, is another
    // tuple: neither is validated, and media from it reaches no session.
    let before = r.session(&id).expect("live").stats().srtp_packets;
    for other in [Tuple::udp(remote), Tuple::tcp(remote, 8)] {
        assert_ne!(other, c.tuple);
        assert!(!r.session(&id).expect("live").accepts(&other));
        assert_eq!(r.owner(&other), None);
        let a = p.audio(1);
        assert_eq!(r.handle_datagram(t, other, &a), None, "{other:?}");
        assert!(r.poll_transmit().is_none(), "nothing to {other:?}");
    }
    assert_eq!(r.session(&id).expect("live").stats().srtp_packets, before);
}

#[test]
fn a_session_over_framed_tcp_aead_aes_128_gcm() {
    publish_over_tcp(GCM, 21);
}

#[test]
fn a_session_over_framed_tcp_aes_cm_hmac_sha1_80() {
    publish_over_tcp(CM80, 22);
}

/// A check on a new connection validates that connection alone, and the
/// first connection keeps the session's sends until media moves them.
#[test]
fn a_second_connection_is_its_own_pair() {
    let t0 = Instant::now();
    let mut r = Router::new(RouterConfig::default()).expect("defaults");
    let mut p = Publisher::new(23, &[GCM]);
    let a = accept(&p.offer(), SessionConfig::default(), t0, 23);
    p.answered(&a);
    let id = SessionId::generate_seeded([23; 32]).expect("id");
    r.insert(id, a.session).expect("room");
    let remote: SocketAddr = "[2001:db8::20]:51000".parse().expect("address");
    let (mut one, mut two) = (Connection::new(remote, 1), Connection::new(remote, 2));
    p.tuple = one.tuple;
    let check = p.check(true);
    one.send(&mut r, t0, &[check]);
    one.receive(&mut r, &mut p);
    let check = p.check(false);
    p.tuple = two.tuple;
    two.send(&mut r, t0, &[check]);
    assert_eq!(p.stun_answers.len(), 1, "the first answer only, so far");
    two.receive(&mut r, &mut p);
    assert_eq!(p.stun_answers.len(), 2, "the second connection is answered");
    let s = r.session(&id).expect("live");
    assert!(s.accepts(&one.tuple) && s.accepts(&two.tuple));
    assert_eq!(s.selected(), Some(one.tuple), "only USE-CANDIDATE moves it");
    assert_eq!(r.owner(&two.tuple), Some(id));
}
