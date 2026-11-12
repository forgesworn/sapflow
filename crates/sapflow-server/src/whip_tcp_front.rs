//! The ICE-TCP listener's view of one connection, without I/O: an optional
//! PROXY protocol v2 header, then RFC 4571 frames (`whip_tcp.rs` does the
//! I/O, `docs/whip-ingest.md` has the design).
//!
//! - **Without PROXY protocol** the TCP peer is the publisher, and its
//!   address is known at once.
//! - **With PROXY protocol v2** the peer must be the trusted proxy,
//!   checked before a byte is read ([`Front::proxied`]); any other peer is
//!   refused unread. The header is parsed with
//!   `sapflow_core::proxy_protocol::parse_trusted`, never `parse`. Under
//!   PROXY its source, in canonical form (an IPv4-mapped address as IPv4),
//!   is the publisher's address. Ports are not compared, so any process on
//!   the proxy's host is trusted.
//! - **LOCAL** is the proxy's own connection, such as a health check, with
//!   no publisher behind it. It is refused once its header is in
//!   ([`Close::Local`]): no frame on it is read. Taking the proxy as the
//!   publisher would put every LOCAL connection on the proxy's address,
//!   under one per-address cap.
//! - **Frames** follow the header in the same stream, and bytes that came
//!   with the header's last bytes are the start of them. A frame longer
//!   than [`Framing::max_frame`] (1500 bytes) ends the connection, since
//!   the framing cannot be trusted after it; empty frames are skipped.
//!
//! The outcome does not depend on how the stream is cut into pushes, as
//! long as no push is longer than the decoder's room
//! ([`Framing::max_buffered`] less one partial frame); the `whip_tcp_front`
//! fuzz target checks that, and [`check`] is its body. Nothing here holds
//! more than one header (at most 536 bytes, plus the push that completed
//! it) and the decoder's bound.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use sapflow_core::proxy_protocol::{self, Command, Parsed};
use sapflow_whip::framing::{Decoder, Framing, FramingError};

/// Why a connection was closed before or while framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    /// PROXY protocol is on and the peer is not the trusted proxy.
    Untrusted,
    /// The header was refused.
    Header(proxy_protocol::Error),
    /// A LOCAL header: the proxy's own connection.
    Local,
    /// The framing failed: a frame over the cap, or too much buffered.
    Framing(FramingError),
}

impl fmt::Display for Close {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted => f.write_str("not the trusted proxy"),
            Self::Header(e) => write!(f, "{e}"),
            Self::Local => f.write_str("PROXY LOCAL: the proxy's own connection"),
            Self::Framing(e) => write!(f, "{e}"),
        }
    }
}

enum State {
    /// Waiting for the whole PROXY header from the trusted proxy.
    Header {
        trusted: IpAddr,
        buf: Vec<u8>,
    },
    /// Frames from the publisher.
    Framed {
        decoder: Decoder,
    },
    Closed(Close),
}

/// One connection's bytes in, frames out.
pub struct Front {
    peer: SocketAddr,
    framing: Framing,
    state: State,
    /// The publisher's address once known, kept after a close.
    client: Option<SocketAddr>,
}

/// The address in the form limits and tuples key on: an IPv4-mapped IPv6
/// address as IPv4.
pub fn canonical(a: SocketAddr) -> SocketAddr {
    SocketAddr::new(a.ip().to_canonical(), a.port())
}

impl Front {
    /// A connection with no PROXY header: the peer is the publisher.
    pub fn direct(peer: SocketAddr, framing: Framing) -> Self {
        Self {
            peer,
            framing,
            state: State::Framed {
                decoder: Decoder::new(framing),
            },
            client: Some(canonical(peer)),
        }
    }

    /// A connection that must start with a PROXY v2 header from `trusted`.
    /// Any other peer is refused before a byte is read.
    pub fn proxied(peer: SocketAddr, trusted: IpAddr, framing: Framing) -> Result<Self, Close> {
        if !proxy_protocol::is_trusted_proxy(peer.ip(), trusted) {
            return Err(Close::Untrusted);
        }
        Ok(Self {
            peer,
            framing,
            state: State::Header {
                trusted,
                buf: Vec::new(),
            },
            client: None,
        })
    }

    /// The publisher's address, once known (and after a close that came
    /// later).
    pub fn client(&self) -> Option<SocketAddr> {
        self.client
    }

    /// Bytes from the connection. Whole frames are appended to `frames`.
    /// Returns the publisher's address on the push that completed the
    /// header (always `None` for a direct connection, whose address is
    /// known from the start). An error is final: close the connection.
    pub fn push(
        &mut self,
        bytes: &[u8],
        frames: &mut Vec<Vec<u8>>,
    ) -> Result<Option<SocketAddr>, Close> {
        let (opened, rest) = match &mut self.state {
            State::Closed(c) => return Err(*c),
            State::Framed { .. } => (None, bytes.to_vec()),
            State::Header { trusted, buf } => {
                buf.extend_from_slice(bytes);
                match proxy_protocol::parse_trusted(self.peer.ip(), *trusted, buf) {
                    Err(e) => return Err(self.close(Close::Header(e))),
                    Ok(Parsed::Complete { header, consumed }) => {
                        if !matches!(header.command(), Command::Proxy { .. }) {
                            return Err(self.close(Close::Local));
                        }
                        let client = canonical(header.client_address(self.peer));
                        let rest = buf.get(consumed..).unwrap_or_default().to_vec();
                        self.state = State::Framed {
                            decoder: Decoder::new(self.framing),
                        };
                        self.client = Some(client);
                        (Some(client), rest)
                    }
                    // Parsed is non-exhaustive: anything but a whole header
                    // waits, and the listener's header timeout bounds that.
                    Ok(_) => return Ok(None),
                }
            }
        };
        let State::Framed { decoder, .. } = &mut self.state else {
            return Ok(opened);
        };
        let mut result = decoder.push(&rest);
        while result.is_ok() {
            match decoder.pop() {
                Ok(Some(f)) => frames.push(f),
                Ok(None) => break,
                Err(e) => result = Err(e),
            }
        }
        match result {
            Ok(()) => Ok(opened),
            Err(e) => Err(self.close(Close::Framing(e))),
        }
    }

    fn close(&mut self, c: Close) -> Close {
        self.state = State::Closed(c);
        c
    }
}

#[cfg(any(test, feature = "fuzzing"))]
#[cfg_attr(not(test), allow(dead_code))] // The fuzz target includes this module directly.
/// What a whole stream came to: the frames, the publisher's address, and
/// the close, if any.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub frames: Vec<Vec<u8>>,
    pub client: Option<SocketAddr>,
    pub close: Option<Close>,
}

#[cfg(any(test, feature = "fuzzing"))]
#[cfg_attr(not(test), allow(dead_code))] // The fuzz target includes this module directly.
/// Runs `stream` through a front in pushes of the sizes `cuts` gives in
/// turn (0 is taken as 1), stopping at the first close.
pub fn run(mut front: Front, stream: &[u8], cuts: &[usize]) -> Outcome {
    let mut out = Outcome {
        frames: Vec::new(),
        client: None,
        close: None,
    };
    let mut at = 0;
    let mut i = 0;
    while at < stream.len() {
        let n = cuts
            .get(i % cuts.len().max(1))
            .copied()
            .unwrap_or(1)
            .clamp(1, stream.len() - at);
        i += 1;
        let chunk = stream.get(at..at + n).unwrap_or_default();
        at += n;
        if let Err(c) = front.push(chunk, &mut out.frames) {
            out.close = Some(c);
            break;
        }
    }
    out.client = front.client();
    out
}

#[cfg(any(test, feature = "fuzzing"))]
#[cfg_attr(not(test), allow(dead_code))] // The fuzz target includes this module directly.
/// The `whip_tcp_front` fuzz target's body: `data`'s first bytes choose the
/// mode and two ways of cutting the rest; both must give the same outcome,
/// and that outcome must keep every rule in the module docs.
pub fn check(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let Some((cuts, stream)) = rest.split_at_checked(4.min(rest.len())) else {
        return;
    };
    let framing = Framing {
        max_frame: 1500,
        max_buffered: 8192,
    };
    // Pushes no longer than the I/O side reads: 4096 bytes.
    let a: Vec<usize> = cuts.iter().map(|&c| usize::from(c) * 16 + 1).collect();
    let b = [4096usize];
    let proxy: IpAddr = "192.0.2.10".parse().unwrap_or(IpAddr::from([0, 0, 0, 0]));
    let peer = match mode % 4 {
        // The trusted proxy, as itself or IPv4-mapped.
        0 => SocketAddr::new(proxy, 40_000),
        1 => "[::ffff:192.0.2.10]:40001"
            .parse()
            .unwrap_or(SocketAddr::new(proxy, 1)),
        // Some other peer.
        _ => "[2001:db8::7]:40002"
            .parse()
            .unwrap_or(SocketAddr::new(proxy, 2)),
    };
    let make = || {
        if mode & 0x80 == 0 {
            Front::proxied(peer, proxy, framing)
        } else {
            Ok(Front::direct(peer, framing))
        }
    };
    let (x, y) = match (make(), make()) {
        (Ok(x), Ok(y)) => (run(x, stream, &a), run(y, stream, &b)),
        (Err(c), _) | (_, Err(c)) => {
            // Refused before a byte is read: only an untrusted peer.
            assert_eq!(c, Close::Untrusted);
            assert!(mode & 0x80 == 0 && mode % 4 >= 2);
            return;
        }
    };
    assert_eq!(x, y, "the cut does not change the outcome");
    assert!(mode & 0x80 != 0 || mode % 4 < 2, "only the proxy is read");
    for f in &x.frames {
        assert!(!f.is_empty() && f.len() <= framing.max_frame);
    }
    if mode & 0x80 != 0 {
        // Direct: the peer, and exactly the decoder's frames.
        assert_eq!(x.client, Some(canonical(peer)));
        let mut d = Decoder::new(framing);
        let mut want = Vec::new();
        let mut failed = None;
        for chunk in stream.chunks(4096) {
            if let Err(e) = d.push(chunk) {
                failed = Some(e);
                break;
            }
            loop {
                match d.pop() {
                    Ok(Some(f)) => want.push(f),
                    Ok(None) => break,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
            if failed.is_some() {
                break;
            }
        }
        assert_eq!(x.frames, want);
        assert_eq!(x.close, failed.map(Close::Framing));
        return;
    }
    // Proxied: frames only after a PROXY header, whose source is the
    // client, canonical.
    match proxy_protocol::parse(stream) {
        Ok(Parsed::Complete { header, .. }) => match header.command() {
            Command::Proxy { source, .. } => {
                assert_eq!(x.client, Some(canonical(source)));
                assert_eq!(x.client.map(|c| c.ip()), Some(source.ip().to_canonical()));
            }
            _ => {
                assert_eq!(x.close, Some(Close::Local));
                assert!(x.frames.is_empty() && x.client.is_none());
            }
        },
        Ok(_) => {
            assert!(x.frames.is_empty() && x.client.is_none() && x.close.is_none());
        }
        Err(e) => {
            assert_eq!(x.close, Some(Close::Header(e)));
            assert!(x.frames.is_empty() && x.client.is_none());
        }
    }
}

#[cfg(test)]
/// A PROXY v2 header over TCP/IPv4, from the specification's layout:
/// the signature, version 2 with `command`, family and protocol 0x11,
/// the length, then source and destination addresses and ports.
pub(crate) fn header_v4(command: u8, source: SocketAddr, tlvs: &[u8]) -> Vec<u8> {
    let mut h = proxy_protocol::SIGNATURE.to_vec();
    h.push(0x20 | command);
    h.push(0x11);
    let len = u16::try_from(12 + tlvs.len()).expect("short");
    h.extend_from_slice(&len.to_be_bytes());
    let SocketAddr::V4(src) = source else {
        panic!("IPv4 source")
    };
    h.extend_from_slice(&src.ip().octets());
    h.extend_from_slice(&[192, 0, 2, 1]);
    h.extend_from_slice(&src.port().to_be_bytes());
    h.extend_from_slice(&443u16.to_be_bytes());
    h.extend_from_slice(tlvs);
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use sapflow_whip::framing::encode;

    const FRAMING: Framing = Framing {
        max_frame: 1500,
        max_buffered: 8192,
    };

    fn framed(packets: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in packets {
            encode(&FRAMING, p, &mut out).expect("frames");
        }
        out
    }

    fn proxy() -> IpAddr {
        "192.0.2.10".parse().expect("ip")
    }

    #[test]
    fn the_header_source_is_the_client_and_frames_follow_it() {
        let source: SocketAddr = "203.0.113.9:40123".parse().expect("addr");
        let mut stream = header_v4(1, source, &[]);
        stream.extend(framed(&[b"\x00\x01stun", &[0x16; 1500], b"", b"\x80rtp"]));
        for cut in [1usize, 2, 15, 16, 28, 29, 1000, 4096] {
            let front = Front::proxied("192.0.2.10:5000".parse().expect("addr"), proxy(), FRAMING)
                .expect("trusted");
            let o = run(front, &stream, &[cut]);
            assert_eq!(o.client, Some(source), "cut {cut}");
            assert_eq!(o.close, None);
            assert_eq!(o.frames.len(), 3, "the empty frame is skipped");
            assert_eq!(o.frames[1].len(), 1500);
        }
    }

    #[test]
    fn an_ipv4_mapped_proxy_is_still_the_proxy_and_its_source_canonical() {
        let peer: SocketAddr = "[::ffff:192.0.2.10]:5000".parse().expect("addr");
        // A PROXY header over TCP/IPv6 whose source is IPv4-mapped.
        let mut h = proxy_protocol::SIGNATURE.to_vec();
        h.extend_from_slice(&[0x21, 0x21, 0, 36]);
        let src: std::net::Ipv6Addr = "::ffff:198.51.100.4".parse().expect("ip");
        h.extend_from_slice(&src.octets());
        h.extend_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
        h.extend_from_slice(&[0x9c, 0x40, 0x01, 0xbb]);
        let mut f = Front::proxied(peer, proxy(), FRAMING).expect("trusted");
        let mut frames = Vec::new();
        assert_eq!(
            f.push(&h, &mut frames),
            Ok(Some("198.51.100.4:40000".parse().expect("addr")))
        );
    }

    #[test]
    fn any_other_peer_is_refused_unread() {
        for peer in ["192.0.2.11:5000", "[2001:db8::10]:5000"] {
            let peer: SocketAddr = peer.parse().expect("addr");
            assert!(matches!(
                Front::proxied(peer, proxy(), FRAMING),
                Err(Close::Untrusted)
            ));
        }
    }

    #[test]
    fn local_is_refused_once_its_header_is_in() {
        let mut h = proxy_protocol::SIGNATURE.to_vec();
        h.extend_from_slice(&[0x20, 0x00, 0, 0]);
        let mut stream = h.clone();
        stream.extend(framed(&[b"\x00\x01stun"]));
        let front = Front::proxied("192.0.2.10:5000".parse().expect("addr"), proxy(), FRAMING)
            .expect("trusted");
        let o = run(front, &stream, &[3]);
        assert_eq!(o.close, Some(Close::Local));
        assert!(o.frames.is_empty() && o.client.is_none());
    }

    #[test]
    fn refusals_and_partial_headers() {
        let peer: SocketAddr = "192.0.2.10:5000".parse().expect("addr");
        let front = || Front::proxied(peer, proxy(), FRAMING).expect("trusted");
        // Media with no header, a version 1 header, a UNIX address.
        let o = run(front(), &framed(&[b"\x00\x01stun"]), &[64]);
        assert_eq!(
            o.close,
            Some(Close::Header(proxy_protocol::Error::Signature))
        );
        let o = run(front(), b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2\r\n", &[64]);
        assert_eq!(
            o.close,
            Some(Close::Header(proxy_protocol::Error::Version1))
        );
        // A partial header waits; it is the listener's timeout that ends it.
        let source: SocketAddr = "203.0.113.9:1".parse().expect("addr");
        let h = header_v4(1, source, &[]);
        let o = run(front(), &h[..20], &[7]);
        assert_eq!(o, run(front(), &h[..20], &[20]));
        assert!(o.close.is_none() && o.client.is_none() && o.frames.is_empty());
    }

    #[test]
    fn a_frame_over_the_cap_ends_the_connection() {
        let peer: SocketAddr = "198.51.100.1:5000".parse().expect("addr");
        let mut stream = framed(&[b"\x00\x01ok"]);
        stream.extend_from_slice(&1501u16.to_be_bytes());
        stream.extend_from_slice(&[0; 1501]);
        let o = run(Front::direct(peer, FRAMING), &stream, &[4096]);
        assert_eq!(o.frames.len(), 1);
        assert_eq!(
            o.close,
            Some(Close::Framing(FramingError::FrameTooLong(1501)))
        );
        assert_eq!(o.client, Some(peer));
    }

    #[test]
    fn committed_seeds_hold_the_fuzz_checks() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/seeds/whip_tcp_front");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).expect("seeds") {
            check(&std::fs::read(entry.expect("entry").path()).expect("seed"));
            n += 1;
        }
        assert!(n >= 8, "{n} seeds");
    }

    #[test]
    fn seeds_hold_the_fuzz_checks() {
        let source: SocketAddr = "203.0.113.9:40123".parse().expect("addr");
        let body = framed(&[b"\x00\x01stun", &[0x80; 1200]]);
        let mut proxied = header_v4(1, source, &[]);
        proxied.extend_from_slice(&body);
        let mut local = proxy_protocol::SIGNATURE.to_vec();
        local.extend_from_slice(&[0x20, 0x00, 0, 0]);
        for (mode, stream) in [
            (0u8, proxied.clone()),
            (1, proxied.clone()),
            (2, proxied),
            (0, local),
            (0x80, body.clone()),
            (0x83, body),
        ] {
            let mut data = vec![mode, 1, 7, 64, 255];
            data.extend_from_slice(&stream);
            check(&data);
            // A fixed-seed mutation pass over each.
            let mut x = 0x9e37_79b9_u32 ^ u32::from(mode);
            for _ in 0..500 {
                let mut m = data.clone();
                for _ in 0..3 {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    let i = (x as usize) % m.len();
                    m[i] ^= (x >> 8) as u8;
                }
                check(&m);
            }
        }
    }
}
