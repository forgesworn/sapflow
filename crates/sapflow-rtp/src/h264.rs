//! H.264 over RTP (RFC 6184), packetisation mode 1 (non-interleaved):
//! single NAL unit packets, STAP-A and FU-A, assembled into access units.
//!
//! [`Depacketiser`] is fed RTP payloads in sequence order and told where
//! packets were lost ([`Depacketiser::lost`]). An access unit is the run
//! of packets with one RTP timestamp, and ends at the marker bit (RFC 6184
//! section 5.1). Each one comes out with 4-byte big-endian length
//! prefixes, its SPS and PPS taken out, and an IDR flag, which is what
//! sapflow's elementary-stream ingest takes.
//!
//! An access unit that lost anything comes out as
//! [`H264Output::Incomplete`], never as a frame with holes:
//!
//! - a gap inside it, or before its marker packet;
//! - an FU-A whose start or end is missing;
//! - a payload that was refused or broke a limit.
//!
//! A gap between one access unit's marker and the next packet cannot be
//! placed: the lost packets may have been whole access units, or the start
//! of the next one. The next access unit is kept only if its first packet
//! begins the way an access unit begins (see [`Depacketiser::push`]).
//!
//! STAP-B, MTAP16, MTAP24 and FU-B belong to the interleaved mode and are
//! refused with [`H264Error::Unsupported`].

use std::collections::VecDeque;
use std::fmt;

/// Single-time aggregation packet A (RFC 6184 section 5.7.1).
pub const STAP_A: u8 = 24;
/// Single-time aggregation packet B.
pub const STAP_B: u8 = 25;
/// Multi-time aggregation packet, 16-bit offsets.
pub const MTAP16: u8 = 26;
/// Multi-time aggregation packet, 24-bit offsets.
pub const MTAP24: u8 = 27;
/// Fragmentation unit A (RFC 6184 section 5.8).
pub const FU_A: u8 = 28;
/// Fragmentation unit B.
pub const FU_B: u8 = 29;

/// NAL unit types (ITU-T H.264 Table 7-1) the depacketiser looks at.
mod nal {
    pub const SLICE: u8 = 1;
    pub const IDR: u8 = 5;
    pub const SPS: u8 = 7;
    pub const PPS: u8 = 8;
    pub const AUD: u8 = 9;
    pub const END_OF_SEQUENCE: u8 = 10;
    pub const END_OF_STREAM: u8 = 11;
    pub const FILLER: u8 = 12;
}

/// Why a payload was refused. The access unit it belonged to is reported
/// incomplete, except for [`H264Error::Reserved`], which is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Error {
    /// An empty payload.
    Empty,
    /// STAP-B, MTAP16, MTAP24 or FU-B: interleaved-mode packets, which
    /// packetisation mode 1 does not use. Carries the type.
    Unsupported(u8),
    /// NAL unit type 0, 30 or 31, which RFC 6184 leaves undefined; the
    /// packet is ignored.
    Reserved(u8),
    /// A STAP-A whose sizes do not add up, or with an empty unit.
    Aggregation,
    /// An FU-A shorter than its two header bytes, with both the start
    /// and end bits set, or fragmenting a type that is not a NAL unit.
    Fragment,
    /// A NAL unit with the forbidden bit set, which marks bit errors
    /// (RFC 6184 section 5.3).
    ForbiddenBit,
}
impl fmt::Display for H264Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "H.264 RTP: {self:?}")
    }
}
impl std::error::Error for H264Error {}

/// Bounds on what one access unit may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264Limits {
    /// Most bytes in one access unit, length prefixes and parameter sets
    /// included. Default 8 MiB.
    pub max_frame_bytes: usize,
    /// Most bytes in one NAL unit, which bounds the FU-A reassembly
    /// buffer. Default 4 MiB, as `sapflow-hls` takes.
    pub max_nal_bytes: usize,
    /// Most NAL units in one access unit, parameter sets included.
    /// Default 256.
    pub max_nal_units: usize,
}
impl Default for H264Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 8 << 20,
            max_nal_bytes: 4 << 20,
            max_nal_units: 256,
        }
    }
}

/// One complete access unit.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessUnit {
    /// RTP timestamp (90 kHz).
    pub timestamp: u32,
    /// NAL units with 4-byte big-endian lengths, in order. SPS, PPS,
    /// access unit delimiters, end of sequence, end of stream and filler
    /// data are left out, as `sapflow-hls` expects.
    pub data: Vec<u8>,
    /// Whether it holds an IDR slice.
    pub idr: bool,
    /// Sequence parameter sets in it, with their NAL header, unprefixed.
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter sets in it, likewise.
    pub pps: Vec<Vec<u8>>,
}
impl fmt::Debug for AccessUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessUnit")
            .field("timestamp", &self.timestamp)
            .field("len", &self.data.len())
            .field("idr", &self.idr)
            .field("sps", &self.sps.len())
            .field("pps", &self.pps.len())
            .finish()
    }
}
impl AccessUnit {
    /// The NAL units in [`data`](Self::data), without their prefixes.
    pub fn nal_units(&self) -> impl Iterator<Item = &[u8]> {
        let mut rest = &self.data[..];
        std::iter::from_fn(move || {
            let (len, tail) = rest.split_first_chunk::<4>()?;
            let len = usize::try_from(u32::from_be_bytes(*len)).ok()?;
            let unit = tail.get(..len)?;
            rest = tail.get(len..)?;
            Some(unit)
        })
    }
}

/// Why an access unit was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// Packets inside it, or its marker packet, were lost.
    Gap,
    /// Packets were lost just before it, and its first packet does not
    /// begin an access unit, so its start may be among them.
    StartLost,
    /// An FU-A lost its start or end with no sequence gap.
    Fragment,
    /// A payload in it was refused.
    Refused,
    /// It broke a [`H264Limits`] bound.
    TooLarge,
    /// The stream was reset or ended before its marker.
    Unfinished,
}

/// What the depacketiser produces, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H264Output {
    /// A complete access unit.
    Frame(AccessUnit),
    /// An access unit that lost data and was dropped.
    Incomplete {
        /// Its RTP timestamp.
        timestamp: u32,
        /// What was wrong.
        reason: Incomplete,
    },
}

#[derive(Debug)]
struct Building {
    unit: AccessUnit,
    nal_units: usize,
    damaged: Option<Incomplete>,
}
impl Building {
    fn new(timestamp: u32) -> Self {
        Self {
            unit: AccessUnit {
                timestamp,
                data: Vec::new(),
                idr: false,
                sps: Vec::new(),
                pps: Vec::new(),
            },
            nal_units: 0,
            damaged: None,
        }
    }
    fn damage(&mut self, why: Incomplete) {
        if self.damaged.is_none() {
            self.damaged = Some(why);
            // Nothing in it will be used.
            self.unit.data = Vec::new();
            self.unit.sps = Vec::new();
            self.unit.pps = Vec::new();
        }
    }
    fn bytes(&self) -> usize {
        self.unit.data.len()
            + self.unit.sps.iter().map(|s| 4 + s.len()).sum::<usize>()
            + self.unit.pps.iter().map(|s| 4 + s.len()).sum::<usize>()
    }
}

/// How the first NAL unit of a payload starts, for the check after a gap.
#[derive(Debug, Clone, Copy)]
struct Start {
    /// NAL unit type.
    kind: u8,
    /// For a slice, whether `first_mb_in_slice` is 0: its ue(v) code is a
    /// single 1 bit, so the first bit after the header is set.
    first_mb_zero: bool,
}

/// Turns in-order H.264 RTP payloads into access units.
#[derive(Debug)]
pub struct Depacketiser {
    limits: H264Limits,
    au: Option<Building>,
    /// The NAL unit an FU-A is reassembling, header first.
    fu: Option<Vec<u8>>,
    /// Packets were lost since the last one.
    gap: bool,
    /// Whether the last complete IDR access unit carried an SPS. If IDRs
    /// come with their parameter sets, one that starts an access unit
    /// right after a gap may have lost them.
    idr_with_sps: bool,
    out: VecDeque<H264Output>,
    missing_markers: u64,
}

impl Depacketiser {
    /// A depacketiser with these limits.
    pub fn new(limits: H264Limits) -> Self {
        Self {
            limits,
            au: None,
            fu: None,
            gap: false,
            idr_with_sps: false,
            out: VecDeque::new(),
            missing_markers: 0,
        }
    }

    /// The next output, in order.
    pub fn poll(&mut self) -> Option<H264Output> {
        self.out.pop_front()
    }

    /// Records that packets were lost before the next one.
    pub fn lost(&mut self) {
        self.gap = true;
    }

    /// Ends the access unit in progress, as incomplete, and forgets any
    /// gap: for a sequence discontinuity or the end of the stream.
    pub fn reset(&mut self) {
        if let Some(mut au) = self.au.take() {
            au.damage(Incomplete::Unfinished);
            self.finish(au);
        }
        self.fu = None;
        self.gap = false;
    }

    /// Access units that ended at a timestamp change with no marker, no
    /// gap and no open fragment, and were delivered.
    pub fn missing_markers(&self) -> u64 {
        self.missing_markers
    }

    /// Whether an access unit is being assembled.
    pub fn in_progress(&self) -> bool {
        self.au.is_some()
    }

    /// One RTP payload, the next in sequence order.
    ///
    /// After a gap, a new access unit is kept only if its first NAL unit
    /// is an access unit delimiter, an SPS, a non-IDR slice with
    /// `first_mb_in_slice` 0, or an IDR slice with `first_mb_in_slice` 0
    /// when this stream's IDRs do not carry an SPS. Anything else (a
    /// later slice, a PPS, SEI, an FU-A continuation) may have lost what
    /// came before it, so the access unit is reported as
    /// [`Incomplete::StartLost`].
    pub fn push(&mut self, timestamp: u32, marker: bool, payload: &[u8]) -> Result<(), H264Error> {
        let gap = std::mem::take(&mut self.gap);
        // A new timestamp ends the access unit in progress.
        if let Some(mut au) = self.au.take_if(|au| au.unit.timestamp != timestamp) {
            if gap {
                au.damage(Incomplete::Gap);
            } else if self.fu.is_some() {
                au.damage(Incomplete::Fragment);
            } else if au.damaged.is_none() {
                self.missing_markers += 1;
            }
            self.fu = None;
            self.finish(au);
        }
        let fresh = self.au.is_none();
        let au = self.au.get_or_insert_with(|| Building::new(timestamp));
        if gap {
            if fresh {
                if !starts_access_unit(first_nal(payload), self.idr_with_sps) {
                    au.damage(Incomplete::StartLost);
                }
            } else {
                au.damage(Incomplete::Gap);
            }
            // Any fragment in progress lost its middle or end.
            self.fu = None;
        }
        let result = self.payload(payload);
        if let Err(e) = result
            && !matches!(e, H264Error::Reserved(_))
            && let Some(au) = self.au.as_mut()
        {
            au.damage(Incomplete::Refused);
        }
        if marker && let Some(mut au) = self.au.take() {
            if self.fu.take().is_some() {
                au.damage(Incomplete::Fragment);
            }
            self.finish(au);
        }
        result
    }

    fn finish(&mut self, au: Building) {
        let timestamp = au.unit.timestamp;
        if let Some(reason) = au.damaged {
            self.out
                .push_back(H264Output::Incomplete { timestamp, reason });
            return;
        }
        let unit = au.unit;
        if unit.data.is_empty() && unit.sps.is_empty() && unit.pps.is_empty() {
            return;
        }
        if unit.idr {
            self.idr_with_sps = !unit.sps.is_empty();
        }
        self.out.push_back(H264Output::Frame(unit));
    }

    fn payload(&mut self, payload: &[u8]) -> Result<(), H264Error> {
        let (&header, body) = payload.split_first().ok_or(H264Error::Empty)?;
        let kind = header & 0x1f;
        if kind != FU_A && self.fu.take().is_some() {
            // A fragmented unit never reached its end.
            self.damage(Incomplete::Fragment);
        }
        match kind {
            1..=23 => self.nal_unit(payload),
            STAP_A => {
                let mut rest = body;
                if rest.is_empty() {
                    return Err(H264Error::Aggregation);
                }
                while !rest.is_empty() {
                    let (size, tail) = rest
                        .split_first_chunk::<2>()
                        .ok_or(H264Error::Aggregation)?;
                    let size = usize::from(u16::from_be_bytes(*size));
                    if size == 0 {
                        return Err(H264Error::Aggregation);
                    }
                    let unit = tail.get(..size).ok_or(H264Error::Aggregation)?;
                    rest = tail.get(size..).ok_or(H264Error::Aggregation)?;
                    let nal_type = unit.first().map_or(0, |b| b & 0x1f);
                    if !(1..=23).contains(&nal_type) {
                        // Aggregates hold NAL units only.
                        return Err(H264Error::Aggregation);
                    }
                    self.nal_unit(unit)?;
                }
                Ok(())
            }
            FU_A => self.fragment(header, body),
            STAP_B | MTAP16 | MTAP24 | FU_B => {
                self.fu = None;
                Err(H264Error::Unsupported(kind))
            }
            _ => Err(H264Error::Reserved(kind)),
        }
    }

    fn fragment(&mut self, indicator: u8, body: &[u8]) -> Result<(), H264Error> {
        let Some((&fu_header, data)) = body.split_first() else {
            self.fu = None;
            return Err(H264Error::Fragment);
        };
        let start = fu_header & 0x80 != 0;
        let end = fu_header & 0x40 != 0;
        let kind = fu_header & 0x1f;
        if (start && end) || !(1..=23).contains(&kind) {
            self.fu = None;
            return Err(H264Error::Fragment);
        }
        if start {
            if self.fu.take().is_some() {
                self.damage(Incomplete::Fragment);
            }
            if indicator & 0x80 != 0 {
                return Err(H264Error::ForbiddenBit);
            }
            let mut unit = Vec::with_capacity(1 + data.len());
            unit.push(indicator & 0xe0 | kind);
            unit.extend_from_slice(data);
            self.fu = Some(unit);
        } else {
            let Some(unit) = self.fu.as_mut() else {
                // A continuation whose start is missing.
                self.damage(Incomplete::Fragment);
                return Ok(());
            };
            if unit.first().is_some_and(|h| h & 0x1f != kind) {
                self.fu = None;
                self.damage(Incomplete::Fragment);
                return Ok(());
            }
            unit.extend_from_slice(data);
        }
        if self
            .fu
            .as_ref()
            .is_some_and(|u| u.len() > self.limits.max_nal_bytes)
        {
            self.fu = None;
            self.damage(Incomplete::TooLarge);
            return Ok(());
        }
        if end && let Some(unit) = self.fu.take() {
            return self.nal_unit(&unit);
        }
        Ok(())
    }

    fn damage(&mut self, why: Incomplete) {
        if let Some(au) = self.au.as_mut() {
            au.damage(why);
        }
    }

    fn nal_unit(&mut self, unit: &[u8]) -> Result<(), H264Error> {
        let Some(au) = self.au.as_mut() else {
            return Ok(());
        };
        let header = unit.first().copied().unwrap_or(0);
        if header & 0x80 != 0 {
            return Err(H264Error::ForbiddenBit);
        }
        if au.damaged.is_some() {
            return Ok(());
        }
        let limits = self.limits;
        if au.nal_units >= limits.max_nal_units
            || unit.len() > limits.max_nal_bytes
            || au.bytes() + 4 + unit.len() > limits.max_frame_bytes
        {
            au.damage(Incomplete::TooLarge);
            return Ok(());
        }
        let Ok(len) = u32::try_from(unit.len()) else {
            au.damage(Incomplete::TooLarge);
            return Ok(());
        };
        au.nal_units += 1;
        match header & 0x1f {
            nal::SPS => au.unit.sps.push(unit.to_vec()),
            nal::PPS => au.unit.pps.push(unit.to_vec()),
            nal::AUD | nal::END_OF_SEQUENCE | nal::END_OF_STREAM | nal::FILLER => {}
            kind => {
                if kind == nal::IDR {
                    au.unit.idr = true;
                }
                au.unit.data.extend_from_slice(&len.to_be_bytes());
                au.unit.data.extend_from_slice(unit);
            }
        }
        Ok(())
    }
}

/// How the first NAL unit in a payload starts, if the payload begins one.
fn first_nal(payload: &[u8]) -> Option<Start> {
    let header = *payload.first()?;
    let (kind, next) = match header & 0x1f {
        1..=23 => (header & 0x1f, payload.get(1)),
        STAP_A => {
            let unit = payload.get(3..)?;
            (unit.first()? & 0x1f, unit.get(1))
        }
        FU_A => {
            let fu = *payload.get(1)?;
            if fu & 0x80 == 0 {
                return None;
            }
            (fu & 0x1f, payload.get(2))
        }
        _ => return None,
    };
    Some(Start {
        kind,
        first_mb_zero: next.is_some_and(|b| b & 0x80 != 0),
    })
}

fn starts_access_unit(start: Option<Start>, idr_with_sps: bool) -> bool {
    let Some(s) = start else {
        return false;
    };
    match s.kind {
        nal::AUD | nal::SPS => true,
        nal::SLICE => s.first_mb_zero,
        nal::IDR => s.first_mb_zero && !idr_with_sps,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1e, 0xda];
    const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];

    fn stap(units: &[&[u8]]) -> Vec<u8> {
        let mut p = vec![0x78];
        for u in units {
            p.extend_from_slice(&(u.len() as u16).to_be_bytes());
            p.extend_from_slice(u);
        }
        p
    }

    /// FU-A packets for one NAL unit, `size` payload bytes each.
    fn fus(unit: &[u8], size: usize) -> Vec<Vec<u8>> {
        let (header, body) = unit.split_first().expect("header");
        let chunks: Vec<_> = body.chunks(size).collect();
        let last = chunks.len() - 1;
        chunks
            .iter()
            .enumerate()
            .map(|(n, c)| {
                let mut p = vec![header & 0xe0 | FU_A, header & 0x1f];
                if n == 0 {
                    p[1] |= 0x80;
                }
                if n == last {
                    p[1] |= 0x40;
                }
                p.extend_from_slice(c);
                p
            })
            .collect()
    }

    fn idr(len: usize) -> Vec<u8> {
        let mut u = vec![0x65, 0x88];
        u.extend((0..len).map(|n| n as u8 | 1));
        u
    }

    fn slice(first_mb_zero: bool, len: usize) -> Vec<u8> {
        let mut u = vec![0x41, if first_mb_zero { 0x9a } else { 0x22 }];
        u.extend((0..len).map(|n| (n as u8) ^ 0x5a | 1));
        u
    }

    fn prefixed(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&(u.len() as u32).to_be_bytes());
            out.extend_from_slice(u);
        }
        out
    }

    fn drain(d: &mut Depacketiser) -> Vec<H264Output> {
        std::iter::from_fn(|| d.poll()).collect()
    }

    fn frame(o: &H264Output) -> &AccessUnit {
        match o {
            H264Output::Frame(au) => au,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn single_stap_and_fu() {
        let mut d = Depacketiser::new(H264Limits::default());
        let unit = idr(2999);
        // STAP-A with SPS, PPS and an AUD (left out), then the IDR in FU-As.
        d.push(9000, false, &stap(&[&[0x09, 0xf0], SPS, PPS]))
            .expect("stap");
        let frags = fus(&unit, 1000);
        assert_eq!(frags.len(), 3);
        for (n, f) in frags.iter().enumerate() {
            d.push(9000, n == 2, f).expect("fu");
        }
        // A P frame of two slices as single NAL unit packets.
        let (a, b) = (slice(true, 40), slice(false, 40));
        d.push(12000, false, &a).expect("slice");
        d.push(12000, true, &b).expect("slice");
        let out = drain(&mut d);
        assert_eq!(out.len(), 2);
        let au = frame(&out[0]);
        assert_eq!(au.timestamp, 9000);
        assert!(au.idr);
        assert_eq!(
            (au.sps.as_slice(), au.pps.as_slice()),
            (&[SPS.to_vec()][..], &[PPS.to_vec()][..])
        );
        assert_eq!(au.data, prefixed(&[&unit]));
        assert_eq!(au.nal_units().collect::<Vec<_>>(), [&unit[..]]);
        let au = frame(&out[1]);
        assert!(!au.idr && au.sps.is_empty());
        assert_eq!(au.data, prefixed(&[&a, &b]));
    }

    #[test]
    fn stap_a_built_from_the_rfc() {
        // RFC 6184 section 5.7.1, Figure 7: a STAP-A with two units.
        let mut d = Depacketiser::new(H264Limits::default());
        let s = slice(true, 5);
        d.push(1, true, &stap(&[SPS, PPS, &s])).expect("stap");
        let au = frame(&drain(&mut d)[0]).clone();
        assert_eq!(au.data, prefixed(&[&s]));
        // Sizes that run past the end, a zero size, a lone size byte, and
        // an aggregate inside an aggregate are refused, and the access unit
        // with them.
        let mut bad = stap(&[SPS, &s]);
        bad.truncate(bad.len() - 1);
        for p in [
            bad,
            vec![0x78, 0, 0],
            [stap(&[SPS]), vec![0]].concat(),
            vec![0x78],
            stap(&[&[0x78, 0, 1, 0x41]]),
        ] {
            let err = d.push(7, true, &p).expect_err("refused");
            assert_eq!(err, H264Error::Aggregation);
            assert_eq!(
                drain(&mut d),
                [H264Output::Incomplete {
                    timestamp: 7,
                    reason: Incomplete::Refused
                }]
            );
        }
    }

    #[test]
    fn interleaved_types_are_refused() {
        let mut d = Depacketiser::new(H264Limits::default());
        for kind in [STAP_B, MTAP16, MTAP24, FU_B] {
            let err = d
                .push(3, true, &[0x60 | kind, 0, 0, 0, 0])
                .expect_err("refused");
            assert_eq!(err, H264Error::Unsupported(kind));
            assert!(matches!(
                drain(&mut d)[..],
                [H264Output::Incomplete {
                    reason: Incomplete::Refused,
                    ..
                }]
            ));
        }
        // Reserved types are ignored, without spoiling the access unit.
        let s = slice(true, 4);
        d.push(4, false, &s).expect("slice");
        assert_eq!(d.push(4, false, &[0x1e, 1]), Err(H264Error::Reserved(30)));
        assert_eq!(d.push(4, false, &[0x00]), Err(H264Error::Reserved(0)));
        d.push(4, true, &slice(false, 4)).expect("slice");
        assert!(matches!(drain(&mut d)[..], [H264Output::Frame(_)]));
        assert_eq!(d.push(5, true, &[]), Err(H264Error::Empty));
    }

    #[test]
    fn lost_fragments_make_incomplete_frames() {
        let unit = idr(4000);
        let frags = fus(&unit, 1000);
        let n = frags.len();
        // Lose each fragment in turn, with the loss signalled.
        for lose in 0..n {
            let mut d = Depacketiser::new(H264Limits::default());
            d.push(100, false, &stap(&[SPS, PPS])).expect("stap");
            for (i, f) in frags.iter().enumerate() {
                if i == lose {
                    d.lost();
                    continue;
                }
                d.push(100, i == n - 1, f).expect("fu");
            }
            if lose == n - 1 {
                // The marker was lost: the next timestamp ends it.
                d.push(200, true, &slice(true, 9)).expect("next");
            }
            let out = drain(&mut d);
            assert_eq!(
                out[0],
                H264Output::Incomplete {
                    timestamp: 100,
                    reason: Incomplete::Gap
                },
                "lost fragment {lose}"
            );
        }
        // Without a gap: a continuation with no start, a start with no
        // end before the marker, and a single NAL unit inside a fragment.
        let mut d = Depacketiser::new(H264Limits::default());
        d.push(1, true, &frags[1]).expect("continuation");
        d.push(2, true, &frags[0]).expect("start");
        d.push(3, false, &frags[0]).expect("start");
        d.push(3, true, &slice(false, 3)).expect("slice");
        d.push(4, false, &frags[0]).expect("start");
        d.push(4, false, &frags[0]).expect("start again");
        d.push(4, true, &frags[n - 1]).expect("end");
        let reasons: Vec<_> = drain(&mut d)
            .into_iter()
            .map(|o| match o {
                H264Output::Incomplete { reason, .. } => reason,
                H264Output::Frame(_) => panic!("no frame"),
            })
            .collect();
        assert_eq!(reasons, [Incomplete::Fragment; 4]);
        // Malformed FU-A headers.
        let mut d = Depacketiser::new(H264Limits::default());
        assert_eq!(d.push(1, true, &[0x7c]), Err(H264Error::Fragment));
        assert_eq!(d.push(2, true, &[0x7c, 0xc5, 1]), Err(H264Error::Fragment));
        assert_eq!(d.push(3, true, &[0x7c, 0x98, 1]), Err(H264Error::Fragment));
        assert_eq!(
            d.push(4, true, &[0xfc, 0x85, 1]),
            Err(H264Error::ForbiddenBit)
        );
        assert_eq!(d.push(5, true, &[0xc1, 1]), Err(H264Error::ForbiddenBit));
        assert_eq!(drain(&mut d).len(), 5);
    }

    #[test]
    fn timestamp_change_without_marker() {
        // No gap and nothing open: the access unit is whole, delivered,
        // and counted.
        let mut d = Depacketiser::new(H264Limits::default());
        let a = slice(true, 10);
        d.push(10, false, &a).expect("a");
        d.push(20, true, &a).expect("b");
        let out = drain(&mut d);
        assert_eq!(frame(&out[0]).timestamp, 10);
        assert_eq!(frame(&out[1]).timestamp, 20);
        assert_eq!(d.missing_markers(), 1);
        // With an FU-A left open, it is incomplete.
        let frags = fus(&idr(100), 40);
        d.push(30, false, &frags[0]).expect("start");
        d.push(40, true, &a).expect("next");
        let out = drain(&mut d);
        assert_eq!(
            out[0],
            H264Output::Incomplete {
                timestamp: 30,
                reason: Incomplete::Fragment
            }
        );
        assert_eq!(frame(&out[1]).timestamp, 40);
    }

    #[test]
    fn the_start_after_a_gap() {
        // IDRs here carry their SPS, so an IDR slice right after a gap may
        // have lost it.
        let mut d = Depacketiser::new(H264Limits::default());
        d.push(0, false, &stap(&[SPS, PPS])).expect("stap");
        d.push(0, true, &idr(10)).expect("idr");
        // Each case is an access unit's packets, the last with the marker.
        let mut fu_start = fus(&slice(true, 50), 20);
        fu_start.push(slice(false, 5));
        let clean: Vec<Vec<Vec<u8>>> = vec![
            vec![stap(&[SPS, PPS]), idr(5)],
            vec![vec![0x09, 0xf0], slice(true, 5)],
            vec![slice(true, 5), slice(false, 5)],
            fu_start,
        ];
        for (n, packets) in clean.iter().enumerate() {
            d.lost();
            for (i, p) in packets.iter().enumerate() {
                d.push(100 + n as u32, i == packets.len() - 1, p)
                    .expect("clean");
            }
        }
        let dirty: [Vec<u8>; 6] = [
            slice(false, 5),
            idr(5),
            vec![0x68, 0xce],
            vec![0x06, 0x05, 0x01],
            fus(&slice(true, 50), 20)[1].clone(),
            stap(&[PPS, &slice(true, 3)]),
        ];
        for (n, p) in dirty.iter().enumerate() {
            d.lost();
            d.push(200 + n as u32, false, p).expect("start");
            d.push(200 + n as u32, true, &slice(false, 5))
                .expect("rest");
        }
        let out = drain(&mut d);
        assert_eq!(out.len(), 1 + clean.len() + dirty.len());
        for o in &out[..1 + clean.len()] {
            assert!(matches!(o, H264Output::Frame(_)), "{o:?}");
        }
        for o in &out[1 + clean.len()..] {
            assert!(
                matches!(
                    o,
                    H264Output::Incomplete {
                        reason: Incomplete::StartLost,
                        ..
                    }
                ),
                "{o:?}"
            );
        }
        // With IDRs that come without an SPS, an IDR start is clean.
        let mut d = Depacketiser::new(H264Limits::default());
        d.push(0, true, &idr(10)).expect("idr");
        d.lost();
        d.push(1, true, &idr(10)).expect("idr");
        assert!(matches!(
            drain(&mut d)[..],
            [H264Output::Frame(_), H264Output::Frame(_)]
        ));
    }

    #[test]
    fn limits() {
        let limits = H264Limits {
            max_frame_bytes: 300,
            max_nal_bytes: 200,
            max_nal_units: 3,
        };
        let mut d = Depacketiser::new(limits);
        // A NAL unit over the limit, whole and reassembled.
        d.push(1, true, &slice(true, 250)).expect("big");
        for (n, f) in fus(&slice(true, 250), 60).iter().enumerate() {
            d.push(2, n == 4, f).expect("fu");
        }
        // Too many units, and too many bytes.
        for _ in 0..3 {
            d.push(3, false, &slice(true, 1)).expect("small");
        }
        d.push(3, true, &slice(true, 1)).expect("fourth");
        d.push(4, false, &slice(true, 150)).expect("one");
        d.push(4, true, &slice(true, 150)).expect("two");
        let out = drain(&mut d);
        assert_eq!(out.len(), 4);
        for o in out {
            assert!(matches!(
                o,
                H264Output::Incomplete {
                    reason: Incomplete::TooLarge,
                    ..
                }
            ));
        }
        // At the limits exactly.
        d.push(5, false, &slice(true, 192)).expect("one");
        d.push(5, true, &slice(true, 92)).expect("two");
        assert!(matches!(drain(&mut d)[..], [H264Output::Frame(_)]));
    }

    #[test]
    fn reset_ends_the_frame() {
        let mut d = Depacketiser::new(H264Limits::default());
        d.push(1, false, &slice(true, 3)).expect("a");
        assert!(d.in_progress());
        d.reset();
        assert_eq!(
            drain(&mut d),
            [H264Output::Incomplete {
                timestamp: 1,
                reason: Incomplete::Unfinished
            }]
        );
        assert!(!d.in_progress());
    }
}
