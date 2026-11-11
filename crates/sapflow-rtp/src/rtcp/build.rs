//! The RTCP that sapflow sends as a receiver: receiver reports, SDES
//! CNAME, generic NACK, PLI and FIR, and a compound packet that starts with
//! the receiver report (RFC 3550 section 6.1).
//!
//! Every function appends to a caller's buffer; nothing else is allocated.

use super::{FMT_FIR, FMT_NACK, FMT_PLI, PT_PSFB, PT_RR, PT_RTPFB, PT_SDES, ReportBlock};
use std::fmt;

/// Most report blocks one RR can carry (the five-bit count).
pub const MAX_BLOCKS_PER_RR: usize = 31;
/// Most NACK pairs or FIR entries in one packet, so that the 16-bit length
/// field cannot overflow.
pub const MAX_FCI: usize = 8192;

/// Why a packet could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// Nothing to report: no lost packets, no FIR entries or an empty CNAME.
    Empty,
    /// More entries than [`MAX_FCI`], or a CNAME over 255 bytes.
    TooLarge,
}
impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RTCP build: {self:?}")
    }
}
impl std::error::Error for BuildError {}

fn header(out: &mut Vec<u8>, count: u8, packet_type: u8, body_len: usize) {
    out.push(0x80 | (count & 0x1f));
    out.push(packet_type);
    // Callers keep bodies under 2^18 bytes and a multiple of four.
    out.extend_from_slice(&((body_len / 4) as u16).to_be_bytes());
}

fn report_block(out: &mut Vec<u8>, b: &ReportBlock) {
    let lost = b.cumulative_lost.clamp(-0x80_0000, 0x7f_ffff) as u32 & 0x00ff_ffff;
    out.extend_from_slice(&b.ssrc.to_be_bytes());
    out.extend_from_slice(&(u32::from(b.fraction_lost) << 24 | lost).to_be_bytes());
    out.extend_from_slice(&b.highest_sequence.to_be_bytes());
    out.extend_from_slice(&b.jitter.to_be_bytes());
    out.extend_from_slice(&b.last_sr.to_be_bytes());
    out.extend_from_slice(&b.delay_since_last_sr.to_be_bytes());
}

/// Appends receiver reports from `sender_ssrc`: one RR with no blocks when
/// `blocks` is empty, otherwise as many RRs of up to 31 blocks as needed
/// (RFC 3550 section 6.4.2). Cumulative loss is clamped to 24 bits.
pub fn receiver_report(out: &mut Vec<u8>, sender_ssrc: u32, blocks: &[ReportBlock]) {
    let mut chunks = blocks.chunks(MAX_BLOCKS_PER_RR).peekable();
    if chunks.peek().is_none() {
        header(out, 0, PT_RR, 4);
        out.extend_from_slice(&sender_ssrc.to_be_bytes());
        return;
    }
    for chunk in chunks {
        header(out, chunk.len() as u8, PT_RR, 4 + 24 * chunk.len());
        out.extend_from_slice(&sender_ssrc.to_be_bytes());
        for b in chunk {
            report_block(out, b);
        }
    }
}

/// Appends an SDES packet with one chunk holding a CNAME item (RFC 3550
/// section 6.5.1). `cname` is 1 to 255 bytes.
pub fn sdes_cname(out: &mut Vec<u8>, ssrc: u32, cname: &[u8]) -> Result<(), BuildError> {
    if cname.is_empty() {
        return Err(BuildError::Empty);
    }
    let Ok(len) = u8::try_from(cname.len()) else {
        return Err(BuildError::TooLarge);
    };
    // SSRC, item type and length, text, then at least one null octet,
    // padded to a word.
    let chunk = (4 + 2 + cname.len() + 1).div_ceil(4) * 4;
    header(out, 1, PT_SDES, chunk);
    let start = out.len();
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.push(1);
    out.push(len);
    out.extend_from_slice(cname);
    out.resize(start + chunk, 0);
    Ok(())
}

/// Coalesces lost sequence numbers into `(PID, BLP)` pairs (RFC 4585
/// section 6.2.1). Give them in ascending order, wrapping from 65535 to 0,
/// as a loss list holds them; any order is covered, but sorted input packs
/// up to 17 losses into each pair. Repeats are dropped.
pub fn nack_pairs(lost: &[u16]) -> impl Iterator<Item = (u16, u16)> + '_ {
    let mut lost = lost.iter().copied().peekable();
    std::iter::from_fn(move || {
        let pid = lost.next()?;
        let mut blp = 0u16;
        while let Some(&next) = lost.peek() {
            let gap = next.wrapping_sub(pid);
            match gap {
                0 => {}
                1..=16 => blp |= 1 << (gap - 1),
                _ => break,
            }
            lost.next();
        }
        Some((pid, blp))
    })
}

/// Appends a generic NACK from `sender_ssrc` about `media_ssrc` for the
/// lost sequence numbers (see [`nack_pairs`]).
pub fn nack(
    out: &mut Vec<u8>,
    sender_ssrc: u32,
    media_ssrc: u32,
    lost: &[u16],
) -> Result<(), BuildError> {
    if lost.is_empty() {
        return Err(BuildError::Empty);
    }
    let pairs = nack_pairs(lost).count();
    if pairs > MAX_FCI {
        return Err(BuildError::TooLarge);
    }
    header(out, FMT_NACK, PT_RTPFB, 8 + 4 * pairs);
    out.extend_from_slice(&sender_ssrc.to_be_bytes());
    out.extend_from_slice(&media_ssrc.to_be_bytes());
    for (pid, blp) in nack_pairs(lost) {
        out.extend_from_slice(&pid.to_be_bytes());
        out.extend_from_slice(&blp.to_be_bytes());
    }
    Ok(())
}

/// Appends a picture loss indication (RFC 4585 section 6.3.1).
pub fn pli(out: &mut Vec<u8>, sender_ssrc: u32, media_ssrc: u32) {
    header(out, FMT_PLI, PT_PSFB, 8);
    out.extend_from_slice(&sender_ssrc.to_be_bytes());
    out.extend_from_slice(&media_ssrc.to_be_bytes());
}

/// Appends a full intra request (RFC 5104 section 4.3.1) with one entry
/// per `(media SSRC, command sequence number)`. The media source field is
/// 0, as RFC 5104 requires; the sender increments the sequence number for
/// each new request.
pub fn fir(out: &mut Vec<u8>, sender_ssrc: u32, entries: &[(u32, u8)]) -> Result<(), BuildError> {
    if entries.is_empty() {
        return Err(BuildError::Empty);
    }
    if entries.len() > MAX_FCI {
        return Err(BuildError::TooLarge);
    }
    header(out, FMT_FIR, PT_PSFB, 8 + 8 * entries.len());
    out.extend_from_slice(&sender_ssrc.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    for (ssrc, seq) in entries {
        out.extend_from_slice(&ssrc.to_be_bytes());
        out.extend_from_slice(&[*seq, 0, 0, 0]);
    }
    Ok(())
}

/// Builds a compound packet in `out`, starting with the receiver reports.
#[derive(Debug)]
pub struct CompoundBuilder<'v> {
    out: &'v mut Vec<u8>,
    sender_ssrc: u32,
}
impl<'v> CompoundBuilder<'v> {
    /// Appends the receiver report(s) first.
    pub fn new(out: &'v mut Vec<u8>, sender_ssrc: u32, reports: &[ReportBlock]) -> Self {
        receiver_report(out, sender_ssrc, reports);
        Self { out, sender_ssrc }
    }
    /// Adds an SDES CNAME for the sender.
    pub fn cname(&mut self, cname: &[u8]) -> Result<&mut Self, BuildError> {
        sdes_cname(self.out, self.sender_ssrc, cname)?;
        Ok(self)
    }
    /// Adds a generic NACK about `media_ssrc`.
    pub fn nack(&mut self, media_ssrc: u32, lost: &[u16]) -> Result<&mut Self, BuildError> {
        nack(self.out, self.sender_ssrc, media_ssrc, lost)?;
        Ok(self)
    }
    /// Adds a PLI about `media_ssrc`.
    pub fn pli(&mut self, media_ssrc: u32) -> &mut Self {
        pli(self.out, self.sender_ssrc, media_ssrc);
        self
    }
    /// Adds a FIR.
    pub fn fir(&mut self, entries: &[(u32, u8)]) -> Result<&mut Self, BuildError> {
        fir(self.out, self.sender_ssrc, entries)?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Compound, Packet};
    use super::*;

    #[test]
    fn nack_coalescing() {
        let pairs: Vec<_> = nack_pairs(&[10, 11, 13, 26, 27, 28, 100]).collect();
        // 11 is bit 0, 13 bit 2, 26 bit 15; 27 starts a new pair.
        assert_eq!(pairs, [(10, 0b1000_0000_0000_0101), (27, 0b1), (100, 0)]);
        // Across the wrap from 65535 to 0.
        let pairs: Vec<_> = nack_pairs(&[65534, 65535, 0, 3]).collect();
        assert_eq!(pairs, [(65534, 0b10011)]);
        // Repeats and out-of-order input are still all covered.
        let lost = [5, 5, 3, 4, 40, 6];
        let mut covered: Vec<u16> = Vec::new();
        for (pid, blp) in nack_pairs(&lost) {
            covered.push(pid);
            covered.extend((0..16).filter(|b| blp & (1 << b) != 0).map(|b| pid + b + 1));
        }
        covered.sort_unstable();
        covered.dedup();
        assert_eq!(covered, [3, 4, 5, 6, 40]);
    }

    #[test]
    fn nack_roundtrip() {
        let lost: Vec<u16> = (0..200u16)
            .map(|n| n.wrapping_mul(7).wrapping_add(65500))
            .collect();
        let mut out = Vec::new();
        nack(&mut out, 1, 2, &lost).expect("built");
        let c = Compound::parse(&out).expect("parses");
        let Some(Ok(Packet::Nack(n))) = c.packets().next() else {
            panic!("a NACK");
        };
        assert_eq!((n.sender_ssrc(), n.media_ssrc()), (1, 2));
        assert_eq!(n.lost().collect::<Vec<_>>(), lost);
    }

    #[test]
    fn compound_starts_with_rr() {
        let blocks: Vec<ReportBlock> = (0..33)
            .map(|n| ReportBlock {
                ssrc: n,
                cumulative_lost: if n == 0 { i32::MIN } else { i32::MAX },
                ..ReportBlock::default()
            })
            .collect();
        let mut out = Vec::new();
        CompoundBuilder::new(&mut out, 7, &blocks)
            .cname(b"sapflow")
            .expect("cname")
            .nack(9, &[1, 2])
            .expect("nack")
            .pli(9)
            .fir(&[(9, 3)])
            .expect("fir");
        let c = Compound::parse(&out).expect("parses");
        assert!(c.starts_with_report());
        let packets: Vec<_> = c.packets().map(|p| p.expect("ok")).collect();
        assert_eq!(packets.len(), 6);
        let Packet::ReceiverReport(first) = packets[0] else {
            panic!("RR first");
        };
        let Packet::ReceiverReport(second) = packets[1] else {
            panic!("second RR");
        };
        let all: Vec<_> = first.reports().chain(second.reports()).collect();
        assert_eq!(all.len(), 33);
        assert_eq!(all[0].cumulative_lost, -0x80_0000);
        assert_eq!(all[1].cumulative_lost, 0x7f_ffff);
        let Packet::Other(sdes) = packets[2] else {
            panic!("SDES");
        };
        assert_eq!(sdes.packet_type, PT_SDES);
        assert_eq!(&sdes.body[..13], b"\0\0\0\x07\x01\x07sapflow");
        assert!(sdes.body.len().is_multiple_of(4) && sdes.body[13] == 0);
        assert!(matches!(packets[3], Packet::Nack(_)));
        assert_eq!(
            packets[4],
            Packet::Pli(super::super::Pli {
                sender_ssrc: 7,
                media_ssrc: 9
            })
        );
        let Packet::Fir(f) = packets[5] else {
            panic!("FIR");
        };
        assert_eq!(f.media_ssrc(), 0);
        assert_eq!(f.entries().collect::<Vec<_>>(), [(9, 3)]);
    }

    #[test]
    fn empty_rr_and_refusals() {
        let mut out = Vec::new();
        receiver_report(&mut out, 5, &[]);
        assert_eq!(out, [0x80, PT_RR, 0, 1, 0, 0, 0, 5]);
        assert_eq!(nack(&mut out, 1, 2, &[]), Err(BuildError::Empty));
        assert_eq!(fir(&mut out, 1, &[]), Err(BuildError::Empty));
        assert_eq!(sdes_cname(&mut out, 1, b""), Err(BuildError::Empty));
        assert_eq!(
            sdes_cname(&mut out, 1, &[b'a'; 256]),
            Err(BuildError::TooLarge)
        );
        // Sorted input cannot need this many pairs; alternating input can.
        let spread: Vec<u16> = (0..=MAX_FCI)
            .map(|n| if n % 2 == 0 { 0 } else { 100 })
            .collect();
        assert_eq!(nack(&mut out, 1, 2, &spread), Err(BuildError::TooLarge));
        assert_eq!(out.len(), 8, "refusals append nothing");
    }
}
