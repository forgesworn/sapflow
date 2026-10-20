//! SRT packets: parsing and encoding (draft-sharabayko-srt-01, section 3).
//!
//! Every datagram that reaches the listener comes through [`Packet::parse`]
//! before anything is known about its sender, so parsing is bounded by the
//! datagram: no length field is trusted, nothing is allocated from a
//! declared size, and an unknown control type is a value, not an error.

use crate::seq::Seq;

/// Every packet starts with four 32-bit words.
pub const HEADER_LEN: usize = 16;
/// The handshake's fixed part (before its extensions).
pub const HANDSHAKE_LEN: usize = 48;
/// The longest stream id the specification allows, in bytes.
pub const MAX_STREAM_ID: usize = 512;
/// More handshake extensions than this is not a real client.
pub const MAX_EXTENSIONS: usize = 16;
/// More NAK entries than this in one packet is not a real sender.
pub const MAX_LOSS_ENTRIES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Shorter than its type requires.
    Truncated,
    /// A handshake extension runs past the end of the datagram.
    BadExtension,
    TooManyExtensions,
    StreamIdTooLong,
    /// A NAK whose loss list is malformed (a range start with no end).
    BadLossList,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated packet",
            Self::BadExtension => "handshake extension overruns the packet",
            Self::TooManyExtensions => "too many handshake extensions",
            Self::StreamIdTooLong => "stream id longer than 512 bytes",
            Self::BadLossList => "malformed loss list",
        })
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet<'a> {
    Data(DataPacket<'a>),
    Control(ControlPacket),
}

/// Where a data packet sits in its message (the PP field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Middle = 0,
    Last = 1,
    First = 2,
    /// A whole message in one packet: what live mode sends.
    Only = 3,
}

impl Position {
    fn from_bits(v: u32) -> Self {
        match v & 3 {
            0 => Self::Middle,
            1 => Self::Last,
            2 => Self::First,
            _ => Self::Only,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPacket<'a> {
    pub seq: Seq,
    pub position: Position,
    pub in_order: bool,
    /// The KK field: 0 when not encrypted, 1 even key, 2 odd key.
    pub key: u8,
    pub retransmitted: bool,
    /// 26 bits.
    pub message: u32,
    pub timestamp: u32,
    pub dest: u32,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPacket {
    pub timestamp: u32,
    pub dest: u32,
    pub body: Control,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Handshake(Handshake),
    KeepAlive,
    Ack(Ack),
    /// Lost sequence numbers as inclusive ranges, in packet order.
    Nak(Vec<(Seq, Seq)>),
    CongestionWarning,
    Shutdown,
    AckAck {
        number: u32,
    },
    DropRequest {
        message: u32,
        first: Seq,
        last: Seq,
    },
    PeerError {
        code: u32,
    },
    /// Any other control type, kept as it came (user-defined key material
    /// refreshes among them).
    Other {
        control_type: u16,
        subtype: u16,
        info: u32,
        cif: Vec<u8>,
    },
}

/// An ACK. `number` is 0 in a light or small ACK, which are not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    pub number: u32,
    /// The first sequence number not acknowledged.
    pub ack: Seq,
    /// Absent in a light ACK.
    pub stats: Option<AckStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckStats {
    /// Microseconds.
    pub rtt: u32,
    pub rtt_var: u32,
    /// Free receive buffer, in packets.
    pub buffer: u32,
    /// Packets per second, link capacity in packets per second, bytes per
    /// second. Absent in a small ACK.
    pub rates: Option<(u32, u32, u32)>,
}

/// The Handshake Type field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeKind {
    Done,
    Agreement,
    Conclusion,
    Waveahand,
    Induction,
    /// A refusal: 1000 and up are the protocol's reasons, 2000 + an HTTP
    /// status the application's.
    Rejection(u32),
}

impl HandshakeKind {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0xFFFF_FFFD => Self::Done,
            0xFFFF_FFFE => Self::Agreement,
            0xFFFF_FFFF => Self::Conclusion,
            0 => Self::Waveahand,
            1 => Self::Induction,
            other => Self::Rejection(other),
        }
    }

    pub fn to_u32(self) -> u32 {
        match self {
            Self::Done => 0xFFFF_FFFD,
            Self::Agreement => 0xFFFF_FFFE,
            Self::Conclusion => 0xFFFF_FFFF,
            Self::Waveahand => 0,
            Self::Induction => 1,
            Self::Rejection(v) => v,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub version: u32,
    pub encryption: u16,
    pub extension: u16,
    pub initial_seq: u32,
    pub mtu: u32,
    pub flow_window: u32,
    pub kind: HandshakeKind,
    pub socket_id: u32,
    pub cookie: u32,
    /// Four 32-bit words as on the wire (little-endian words; an IPv4
    /// address is the first word).
    pub peer_ip: [u8; 16],
    pub extensions: Vec<Extension>,
}

/// The extension flags in a version 5 conclusion's Extension Field.
pub mod ext_flags {
    pub const HSREQ: u16 = 1;
    pub const KMREQ: u16 = 2;
    pub const CONFIG: u16 = 4;
}

/// The magic number a version 5 listener puts in its induction reply.
pub const SRT_MAGIC: u16 = 0x4A17;

/// SRT flags in the handshake extension message.
pub mod srt_flags {
    pub const TSBPDSND: u32 = 0x01;
    pub const TSBPDRCV: u32 = 0x02;
    pub const CRYPT: u32 = 0x04;
    pub const TLPKTDROP: u32 = 0x08;
    pub const PERIODICNAK: u32 = 0x10;
    pub const REXMITFLG: u32 = 0x20;
    pub const STREAM: u32 = 0x40;
    pub const PACKET_FILTER: u32 = 0x80;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrtHandshake {
    /// major * 0x10000 + minor * 0x100 + patch.
    pub version: u32,
    pub flags: u32,
    /// Milliseconds.
    pub receiver_delay: u16,
    pub sender_delay: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extension {
    HsReq(SrtHandshake),
    HsRsp(SrtHandshake),
    KmReq(Vec<u8>),
    KmRsp(Vec<u8>),
    /// The stream id's bytes, without the padding.
    StreamId(Vec<u8>),
    Congestion(Vec<u8>),
    Filter(Vec<u8>),
    Group(Vec<u8>),
    Other {
        kind: u16,
        body: Vec<u8>,
    },
}

pub mod ext_type {
    pub const HSREQ: u16 = 1;
    pub const HSRSP: u16 = 2;
    pub const KMREQ: u16 = 3;
    pub const KMRSP: u16 = 4;
    pub const SID: u16 = 5;
    pub const CONGESTION: u16 = 6;
    pub const FILTER: u16 = 7;
    pub const GROUP: u16 = 8;
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    let w = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(w.try_into().ok()?))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let w = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes(w.try_into().ok()?))
}

/// Text in SRT's string extensions: 32-bit little-endian words, so each
/// group of four bytes is reversed, and the last is padded with zeros.
fn decode_words(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    for w in body.chunks(4) {
        out.extend(w.iter().rev());
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn encode_words(text: &[u8], out: &mut Vec<u8>) {
    for w in text.chunks(4) {
        let mut word = [0u8; 4];
        for (d, s) in word.iter_mut().zip(w) {
            *d = *s;
        }
        word.reverse();
        out.extend_from_slice(&word);
    }
}

impl<'a> Packet<'a> {
    pub fn parse(b: &'a [u8]) -> Result<Self, ParseError> {
        let (w0, w1, timestamp, dest) = match (be32(b, 0), be32(b, 4), be32(b, 8), be32(b, 12)) {
            (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
            _ => return Err(ParseError::Truncated),
        };
        let cif = b.get(HEADER_LEN..).unwrap_or_default();
        if w0 & 0x8000_0000 == 0 {
            return Ok(Self::Data(DataPacket {
                seq: Seq::new(w0),
                position: Position::from_bits(w1 >> 30),
                in_order: w1 & (1 << 29) != 0,
                key: ((w1 >> 27) & 3) as u8,
                retransmitted: w1 & (1 << 26) != 0,
                message: w1 & 0x03FF_FFFF,
                timestamp,
                dest,
                payload: cif,
            }));
        }
        let control_type = ((w0 >> 16) & 0x7FFF) as u16;
        let subtype = (w0 & 0xFFFF) as u16;
        let body = match control_type {
            0 => Control::Handshake(Handshake::parse(cif)?),
            1 => Control::KeepAlive,
            2 => Control::Ack(parse_ack(w1, cif)?),
            3 => Control::Nak(parse_loss_list(cif)?),
            4 => Control::CongestionWarning,
            5 => Control::Shutdown,
            6 => Control::AckAck { number: w1 },
            7 => Control::DropRequest {
                message: w1,
                first: Seq::new(be32(cif, 0).ok_or(ParseError::Truncated)?),
                last: Seq::new(be32(cif, 4).ok_or(ParseError::Truncated)?),
            },
            8 => Control::PeerError { code: w1 },
            _ => Control::Other {
                control_type,
                subtype,
                info: w1,
                cif: cif.to_vec(),
            },
        };
        Ok(Self::Control(ControlPacket {
            timestamp,
            dest,
            body,
        }))
    }

    /// Appends the packet's bytes to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Data(d) => d.encode(out),
            Self::Control(c) => c.encode(out),
        }
    }
}

impl DataPacket<'_> {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let w1 = ((self.position as u32) << 30)
            | (u32::from(self.in_order) << 29)
            | (u32::from(self.key & 3) << 27)
            | (u32::from(self.retransmitted) << 26)
            | (self.message & 0x03FF_FFFF);
        out.extend_from_slice(&self.seq.get().to_be_bytes());
        out.extend_from_slice(&w1.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.dest.to_be_bytes());
        out.extend_from_slice(self.payload);
    }
}

fn parse_ack(number: u32, cif: &[u8]) -> Result<Ack, ParseError> {
    let ack = Seq::new(be32(cif, 0).ok_or(ParseError::Truncated)?);
    let stats = match (be32(cif, 4), be32(cif, 8), be32(cif, 12)) {
        (Some(rtt), Some(rtt_var), Some(buffer)) => Some(AckStats {
            rtt,
            rtt_var,
            buffer,
            rates: match (be32(cif, 16), be32(cif, 20), be32(cif, 24)) {
                (Some(a), Some(b), Some(c)) => Some((a, b, c)),
                _ => None,
            },
        }),
        _ => None,
    };
    Ok(Ack { number, ack, stats })
}

/// Appendix A: a word with the top bit set starts a range whose last
/// number is the next word; any other word is one lost number.
fn parse_loss_list(cif: &[u8]) -> Result<Vec<(Seq, Seq)>, ParseError> {
    let mut out = Vec::new();
    let mut words = cif.chunks_exact(4).map(|w| {
        let mut a = [0u8; 4];
        a.copy_from_slice(w);
        u32::from_be_bytes(a)
    });
    while let Some(w) = words.next() {
        if out.len() >= MAX_LOSS_ENTRIES {
            break;
        }
        if w & 0x8000_0000 != 0 {
            let last = words.next().ok_or(ParseError::BadLossList)?;
            out.push((Seq::new(w), Seq::new(last)));
        } else {
            out.push((Seq::new(w), Seq::new(w)));
        }
    }
    Ok(out)
}

/// Encodes a loss list: a single number as itself, two or more as a range
/// (as libsrt does, observed: it sends a range for two numbers too).
pub fn encode_loss_list(ranges: &[(Seq, Seq)], out: &mut Vec<u8>) {
    for (a, b) in ranges {
        if a == b {
            out.extend_from_slice(&a.get().to_be_bytes());
        } else {
            out.extend_from_slice(&(a.get() | 0x8000_0000).to_be_bytes());
            out.extend_from_slice(&b.get().to_be_bytes());
        }
    }
}

impl ControlPacket {
    pub fn new(timestamp: u32, dest: u32, body: Control) -> Self {
        Self {
            timestamp,
            dest,
            body,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let (control_type, subtype, info) = match &self.body {
            Control::Handshake(_) => (0, 0, 0),
            Control::KeepAlive => (1, 0, 0),
            Control::Ack(a) => (2, 0, a.number),
            Control::Nak(_) => (3, 0, 0),
            Control::CongestionWarning => (4, 0, 0),
            Control::Shutdown => (5, 0, 0),
            Control::AckAck { number } => (6, 0, *number),
            Control::DropRequest { message, .. } => (7, 0, *message),
            Control::PeerError { code } => (8, 0, *code),
            Control::Other {
                control_type,
                subtype,
                info,
                ..
            } => (*control_type & 0x7FFF, *subtype, *info),
        };
        let w0 = 0x8000_0000 | (u32::from(control_type) << 16) | u32::from(subtype);
        out.extend_from_slice(&w0.to_be_bytes());
        out.extend_from_slice(&info.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.dest.to_be_bytes());
        match &self.body {
            Control::Handshake(h) => h.encode(out),
            Control::Ack(a) => {
                out.extend_from_slice(&a.ack.get().to_be_bytes());
                if let Some(s) = &a.stats {
                    for v in [s.rtt, s.rtt_var, s.buffer] {
                        out.extend_from_slice(&v.to_be_bytes());
                    }
                    if let Some((p, c, r)) = s.rates {
                        for v in [p, c, r] {
                            out.extend_from_slice(&v.to_be_bytes());
                        }
                    }
                }
            }
            Control::Nak(list) => encode_loss_list(list, out),
            Control::DropRequest { first, last, .. } => {
                out.extend_from_slice(&first.get().to_be_bytes());
                out.extend_from_slice(&last.get().to_be_bytes());
            }
            Control::Other { cif, .. } => out.extend_from_slice(cif),
            Control::KeepAlive
            | Control::CongestionWarning
            | Control::Shutdown
            | Control::AckAck { .. }
            | Control::PeerError { .. } => {}
        }
    }
}

impl Handshake {
    pub fn parse(cif: &[u8]) -> Result<Self, ParseError> {
        let word = |at| be32(cif, at).ok_or(ParseError::Truncated);
        let version = word(0)?;
        let enc_ext = word(4)?;
        let mut peer_ip = [0u8; 16];
        peer_ip.copy_from_slice(cif.get(32..48).ok_or(ParseError::Truncated)?);
        let mut h = Self {
            version,
            encryption: (enc_ext >> 16) as u16,
            extension: (enc_ext & 0xFFFF) as u16,
            initial_seq: word(8)?,
            mtu: word(12)?,
            flow_window: word(16)?,
            kind: HandshakeKind::from_u32(word(20)?),
            socket_id: word(24)?,
            cookie: word(28)?,
            peer_ip,
            extensions: Vec::new(),
        };
        let mut rest = cif.get(HANDSHAKE_LEN..).unwrap_or_default();
        while rest.len() >= 4 {
            if h.extensions.len() >= MAX_EXTENSIONS {
                return Err(ParseError::TooManyExtensions);
            }
            let kind = be16(rest, 0).ok_or(ParseError::Truncated)?;
            let words = usize::from(be16(rest, 2).ok_or(ParseError::Truncated)?);
            let end = 4 + words * 4;
            let body = rest.get(4..end).ok_or(ParseError::BadExtension)?;
            rest = rest.get(end..).unwrap_or_default();
            h.extensions.push(match kind {
                ext_type::HSREQ | ext_type::HSRSP => {
                    let s = SrtHandshake {
                        version: be32(body, 0).ok_or(ParseError::BadExtension)?,
                        flags: be32(body, 4).ok_or(ParseError::BadExtension)?,
                        receiver_delay: be16(body, 8).ok_or(ParseError::BadExtension)?,
                        sender_delay: be16(body, 10).ok_or(ParseError::BadExtension)?,
                    };
                    if kind == ext_type::HSREQ {
                        Extension::HsReq(s)
                    } else {
                        Extension::HsRsp(s)
                    }
                }
                ext_type::KMREQ => Extension::KmReq(body.to_vec()),
                ext_type::KMRSP => Extension::KmRsp(body.to_vec()),
                ext_type::SID => {
                    if body.len() > MAX_STREAM_ID {
                        return Err(ParseError::StreamIdTooLong);
                    }
                    Extension::StreamId(decode_words(body))
                }
                ext_type::CONGESTION => Extension::Congestion(decode_words(body)),
                ext_type::FILTER => Extension::Filter(decode_words(body)),
                ext_type::GROUP => Extension::Group(body.to_vec()),
                kind => Extension::Other {
                    kind,
                    body: body.to_vec(),
                },
            });
        }
        Ok(h)
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&self.encryption.to_be_bytes());
        out.extend_from_slice(&self.extension.to_be_bytes());
        for v in [
            self.initial_seq,
            self.mtu,
            self.flow_window,
            self.kind.to_u32(),
            self.socket_id,
            self.cookie,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&self.peer_ip);
        for e in &self.extensions {
            let (kind, start) = (e.kind(), out.len());
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&[0, 0]);
            match e {
                Extension::HsReq(s) | Extension::HsRsp(s) => {
                    out.extend_from_slice(&s.version.to_be_bytes());
                    out.extend_from_slice(&s.flags.to_be_bytes());
                    out.extend_from_slice(&s.receiver_delay.to_be_bytes());
                    out.extend_from_slice(&s.sender_delay.to_be_bytes());
                }
                Extension::StreamId(t) | Extension::Congestion(t) | Extension::Filter(t) => {
                    encode_words(t, out);
                }
                Extension::KmReq(b)
                | Extension::KmRsp(b)
                | Extension::Group(b)
                | Extension::Other { body: b, .. } => {
                    out.extend_from_slice(b);
                    // Whole words only: the length is counted in words.
                    out.resize(out.len() + (4 - b.len() % 4) % 4, 0);
                }
            }
            let words = (out.len() - start - 4) / 4;
            let words = u16::try_from(words).unwrap_or(u16::MAX).to_be_bytes();
            if let Some(len) = out.get_mut(start + 2..start + 4) {
                len.copy_from_slice(&words);
            }
        }
    }

    pub fn stream_id(&self) -> Option<&[u8]> {
        self.extensions.iter().find_map(|e| match e {
            Extension::StreamId(s) => Some(s.as_slice()),
            _ => None,
        })
    }

    pub fn srt_request(&self) -> Option<SrtHandshake> {
        self.extensions.iter().find_map(|e| match e {
            Extension::HsReq(s) => Some(*s),
            _ => None,
        })
    }
}

impl Extension {
    pub fn kind(&self) -> u16 {
        match self {
            Self::HsReq(_) => ext_type::HSREQ,
            Self::HsRsp(_) => ext_type::HSRSP,
            Self::KmReq(_) => ext_type::KMREQ,
            Self::KmRsp(_) => ext_type::KMRSP,
            Self::StreamId(_) => ext_type::SID,
            Self::Congestion(_) => ext_type::CONGESTION,
            Self::Filter(_) => ext_type::FILTER,
            Self::Group(_) => ext_type::GROUP,
            Self::Other { kind, .. } => *kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn round_trip(p: &Packet) -> Vec<u8> {
        let mut out = Vec::new();
        p.encode(&mut out);
        let again = Packet::parse(&out).expect("parses");
        assert_eq!(&again, p);
        out
    }

    /// A conclusion from `srt-live-transmit` (libsrt 1.5.4), captured.
    const CONCLUSION: &str = "800000000000000000000b5e00000000000000050000000545127ddb000005dc\
        00002000ffffffff3411671effc6f8a50100007f000000000000000000000000\
        0001000300010504000000bf07d007d0000500066c6275703a6873696576696c\
        6576652f782d746e703a753a";

    #[test]
    fn parses_a_captured_conclusion() {
        let b = hex(CONCLUSION);
        let Packet::Control(c) = Packet::parse(&b).expect("parses") else {
            panic!("control");
        };
        assert_eq!(c.timestamp, 2910);
        assert_eq!(c.dest, 0);
        let Control::Handshake(h) = &c.body else {
            panic!("handshake");
        };
        assert_eq!(h.version, 5);
        assert_eq!(h.extension, ext_flags::HSREQ | ext_flags::CONFIG);
        assert_eq!(h.kind, HandshakeKind::Conclusion);
        assert_eq!(h.socket_id, 0x3411_671E);
        assert_eq!(h.cookie, 0xFFC6_F8A5);
        assert_eq!(h.peer_ip[..4], [1, 0, 0, 127]);
        assert_eq!(h.stream_id(), Some(&b"publish:live/event-x:u:p"[..]));
        let req = h.srt_request().expect("HSREQ");
        assert_eq!(req.version, 0x01_05_04);
        assert_eq!(req.flags, 0xBF);
        assert_eq!((req.receiver_delay, req.sender_delay), (2000, 2000));
        // Encoding gives the same bytes back.
        assert_eq!(round_trip(&Packet::Control(c)), b);
    }

    #[test]
    fn stream_ids_are_little_endian_words_padded_with_zeros() {
        // "read:live/event-x" as srt-live-transmit sent it.
        let body = hex("6461657276696c3a76652f652d746e6500000078");
        assert_eq!(decode_words(&body), b"read:live/event-x");
        let mut out = Vec::new();
        encode_words(b"read:live/event-x", &mut out);
        assert_eq!(out, body);
    }

    #[test]
    fn data_packet_fields() {
        let payload = [1u8, 2, 3];
        let d = DataPacket {
            seq: Seq::new(0x7FFF_FFFF),
            position: Position::Only,
            in_order: false,
            key: 0,
            retransmitted: true,
            message: 0x03FF_FFFF,
            timestamp: 0xFFFF_FFFF,
            dest: 7,
            payload: &payload,
        };
        let out = round_trip(&Packet::Data(d));
        assert_eq!(&out[..4], &[0x7F, 0xFF, 0xFF, 0xFF]);
        assert_eq!(out[4], 0b1100_0111, "PP=11, O=0, KK=00, R=1, top msg bits");
        assert_eq!(&out[16..], &payload);
    }

    #[test]
    fn captured_ack_and_naks() {
        // Full ACK from libsrt's receiver.
        let b = hex("80020000000000010018307000000000\
            45127de5000186a00000c35000001ff500000000000003e800000000");
        let Ok(Packet::Control(ControlPacket {
            body: Control::Ack(a),
            ..
        })) = Packet::parse(&b)
        else {
            panic!("ACK");
        };
        assert_eq!(a.number, 1);
        assert_eq!(a.ack, Seq::new(0x4512_7DE5));
        let s = a.stats.expect("full");
        assert_eq!((s.rtt, s.rtt_var, s.buffer), (100_000, 50_000, 0x1FF5));
        assert_eq!(s.rates, Some((0, 1000, 0)));
        let mut out = Vec::new();
        Packet::parse(&b).expect("ACK").encode(&mut out);
        assert_eq!(out, b);

        // A NAK with a two-number range then a single number.
        let b = hex("8003000000000000000000001d3ac291f9781e1679781e1779781e20");
        let Ok(Packet::Control(ControlPacket {
            body: Control::Nak(list),
            ..
        })) = Packet::parse(&b)
        else {
            panic!("NAK");
        };
        let s = |v| Seq::new(v);
        assert_eq!(
            list,
            vec![
                (s(0x7978_1E16), s(0x7978_1E17)),
                (s(0x7978_1E20), s(0x7978_1E20))
            ]
        );
        let mut out = Vec::new();
        Packet::parse(&b).expect("NAK").encode(&mut out);
        assert_eq!(out, b);
    }

    #[test]
    fn light_and_small_acks() {
        for stats in [
            None,
            Some(AckStats {
                rtt: 1,
                rtt_var: 2,
                buffer: 3,
                rates: None,
            }),
        ] {
            let p = Packet::Control(ControlPacket::new(
                9,
                10,
                Control::Ack(Ack {
                    number: 0,
                    ack: Seq::new(5),
                    stats,
                }),
            ));
            let out = round_trip(&p);
            assert_eq!(out.len(), if stats.is_some() { 32 } else { 20 });
        }
    }

    #[test]
    fn every_control_type_round_trips() {
        let s = Seq::new;
        for body in [
            Control::KeepAlive,
            Control::Shutdown,
            Control::CongestionWarning,
            Control::AckAck { number: 77 },
            Control::PeerError { code: 4000 },
            Control::DropRequest {
                message: 3,
                first: s(10),
                last: s(12),
            },
            Control::Nak(vec![(s(1), s(1)), (s(3), s(9))]),
            Control::Other {
                control_type: 0x7FFF,
                subtype: 3,
                info: 1,
                cif: vec![1, 2, 3, 4],
            },
        ] {
            round_trip(&Packet::Control(ControlPacket::new(1, 2, body)));
        }
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert_eq!(Packet::parse(&[]), Err(ParseError::Truncated));
        assert_eq!(Packet::parse(&[0x80; 15]), Err(ParseError::Truncated));
        // A handshake too short for its fixed part.
        let mut b = hex("80000000000000000000000000000000");
        b.extend([0; 47]);
        assert_eq!(Packet::parse(&b), Err(ParseError::Truncated));
        // An extension claiming more words than there are.
        b.push(0);
        b.extend([0, 5, 0, 9, 1, 2, 3, 4]);
        assert_eq!(Packet::parse(&b), Err(ParseError::BadExtension));
        // A range start with no end.
        let b = hex("80030000000000000000000000000000f9781e16");
        assert_eq!(Packet::parse(&b), Err(ParseError::BadLossList));
        // ACK with no CIF; drop request with half a CIF.
        assert_eq!(
            Packet::parse(&hex("80020000000000000000000000000000")),
            Err(ParseError::Truncated)
        );
        assert_eq!(
            Packet::parse(&hex("8007000000000000000000000000000000000001")),
            Err(ParseError::Truncated)
        );
    }

    #[test]
    fn stream_id_limit_and_extension_count() {
        let mut h = Handshake {
            version: 5,
            encryption: 0,
            extension: 0,
            initial_seq: 0,
            mtu: 1500,
            flow_window: 8192,
            kind: HandshakeKind::Conclusion,
            socket_id: 1,
            cookie: 2,
            peer_ip: [0; 16],
            extensions: vec![Extension::StreamId(vec![b'a'; MAX_STREAM_ID])],
        };
        let mut out = Vec::new();
        h.encode(&mut out);
        assert!(Handshake::parse(&out).is_ok());
        h.extensions = vec![Extension::StreamId(vec![b'a'; MAX_STREAM_ID + 1])];
        out.clear();
        h.encode(&mut out);
        assert_eq!(Handshake::parse(&out), Err(ParseError::StreamIdTooLong));
        h.extensions = vec![
            Extension::Other {
                kind: 99,
                body: vec![]
            };
            MAX_EXTENSIONS + 1
        ];
        out.clear();
        h.encode(&mut out);
        assert_eq!(Handshake::parse(&out), Err(ParseError::TooManyExtensions));
    }

    #[test]
    fn unknown_control_types_are_kept() {
        let b = hex("8123456700000009000000010000000201020304");
        let p = Packet::parse(&b).expect("parses");
        assert_eq!(
            p,
            Packet::Control(ControlPacket::new(
                1,
                2,
                Control::Other {
                    control_type: 0x0123,
                    subtype: 0x4567,
                    info: 9,
                    cif: vec![1, 2, 3, 4]
                }
            ))
        );
        let mut out = Vec::new();
        p.encode(&mut out);
        assert_eq!(out, b);
    }
}
