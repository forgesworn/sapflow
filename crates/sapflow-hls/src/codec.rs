//! H.264, AAC and Opus: just enough to describe the tracks and frame the
//! samples.
//!
//! Bitstream parsing is delegated to `h264-reader` (Annex B splitting, SPS)
//! and `adts-reader` (ADTS headers). This module is glue around them, plus
//! the Opus packet framing check (RFC 6716 sections 3.1 to 3.4), which reads
//! only the TOC byte, frame count, frame lengths and padding.

use std::io::Read;

use h264_reader::annexb::AnnexBReader;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal, UnitType};
use h264_reader::push::NalInterest;

/// Upper bound on one NAL unit, so a hostile stream cannot grow a buffer
/// without limit. 4 MiB is far above any 1080p I-frame slice.
const MAX_NAL: usize = 4 * 1024 * 1024;

/// What the init segment and the multivariant playlist need to know about
/// the video track.
#[derive(Debug, Clone, PartialEq)]
pub struct H264Config {
    /// The SPS NAL unit, with its header and without a start code.
    pub sps: Vec<u8>,
    /// The PPS NAL unit, with its header and without a start code.
    pub pps: Vec<u8>,
    /// Picture width in pixels, from the SPS.
    pub width: u16,
    /// Picture height in pixels, from the SPS.
    pub height: u16,
    /// Frame rate from the SPS timing information, if it carries any.
    pub fps: Option<f64>,
}

impl H264Config {
    /// Builds a config from raw SPS and PPS NAL units (with their headers,
    /// without start codes). Fails if the SPS does not parse.
    pub fn new(sps: &[u8], pps: &[u8]) -> Option<Self> {
        if sps.len() < 4 || pps.is_empty() {
            return None;
        }
        let nal = RefNal::new(sps, &[], true);
        let parsed = SeqParameterSet::from_bits(nal.rbsp_bits()).ok()?;
        let (w, h) = parsed.pixel_dimensions().ok()?;
        Some(Self {
            sps: sps.to_vec(),
            pps: pps.to_vec(),
            width: u16::try_from(w).ok()?,
            height: u16::try_from(h).ok()?,
            fps: parsed.fps(),
        })
    }

    /// `profile_idc` from the SPS.
    pub fn profile(&self) -> u8 {
        self.sps[1]
    }

    /// The profile compatibility byte of the SPS (`constraint_set` flags).
    pub fn compatibility(&self) -> u8 {
        self.sps[2]
    }

    /// `level_idc` from the SPS.
    pub fn level(&self) -> u8 {
        self.sps[3]
    }

    /// RFC 6381 codec string, e.g. `avc1.64001f`.
    pub fn codec_string(&self) -> String {
        format!(
            "avc1.{:02x}{:02x}{:02x}",
            self.profile(),
            self.compatibility(),
            self.level()
        )
    }
}

/// One H.264 access unit, converted from Annex B to length-prefixed form.
#[derive(Debug, Default)]
pub struct AccessUnit {
    /// NAL units with 4-byte big-endian lengths, parameter sets and access
    /// unit delimiters removed (they live in the init segment).
    pub data: Vec<u8>,
    /// Whether the access unit holds an IDR slice, so it is a keyframe.
    pub idr: bool,
    /// An SPS NAL unit found in the access unit, with its header and no length
    /// prefix.
    pub sps: Option<Vec<u8>>,
    /// A PPS NAL unit found in the access unit, with its header and no length
    /// prefix.
    pub pps: Option<Vec<u8>>,
}

/// Splits an Annex B buffer (one PES payload) into an access unit.
pub fn access_unit(annexb: &[u8]) -> AccessUnit {
    let mut au = AccessUnit::default();
    {
        let mut reader = AnnexBReader::accumulate(|nal: RefNal<'_>| {
            if !nal.is_complete() {
                return NalInterest::Buffer;
            }
            let Ok(header) = nal.header() else {
                return NalInterest::Ignore;
            };
            let mut bytes = Vec::new();
            if nal
                .reader()
                .take(MAX_NAL as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() > MAX_NAL
            {
                return NalInterest::Ignore;
            }
            match header.nal_unit_type() {
                UnitType::SeqParameterSet => au.sps = Some(bytes),
                UnitType::PicParameterSet => au.pps = Some(bytes),
                UnitType::AccessUnitDelimiter
                | UnitType::EndOfSeq
                | UnitType::EndOfStream
                | UnitType::FillerData => {}
                t => {
                    if t == UnitType::SliceLayerWithoutPartitioningIdr {
                        au.idr = true;
                    }
                    if let Ok(len) = u32::try_from(bytes.len()) {
                        au.data.extend_from_slice(&len.to_be_bytes());
                        au.data.extend_from_slice(&bytes);
                    }
                }
            }
            NalInterest::Ignore
        });
        reader.push(annexb);
        reader.reset();
    }
    au
}

/// Converts a length-prefixed access unit (FLV/MP4 style, `length_size`
/// bytes per length) to the 4-byte form, with parameter sets pulled out.
/// Stops at the first length that runs past the end.
pub fn access_unit_avcc(data: &[u8], length_size: usize) -> AccessUnit {
    let mut au = AccessUnit::default();
    if !(1..=4).contains(&length_size) {
        return au;
    }
    let mut rest = data;
    while rest.len() > length_size {
        let len = rest[..length_size]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        rest = &rest[length_size..];
        if len == 0 || len > rest.len() || len > MAX_NAL {
            break;
        }
        let (nal, tail) = rest.split_at(len);
        rest = tail;
        match nal[0] & 0x1f {
            7 => au.sps = Some(nal.to_vec()),
            8 => au.pps = Some(nal.to_vec()),
            9..=12 => {}
            t => {
                if t == 5 {
                    au.idr = true;
                }
                au.data.extend_from_slice(&(len as u32).to_be_bytes());
                au.data.extend_from_slice(nal);
            }
        }
    }
    au
}

/// SPS, PPS and NAL length size from an AVCDecoderConfigurationRecord
/// (RTMP's AVC sequence header, ISO/IEC 14496-15 5.3.3.1).
///
/// Parsed here rather than with `h264-reader`'s `avcc` module, whose
/// parameter-set iterator panics on truncated records (found by fuzzing).
/// Only the first SPS and PPS are used.
pub fn avc_decoder_config(record: &[u8]) -> Option<(Vec<u8>, Vec<u8>, usize)> {
    fn take<'a>(buf: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        if buf.len() < n {
            return None;
        }
        let (head, tail) = buf.split_at(n);
        *buf = tail;
        Some(head)
    }
    fn param_set(buf: &mut &[u8]) -> Option<Vec<u8>> {
        let len = take(buf, 2)?;
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        let nal = take(buf, len)?;
        (!nal.is_empty()).then(|| nal.to_vec())
    }
    let mut buf = record;
    let head = take(&mut buf, 6)?;
    if head[0] != 1 {
        return None;
    }
    let length_size = usize::from(head[4] & 0x03) + 1;
    let num_sps = usize::from(head[5] & 0x1f);
    let mut sps = None;
    for _ in 0..num_sps {
        let s = param_set(&mut buf)?;
        sps.get_or_insert(s);
    }
    let num_pps = usize::from(*take(&mut buf, 1)?.first()?);
    let mut pps = None;
    for _ in 0..num_pps {
        let p = param_set(&mut buf)?;
        pps.get_or_insert(p);
    }
    let (sps, pps) = (sps?, pps?);
    if sps[0] & 0x1f != 7 || pps[0] & 0x1f != 8 {
        return None;
    }
    Some((sps, pps, length_size))
}

/// AAC as described by an ADTS header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    /// MPEG-4 audio object type (2 = AAC-LC).
    pub object_type: u8,
    /// Index into the MPEG-4 sampling frequency table (3 is 48 kHz, 4 is 44.1
    /// kHz).
    pub sample_rate_index: u8,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// MPEG-4 channel configuration (1 is mono, 2 is stereo).
    pub channels: u8,
}

impl AacConfig {
    /// The two-byte AudioSpecificConfig for `esds`.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let v: u16 = (u16::from(self.object_type) << 11)
            | (u16::from(self.sample_rate_index) << 7)
            | (u16::from(self.channels) << 3);
        v.to_be_bytes()
    }

    /// Parses a two-byte AudioSpecificConfig (RTMP's AAC sequence header).
    /// Escaped object types and explicit sample rates are not supported.
    pub fn from_audio_specific_config(asc: &[u8]) -> Option<Self> {
        let [a, b, ..] = *asc else { return None };
        let object_type = a >> 3;
        let sample_rate_index = ((a & 0x07) << 1) | (b >> 7);
        let channels = (b >> 3) & 0x0f;
        if object_type == 0 || object_type == 31 || channels == 0 {
            return None;
        }
        let sample_rate = *SAMPLE_RATES.get(usize::from(sample_rate_index))?;
        Some(Self {
            object_type,
            sample_rate_index,
            sample_rate,
            channels,
        })
    }

    /// RFC 6381 codec string, e.g. `mp4a.40.2`.
    pub fn codec_string(&self) -> String {
        format!("mp4a.40.{}", self.object_type)
    }
}

const SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Samples per AAC frame.
pub const AAC_FRAME_SAMPLES: u32 = 1024;

/// Samples per second of every Opus stream: the decoder's output rate and the
/// RTP clock (RFC 7587 section 4.1), and the track timescale sapflow uses.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;

/// The longest Opus packet, 120 ms at 48 kHz (RFC 6716 section 3.2.5).
pub const OPUS_MAX_PACKET_SAMPLES: u32 = 5_760;

/// The largest Opus frame in bytes (RFC 6716 section 3.4, R2).
const OPUS_MAX_FRAME_BYTES: usize = 1_275;

/// Opus as one connection carries it: 48 kHz, one or two output channels,
/// channel mapping family 0 (RFC 7845 section 5.1.1.1).
///
/// RTP carries no Opus identification header, so the count is set by rule:
/// 2 when stereo was negotiated or the first packet's TOC byte is stereo,
/// otherwise 1 ([`OpusConfig::for_first_packet`]). A decoder set up for
/// either count decodes both mono and stereo packets, so a stream whose TOC
/// stereo flag later changes keeps its configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpusConfig {
    /// Output channel count: 1 or 2.
    pub channels: u8,
}

impl OpusConfig {
    /// One output channel.
    pub const MONO: Self = Self { channels: 1 };
    /// Two output channels.
    pub const STEREO: Self = Self { channels: 2 };

    /// The configuration for `channels`, or `None` for any count but 1 or 2.
    pub fn new(channels: u8) -> Option<Self> {
        let config = Self { channels };
        config.is_supported().then_some(config)
    }

    /// The configuration for a stream that starts with `packet`: stereo if
    /// `stereo_negotiated` (`stereo=1` in the SDP answer) or if the packet's
    /// TOC stereo flag is set, otherwise mono. `None` if the packet is not a
    /// valid Opus packet ([`opus_packet`]).
    pub fn for_first_packet(stereo_negotiated: bool, packet: &[u8]) -> Option<Self> {
        let packet = opus_packet(packet)?;
        Some(if stereo_negotiated || packet.stereo {
            Self::STEREO
        } else {
            Self::MONO
        })
    }

    /// Whether sapflow can describe this configuration: one or two channels.
    pub fn is_supported(&self) -> bool {
        (1..=2).contains(&self.channels)
    }

    /// The `CODECS` value for Opus in an MP4 `Opus` sample entry: `Opus`,
    /// the sample entry's four-character code, which RFC 6381 section 3.3
    /// makes the first element of an ISO BMFF codecs value and calls case
    /// sensitive. Chrome 157 and Firefox 160 accept both spellings.
    pub fn codec_string(&self) -> String {
        "Opus".to_owned()
    }
}

/// What one Opus packet's TOC byte and frame count say (RFC 6716 section 3.1
/// and 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpusPacket {
    /// The TOC stereo flag.
    pub stereo: bool,
    /// Frames in the packet, 1 to 48.
    pub frames: u8,
    /// Duration of each frame in 48 kHz samples: 120 (2.5 ms) to 2880 (60 ms).
    pub frame_samples: u32,
}

impl OpusPacket {
    /// A packet description, for code that stands in for [`opus_packet`].
    pub fn new(stereo: bool, frames: u8, frame_samples: u32) -> Self {
        Self {
            stereo,
            frames,
            frame_samples,
        }
    }

    /// The packet's duration in 48 kHz samples, at most
    /// [`OPUS_MAX_PACKET_SAMPLES`].
    pub fn samples(&self) -> u32 {
        u32::from(self.frames) * self.frame_samples
    }
}

/// Frame duration in 48 kHz samples for a TOC configuration number (RFC 6716
/// section 3.1, Table 2).
fn opus_frame_samples(config: u8) -> u32 {
    match config {
        // SILK only: 10, 20, 40 or 60 ms.
        0..=11 => [480, 960, 1920, 2880][usize::from(config % 4)],
        // Hybrid: 10 or 20 ms.
        12..=15 => [480, 960][usize::from(config % 2)],
        // CELT only: 2.5, 5, 10 or 20 ms.
        _ => [120, 240, 480, 960][usize::from(config % 4)],
    }
}

/// One frame length (RFC 6716 section 3.2.1): a byte below 252, otherwise
/// that byte plus four times the next. Returns the length and the bytes read.
fn opus_frame_length(buf: &[u8]) -> Option<(usize, usize)> {
    let first = usize::from(*buf.first()?);
    if first < 252 {
        return Some((first, 1));
    }
    let second = usize::from(*buf.get(1)?);
    Some((4 * second + first, 2))
}

/// Checks one Opus packet against the framing rules R1 to R7 of RFC 6716
/// section 3.4 and reads its duration. `None` for a malformed packet,
/// including one longer than 120 ms. Nothing in the packet is decoded.
pub fn opus_packet(packet: &[u8]) -> Option<OpusPacket> {
    // R1: at least one byte.
    let (&toc, rest) = packet.split_first()?;
    let frame_samples = opus_frame_samples(toc >> 3);
    let frames = match toc & 0x03 {
        // One frame (R2).
        0 => {
            if rest.len() > OPUS_MAX_FRAME_BYTES {
                return None;
            }
            1
        }
        // Two frames of equal size (R2, R3).
        1 => {
            if rest.len() % 2 != 0 || rest.len() / 2 > OPUS_MAX_FRAME_BYTES {
                return None;
            }
            2
        }
        // Two frames, the first one's length coded (R2, R4).
        2 => {
            let (first, used) = opus_frame_length(rest)?;
            let remaining = rest.len() - used;
            if first > remaining
                || first > OPUS_MAX_FRAME_BYTES
                || remaining - first > OPUS_MAX_FRAME_BYTES
            {
                return None;
            }
            2
        }
        // Any number of frames (section 3.2.5; R2, R5 to R7).
        _ => {
            let (&count, mut rest) = rest.split_first()?;
            let vbr = count & 0x80 != 0;
            let frames = count & 0x3f;
            if frames == 0 || u32::from(frames) * frame_samples > OPUS_MAX_PACKET_SAMPLES {
                return None;
            }
            // Padding: each length byte of 255 means 254 octets and another
            // length byte; the last is 0 to 254. The octets end the packet.
            let mut padding = 0usize;
            if count & 0x40 != 0 {
                loop {
                    let (&p, tail) = rest.split_first()?;
                    rest = tail;
                    if p == 255 {
                        padding += 254;
                    } else {
                        padding += usize::from(p);
                        break;
                    }
                }
            }
            let body = rest.get(..rest.len().checked_sub(padding)?)?;
            let frames_usize = usize::from(frames);
            if vbr {
                let (mut at, mut coded) = (0usize, 0usize);
                for _ in 1..frames {
                    let (len, used) = opus_frame_length(body.get(at..)?)?;
                    if len > OPUS_MAX_FRAME_BYTES {
                        return None;
                    }
                    at += used;
                    coded += len;
                }
                let data = body.len().checked_sub(at)?;
                if coded > data || data - coded > OPUS_MAX_FRAME_BYTES {
                    return None;
                }
            } else if body.len() % frames_usize != 0
                || body.len() / frames_usize > OPUS_MAX_FRAME_BYTES
            {
                return None;
            }
            frames
        }
    };
    Some(OpusPacket {
        stereo: toc & 0x04 != 0,
        frames,
        frame_samples,
    })
}

/// The audio track's codec: AAC as before, or Opus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioConfig {
    /// AAC, described by its AudioSpecificConfig.
    Aac(AacConfig),
    /// Opus at 48 kHz.
    Opus(OpusConfig),
}

impl AudioConfig {
    /// The track timescale: the AAC sample rate, or 48 kHz for Opus.
    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::Aac(a) => a.sample_rate,
            Self::Opus(_) => OPUS_SAMPLE_RATE,
        }
    }

    /// The channel count the sample entry declares.
    pub fn channels(&self) -> u8 {
        match self {
            Self::Aac(a) => a.channels,
            Self::Opus(o) => o.channels,
        }
    }

    /// RFC 6381 codec string: `mp4a.40.2` for AAC-LC, `Opus` for Opus.
    pub fn codec_string(&self) -> String {
        match self {
            Self::Aac(a) => a.codec_string(),
            Self::Opus(o) => o.codec_string(),
        }
    }

    /// The AAC configuration, or `None` for Opus.
    pub fn aac(&self) -> Option<&AacConfig> {
        match self {
            Self::Aac(a) => Some(a),
            Self::Opus(_) => None,
        }
    }

    /// The Opus configuration, or `None` for AAC.
    pub fn opus(&self) -> Option<&OpusConfig> {
        match self {
            Self::Opus(o) => Some(o),
            Self::Aac(_) => None,
        }
    }
}

impl From<AacConfig> for AudioConfig {
    fn from(value: AacConfig) -> Self {
        Self::Aac(value)
    }
}

impl From<OpusConfig> for AudioConfig {
    fn from(value: OpusConfig) -> Self {
        Self::Opus(value)
    }
}

/// Splits a buffer of ADTS frames (one PES payload) into raw AAC frames.
/// Stops at the first malformed header.
pub fn adts_frames(mut buf: &[u8]) -> Vec<(AacConfig, &[u8])> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let Ok(header) = adts_reader::AdtsHeader::from_bytes(buf) else {
            break;
        };
        let frame_len = usize::from(header.frame_length());
        if frame_len == 0 || frame_len > buf.len() {
            break;
        }
        // adts-reader reports the count itself (the header field plus one).
        if header.number_of_raw_data_blocks_in_frame() != 1 {
            // Several raw blocks per ADTS frame: rare, and not worth the
            // splitting code until an encoder needs it.
            break;
        }
        let Ok(payload) = header.payload() else {
            break;
        };
        let sri = sampling_index(&header);
        let Some(&sample_rate) = SAMPLE_RATES.get(usize::from(sri)) else {
            break;
        };
        let config = AacConfig {
            object_type: object_type(&header),
            sample_rate_index: sri,
            sample_rate,
            channels: channel_config(&header),
        };
        out.push((config, payload));
        buf = &buf[frame_len..];
    }
    out
}

fn object_type(h: &adts_reader::AdtsHeader<'_>) -> u8 {
    // ADTS carries the object type minus one in two bits, so this is 1..=4.
    u8::from(h.audio_object_type())
}

fn sampling_index(h: &adts_reader::AdtsHeader<'_>) -> u8 {
    // `AdtsHeader::from_bytes` has already rejected the escape value 15.
    u8::from(h.sampling_frequency())
}

fn channel_config(h: &adts_reader::AdtsHeader<'_>) -> u8 {
    u8::from(h.channel_configuration())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_specific_config_for_aac_lc_48k_stereo() {
        let c = AacConfig {
            object_type: 2,
            sample_rate_index: 3,
            sample_rate: 48000,
            channels: 2,
        };
        assert_eq!(c.audio_specific_config(), [0x11, 0x90]);
        assert_eq!(c.codec_string(), "mp4a.40.2");
    }

    #[test]
    fn access_unit_strips_parameter_sets_and_length_prefixes() {
        // AUD, SPS, PPS, IDR slice, each with a 4- or 3-byte start code.
        let sps = [
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03,
            0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ];
        let mut buf = vec![0, 0, 0, 1, 0x09, 0xf0];
        buf.extend([0, 0, 0, 1]);
        buf.extend(sps);
        buf.extend([0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
        buf.extend([0, 0, 1, 0x65, 0x88, 0x84, 0x00]);
        let au = access_unit(&buf);
        assert!(au.idr);
        assert_eq!(au.sps.as_deref(), Some(&sps[..]));
        assert_eq!(au.pps.as_deref(), Some(&[0x68, 0xce, 0x3c, 0x80][..]));
        assert_eq!(au.data, vec![0, 0, 0, 4, 0x65, 0x88, 0x84, 0x00]);
    }

    #[test]
    fn splits_adts_frames() {
        // Two AAC-LC 48 kHz stereo frames: 7-byte header + payload each.
        let frame = |payload: &[u8]| {
            let len = (7 + payload.len()) as u16;
            let mut f = vec![
                0xff,
                0xf1,
                0x4c,
                0x80 | (len >> 11) as u8,
                (len >> 3) as u8,
                ((len & 7) << 5) as u8 | 0x1f,
                0xfc,
            ];
            f.extend_from_slice(payload);
            f
        };
        let mut buf = frame(b"abc");
        buf.extend(frame(b"defgh"));
        let frames = adts_frames(&buf);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].1, b"abc");
        assert_eq!(frames[1].1, b"defgh");
        let c = frames[0].0;
        assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 48000, 2));
        // Truncated input yields the complete frames only.
        assert_eq!(adts_frames(&buf[..buf.len() - 1]).len(), 1);
        assert!(adts_frames(&[0xff]).is_empty());
    }

    #[test]
    fn audio_specific_config_round_trips() {
        let c = AacConfig::from_audio_specific_config(&[0x11, 0x90]).expect("valid");
        assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 48000, 2));
        assert_eq!(c.audio_specific_config(), [0x11, 0x90]);
        assert_eq!(AacConfig::from_audio_specific_config(&[0x11]), None);
        assert_eq!(AacConfig::from_audio_specific_config(&[0xf8, 0x00]), None);
        assert_eq!(AacConfig::from_audio_specific_config(&[0x17, 0x90]), None); // index 15
    }

    #[test]
    fn avcc_access_units_are_normalised() {
        // 2-byte lengths: AUD, SPS-like, IDR.
        let data = [0, 2, 0x09, 0xf0, 0, 2, 0x67, 0x42, 0, 3, 0x65, 0x88, 0x84];
        let au = access_unit_avcc(&data, 2);
        assert!(au.idr);
        assert_eq!(au.sps.as_deref(), Some(&[0x67, 0x42][..]));
        assert_eq!(au.data, vec![0, 0, 0, 3, 0x65, 0x88, 0x84]);
        // A length past the end stops parsing, without panicking.
        let au = access_unit_avcc(&[0, 0, 0, 9, 0x41, 1], 4);
        assert!(au.data.is_empty());
        assert!(access_unit_avcc(&data, 0).data.is_empty());
    }

    #[test]
    fn avc_decoder_config_records() {
        let sps = [0x67, 0x4d, 0x40, 0x0d];
        let pps = [0x68, 0xef, 0xbc, 0x80];
        let mut rec = vec![1, 0x4d, 0x40, 0x0d, 0xff, 0xe1, 0, 4];
        rec.extend(sps);
        rec.extend([1, 0, 4]);
        rec.extend(pps);
        let (s, p, len) = avc_decoder_config(&rec).expect("valid");
        assert_eq!((s.as_slice(), p.as_slice(), len), (&sps[..], &pps[..], 4));
        // Every truncation fails cleanly.
        for n in 0..rec.len() {
            assert_eq!(avc_decoder_config(&rec[..n]), None, "{n}");
        }
        // The input that made h264-reader panic.
        assert_eq!(avc_decoder_config(&[1, 0, 0, 0, 0xff, 0xe1]), None);
    }

    /// RFC 6716 Table 2, written out: frame size in ms for configurations
    /// 0 to 31.
    const TABLE_2_MS: [f64; 32] = [
        10.0, 20.0, 40.0, 60.0, 10.0, 20.0, 40.0, 60.0, 10.0, 20.0, 40.0,
        60.0, // SILK NB, MB, WB
        10.0, 20.0, 10.0, 20.0, // Hybrid SWB, FB
        2.5, 5.0, 10.0, 20.0, 2.5, 5.0, 10.0, 20.0, 2.5, 5.0, 10.0, 20.0, 2.5, 5.0, 10.0,
        20.0, // CELT NB, WB, SWB, FB
    ];

    fn ms(samples: u32) -> f64 {
        f64::from(samples) / 48.0
    }

    #[test]
    fn opus_duration_for_every_configuration_and_frame_count_code() {
        for config in 0u8..32 {
            let frame = TABLE_2_MS[usize::from(config)];
            for stereo in [false, true] {
                let toc = config << 3 | u8::from(stereo) << 2;
                // Code 0: one frame of three bytes.
                let p = opus_packet(&[toc, 1, 2, 3]).expect("code 0");
                assert_eq!((ms(p.samples()), p.frames, p.stereo), (frame, 1, stereo));
                // Code 1: two equal frames of two bytes each.
                let p = opus_packet(&[toc | 1, 1, 2, 3, 4]).expect("code 1");
                assert_eq!((ms(p.samples()), p.frames), (2.0 * frame, 2));
                // Code 2: a one-byte frame, then a two-byte frame.
                let p = opus_packet(&[toc | 2, 1, 9, 8, 7]).expect("code 2");
                assert_eq!((ms(p.samples()), p.frames), (2.0 * frame, 2));
                // Code 3: as many frames as fit in 120 ms, CBR, one byte each.
                let most = (120.0 / frame) as u8;
                let mut cbr = vec![toc | 3, most];
                cbr.extend(std::iter::repeat_n(0xaa, usize::from(most)));
                let p = opus_packet(&cbr).expect("code 3");
                assert_eq!((ms(p.samples()), p.frames), (f64::from(most) * frame, most));
                assert_eq!(p.samples(), OPUS_MAX_PACKET_SAMPLES);
                // One frame more is over 120 ms (R5).
                let mut long = vec![toc | 3, most + 1];
                long.extend(std::iter::repeat_n(0xaa, usize::from(most) + 1));
                assert_eq!(opus_packet(&long), None, "config {config}: over 120 ms");
            }
        }
    }

    #[test]
    fn opus_code_3_padding_and_vbr() {
        // CELT FB 20 ms (config 31), stereo, code 3.
        let toc = 31 << 3 | 0x04 | 3;
        // CBR, three frames of two bytes, padding: one length byte of 4,
        // then four octets.
        let mut p = vec![toc, 0x40 | 3, 4];
        p.extend([1, 2, 3, 4, 5, 6]);
        p.extend([0; 4]);
        let info = opus_packet(&p).expect("padded CBR");
        assert_eq!((info.frames, info.samples(), info.stereo), (3, 2880, true));
        // A length byte of 255 is 254 octets and another length byte.
        let mut p = vec![toc, 0x40 | 2, 255, 1, 7, 7];
        p.extend(vec![0; 255]);
        assert_eq!(opus_packet(&p).map(|i| i.samples()), Some(1920));
        // Padding past the end (R6, R7).
        assert_eq!(opus_packet(&[toc, 0x40 | 1, 5, 1, 2]), None);
        assert_eq!(opus_packet(&[toc, 0x40 | 1]), None);
        assert_eq!(opus_packet(&[toc, 0x40 | 1, 255]), None);
        // CBR whose bytes do not divide into equal frames (R6).
        assert_eq!(opus_packet(&[toc, 3, 1, 2, 3, 4]), None);
        // VBR, three frames: lengths 1 and 2 coded, the last takes the rest,
        // with two octets of padding.
        let p = [toc, 0x80 | 0x40 | 3, 2, 1, 2, 9, 8, 8, 7, 7, 7, 0, 0];
        assert_eq!(opus_packet(&p).map(|i| i.frames), Some(3));
        // A two-byte frame length: 252 + 4 * 1 = 256 bytes.
        let mut p = vec![toc, 0x80 | 2, 252, 1];
        p.extend(vec![1; 256 + 3]);
        assert_eq!(opus_packet(&p).map(|i| i.frames), Some(2));
        // VBR lengths running past the end (R7).
        assert_eq!(opus_packet(&[toc, 0x80 | 3, 5, 1, 1]), None);
        assert_eq!(opus_packet(&[toc, 0x80 | 2, 252]), None);
        // No frame-count byte, or a count of zero (R5).
        assert_eq!(opus_packet(&[toc]), None);
        assert_eq!(opus_packet(&[toc, 0]), None);
    }

    #[test]
    fn opus_framing_refusals() {
        // R1: an empty packet.
        assert_eq!(opus_packet(&[]), None);
        // A TOC byte alone is one zero-length frame (DTX), which is valid.
        assert_eq!(opus_packet(&[0x78]).map(|p| p.samples()), Some(960));
        // R2: one frame over 1275 bytes.
        let mut p = vec![0x78];
        p.extend(vec![0; 1275]);
        assert!(opus_packet(&p).is_some());
        p.push(0);
        assert_eq!(opus_packet(&p), None);
        // R3: code 1 with an odd number of frame bytes.
        assert_eq!(opus_packet(&[0x79, 1, 2, 3]), None);
        // R4: code 2 whose first length runs past the end, or is missing.
        assert_eq!(opus_packet(&[0x7a, 5, 1, 2]), None);
        assert_eq!(opus_packet(&[0x7a]), None);
        assert_eq!(opus_packet(&[0x7a, 253]), None);
        // R2 for code 2's second frame.
        let mut p = vec![0x7a, 0];
        p.extend(vec![0; 1276]);
        assert_eq!(opus_packet(&p), None);
    }

    #[test]
    fn opus_channel_count_follows_negotiation_or_the_first_toc() {
        let mono = [0x78, 1];
        let stereo = [0x7c, 1];
        assert_eq!(
            OpusConfig::for_first_packet(false, &mono),
            Some(OpusConfig::MONO)
        );
        assert_eq!(
            OpusConfig::for_first_packet(false, &stereo),
            Some(OpusConfig::STEREO)
        );
        assert_eq!(
            OpusConfig::for_first_packet(true, &mono),
            Some(OpusConfig::STEREO)
        );
        assert_eq!(OpusConfig::for_first_packet(true, &[]), None);
        let audio = AudioConfig::from(OpusConfig::STEREO);
        assert_eq!(
            (audio.sample_rate(), audio.channels(), audio.codec_string()),
            (48_000, 2, "Opus".to_owned())
        );
        assert!(audio.aac().is_none() && audio.opus().is_some());
        assert!(!OpusConfig { channels: 3 }.is_supported());
        assert!(!OpusConfig { channels: 0 }.is_supported());
        assert_eq!(OpusConfig::new(2), Some(OpusConfig::STEREO));
        assert_eq!(OpusConfig::new(1), Some(OpusConfig::MONO));
        assert_eq!(OpusConfig::new(3), None);
        assert_eq!(OpusPacket::new(true, 2, 960).samples(), 1920);
    }

    #[test]
    fn non_idr_access_unit() {
        let au = access_unit(&[0, 0, 0, 1, 0x41, 0x9a, 0x00, 0x10]);
        assert!(!au.idr);
        assert_eq!(au.data, vec![0, 0, 0, 4, 0x41, 0x9a, 0x00, 0x10]);
    }
}
