//! Hostile receiver traffic, bounded media admission and time advances before
//! or after an original caller/listener exchange. No reference source is used.
#![no_main]
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_srt::packet::{Control, Packet};
use sapflow_srt::seq::Seq;
use sapflow_srt::{Caller, CallerConfig, Config, ConnId, Event, Listener, RejectCode};

fn drain(caller: &mut Caller, now: Instant) {
    // Every output is bounded and parseable. Fixed time prevents an unbounded
    // pacing loop, but allow controls plus the entire configured send window.
    for _ in 0..128 {
        let Some((_, bytes)) = caller.poll_transmit(now) else {
            return;
        };
        assert!(bytes.len() <= 1500);
        Packet::parse(&bytes).expect("caller emits valid wire packets");
    }
    panic!("caller exceeded bounded immediate output");
}

type Fixed = BuildHasherDefault<DefaultHasher>;

// High-bit modes retain the four-byte record framing. Bit 0 negotiates,
// bit 1 selects AES256; bits 2..3 select good / wrong secret / missing caller
// secret / missing listener secret. Bit 5 starts near sequence wrap, bit 6
// sends 48 trusted packets first to reach both key slots and key refresh.
// Secrets are synthetic. Only setup can accept a connection (and derive a
// KEK); hostile records cannot repeat expensive PBKDF work. Key material is
// still obtained through the production CSPRNG, never a test RNG in core.
struct Link {
    caller: Caller,
    listener: Listener<Fixed>,
    peer: SocketAddr,
    source: SocketAddr,
    id: Option<ConnId>,
    // First/latest data, first/latest KMREQ, first/latest KMRSP. Fixed count;
    // each copy <=1500 bytes. No retained whole-session media or event list.
    captured: [Option<Vec<u8>>; 6],
    key_flags: u8,
    delivered: usize,
    negative: bool,
    drop_responses: bool,
}

impl Link {
    fn capture(&mut self, bytes: &[u8], forward: bool) {
        assert!(bytes.len() <= 1500);
        let slot = match Packet::parse(bytes).expect("valid emitted packet") {
            Packet::Data(data) if forward => {
                self.key_flags |= data.key;
                Some(0)
            }
            Packet::Control(packet) => match packet.body {
                Control::Other {
                    control_type: 0x7fff,
                    subtype: 3,
                    ..
                } if forward => Some(2),
                Control::Other {
                    control_type: 0x7fff,
                    subtype: 4,
                    ..
                } if !forward => Some(4),
                _ => None,
            },
            _ => None,
        };
        if let Some(slot) = slot {
            self.captured[slot].get_or_insert_with(|| bytes.to_vec());
            self.captured[slot + 1] = Some(bytes.to_vec());
        }
    }

    fn events(&mut self, now: Instant, accept: bool) {
        for _ in 0..128 {
            let Some(event) = self.listener.poll_event() else {
                break;
            };
            match event {
                Event::Connect { id, .. } if accept && self.id.is_none() => {
                    self.id = Some(id);
                    self.listener.accept(id, now);
                }
                Event::Connect { id, .. } => {
                    self.listener.reject(id, RejectCode::ROGUE);
                }
                Event::Data { .. } => {
                    assert!(
                        !self.negative,
                        "wrong/missing secret must not deliver media"
                    );
                    self.delivered += 1;
                }
                Event::Closed { .. } => {}
            }
        }
        for _ in 0..128 {
            if self.caller.poll_event().is_none() {
                break;
            }
        }
    }

    fn pump(&mut self, now: Instant, accept: bool) {
        self.caller.handle_timeout(now);
        self.listener.handle_timeout(now);
        // Fixed rounds and per-direction packet bounds also bound malformed
        // traffic that causes more replies. No drain-until-peer-quiesces loop.
        for _ in 0..4 {
            for _ in 0..64 {
                let Some((_, bytes)) = self.caller.poll_transmit(now) else {
                    break;
                };
                self.capture(&bytes, true);
                self.listener.handle_datagram(now, self.source, &bytes);
            }
            self.events(now, accept);
            for _ in 0..64 {
                let Some((_, bytes)) = self.listener.poll_transmit() else {
                    break;
                };
                self.capture(&bytes, false);
                if !self.drop_responses {
                    self.caller.handle_datagram(now, self.peer, &bytes);
                }
            }
        }
        self.events(now, accept);
        let stats = self.caller.stats().send;
        assert!(stats.buffered_packets <= 64 && stats.buffered_bytes <= 64 * 1316);
        assert!(self.listener.connections() <= 1);
        if self.negative {
            assert!(!self.caller.is_connected());
        }
    }
}

fn encrypted(mode: u8, mut rest: &[u8]) {
    let start = Instant::now();
    let size = if mode & 2 == 0 {
        KeySize::Aes128
    } else {
        KeySize::Aes256
    };
    let profile = (mode >> 2) & 3;
    let mut good = CryptoConfig::new(b"fuzz scoped SRT secret".to_vec(), size).expect("fixture");
    good.refresh_packets = 16;
    good.preannounce_packets = 5;
    let mut config = CallerConfig {
        crypto: (profile != 2).then(|| good.clone()),
        latency: Duration::from_millis(10),
        ..CallerConfig::default()
    };
    config.send.max_packets = 64;
    config.send.max_bytes = 64 * 1316;
    config.send.bandwidth = 100_000;
    let peer = "192.0.2.1:9000".parse().expect("peer");
    let source = "192.0.2.2:9001".parse().expect("source");
    let caller = Caller::with_ids(
        config,
        peer,
        11,
        Seq::new(if mode & 32 != 0 { 0x7fff_fff8 } else { 100 }),
        start,
    )
    .expect("fixture");
    let listener = Listener::with_key(
        Config {
            crypto: match profile {
                1 => Some(
                    CryptoConfig::new(b"wrong fuzz SRT secret".to_vec(), size).expect("fixture"),
                ),
                3 => None,
                _ => Some(good),
            },
            require_encryption: true,
            latency: Duration::from_millis(10),
            max_connections: 1,
            buffer_packets: 64,
            handshakes_per_second: 8,
            ..Config::default()
        },
        start,
        Fixed::default(),
    );
    let mut link = Link {
        caller,
        listener,
        peer,
        source,
        id: None,
        captured: Default::default(),
        key_flags: 0,
        delivered: 0,
        negative: profile != 0,
        drop_responses: false,
    };
    let mut now = start;
    if mode & 1 != 0 {
        link.pump(now, true);
        if profile == 0 {
            assert!(link.caller.is_connected(), "own encrypted negotiation");
        } else {
            assert!(link.caller.is_closed(), "wrong/missing secret fails closed");
        }
        if mode & 64 != 0 && profile == 0 {
            for n in 0..48 {
                now += Duration::from_millis(20);
                link.caller
                    .enqueue(now, vec![n; 188])
                    .expect("warmup window");
                link.pump(now, false);
            }
            now += Duration::from_millis(20);
            link.pump(now, false);
            assert_eq!(link.key_flags, 3, "both encryption slots exercised");
            assert_eq!(link.delivered, 48, "trusted warmup delivered");
            assert!(link.caller.stats().send.km_responses >= 2);
        }
    }
    // At most 256 records, 255ms each. Bit 7 presents a backwards timestamp
    // within this epoch; pump uses the greatest time already presented.
    for _ in 0..256 {
        if rest.len() < 4 {
            break;
        }
        let (op, ms) = (rest[0], rest[1]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]])).min(rest.len() - 4);
        let bytes = &rest[4..4 + len];
        rest = &rest[4 + len..];
        let step = Duration::from_millis(u64::from(ms));
        let at = if op & 128 != 0 {
            now.checked_sub(step).unwrap_or(start).max(start)
        } else {
            now += step;
            now
        };
        // One byte beyond MTU reaches oversized rejection without retaining
        // attacker-controlled record lengths. Admission is separately <=1316.
        let datagram = &bytes[..bytes.len().min(1501)];
        match op & 7 {
            0 => link.caller.handle_datagram(at, peer, datagram),
            1 => link.caller.handle_datagram(at, source, datagram),
            2 => {
                let _ = link
                    .caller
                    .enqueue(at, bytes[..bytes.len().min(1316)].to_vec());
            }
            3 => {
                link.caller.handle_timeout(at);
                link.listener.handle_timeout(at);
            }
            4 => link.listener.handle_datagram(at, source, datagram),
            5 | 6 => {
                if let Some((&selector, mutations)) = bytes.split_first() {
                    let slot = if op & 7 == 5 {
                        usize::from(selector % 4)
                    } else {
                        4 + usize::from(selector % 2)
                    };
                    if let Some(mut packet) = link.captured[slot].clone() {
                        // Empty mutation list is exact replay. Triples xor an
                        // arbitrary wire byte, including KM length/key flags.
                        for mutation in mutations.chunks_exact(3).take(32) {
                            let offset =
                                usize::from(u16::from_be_bytes([mutation[0], mutation[1]]))
                                    % packet.len();
                            packet[offset] ^= mutation[2];
                        }
                        if op & 7 == 5 {
                            link.listener.handle_datagram(at, source, &packet);
                        } else {
                            link.caller.handle_datagram(at, peer, &packet);
                        }
                    }
                }
            }
            _ => match bytes.first().copied().unwrap_or(0) % 3 {
                0 => link.caller.stop(at),
                1 => link.drop_responses = true,
                _ => link.drop_responses = false,
            },
        }
        link.pump(now, false);
    }
    link.caller.stop(now);
    link.pump(now, false);
    assert!(link.caller.is_closed() && link.caller.poll_timeout().is_none());
    assert_eq!(link.caller.stats().send.buffered_bytes, 0);
}

fuzz_target!(|input: &[u8]| {
    let Some((&mode, mut rest)) = input.split_first() else {
        return;
    };
    if mode & 0x80 != 0 {
        encrypted(mode, rest);
        return;
    }
    let start = Instant::now();
    let peer: SocketAddr = "192.0.2.1:9000".parse().expect("peer");
    let source: SocketAddr = "192.0.2.2:9001".parse().expect("source");
    let mut config = CallerConfig::default();
    config.send.max_packets = 64;
    config.send.max_bytes = 64 * 1316;
    let mut caller = Caller::with_ids(config, peer, 11, Seq::new(100), start).expect("config");
    if mode & 1 != 0 {
        let mut listener = Listener::new(Config::default(), start);
        for _ in 0..12 {
            while let Some((_, bytes)) = caller.poll_transmit(start) {
                listener.handle_datagram(start, source, &bytes);
            }
            while let Some(event) = listener.poll_event() {
                if let Event::Connect { id, .. } = event {
                    listener.accept(id, start);
                }
            }
            while let Some((_, bytes)) = listener.poll_transmit() {
                caller.handle_datagram(start, peer, &bytes);
            }
            if caller.is_connected() {
                break;
            }
        }
        assert!(
            caller.is_connected(),
            "original exchange must reach open state"
        );
    }
    let mut now = start;
    // Each record is [operation, milliseconds, length_hi, length_lo, bytes].
    for _ in 0..512 {
        if rest.len() < 4 {
            break;
        }
        let (op, ms) = (rest[0], rest[1]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]])).min(rest.len() - 4);
        let bytes = &rest[4..4 + len];
        rest = &rest[4 + len..];
        now += Duration::from_millis(u64::from(ms));
        match op & 3 {
            0 => caller.handle_datagram(now, peer, bytes),
            1 => caller.handle_datagram(now, source, bytes),
            2 => {
                let _ = caller.enqueue(now, bytes[..bytes.len().min(1316)].to_vec());
            }
            _ => caller.handle_timeout(now),
        }
        drain(&mut caller, now);
        while caller.poll_event().is_some() {}
        let stats = caller.stats().send;
        assert!(stats.buffered_packets <= 64);
        assert!(stats.buffered_bytes <= 64 * 1316);
        if caller.poll_timeout().is_some_and(|at| at <= now) {
            caller.handle_timeout(now);
            drain(&mut caller, now);
            assert!(caller.poll_timeout().is_none_or(|at| at > now));
        }
    }
    caller.stop(now);
    drain(&mut caller, now);
    assert!(caller.is_closed());
    assert!(caller.poll_timeout().is_none());
    assert_eq!(caller.stats().send.buffered_bytes, 0);
});
