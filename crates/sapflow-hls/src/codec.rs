//! H.264 and AAC: just enough to describe the tracks and frame the samples.
//!
//! Bitstream parsing is delegated to `h264-reader` (Annex B splitting, SPS)
//! and `adts-reader` (ADTS headers). This module is glue around them.

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

    #[test]
    fn non_idr_access_unit() {
        let au = access_unit(&[0, 0, 0, 1, 0x41, 0x9a, 0x00, 0x10]);
        assert!(!au.idr);
        assert_eq!(au.data, vec![0, 0, 0, 4, 0x41, 0x9a, 0x00, 0x10]);
    }
}
