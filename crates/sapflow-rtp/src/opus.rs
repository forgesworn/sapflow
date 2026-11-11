//! Opus over RTP (RFC 7587): the TOC byte and packet framing (RFC 6716
//! sections 3.1 to 3.4), constant encoded-silence packets, and a
//! depacketiser that keeps the audio track continuous.
//!
//! Each RTP payload is one Opus packet (RFC 7587 section 4.2), timestamped
//! at 48 kHz. [`parse_packet`] checks a packet against the framing rules
//! of RFC 6716 section 3.4 (R1 to R7) and gives its frame count and total
//! duration; nothing in the packet is trusted before that.
//!
//! [`Depacketiser`] is fed packets in sequence order and told where
//! packets were lost. A timestamp gap with no loss before it is the
//! sender's silence suppression (DTX); a gap after a loss is loss. Both
//! are filled with [`silence`] packets of the stream's channel layout, so
//! the output timeline has no holes, and the two are marked apart
//! ([`FrameKind`]).

use std::collections::VecDeque;
use std::fmt;

/// Samples per second of the Opus RTP clock (RFC 7587 section 4.1).
pub const CLOCK_RATE: u32 = 48_000;
/// Longest Opus packet duration, 120 ms at 48 kHz (RFC 6716 section 3.2.5).
pub const MAX_PACKET_SAMPLES: u32 = 5_760;
/// Largest frame (RFC 6716 section 3.4, R2).
pub const MAX_FRAME_BYTES: usize = 1_275;
/// Most frames in one packet: 120 ms of 2.5 ms frames.
pub const MAX_FRAMES: u8 = 48;
/// The smallest unit silence can fill: one 2.5 ms CELT frame.
pub const FILL_GRANULE: u32 = 120;

/// The body of one CELT frame whose silence flag is set (RFC 6716 section
/// 4.3, the first symbol of a CELT frame). This is the frame libopus
/// produces for digital silence, observed by running FFmpeg's libopus
/// encoder as a black box on `anullsrc`, in every CELT frame size; the
/// fixture README records the decode check.
pub const SILENT_CELT_FRAME: [u8; 2] = [0xff, 0xfe];

/// Why an Opus packet was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusError {
    /// No bytes at all (R1).
    Empty,
    /// A frame longer than 1275 bytes (R2).
    FrameTooLong,
    /// Code 1 with an even total length, so the two frames differ (R3).
    UnequalFrames,
    /// A frame length runs past the end of the packet (R4, R7).
    Truncated,
    /// Code 3 with no frame-count byte, or a count of zero (R5).
    FrameCount,
    /// More than 120 ms of audio in one packet (R5).
    TooLong,
    /// Code 3 CBR whose bytes do not divide into equal frames (R6).
    Uneven,
    /// The padding runs past the end of the packet (R6, R7).
    Padding,
}
impl fmt::Display for OpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Opus: {self:?}")
    }
}
impl std::error::Error for OpusError {}

/// The coding mode a configuration uses (RFC 6716 section 3.1, Table 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// SILK only (configurations 0 to 11).
    Silk,
    /// SILK and CELT (12 to 15).
    Hybrid,
    /// CELT only (16 to 31).
    Celt,
}

/// The audio bandwidth a configuration codes (RFC 6716 Table 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// 4 kHz.
    Narrowband,
    /// 6 kHz.
    Mediumband,
    /// 8 kHz.
    Wideband,
    /// 12 kHz.
    SuperWideband,
    /// 20 kHz.
    Fullband,
}

/// The table-of-contents byte (RFC 6716 section 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toc(pub u8);
impl Toc {
    /// Configuration number, 0 to 31.
    pub fn config(self) -> u8 {
        self.0 >> 3
    }
    /// The stereo flag.
    pub fn stereo(self) -> bool {
        self.0 & 0x04 != 0
    }
    /// Frame-count code, 0 to 3 (RFC 6716 section 3.2).
    pub fn code(self) -> u8 {
        self.0 & 0x03
    }
    /// The coding mode.
    pub fn mode(self) -> Mode {
        match self.config() {
            0..=11 => Mode::Silk,
            12..=15 => Mode::Hybrid,
            _ => Mode::Celt,
        }
    }
    /// The audio bandwidth.
    pub fn bandwidth(self) -> Bandwidth {
        match self.config() {
            0..=3 => Bandwidth::Narrowband,
            4..=7 => Bandwidth::Mediumband,
            8..=11 => Bandwidth::Wideband,
            12..=13 => Bandwidth::SuperWideband,
            14..=15 => Bandwidth::Fullband,
            16..=19 => Bandwidth::Narrowband,
            20..=23 => Bandwidth::Wideband,
            24..=27 => Bandwidth::SuperWideband,
            _ => Bandwidth::Fullband,
        }
    }
    /// Duration of each frame in 48 kHz samples: 10, 20, 40 or 60 ms for
    /// SILK, 10 or 20 ms for hybrid, 2.5, 5, 10 or 20 ms for CELT.
    pub fn frame_samples(self) -> u32 {
        let c = self.config();
        match (self.mode(), c % 4) {
            (Mode::Silk, 0) | (Mode::Celt, 2) => 480,
            (Mode::Silk, 1) | (Mode::Celt, 3) => 960,
            (Mode::Silk, 2) => 1920,
            (Mode::Silk, _) => 2880,
            (Mode::Hybrid, n) => {
                if n % 2 == 0 {
                    480
                } else {
                    960
                }
            }
            (Mode::Celt, 0) => 120,
            (Mode::Celt, _) => 240,
        }
    }
}

/// What [`parse_packet`] found in a valid packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketInfo {
    /// The TOC byte.
    pub toc: Toc,
    /// Number of frames, 1 to 48.
    pub frames: u8,
    /// Whether code 3 signalled variable-size frames.
    pub vbr: bool,
    /// Padding octets (code 3 only), counting the padding length bytes.
    pub padding: usize,
    /// Bytes of frame data, without the TOC, counts, lengths and padding.
    pub frame_bytes: usize,
}
impl PacketInfo {
    /// Total duration in 48 kHz samples, at most 5760 (120 ms).
    pub fn duration(&self) -> u32 {
        u32::from(self.frames) * self.toc.frame_samples()
    }
}

/// Reads one frame length (RFC 6716 section 3.2.1): one byte up to 251,
/// otherwise two. Returns the length and the bytes it took.
fn frame_length(buf: &[u8]) -> Result<(usize, usize), OpusError> {
    let first = usize::from(*buf.first().ok_or(OpusError::Truncated)?);
    if first < 252 {
        return Ok((first, 1));
    }
    let second = usize::from(*buf.get(1).ok_or(OpusError::Truncated)?);
    Ok((4 * second + first, 2))
}

/// Checks one Opus packet against RFC 6716 sections 3.2 and 3.4.
pub fn parse_packet(packet: &[u8]) -> Result<PacketInfo, OpusError> {
    let (&toc, rest) = packet.split_first().ok_or(OpusError::Empty)?;
    let toc = Toc(toc);
    let info = |frames: u8, vbr, padding, frame_bytes| PacketInfo {
        toc,
        frames,
        vbr,
        padding,
        frame_bytes,
    };
    match toc.code() {
        // One frame: the rest of the packet.
        0 => {
            if rest.len() > MAX_FRAME_BYTES {
                return Err(OpusError::FrameTooLong);
            }
            Ok(info(1, false, 0, rest.len()))
        }
        // Two frames of equal size.
        1 => {
            if rest.len() % 2 != 0 {
                return Err(OpusError::UnequalFrames);
            }
            if rest.len() / 2 > MAX_FRAME_BYTES {
                return Err(OpusError::FrameTooLong);
            }
            Ok(info(2, false, 0, rest.len()))
        }
        // Two frames, the first one's length coded.
        2 => {
            let (first, used) = frame_length(rest)?;
            let remaining = rest.len() - used;
            if first > remaining {
                return Err(OpusError::Truncated);
            }
            if first > MAX_FRAME_BYTES || remaining - first > MAX_FRAME_BYTES {
                return Err(OpusError::FrameTooLong);
            }
            Ok(info(2, true, 0, remaining))
        }
        // An arbitrary number of frames (section 3.2.5).
        _ => {
            let (&count, mut rest) = rest.split_first().ok_or(OpusError::FrameCount)?;
            let vbr = count & 0x80 != 0;
            let padded = count & 0x40 != 0;
            let frames = count & 0x3f;
            if frames == 0 {
                return Err(OpusError::FrameCount);
            }
            if u32::from(frames) * toc.frame_samples() > MAX_PACKET_SAMPLES {
                return Err(OpusError::TooLong);
            }
            // Padding (section 3.2.5): length bytes, each 255 meaning 254
            // octets and another length byte, the last 0 to 254; the
            // octets themselves end the packet.
            let (mut octets, mut length_bytes) = (0usize, 0usize);
            if padded {
                loop {
                    let (&p, tail) = rest.split_first().ok_or(OpusError::Padding)?;
                    rest = tail;
                    length_bytes += 1;
                    if p == 255 {
                        octets += 254;
                    } else {
                        octets += usize::from(p);
                        break;
                    }
                }
            }
            if octets > rest.len() {
                return Err(OpusError::Padding);
            }
            let padding = octets + length_bytes;
            let body = rest.get(..rest.len() - octets).ok_or(OpusError::Padding)?;
            if vbr {
                let mut at = 0usize;
                let mut total = 0usize;
                for _ in 1..frames {
                    let (len, used) = frame_length(body.get(at..).ok_or(OpusError::Truncated)?)?;
                    if len > MAX_FRAME_BYTES {
                        return Err(OpusError::FrameTooLong);
                    }
                    at += used;
                    total += len;
                }
                let data = body.len().checked_sub(at).ok_or(OpusError::Truncated)?;
                if total > data {
                    return Err(OpusError::Truncated);
                }
                if data - total > MAX_FRAME_BYTES {
                    return Err(OpusError::FrameTooLong);
                }
                Ok(info(frames, true, padding, data))
            } else {
                if body.len() % usize::from(frames) != 0 {
                    return Err(OpusError::Uneven);
                }
                if body.len() / usize::from(frames) > MAX_FRAME_BYTES {
                    return Err(OpusError::FrameTooLong);
                }
                Ok(info(frames, false, padding, body.len()))
            }
        }
    }
}

/// A constant encoded-silence packet of `frames` CELT frames of
/// `frame_samples` each (120, 240, 480 or 960), fullband, mono or stereo:
/// code 0 for one frame, code 1 for two, code 3 (CBR) for more. `None` if
/// the shape is not one Opus can carry in one packet.
pub fn silence(frame_samples: u32, frames: u8, stereo: bool) -> Option<Vec<u8>> {
    let config: u8 = match frame_samples {
        120 => 28,
        240 => 29,
        480 => 30,
        960 => 31,
        _ => return None,
    };
    if frames == 0 || u32::from(frames) * frame_samples > MAX_PACKET_SAMPLES {
        return None;
    }
    let toc = config << 3 | u8::from(stereo) << 2;
    let mut out = Vec::with_capacity(2 + 2 * usize::from(frames));
    match frames {
        1 => out.push(toc),
        2 => out.push(toc | 1),
        n => {
            out.push(toc | 3);
            out.push(n);
        }
    }
    for _ in 0..frames {
        out.extend_from_slice(&SILENT_CELT_FRAME);
    }
    Some(out)
}

/// What an output frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A packet the sender sent.
    Media,
    /// Silence in place of the sender's silence suppression: a timestamp
    /// gap with no sequence gap.
    Dtx,
    /// Silence in place of packets that were lost.
    Loss,
}

/// One Opus packet for the audio track.
#[derive(Clone, PartialEq, Eq)]
pub struct OpusFrame {
    /// RTP timestamp (48 kHz) of its first sample.
    pub timestamp: u32,
    /// Duration in 48 kHz samples.
    pub duration: u32,
    /// The TOC stereo flag.
    pub stereo: bool,
    /// The Opus packet.
    pub data: Vec<u8>,
    /// Sent, or filled in for DTX or loss.
    pub kind: FrameKind,
}
impl fmt::Debug for OpusFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpusFrame")
            .field("timestamp", &self.timestamp)
            .field("duration", &self.duration)
            .field("stereo", &self.stereo)
            .field("len", &self.data.len())
            .field("kind", &self.kind)
            .finish()
    }
}

/// Limits for [`Depacketiser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusLimits {
    /// Longest timestamp gap filled with silence, in 48 kHz samples. A
    /// longer gap, or a timestamp that goes backwards, is a discontinuity
    /// and is not filled. Default 96 000 (2 s).
    pub max_fill: u32,
}
impl Default for OpusLimits {
    fn default() -> Self {
        Self { max_fill: 96_000 }
    }
}

/// What the depacketiser produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpusOutput {
    /// A frame for the track, in timestamp order.
    Frame(OpusFrame),
    /// The timestamp jumped by more than the fill limit, or went
    /// backwards; nothing was filled and the timeline restarts here.
    Discontinuity {
        /// Where the next packet was expected.
        expected: u32,
        /// Where it was.
        timestamp: u32,
    },
}

/// Turns in-order Opus RTP payloads into a continuous run of frames.
#[derive(Debug)]
pub struct Depacketiser {
    limits: OpusLimits,
    /// Timestamp just after the last frame, once a packet has been seen.
    next: Option<u32>,
    /// The last sent packet's shape: frame size, frame count and stereo.
    shape: (u32, u8, bool),
    /// Packets were lost since the last one.
    gap: bool,
    out: VecDeque<OpusOutput>,
    /// Samples of gaps too short to fill (under 2.5 ms), in total.
    unfilled: u64,
}

impl Depacketiser {
    /// A depacketiser with these limits.
    pub fn new(limits: OpusLimits) -> Self {
        Self {
            limits,
            next: None,
            shape: (960, 1, false),
            gap: false,
            out: VecDeque::new(),
            unfilled: 0,
        }
    }

    /// Records that packets were lost before the next one.
    pub fn lost(&mut self) {
        self.gap = true;
    }

    /// Forgets the timeline, as after a sequence discontinuity: the next
    /// packet is not filled up to.
    pub fn reset(&mut self) {
        self.next = None;
        self.gap = false;
    }

    /// Samples of timestamp gaps shorter than 2.5 ms that could not be
    /// filled, in total.
    pub fn unfilled_samples(&self) -> u64 {
        self.unfilled
    }

    /// The next output, in order.
    pub fn poll(&mut self) -> Option<OpusOutput> {
        self.out.pop_front()
    }

    /// One RTP payload, the next in sequence order. A packet that fails
    /// [`parse_packet`] is refused and counts as lost.
    pub fn push(&mut self, timestamp: u32, payload: &[u8]) -> Result<(), OpusError> {
        let info = match parse_packet(payload) {
            Ok(info) => info,
            Err(e) => {
                self.gap = true;
                return Err(e);
            }
        };
        if let Some(expected) = self.next {
            let delta = timestamp.wrapping_sub(expected);
            if delta != 0 {
                // A negative step reads as a huge positive one.
                if delta <= self.limits.max_fill {
                    let kind = if self.gap {
                        FrameKind::Loss
                    } else {
                        FrameKind::Dtx
                    };
                    self.fill(expected, delta, kind);
                } else {
                    self.out.push_back(OpusOutput::Discontinuity {
                        expected,
                        timestamp,
                    });
                }
            }
        }
        self.gap = false;
        let toc = info.toc;
        self.shape = (toc.frame_samples(), info.frames, toc.stereo());
        let duration = info.duration();
        self.next = Some(timestamp.wrapping_add(duration));
        self.out.push_back(OpusOutput::Frame(OpusFrame {
            timestamp,
            duration,
            stereo: toc.stereo(),
            data: payload.to_vec(),
            kind: FrameKind::Media,
        }));
        Ok(())
    }

    /// Fills `samples` from `start` with silence: packets shaped like the
    /// sender's last one while they fit, then the largest CELT frames that
    /// do. Under 2.5 ms is left as a short timestamp step.
    fn fill(&mut self, start: u32, samples: u32, kind: FrameKind) {
        let (frame_samples, frames, stereo) = self.shape;
        // The sender's shape, if silence can take it (CELT frame sizes);
        // otherwise 20 ms frames in the same total duration.
        let (frame_samples, frames) = if silence(frame_samples, frames, stereo).is_some() {
            (frame_samples, frames)
        } else {
            let total = frame_samples * u32::from(frames);
            if total % 960 == 0 && total / 960 >= 1 {
                (960, (total / 960) as u8)
            } else {
                (960, 1)
            }
        };
        let mut at = start;
        let mut left = samples;
        let chunk = frame_samples * u32::from(frames);
        let mut push = |at: &mut u32, left: &mut u32, fs: u32, n: u8| {
            if let Some(data) = silence(fs, n, stereo) {
                let duration = fs * u32::from(n);
                self.out.push_back(OpusOutput::Frame(OpusFrame {
                    timestamp: *at,
                    duration,
                    stereo,
                    data,
                    kind,
                }));
                *at = at.wrapping_add(duration);
                *left -= duration;
            }
        };
        while left >= chunk {
            push(&mut at, &mut left, frame_samples, frames);
        }
        for fs in [960, 480, 240, 120] {
            while left >= fs {
                push(&mut at, &mut left, fs, 1);
            }
        }
        self.unfilled += u64::from(left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_fields() {
        // Every configuration's mode, bandwidth and frame size (Table 2).
        let expect: [(Mode, Bandwidth, u32); 32] = {
            use Bandwidth::*;
            use Mode::*;
            [
                (Silk, Narrowband, 480),
                (Silk, Narrowband, 960),
                (Silk, Narrowband, 1920),
                (Silk, Narrowband, 2880),
                (Silk, Mediumband, 480),
                (Silk, Mediumband, 960),
                (Silk, Mediumband, 1920),
                (Silk, Mediumband, 2880),
                (Silk, Wideband, 480),
                (Silk, Wideband, 960),
                (Silk, Wideband, 1920),
                (Silk, Wideband, 2880),
                (Hybrid, SuperWideband, 480),
                (Hybrid, SuperWideband, 960),
                (Hybrid, Fullband, 480),
                (Hybrid, Fullband, 960),
                (Celt, Narrowband, 120),
                (Celt, Narrowband, 240),
                (Celt, Narrowband, 480),
                (Celt, Narrowband, 960),
                (Celt, Wideband, 120),
                (Celt, Wideband, 240),
                (Celt, Wideband, 480),
                (Celt, Wideband, 960),
                (Celt, SuperWideband, 120),
                (Celt, SuperWideband, 240),
                (Celt, SuperWideband, 480),
                (Celt, SuperWideband, 960),
                (Celt, Fullband, 120),
                (Celt, Fullband, 240),
                (Celt, Fullband, 480),
                (Celt, Fullband, 960),
            ]
        };
        for (config, (mode, bw, samples)) in expect.iter().enumerate() {
            let toc = Toc((config as u8) << 3 | 0x04 | 2);
            assert_eq!(toc.config(), config as u8);
            assert_eq!((toc.mode(), toc.bandwidth()), (*mode, *bw), "{config}");
            assert_eq!(toc.frame_samples(), *samples, "{config}");
            assert!(toc.stereo());
            assert_eq!(toc.code(), 2);
        }
    }

    #[test]
    fn codes_zero_to_two() {
        // Code 0: one frame, any length up to 1275, zero included.
        let p = parse_packet(&[0x78]).expect("TOC only");
        assert_eq!((p.frames, p.frame_bytes, p.duration()), (1, 0, 960));
        let mut big = vec![0xf8];
        big.resize(1 + 1275, 0);
        assert_eq!(parse_packet(&big).map(|p| p.frame_bytes), Ok(1275));
        big.push(0);
        assert_eq!(parse_packet(&big), Err(OpusError::FrameTooLong));
        // Code 1: an even number of bytes after the TOC.
        let p = parse_packet(&[0xf9, 1, 2, 3, 4]).expect("code 1");
        assert_eq!((p.frames, p.duration()), (2, 1920));
        assert_eq!(
            parse_packet(&[0xf9, 1, 2, 3]),
            Err(OpusError::UnequalFrames)
        );
        assert_eq!(parse_packet(&[0xf9]).map(|p| p.frames), Ok(2));
        // Code 2: one- and two-byte lengths.
        let p = parse_packet(&[0x0a, 2, 9, 9, 8]).expect("code 2");
        assert_eq!(
            (p.frames, p.vbr, p.frame_bytes, p.duration()),
            (2, true, 3, 1920)
        );
        assert_eq!(parse_packet(&[0x0a, 4, 9, 9]), Err(OpusError::Truncated));
        assert_eq!(parse_packet(&[0x0a]), Err(OpusError::Truncated));
        assert_eq!(parse_packet(&[0x0a, 252]), Err(OpusError::Truncated));
        // 252 + 4 * 1 = 256 bytes in the first frame.
        let mut two = vec![0x0a, 252, 1];
        two.resize(3 + 256 + 7, 0);
        assert_eq!(parse_packet(&two).map(|p| p.frame_bytes), Ok(263));
        let mut long = vec![0x0a, 0];
        long.resize(2 + 1276, 0);
        assert_eq!(parse_packet(&long), Err(OpusError::FrameTooLong));
        assert_eq!(parse_packet(&[]), Err(OpusError::Empty));
    }

    #[test]
    fn code_three() {
        // CBR, three 20 ms frames of two bytes: libopus's 60 ms silence.
        let p = parse_packet(&[0xfb, 0x03, 0xff, 0xfe, 0xff, 0xfe, 0xff, 0xfe]).expect("cbr");
        assert_eq!(
            (p.frames, p.vbr, p.padding, p.duration()),
            (3, false, 0, 2880)
        );
        assert_eq!(
            parse_packet(&[0xfb, 0x03, 0xff, 0xfe, 0xff, 0xfe]),
            Err(OpusError::Uneven)
        );
        assert_eq!(parse_packet(&[0xfb]), Err(OpusError::FrameCount));
        assert_eq!(parse_packet(&[0xfb, 0x00]), Err(OpusError::FrameCount));
        // 120 ms is the most: six 20 ms frames, or 48 of 2.5 ms.
        assert!(parse_packet(&[0xfb, 6]).is_ok());
        assert_eq!(parse_packet(&[0xfb, 7]), Err(OpusError::TooLong));
        assert!(parse_packet(&[0xe3, 48]).is_ok());
        assert_eq!(parse_packet(&[0xe3, 49]), Err(OpusError::TooLong));
        // SILK 60 ms: two frames is the most.
        assert!(parse_packet(&[0x1b, 2]).is_ok());
        assert_eq!(parse_packet(&[0x1b, 3]), Err(OpusError::TooLong));
        // VBR: lengths for all but the last frame.
        let p = parse_packet(&[0xfb, 0x83, 1, 2, 7, 8, 8, 9, 9, 9]).expect("vbr");
        assert_eq!((p.frames, p.vbr, p.frame_bytes), (3, true, 6));
        assert_eq!(
            parse_packet(&[0xfb, 0x83, 1, 9, 7]),
            Err(OpusError::Truncated)
        );
        assert_eq!(parse_packet(&[0xfb, 0x83, 1]), Err(OpusError::Truncated));
        // Padding: a length byte of 2 means two padding octets at the end.
        let p = parse_packet(&[0xfb, 0x42, 2, 5, 5, 0, 0]).expect("padded");
        assert_eq!((p.frames, p.padding, p.frame_bytes), (2, 3, 2));
        assert_eq!(
            parse_packet(&[0xfb, 0x42, 9, 5, 5]),
            Err(OpusError::Padding)
        );
        assert_eq!(parse_packet(&[0xfb, 0x42]), Err(OpusError::Padding));
        // 255 then 0: 254 padding octets and two length bytes.
        let mut pad = vec![0xfb, 0x41, 255, 0, 7];
        pad.resize(5 + 254, 0);
        let p = parse_packet(&pad).expect("long padding");
        assert_eq!((p.padding, p.frame_bytes), (256, 1));
        // One octet short of the padding.
        pad.truncate(pad.len() - 2);
        assert_eq!(parse_packet(&pad), Err(OpusError::Padding));
        // VBR with padding.
        let p = parse_packet(&[0xfb, 0xc2, 1, 1, 4, 5, 6, 0]).expect("vbr padded");
        assert_eq!((p.frames, p.padding, p.frame_bytes), (2, 2, 3));
    }

    #[test]
    fn silence_shapes() {
        // The shapes libopus itself made for silence (FFmpeg as a black
        // box): 20 ms mono and stereo, 40 ms as code 1, 60 ms as code 3.
        assert_eq!(silence(960, 1, false), Some(vec![0xf8, 0xff, 0xfe]));
        assert_eq!(silence(960, 1, true), Some(vec![0xfc, 0xff, 0xfe]));
        assert_eq!(
            silence(960, 2, false),
            Some(vec![0xf9, 0xff, 0xfe, 0xff, 0xfe])
        );
        assert_eq!(
            silence(960, 3, false),
            Some(vec![0xfb, 3, 0xff, 0xfe, 0xff, 0xfe, 0xff, 0xfe])
        );
        assert_eq!(silence(480, 1, false), Some(vec![0xf0, 0xff, 0xfe]));
        assert_eq!(silence(240, 1, false), Some(vec![0xe8, 0xff, 0xfe]));
        assert_eq!(silence(120, 1, true), Some(vec![0xe4, 0xff, 0xfe]));
        assert_eq!(silence(960, 7, false), None);
        assert_eq!(silence(960, 0, false), None);
        assert_eq!(silence(1920, 1, false), None);
        for fs in [120, 240, 480, 960] {
            for n in 1..=(MAX_PACKET_SAMPLES / fs) as u8 {
                for stereo in [false, true] {
                    let p = silence(fs, n, stereo).expect("shape");
                    let info = parse_packet(&p).expect("valid");
                    assert_eq!(info.toc.mode(), Mode::Celt);
                    assert_eq!(info.toc.bandwidth(), Bandwidth::Fullband);
                    assert_eq!(info.toc.stereo(), stereo);
                    assert_eq!(info.frames, n);
                    assert_eq!(info.duration(), fs * u32::from(n));
                    assert_eq!(info.frame_bytes, 2 * usize::from(n));
                }
            }
        }
    }

    fn frames(d: &mut Depacketiser) -> Vec<OpusOutput> {
        std::iter::from_fn(|| d.poll()).collect()
    }

    fn kinds(out: &[OpusOutput]) -> Vec<(u32, u32, FrameKind)> {
        out.iter()
            .filter_map(|o| match o {
                OpusOutput::Frame(f) => Some((f.timestamp, f.duration, f.kind)),
                OpusOutput::Discontinuity { .. } => None,
            })
            .collect()
    }

    #[test]
    fn dtx_and_loss_fill() {
        let mut d = Depacketiser::new(OpusLimits::default());
        let speech = [0xfcu8, 1, 2, 3];
        d.push(1000, &speech).expect("ok");
        d.push(1960, &speech).expect("ok");
        // DTX: the next packet is 3 frames later with no loss.
        d.push(1960 + 960 * 3, &speech).expect("ok");
        // Loss: one packet lost, its 20 ms filled and marked.
        d.lost();
        d.push(1960 + 960 * 5, &speech).expect("ok");
        let out = frames(&mut d);
        use FrameKind::*;
        assert_eq!(
            kinds(&out),
            [
                (1000, 960, Media),
                (1960, 960, Media),
                (2920, 960, Dtx),
                (3880, 960, Dtx),
                (4840, 960, Media),
                (5800, 960, Loss),
                (6760, 960, Media),
            ]
        );
        for o in &out {
            if let OpusOutput::Frame(f) = o
                && f.kind != Media
            {
                assert_eq!(f.data, [0xfc, 0xff, 0xfe], "stereo silence");
            }
        }
    }

    #[test]
    fn fill_follows_the_senders_shape_and_granule() {
        let mut d = Depacketiser::new(OpusLimits::default());
        // 60 ms SILK packets (config 3, mono): silence comes as 3 x 20 ms
        // CELT, then the rest in the largest frames that fit.
        let silk60 = [0x18u8, 9, 9];
        d.push(0, &silk60).expect("ok");
        d.lost();
        // 2880 * 2 + 960 + 240 + 120 + 50 samples later.
        let gap = 2880 * 2 + 960 + 240 + 120 + 50;
        d.push(2880 + gap, &silk60).expect("ok");
        let out = frames(&mut d);
        let k: Vec<u32> = kinds(&out).iter().map(|k| k.1).collect();
        assert_eq!(k, [2880, 2880, 2880, 960, 240, 120, 2880]);
        assert_eq!(d.unfilled_samples(), 50);
        // Each filled frame starts where the last ended.
        let filled = kinds(&out);
        for w in filled.windows(2).take(5) {
            assert_eq!(w[0].0 + w[0].1, w[1].0);
        }
    }

    #[test]
    fn discontinuities_are_not_filled() {
        let mut d = Depacketiser::new(OpusLimits { max_fill: 9600 });
        let p = [0xf8u8, 1];
        d.push(0, &p).expect("ok");
        d.push(960 + 9600, &p).expect("ok, at the limit");
        assert_eq!(frames(&mut d).len(), 1 + 10 + 1);
        d.push(20_000 + 9601, &p).expect("ok");
        let out = frames(&mut d);
        assert!(matches!(out[0], OpusOutput::Discontinuity { .. }));
        assert_eq!(out.len(), 2);
        // Backwards, and the 2^31 jump the fuzzer would find: no fill.
        d.push(5, &p).expect("ok");
        d.push(5u32.wrapping_add(1 << 31), &p).expect("ok");
        let out = frames(&mut d);
        assert_eq!(out.len(), 4);
        // Across the 32-bit wrap is an ordinary step.
        d.push(u32::MAX - 959, &p).expect("ok");
        d.push(0, &p).expect("ok");
        d.push(960 * 2, &p).expect("ok");
        let out = frames(&mut d);
        assert_eq!(
            kinds(&out).iter().map(|k| k.2).collect::<Vec<_>>(),
            [
                FrameKind::Media,
                FrameKind::Media,
                FrameKind::Dtx,
                FrameKind::Media
            ]
        );
    }

    #[test]
    fn refused_packets_count_as_loss() {
        let mut d = Depacketiser::new(OpusLimits::default());
        d.push(0, &[0xf8, 1]).expect("ok");
        assert_eq!(d.push(960, &[0xf9, 1]), Err(OpusError::UnequalFrames));
        d.push(1920, &[0xf8, 1]).expect("ok");
        let out = frames(&mut d);
        assert_eq!(kinds(&out)[1], (960, 960, FrameKind::Loss));
        // A reset forgets the timeline.
        d.reset();
        d.push(50_000, &[0xf8, 1]).expect("ok");
        assert_eq!(frames(&mut d).len(), 1);
    }
}
