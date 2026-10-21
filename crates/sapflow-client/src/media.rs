//! Validated native encoder descriptions, timing and shared encoded payloads.

use std::sync::Arc;

use h264_reader::Context;
use h264_reader::nal::pps::PicParameterSet;
use h264_reader::nal::sps::SeqParameterSet;
use h264_reader::nal::{Nal, RefNal};

/// Bounds work at the compressed ingress boundary, before walking NAL lengths.
pub const MAX_SAMPLE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PARAMETER_SET_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    InvalidTimescale,
    InvalidTimestamp,
    InvalidDuration,
    TimestampOverflow,
    InvalidH264Config,
    UnsupportedAacConfig,
    EmptyPayload,
    OversizedPayload,
    InvalidAvcc,
    KeyframeMismatch,
    InvalidAudioTiming,
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MediaError {}

/// One SPS/PPS pair and a fixed four-byte AVCC NAL length prefix.
/// Parameter-set syntax is checked by the existing maintained H.264 reader.
#[derive(Debug, Clone)]
pub struct H264Config {
    sps: Arc<[u8]>,
    pps: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl H264Config {
    pub fn new(sps: &[u8], pps: &[u8]) -> Result<Self, MediaError> {
        let valid = |data: &[u8], kind| {
            !data.is_empty()
                && data.len() <= MAX_PARAMETER_SET_BYTES
                && data[0] & 0x80 == 0
                && data[0] & 0x1f == kind
        };
        if !valid(sps, 7) || !valid(pps, 8) {
            return Err(MediaError::InvalidH264Config);
        }
        let sps_nal = RefNal::new(sps, &[], true);
        let parsed = SeqParameterSet::from_bits(sps_nal.rbsp_bits())
            .map_err(|_| MediaError::InvalidH264Config)?;
        let (width, height) = parsed
            .pixel_dimensions()
            .map_err(|_| MediaError::InvalidH264Config)?;
        if width == 0 || height == 0 {
            return Err(MediaError::InvalidH264Config);
        }
        let mut context = Context::new();
        context.put_seq_param_set(parsed);
        let pps_nal = RefNal::new(pps, &[], true);
        PicParameterSet::from_bits(&context, pps_nal.rbsp_bits())
            .map_err(|_| MediaError::InvalidH264Config)?;
        Ok(Self {
            sps: Arc::from(sps),
            pps: Arc::from(pps),
            width,
            height,
        })
    }

    pub fn sps(&self) -> &[u8] {
        &self.sps
    }
    pub fn pps(&self) -> &[u8] {
        &self.pps
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Deliberately narrow native AAC-LC profile: mono/stereo, 1024 samples/frame,
/// indexed frequency, no SBR, PCE, 960-sample frames or core-coder dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    asc: [u8; 2],
    sample_rate: u32,
    channels: u8,
}

impl AacConfig {
    pub fn new(asc: &[u8]) -> Result<Self, MediaError> {
        let [a, b] = *asc else {
            return Err(MediaError::UnsupportedAacConfig);
        };
        let object_type = a >> 3;
        let rate_index = ((a & 7) << 1) | (b >> 7);
        let channels = (b >> 3) & 15;
        const RATES: [u32; 13] = [
            96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
        ];
        if object_type != 2 || !(1..=2).contains(&channels) || b & 7 != 0 {
            return Err(MediaError::UnsupportedAacConfig);
        }
        let sample_rate = *RATES
            .get(usize::from(rate_index))
            .ok_or(MediaError::UnsupportedAacConfig)?;
        Ok(Self {
            asc: [a, b],
            sample_rate,
            channels,
        })
    }
    pub fn audio_specific_config(self) -> [u8; 2] {
        self.asc
    }
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    pub fn channels(self) -> u8 {
        self.channels
    }
}

#[derive(Debug, Clone)]
pub enum CodecConfig {
    H264(H264Config),
    Aac(AacConfig),
}

/// Non-negative PTS/DTS in one explicit clock, with positive duration.
/// Epoch rebasing and per-track DTS continuity belong to the native adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleTiming {
    pts: i64,
    dts: i64,
    duration: u32,
    timescale: u32,
}

impl SampleTiming {
    pub fn new(pts: i64, dts: i64, duration: u32, timescale: u32) -> Result<Self, MediaError> {
        if timescale == 0 || timescale > 1_000_000_000 {
            return Err(MediaError::InvalidTimescale);
        }
        if pts < 0 || dts < 0 {
            return Err(MediaError::InvalidTimestamp);
        }
        if duration == 0 {
            return Err(MediaError::InvalidDuration);
        }
        let timing = Self {
            pts,
            dts,
            duration,
            timescale,
        };
        timing.decode_end_ns()?;
        timing.presentation_end_ns()?;
        Ok(timing)
    }
    pub fn pts(self) -> i64 {
        self.pts
    }
    pub fn dts(self) -> i64 {
        self.dts
    }
    pub fn duration(self) -> u32 {
        self.duration
    }
    pub fn timescale(self) -> u32 {
        self.timescale
    }
    fn ns(self, ticks: u128) -> Result<u64, MediaError> {
        u64::try_from(ticks * 1_000_000_000 / u128::from(self.timescale))
            .map_err(|_| MediaError::TimestampOverflow)
    }
    pub fn decode_time_ns(self) -> u64 {
        // Constructor checked the larger end timestamp.
        (self.dts as u128 * 1_000_000_000 / u128::from(self.timescale)) as u64
    }
    pub fn decode_end_ns(self) -> Result<u64, MediaError> {
        self.ns(self.dts as u128 + u128::from(self.duration))
    }
    pub fn presentation_end_ns(self) -> Result<u64, MediaError> {
        self.ns(self.pts as u128 + u128::from(self.duration))
    }
}

/// One encoder ownership handoff. Cloning this handle never clones its bytes.
pub type SharedSample = Arc<EncodedSample>;

#[derive(Debug)]
pub struct EncodedSample {
    config: Arc<CodecConfig>,
    timing: SampleTiming,
    keyframe: bool,
    payload: Arc<[u8]>,
}

impl EncodedSample {
    pub fn new(
        config: Arc<CodecConfig>,
        timing: SampleTiming,
        keyframe: bool,
        payload: Arc<[u8]>,
    ) -> Result<Self, MediaError> {
        if payload.is_empty() {
            return Err(MediaError::EmptyPayload);
        }
        if payload.len() > MAX_SAMPLE_BYTES {
            return Err(MediaError::OversizedPayload);
        }
        match config.as_ref() {
            CodecConfig::H264(_) => validate_avcc(&payload, keyframe)?,
            CodecConfig::Aac(audio) => {
                if keyframe
                    || timing.pts != timing.dts
                    || timing.timescale != audio.sample_rate
                    || timing.duration != 1024
                {
                    return Err(MediaError::InvalidAudioTiming);
                }
                // Raw AAC access units only. Decoding/validating AAC syntax is
                // the platform codec's job; never silently accept ADTS framing.
                if payload.len() >= 2 && payload[0] == 0xff && payload[1] & 0xf6 == 0xf0 {
                    return Err(MediaError::UnsupportedAacConfig);
                }
            }
        }
        Ok(Self {
            config,
            timing,
            keyframe,
            payload,
        })
    }
    pub fn config(&self) -> &Arc<CodecConfig> {
        &self.config
    }
    pub fn timing(&self) -> SampleTiming {
        self.timing
    }
    pub fn is_keyframe(&self) -> bool {
        self.keyframe
    }
    pub fn is_video(&self) -> bool {
        matches!(self.config.as_ref(), CodecConfig::H264(_))
    }
    pub fn payload(&self) -> &Arc<[u8]> {
        &self.payload
    }
}

// Framing validation only; native encoders supply complete access units.
// SPS/PPS are out of band. Unsupported partitioned/extension NALs are refused.
fn validate_avcc(mut rest: &[u8], keyframe: bool) -> Result<(), MediaError> {
    let mut idr = false;
    let mut slice = false;
    while !rest.is_empty() {
        let prefix: [u8; 4] = rest
            .get(..4)
            .ok_or(MediaError::InvalidAvcc)?
            .try_into()
            .map_err(|_| MediaError::InvalidAvcc)?;
        let len = u32::from_be_bytes(prefix) as usize;
        rest = &rest[4..];
        let nal = rest
            .get(..len)
            .filter(|nal| !nal.is_empty())
            .ok_or(MediaError::InvalidAvcc)?;
        if nal[0] & 0x80 != 0 {
            return Err(MediaError::InvalidAvcc);
        }
        match nal[0] & 0x1f {
            1 => slice = true,
            5 => {
                slice = true;
                idr = true;
            }
            6 | 9 | 12 => {}
            _ => return Err(MediaError::InvalidAvcc),
        }
        rest = &rest[len..];
    }
    if !slice {
        return Err(MediaError::InvalidAvcc);
    }
    if keyframe != idr {
        return Err(MediaError::KeyframeMismatch);
    }
    Ok(())
}

/// Per-track continuity within one explicitly configured encoder epoch.
/// Configuration replacement requires a new validator and queue discontinuity;
/// pointer identity makes an accidental mid-GOP reconfiguration explicit.
#[derive(Debug, Default)]
pub struct SampleContinuity {
    video: Option<(Arc<CodecConfig>, u64)>,
    audio: Option<(Arc<CodecConfig>, u64)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityError {
    ConfigurationChanged,
    TimestampRegression,
    NeedsKeyframe,
}
impl SampleContinuity {
    pub fn validate(&mut self, sample: &EncodedSample) -> Result<(), ContinuityError> {
        let previous = if sample.is_video() {
            &mut self.video
        } else {
            &mut self.audio
        };
        if let Some((config, end_ns)) = previous {
            if !Arc::ptr_eq(config, sample.config()) {
                return Err(ContinuityError::ConfigurationChanged);
            }
            if sample.timing().decode_time_ns() < *end_ns {
                return Err(ContinuityError::TimestampRegression);
            }
        } else if sample.is_video() && !sample.is_keyframe() {
            return Err(ContinuityError::NeedsKeyframe);
        }
        // A SampleTiming is immutable and constructor-validated.
        let end_ns = sample
            .timing()
            .decode_end_ns()
            .map_err(|_| ContinuityError::TimestampRegression)?;
        *previous = Some((sample.config().clone(), end_ns));
        Ok(())
    }
    /// Call only with an explicit discontinuity delivered to both sinks and
    /// appropriate recording segmentation. Next video must be an IDR again.
    pub fn reset(&mut self) {
        self.video = None;
        self.audio = None;
    }
}
