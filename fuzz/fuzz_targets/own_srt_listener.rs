//! Our SRT listener (`sapflow_srt::Listener`) as the server drives it:
//! datagrams from a few peers, the application's answers and the passing of
//! time. Like a real client, each peer echoes the SYN cookie it was sent and
//! addresses later packets to the socket id it was given, so handshakes
//! complete and data, ACKACKs, NAKs, drop requests and shutdowns reach open
//! connections.
//!
//! Each record: control byte, time step (x 1 ms), length (2 bytes), datagram.
//! Control bits: 0x80 time passes only; 0x40 send the datagram as it is
//! (no cookie or socket id rewritten); 0x20 refuse the next caller; the low
//! two bits pick the peer.
#![no_main]
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use sapflow_srt::packet::{Control, ControlPacket, HandshakeKind, Packet};
use sapflow_srt::{Config, ConnId, Event, Listener, RejectCode};

type Fixed = BuildHasherDefault<DefaultHasher>;

/// Rewrites a client datagram the way a real client fills it in.
fn as_client(datagram: &[u8], cookie: Option<u32>, socket: Option<u32>) -> Vec<u8> {
    let mut d = datagram.to_vec();
    let control = d.first().is_some_and(|b| b & 0x80 != 0);
    let handshake = control && d.get(..2) == Some(&[0x80, 0x00]);
    if handshake {
        // A conclusion carries the cookie from the induction reply.
        let kind = d.get(36..40).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        if kind == Some(HandshakeKind::Conclusion.to_u32())
            && let (Some(c), Some(field)) = (cookie, d.get_mut(44..48))
        {
            field.copy_from_slice(&c.to_be_bytes());
        }
    } else if let (Some(s), Some(field)) = (socket, d.get_mut(12..16)) {
        field.copy_from_slice(&s.to_be_bytes());
    }
    d
}

fuzz_target!(|data: &[u8]| {
    let start = Instant::now();
    let peers: [SocketAddr; 3] = [
        "192.0.2.1:5000".parse().expect("addr"),
        "192.0.2.2:5000".parse().expect("addr"),
        "[2001:db8::1]:5000".parse().expect("addr"),
    ];
    let config = Config {
        max_connections: 4,
        buffer_packets: 256,
        ..Config::default()
    };
    let mut listener = Listener::with_key(config, start, Fixed::default());
    let mut cookies: HashMap<SocketAddr, u32> = HashMap::new();
    let mut sockets: HashMap<SocketAddr, u32> = HashMap::new();
    let mut now = start;
    let mut rest = data;
    while rest.len() >= 4 {
        let (ctl, step) = (rest[0], rest[1]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]])).min(rest.len() - 4);
        let datagram = &rest[4..4 + len];
        rest = &rest[4 + len..];
        now += Duration::from_millis(u64::from(step));
        let peer = peers[usize::from(ctl & 3) % 3];
        let refuse = ctl & 0x20 != 0;
        if ctl & 0x80 != 0 {
            listener.handle_timeout(now);
        } else if ctl & 0x40 != 0 {
            listener.handle_datagram(now, peer, datagram);
        } else {
            let d = as_client(datagram, cookies.get(&peer).copied(), sockets.get(&peer).copied());
            listener.handle_datagram(now, peer, &d);
        }
        // Timers that came due during the step.
        if let Some(t) = listener.poll_timeout()
            && t <= now
        {
            listener.handle_timeout(now);
        }
        while let Some(ev) = listener.poll_event() {
            match ev {
                Event::Connect { id, peer, .. } => {
                    if refuse {
                        listener.reject(id, RejectCode::http(401));
                    } else {
                        listener.accept(id, now);
                        sockets.insert(peer, id.0);
                    }
                }
                Event::Data { .. } => {}
                Event::Closed { id, .. } => {
                    sockets.retain(|_, s| *s != id.0);
                }
            }
        }
        while let Some((to, out)) = listener.poll_transmit() {
            let p = Packet::parse(&out).expect("what we send parses");
            if let Packet::Control(ControlPacket {
                body: Control::Handshake(h),
                ..
            }) = p
                && h.kind == HandshakeKind::Induction
            {
                cookies.insert(to, h.cookie);
            }
        }
        // No timer without connections; one with them.
        assert_eq!(
            listener.poll_timeout().is_some(),
            listener.connections() > 0
        );
        // The application closing a connection now and then.
        if ctl & 0x1C == 0x1C
            && let Some(&s) = sockets.get(&peer)
        {
            listener.close(ConnId(s), now);
            sockets.remove(&peer);
        }
    }
});
