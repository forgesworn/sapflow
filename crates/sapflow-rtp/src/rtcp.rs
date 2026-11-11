//! RTCP compound packets: parsing (RFC 3550 section 6, RFC 4585 section
//! 6, RFC 5104 section 4.3.1) and the packets sapflow sends ([`build`]).
//!
//! [`Compound::parse`] walks every packet header in a datagram first:
//! version 2, a length inside the datagram, lengths that add up exactly,
//! and padding only on the last packet and inside it. Reduced-size RTCP
//! (RFC 5506) is accepted, so the first packet need not be a report;
//! [`Compound::starts_with_report`] tells a caller that wants the RFC 3550
//! rule. [`Compound::packets`] then reads each body: sender and receiver
//! reports, generic NACK, PLI and FIR. Everything else (SDES, BYE, APP,
//! transport-wide feedback, REMB and unknown types) is returned as
//! [`Packet::Other`] with its length already checked, so it can be skipped.

pub mod build;

use crate::rtp::{be16, be32};
use std::fmt;

/// Sender report (RFC 3550 section 6.4.1).
pub const PT_SR: u8 = 200;
/// Receiver report (RFC 3550 section 6.4.2).
pub const PT_RR: u8 = 201;
/// Source description (RFC 3550 section 6.5).
pub const PT_SDES: u8 = 202;
/// Goodbye (RFC 3550 section 6.6).
pub const PT_BYE: u8 = 203;
/// Application-defined (RFC 3550 section 6.7).
pub const PT_APP: u8 = 204;
/// Transport-layer feedback (RFC 4585 section 6.2).
pub const PT_RTPFB: u8 = 205;
/// Payload-specific feedback (RFC 4585 section 6.3).
pub const PT_PSFB: u8 = 206;
/// RTPFB format: generic NACK (RFC 4585 section 6.2.1).
pub const FMT_NACK: u8 = 1;
/// PSFB format: picture loss indication (RFC 4585 section 6.3.1).
pub const FMT_PLI: u8 = 1;
/// PSFB format: full intra request (RFC 5104 section 4.3.1).
pub const FMT_FIR: u8 = 4;
/// RTPFB format: transport-wide congestion control feedback
/// (draft-holmer-rmcat-transport-wide-cc-extensions-01 section 3.1).
pub const FMT_TRANSPORT_CC: u8 = 15;

/// Length of one report block.
const REPORT_BLOCK_LEN: usize = 24;

/// Why an RTCP datagram, or one packet in it, was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcpError {
    /// No packets at all.
    Empty,
    /// Fewer than four bytes left for a packet header.
    TooShort,
    /// A version other than 2.
    Version(u8),
    /// A length field that runs past the datagram.
    Length,
    /// The padding bit on a packet other than the last, or a padding count
    /// of zero or past the packet body.
    Padding,
    /// A body too short or the wrong shape for its packet type and count.
    Body {
        /// The packet type.
        packet_type: u8,
        /// The count or FMT field.
        count: u8,
    },
}
impl fmt::Display for RtcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RTCP: {self:?}")
    }
}
impl std::error::Error for RtcpError {}

/// A checked RTCP datagram: one packet or a compound of several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compound<'a> {
    buf: &'a [u8],
}

impl<'a> Compound<'a> {
    /// Checks every packet header in `buf`. Bodies are read later, by
    /// [`packets`](Self::packets).
    pub fn parse(buf: &'a [u8]) -> Result<Self, RtcpError> {
        if buf.is_empty() {
            return Err(RtcpError::Empty);
        }
        let mut pos = 0;
        while pos < buf.len() {
            let (len, _, _) = header(buf, pos)?;
            pos += len;
        }
        Ok(Self { buf })
    }

    /// Whether the first packet is a sender or receiver report, as RFC 3550
    /// section 6.1 requires of a full compound packet. Reduced-size RTCP
    /// (RFC 5506) need not be.
    pub fn starts_with_report(&self) -> bool {
        matches!(self.buf.get(1), Some(&PT_SR | &PT_RR))
    }

    /// The datagram.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.buf
    }

    /// The packets in order. After an error the iterator ends.
    pub fn packets(&self) -> Packets<'a> {
        Packets {
            buf: self.buf,
            pos: 0,
        }
    }
}

/// Checks the header at `pos`: returns the packet length, the body (after
/// the four-byte header, without padding) bounds.
fn header(buf: &[u8], pos: usize) -> Result<(usize, usize, usize), RtcpError> {
    let first = *buf.get(pos).ok_or(RtcpError::TooShort)?;
    let words = be16(buf, pos + 2).ok_or(RtcpError::TooShort)?;
    let version = first >> 6;
    if version != 2 {
        return Err(RtcpError::Version(version));
    }
    let len = 4 * (usize::from(words) + 1);
    let end = pos.checked_add(len).ok_or(RtcpError::Length)?;
    if end > buf.len() {
        return Err(RtcpError::Length);
    }
    let mut body_end = end;
    if first & 0x20 != 0 {
        if end != buf.len() {
            return Err(RtcpError::Padding);
        }
        let count = usize::from(*buf.get(end - 1).ok_or(RtcpError::Padding)?);
        if count == 0 || count > len - 4 {
            return Err(RtcpError::Padding);
        }
        body_end = end - count;
    }
    Ok((len, pos + 4, body_end))
}

/// Iterator over the packets of a [`Compound`].
#[derive(Debug, Clone)]
pub struct Packets<'a> {
    buf: &'a [u8],
    pos: usize,
}
impl<'a> Iterator for Packets<'a> {
    type Item = Result<Packet<'a>, RtcpError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let parsed = header(self.buf, self.pos).and_then(|(len, start, end)| {
            let first = self.buf.get(self.pos).copied().unwrap_or(0);
            let packet_type = self.buf.get(self.pos + 1).copied().unwrap_or(0);
            let body = self.buf.get(start..end).ok_or(RtcpError::Length)?;
            Packet::read(packet_type, first & 0x1f, body).map(|p| (len, p))
        });
        match parsed {
            Ok((len, packet)) => {
                self.pos += len;
                Some(Ok(packet))
            }
            Err(e) => {
                self.pos = self.buf.len();
                Some(Err(e))
            }
        }
    }
}
impl std::iter::FusedIterator for Packets<'_> {}

/// One RTCP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// A sender report.
    SenderReport(SenderReport<'a>),
    /// A receiver report.
    ReceiverReport(ReceiverReport<'a>),
    /// A generic NACK.
    Nack(Nack<'a>),
    /// A picture loss indication.
    Pli(Pli),
    /// A full intra request.
    Fir(Fir<'a>),
    /// Any other packet: SDES, BYE, APP, other feedback formats, unknown
    /// types. Its length has been checked; it can be skipped.
    Other(Other<'a>),
}

impl<'a> Packet<'a> {
    fn read(packet_type: u8, count: u8, body: &'a [u8]) -> Result<Self, RtcpError> {
        let bad = RtcpError::Body { packet_type, count };
        let blocks = REPORT_BLOCK_LEN * usize::from(count);
        Ok(match (packet_type, count) {
            (PT_SR, _) => {
                if body.len() < 24 + blocks {
                    return Err(bad);
                }
                Packet::SenderReport(SenderReport {
                    body,
                    count: usize::from(count),
                })
            }
            (PT_RR, _) => {
                if body.len() < 4 + blocks {
                    return Err(bad);
                }
                Packet::ReceiverReport(ReceiverReport {
                    body,
                    count: usize::from(count),
                })
            }
            (PT_RTPFB, FMT_NACK) => {
                let fci = body.get(8..).ok_or(bad)?;
                if fci.is_empty() || !fci.len().is_multiple_of(4) {
                    return Err(bad);
                }
                Packet::Nack(Nack { body })
            }
            (PT_PSFB, FMT_PLI) => {
                if body.len() < 8 {
                    return Err(bad);
                }
                Packet::Pli(Pli {
                    sender_ssrc: be32(body, 0).ok_or(bad)?,
                    media_ssrc: be32(body, 4).ok_or(bad)?,
                })
            }
            (PT_PSFB, FMT_FIR) => {
                let fci = body.get(8..).ok_or(bad)?;
                if fci.is_empty() || !fci.len().is_multiple_of(8) {
                    return Err(bad);
                }
                Packet::Fir(Fir { body })
            }
            _ => Packet::Other(Other {
                packet_type,
                count,
                body,
            }),
        })
    }
}

/// A packet this crate does not interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Other<'a> {
    /// Packet type.
    pub packet_type: u8,
    /// The five-bit count or FMT field.
    pub count: u8,
    /// The body after the four-byte header, without padding.
    pub body: &'a [u8],
}

/// One reception report block (RFC 3550 section 6.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportBlock {
    /// The source this block reports on.
    pub ssrc: u32,
    /// Fraction lost since the last report, in 1/256ths.
    pub fraction_lost: u8,
    /// Cumulative packets lost, a signed 24-bit value (duplicates can make
    /// it negative). Clamped to that range when written.
    pub cumulative_lost: i32,
    /// Extended highest sequence number received.
    pub highest_sequence: u32,
    /// Interarrival jitter, in timestamp units.
    pub jitter: u32,
    /// Middle 32 bits of the NTP timestamp of the last SR received.
    pub last_sr: u32,
    /// Delay since that SR, in 1/65536 seconds.
    pub delay_since_last_sr: u32,
}
impl ReportBlock {
    fn read(b: &[u8]) -> Option<Self> {
        let lost = be32(b, 4)?;
        // Sign-extend the low 24 bits.
        let cumulative_lost = ((lost << 8) as i32) >> 8;
        Some(Self {
            ssrc: be32(b, 0)?,
            fraction_lost: (lost >> 24) as u8,
            cumulative_lost,
            highest_sequence: be32(b, 8)?,
            jitter: be32(b, 12)?,
            last_sr: be32(b, 16)?,
            delay_since_last_sr: be32(b, 20)?,
        })
    }
}

/// Iterator over the report blocks of an SR or RR.
#[derive(Debug, Clone)]
pub struct ReportBlocks<'a> {
    blocks: std::slice::ChunksExact<'a, u8>,
}
impl Iterator for ReportBlocks<'_> {
    type Item = ReportBlock;
    fn next(&mut self) -> Option<ReportBlock> {
        self.blocks.next().and_then(ReportBlock::read)
    }
}

fn blocks(body: &[u8], from: usize, count: usize) -> ReportBlocks<'_> {
    let bytes = body
        .get(from..from + REPORT_BLOCK_LEN * count)
        .unwrap_or_default();
    ReportBlocks {
        blocks: bytes.chunks_exact(REPORT_BLOCK_LEN),
    }
}

/// A sender report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderReport<'a> {
    body: &'a [u8],
    count: usize,
}
impl<'a> SenderReport<'a> {
    /// The sender's SSRC.
    pub fn ssrc(&self) -> u32 {
        be32(self.body, 0).unwrap_or(0)
    }
    /// The 64-bit NTP timestamp.
    pub fn ntp_time(&self) -> u64 {
        u64::from(be32(self.body, 4).unwrap_or(0)) << 32
            | u64::from(be32(self.body, 8).unwrap_or(0))
    }
    /// The RTP timestamp matching [`ntp_time`](Self::ntp_time).
    pub fn rtp_time(&self) -> u32 {
        be32(self.body, 12).unwrap_or(0)
    }
    /// Packets sent.
    pub fn packet_count(&self) -> u32 {
        be32(self.body, 16).unwrap_or(0)
    }
    /// Payload octets sent.
    pub fn octet_count(&self) -> u32 {
        be32(self.body, 20).unwrap_or(0)
    }
    /// The report blocks.
    pub fn reports(&self) -> ReportBlocks<'a> {
        blocks(self.body, 24, self.count)
    }
}

/// A receiver report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverReport<'a> {
    body: &'a [u8],
    count: usize,
}
impl<'a> ReceiverReport<'a> {
    /// The reporter's SSRC.
    pub fn ssrc(&self) -> u32 {
        be32(self.body, 0).unwrap_or(0)
    }
    /// The report blocks.
    pub fn reports(&self) -> ReportBlocks<'a> {
        blocks(self.body, 4, self.count)
    }
}

/// A generic NACK (RFC 4585 section 6.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nack<'a> {
    body: &'a [u8],
}
impl<'a> Nack<'a> {
    /// SSRC of the packet sender.
    pub fn sender_ssrc(&self) -> u32 {
        be32(self.body, 0).unwrap_or(0)
    }
    /// SSRC of the media source the NACK is about.
    pub fn media_ssrc(&self) -> u32 {
        be32(self.body, 4).unwrap_or(0)
    }
    /// The `(PID, BLP)` pairs.
    pub fn pairs(&self) -> impl Iterator<Item = (u16, u16)> + 'a {
        self.body
            .get(8..)
            .unwrap_or_default()
            .chunks_exact(4)
            .filter_map(|c| Some((be16(c, 0)?, be16(c, 2)?)))
    }
    /// Every sequence number the NACK reports lost, pair by pair: the PID,
    /// then PID+1 to PID+16 where BLP has the bit set.
    pub fn lost(&self) -> impl Iterator<Item = u16> + 'a {
        self.pairs().flat_map(|(pid, blp)| {
            std::iter::once(pid).chain(
                (0..16u16)
                    .filter(move |bit| blp & (1 << bit) != 0)
                    .map(move |bit| pid.wrapping_add(bit + 1)),
            )
        })
    }
}

/// A picture loss indication (RFC 4585 section 6.3.1). Bytes after the two
/// SSRCs, which PLI does not define, are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pli {
    /// SSRC of the packet sender.
    pub sender_ssrc: u32,
    /// SSRC of the media source.
    pub media_ssrc: u32,
}

/// A full intra request (RFC 5104 section 4.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fir<'a> {
    body: &'a [u8],
}
impl<'a> Fir<'a> {
    /// SSRC of the packet sender.
    pub fn sender_ssrc(&self) -> u32 {
        be32(self.body, 0).unwrap_or(0)
    }
    /// The media source field, which RFC 5104 says is set to 0.
    pub fn media_ssrc(&self) -> u32 {
        be32(self.body, 4).unwrap_or(0)
    }
    /// The `(SSRC, command sequence number)` entries.
    pub fn entries(&self) -> impl Iterator<Item = (u32, u8)> + 'a {
        self.body
            .get(8..)
            .unwrap_or_default()
            .chunks_exact(8)
            .filter_map(|c| Some((be32(c, 0)?, *c.get(4)?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7714 section 17.1's plaintext: a sender report whose length field
    // (13) promises one word more than the 52 bytes present.
    const RFC7714_SR: [u8; 52] = [
        0x81, 0xc8, 0x00, 0x0d, 0x4d, 0x61, 0x72, 0x73, 0x4e, 0x54, 0x50, 0x31, 0x4e, 0x54, 0x50,
        0x32, 0x52, 0x54, 0x50, 0x20, 0x00, 0x00, 0x04, 0x2a, 0x00, 0x00, 0xe9, 0x30, 0x4c, 0x75,
        0x6e, 0x61, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef, 0xde,
        0xad, 0xbe, 0xef, 0xde, 0xad, 0xbe, 0xef,
    ];

    #[test]
    fn rfc7714_vector_length_mismatch_is_refused() {
        assert_eq!(Compound::parse(&RFC7714_SR), Err(RtcpError::Length));
        let mut fixed = RFC7714_SR;
        fixed[3] = 12;
        let c = Compound::parse(&fixed).expect("parses");
        let Some(Ok(Packet::SenderReport(sr))) = c.packets().next() else {
            panic!("a sender report");
        };
        assert_eq!(sr.ssrc(), 0x4d61_7273);
        assert_eq!(sr.ntp_time(), 0x4e54_5031_4e54_5032);
        assert_eq!(sr.rtp_time(), 0x5254_5020);
        assert_eq!(sr.packet_count(), 0x42a);
        assert_eq!(sr.octet_count(), 0xe930);
        let blocks: Vec<_> = sr.reports().collect();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ssrc, 0x4c75_6e61);
        assert_eq!(blocks[0].fraction_lost, 0xde);
        assert_eq!(blocks[0].cumulative_lost, -0x524111);
        assert_eq!(blocks[0].highest_sequence, 0xdead_beef);
    }

    fn rr(blocks: &[ReportBlock]) -> Vec<u8> {
        let mut out = Vec::new();
        build::receiver_report(&mut out, 0x1234_5678, blocks);
        out
    }

    #[test]
    fn report_block_roundtrip() {
        let block = ReportBlock {
            ssrc: 9,
            fraction_lost: 25,
            cumulative_lost: -3,
            highest_sequence: 0x0001_0005,
            jitter: 77,
            last_sr: 0xaabb_ccdd,
            delay_since_last_sr: 0x1_0000,
        };
        let bytes = rr(&[block, ReportBlock::default()]);
        let c = Compound::parse(&bytes).expect("parses");
        assert!(c.starts_with_report());
        let Some(Ok(Packet::ReceiverReport(r))) = c.packets().next() else {
            panic!("a receiver report");
        };
        assert_eq!(r.ssrc(), 0x1234_5678);
        assert_eq!(
            r.reports().collect::<Vec<_>>(),
            [block, ReportBlock::default()]
        );
    }

    #[test]
    fn reduced_size_feedback_alone_is_accepted() {
        let mut out = Vec::new();
        build::pli(&mut out, 1, 2);
        let c = Compound::parse(&out).expect("parses");
        assert!(!c.starts_with_report());
        assert_eq!(
            c.packets().collect::<Vec<_>>(),
            [Ok(Packet::Pli(Pli {
                sender_ssrc: 1,
                media_ssrc: 2
            }))]
        );
    }

    #[test]
    fn others_are_skipped_with_length_checked() {
        // SDES with one chunk, BYE, APP, REMB (PSFB 15), transport-cc
        // (RTPFB 15), an unknown type.
        let mut out = rr(&[]);
        out.extend_from_slice(&[0x81, PT_SDES, 0, 2, 0, 0, 0, 1, 1, 1, b'a', 0]);
        out.extend_from_slice(&[0x81, PT_BYE, 0, 1, 0, 0, 0, 1]);
        out.extend_from_slice(&[0x80, PT_APP, 0, 2, 0, 0, 0, 1, b'n', b'a', b'm', b'e']);
        out.extend_from_slice(&[0x8f, PT_PSFB, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0]);
        out.extend_from_slice(&[0x8f, PT_RTPFB, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2]);
        out.extend_from_slice(&[0x80, 210, 0, 0]);
        build::nack(&mut out, 1, 2, &[5]).expect("nack");
        let c = Compound::parse(&out).expect("parses");
        let kinds: Vec<_> = c
            .packets()
            .map(|p| match p.expect("ok") {
                Packet::Other(o) => (o.packet_type, o.count),
                Packet::ReceiverReport(_) => (PT_RR, 0),
                Packet::Nack(_) => (PT_RTPFB, FMT_NACK),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                (PT_RR, 0),
                (PT_SDES, 1),
                (PT_BYE, 1),
                (PT_APP, 0),
                (PT_PSFB, 15),
                (PT_RTPFB, 15),
                (210, 0),
                (PT_RTPFB, FMT_NACK)
            ]
        );
    }

    #[test]
    fn padding_only_on_the_last_packet() {
        let mut out = rr(&[]);
        out.extend_from_slice(&[0xa0, PT_BYE, 0, 2, 0, 0, 0, 1, 0, 0, 0, 4]);
        let c = Compound::parse(&out).expect("parses");
        let last = c.packets().last().expect("two").expect("ok");
        assert_eq!(
            last,
            Packet::Other(Other {
                packet_type: PT_BYE,
                count: 0,
                body: &[0, 0, 0, 1]
            })
        );
        // Padding bit on the first of two packets.
        let mut first = out.clone();
        first[0] |= 0x20;
        assert_eq!(Compound::parse(&first), Err(RtcpError::Padding));
        // Padding count of zero, or reaching into the header.
        let mut zero = out.clone();
        *zero.last_mut().expect("byte") = 0;
        assert_eq!(Compound::parse(&zero), Err(RtcpError::Padding));
        let mut big = out.clone();
        *big.last_mut().expect("byte") = 9;
        assert_eq!(Compound::parse(&big), Err(RtcpError::Padding));
    }

    #[test]
    fn body_errors_end_iteration() {
        // An RR claiming two blocks with room for none, then a valid PLI.
        let mut out = vec![0x82, PT_RR, 0, 1, 0, 0, 0, 1];
        build::pli(&mut out, 1, 2);
        let c = Compound::parse(&out).expect("headers are fine");
        let all: Vec<_> = c.packets().collect();
        assert_eq!(
            all,
            [Err(RtcpError::Body {
                packet_type: PT_RR,
                count: 2
            })]
        );
    }

    #[test]
    fn malformed_and_truncated_refused() {
        assert_eq!(Compound::parse(&[]), Err(RtcpError::Empty));
        let mut out = rr(&[ReportBlock::default()]);
        build::nack(&mut out, 1, 2, &[1, 2, 40]).expect("nack");
        build::fir(&mut out, 1, &[(5, 1)]).expect("fir");
        for len in 1..out.len() {
            let Ok(c) = Compound::parse(&out[..len]) else {
                continue;
            };
            // Only whole-packet prefixes parse.
            assert!([32, 52].contains(&len), "{len}");
            assert!(c.packets().all(|p| p.is_ok()));
        }
        let mut v1 = out.clone();
        v1[0] = 0x40;
        assert_eq!(Compound::parse(&v1), Err(RtcpError::Version(1)));
        // Bodies too short for their type.
        for bad in [
            &[0x80, PT_SR, 0, 1, 0, 0, 0, 1][..],
            &[0x81, PT_RR, 0, 1, 0, 0, 0, 1],
            &[0x81, PT_RTPFB, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2],
            &[0x81, PT_PSFB, 0, 1, 0, 0, 0, 1],
            &[0x84, PT_PSFB, 0, 2, 0, 0, 0, 1, 0, 0, 0, 0],
            &[0x84, PT_PSFB, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 5],
        ] {
            let c = Compound::parse(bad).expect("headers are fine");
            assert!(
                matches!(c.packets().next(), Some(Err(RtcpError::Body { .. }))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn every_bit_flip_is_safe() {
        let mut out = rr(&[ReportBlock::default(), ReportBlock::default()]);
        build::nack(&mut out, 1, 2, &[1, 3, 1000]).expect("nack");
        build::pli(&mut out, 1, 2);
        build::fir(&mut out, 1, &[(7, 3), (8, 4)]).expect("fir");
        for at in 0..out.len() {
            for bit in 0..8 {
                let mut m = out.clone();
                m[at] ^= 1 << bit;
                if let Ok(c) = Compound::parse(&m) {
                    for p in c.packets() {
                        match p {
                            Ok(Packet::Nack(n)) => {
                                let _ = n.lost().count();
                            }
                            Ok(Packet::Fir(f)) => {
                                let _ = f.entries().count();
                            }
                            Ok(Packet::SenderReport(s)) => {
                                let _ = s.reports().count();
                            }
                            Ok(Packet::ReceiverReport(r)) => {
                                let _ = r.reports().count();
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}
