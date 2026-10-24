//! Bounded, pull-driven MPEG-TS packaging of validated native samples.
//!
//! `mpeg2ts` owns PAT/PMT, PES, TS, timestamp and CRC encoding. This module
//! owns the programme layout, cadence and AVCC/ADTS glue. One shared sample
//! is retained until drained; `poll_chunk` emits at most seven TS packets.
//! No network, files, runtime, full-show buffer or full-PES copy is involved.
//!
//! Video is the PCR clock when present; otherwise audio is. Tracks need
//! monotonic per-track DTS, not global callback ordering. PCR/PSI cadence
//! follows that media clock: adapters must continue supplying samples or
//! explicitly reset after an interruption. Output is variable bitrate.
//!
//! Storage is bounded by max_sample_bytes plus metadata for at most 1024
//! NALs, <=8192 configuration bytes, one <=1316-byte returned chunk and small
//! packet scratch. Piece capacity is reported in stats; metadata has no
//! payload copies. Sample media duration and PCR gaps have separate caps.
//! Residence time is the adapter's responsibility: this pull API has no
//! wall-clock timer. Dropping or resetting the muxer releases its sample.
//! Copy counters expose scratch/packet/writer staging; emitted_bytes counts
//! final serialisation. The maintained writer also allocates PSI metadata.
//! These are accounted copies/allocations, not a zero-copy or pooled path.

use std::collections::VecDeque;
use std::time::Duration;

use mpeg2ts::es::{StreamId, StreamType};
use mpeg2ts::pes::PesHeader;
use mpeg2ts::time::{ClockReference, Timestamp};
use mpeg2ts::ts::payload::{Bytes, Pat, Pes, Pmt};
use mpeg2ts::ts::{
    AdaptationField, ContinuityCounter, EsInfo, Pid, ProgramAssociation,
    TransportScramblingControl, TsHeader, TsPacket, TsPacketWriter, TsPayload, VersionNumber,
    WriteTsPacket,
};

use crate::media::{AacConfig, CodecConfig, H264Config, MAX_SAMPLE_BYTES, SharedSample};

pub const TS_PACKET_BYTES: usize = 188;
pub const TS_CHUNK_BYTES: usize = 7 * TS_PACKET_BYTES;
pub const VIDEO_PID: u16 = 0x100;
pub const AUDIO_PID: u16 = 0x101;
pub const PMT_PID: u16 = 0x1000;
const MAX_NALS: usize = 1024;
const START: [u8; 4] = [0, 0, 0, 1];

#[derive(Debug, Clone)]
pub struct MpegTsConfig {
    pub video: Option<H264Config>,
    pub audio: Option<AacConfig>,
    pub max_sample_bytes: usize,
    pub max_sample_duration: Duration,
    /// A larger gap in the PCR track requires an explicit reset.
    pub max_clock_gap: Duration,
    pub pcr_interval: Duration,
    pub tables_interval: Duration,
}
impl Default for MpegTsConfig {
    fn default() -> Self {
        Self {
            video: None,
            audio: None,
            max_sample_bytes: MAX_SAMPLE_BYTES,
            max_sample_duration: Duration::from_millis(250),
            max_clock_gap: Duration::from_secs(2),
            pcr_interval: Duration::from_millis(40),
            tables_interval: Duration::from_millis(100),
        }
    }
}
impl MpegTsConfig {
    fn validate(&self) -> Result<(), MuxError> {
        if (self.video.is_none() && self.audio.is_none())
            || !(1..=MAX_SAMPLE_BYTES).contains(&self.max_sample_bytes)
            || self.max_sample_duration.is_zero()
            || self.max_sample_duration > Duration::from_secs(1)
            || !(Duration::from_millis(1)..=Duration::from_millis(100)).contains(&self.pcr_interval)
            || !(Duration::from_millis(1)..=Duration::from_millis(500))
                .contains(&self.tables_interval)
            || self.max_clock_gap < self.pcr_interval
            || self.max_clock_gap > Duration::from_secs(2)
        {
            return Err(MuxError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxError {
    InvalidConfig,
    Busy,
    SampleTooLarge,
    SampleTooLong,
    TooManyNals,
    TrackNotConfigured,
    ConfigurationChanged,
    TimestampRegression,
    ClockGap,
    NeedsKeyframe,
    InvalidAvcc,
    AdtsLength,
    Writer,
}
impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MuxError {}
impl From<mpeg2ts::Error> for MuxError {
    fn from(_: mpeg2ts::Error) -> Self {
        Self::Writer
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MuxStats {
    pub accepted_samples: u64,
    pub accepted_sample_bytes: u64,
    pub emitted_packets: u64,
    pub emitted_bytes: u64,
    pub output_allocations: u64,
    /// Boundary-crossing bytes copied into the small elementary scratch.
    pub elementary_staging_copy_bytes: u64,
    /// Elementary bytes copied into mpeg2ts::payload::Bytes, directly or via scratch.
    pub packet_payload_copy_bytes: u64,
    /// The same elementary bytes copied by the dependency into its TS payload scratch.
    /// Its final serialisation copy is counted separately in emitted_bytes.
    pub writer_payload_copy_bytes: u64,
    pub buffered_sample_bytes: usize,
    pub buffered_media_duration_ns: u64,
    pub peak_sample_bytes: usize,
    pub piece_metadata_bytes: usize,
    pub resets: u64,
    pub discarded_sample_bytes: u64,
}

#[derive(Debug)]
enum Piece {
    Literal(&'static [u8]),
    Header([u8; 7]),
    Sample { start: usize, len: usize },
    Sps,
    Pps,
}
struct Elementary {
    sample: SharedSample,
    pieces: VecDeque<Piece>,
    offset: usize,
    remaining: usize,
}
impl Elementary {
    fn new(sample: SharedSample) -> Result<Self, MuxError> {
        let mut pieces = VecDeque::new();
        let mut total = 0;
        match sample.config().as_ref() {
            CodecConfig::Aac(config) => {
                let header = adts_header(*config, sample.payload().len())?;
                total = 7 + sample.payload().len();
                pieces.push_back(Piece::Header(header));
                pieces.push_back(Piece::Sample {
                    start: 0,
                    len: sample.payload().len(),
                });
            }
            CodecConfig::H264(config) => {
                pieces.push_back(Piece::Literal(&[0, 0, 0, 1, 9, 0xf0]));
                total += 6;
                if sample.is_keyframe() {
                    pieces.push_back(Piece::Literal(&START));
                    pieces.push_back(Piece::Sps);
                    pieces.push_back(Piece::Literal(&START));
                    pieces.push_back(Piece::Pps);
                    total += 8 + config.sps().len() + config.pps().len();
                }
                let payload = sample.payload();
                let mut offset = 0;
                let mut nals = 0;
                while offset < payload.len() {
                    nals += 1;
                    if nals > MAX_NALS {
                        return Err(MuxError::TooManyNals);
                    }
                    let prefix: [u8; 4] = payload
                        .get(offset..offset + 4)
                        .ok_or(MuxError::InvalidAvcc)?
                        .try_into()
                        .map_err(|_| MuxError::InvalidAvcc)?;
                    let len = u32::from_be_bytes(prefix) as usize;
                    offset += 4;
                    let nal = payload
                        .get(offset..offset.checked_add(len).ok_or(MuxError::InvalidAvcc)?)
                        .filter(|b| !b.is_empty())
                        .ok_or(MuxError::InvalidAvcc)?;
                    // Exactly one AUD, before parameter sets, per access unit.
                    if nal.first().is_some_and(|b| b & 0x1f != 9) {
                        pieces.push_back(Piece::Literal(&START));
                        pieces.push_back(Piece::Sample { start: offset, len });
                        total += 4 + len;
                    }
                    offset += len;
                }
            }
        }
        Ok(Self {
            sample,
            pieces,
            offset: 0,
            remaining: total,
        })
    }
    fn front(&self) -> Result<Option<&[u8]>, MuxError> {
        let Some(piece) = self.pieces.front() else {
            return Ok(None);
        };
        let bytes: &[u8] = match piece {
            Piece::Literal(bytes) => bytes,
            Piece::Header(bytes) => bytes,
            Piece::Sample { start, len } => self
                .sample
                .payload()
                .get(*start..start + len)
                .ok_or(MuxError::InvalidAvcc)?,
            Piece::Sps => match self.sample.config().as_ref() {
                CodecConfig::H264(c) => c.sps(),
                _ => return Err(MuxError::InvalidAvcc),
            },
            Piece::Pps => match self.sample.config().as_ref() {
                CodecConfig::H264(c) => c.pps(),
                _ => return Err(MuxError::InvalidAvcc),
            },
        };
        Ok(Some(bytes.get(self.offset..).ok_or(MuxError::InvalidAvcc)?))
    }

    fn advance(&mut self, count: usize) -> Result<(), MuxError> {
        let rest_len = self.front()?.ok_or(MuxError::InvalidAvcc)?.len();
        if count > rest_len || count > self.remaining {
            return Err(MuxError::InvalidAvcc);
        }
        let piece_len = self
            .offset
            .checked_add(rest_len)
            .ok_or(MuxError::InvalidAvcc)?;
        self.offset += count;
        self.remaining -= count;
        if self.offset == piece_len {
            self.pieces.pop_front();
            self.offset = 0;
        }
        Ok(())
    }

    fn copy(&mut self, out: &mut [u8]) -> Result<usize, MuxError> {
        let mut written = 0;
        while written < out.len() {
            let Some(rest) = self.front()? else {
                break;
            };
            let count = rest.len().min(out.len() - written);
            out.get_mut(written..written + count)
                .ok_or(MuxError::InvalidAvcc)?
                .copy_from_slice(rest.get(..count).ok_or(MuxError::InvalidAvcc)?);
            self.advance(count)?;
            written += count;
        }
        Ok(written)
    }

    fn packet_bytes(&mut self, max: usize) -> Result<(Bytes, bool), MuxError> {
        let max = max.min(self.remaining);
        if let Some(rest) = self.front()?
            && rest.len() >= max
        {
            let data = Bytes::new(rest.get(..max).ok_or(MuxError::InvalidAvcc)?)?;
            self.advance(max)?;
            return Ok((data, false));
        }
        let mut scratch = [0u8; 184];
        let out = scratch.get_mut(..max).ok_or(MuxError::Writer)?;
        let len = self.copy(out)?;
        let data = Bytes::new(out.get(..len).ok_or(MuxError::Writer)?)?;
        Ok((data, true))
    }
}

struct Pending {
    elementary: Elementary,
    first: bool,
    clock_target: Option<u64>,
    force_tables: u8,
    pts: u64,
    dts: u64,
    total: usize,
}

pub struct MpegTsMuxer {
    config: MpegTsConfig,
    pending: Option<Pending>,
    counters: [ContinuityCounter; 4], // PAT, PMT, video, audio: next payload CC
    payload_seen: [bool; 4],
    discontinuity: [bool; 4],
    version: VersionNumber,
    video_end: Option<u64>,
    audio_end: Option<u64>,
    clock_last: Option<u64>,
    next_pcr: Option<u64>,
    next_tables: Option<u64>,
    initial_tables: bool,
    periodic_table_stage: u8,
    stats: MuxStats,
}
impl MpegTsMuxer {
    pub fn new(config: MpegTsConfig) -> Result<Self, MuxError> {
        config.validate()?;
        Ok(Self {
            config,
            pending: None,
            counters: [ContinuityCounter::new(); 4],
            payload_seen: [false; 4],
            discontinuity: [true; 4],
            version: VersionNumber::new(),
            video_end: None,
            audio_end: None,
            clock_last: None,
            next_pcr: None,
            next_tables: None,
            initial_tables: true,
            periodic_table_stage: 0,
            stats: MuxStats::default(),
        })
    }
    pub fn stats(&self) -> MuxStats {
        self.stats
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_none()
    }

    /// Refuses Busy without changing state. Caller retains its Arc if it
    /// needs to retry. No codec configuration is adopted from sample input.
    pub fn push(&mut self, sample: SharedSample) -> Result<(), MuxError> {
        if self.pending.is_some() {
            return Err(MuxError::Busy);
        }
        if sample.payload().len() > self.config.max_sample_bytes {
            return Err(MuxError::SampleTooLarge);
        }
        let timing = sample.timing();
        let start = timing.decode_time_ns();
        let end = timing
            .decode_end_ns()
            .map_err(|_| MuxError::TimestampRegression)?;
        if end - start > self.config.max_sample_duration.as_nanos() as u64 {
            return Err(MuxError::SampleTooLong);
        }
        // A keyframe after a gap longer than the clock allows (a publishing
        // queue that dropped its backlog) starts a discontinuity instead of
        // failing the connection; anything else after such a gap still fails.
        if sample.is_keyframe()
            && (sample.is_video() || self.config.video.is_none())
            && self.clock_last.is_some_and(|last| {
                rescale(timing.dts(), timing.timescale()).saturating_sub(last)
                    > duration_ticks(self.config.max_clock_gap)
            })
        {
            self.reset(self.config.clone())?;
        }
        let video = sample.is_video();
        let previous = if video {
            self.video_end
        } else {
            self.audio_end
        };
        if previous.is_some_and(|previous| start < previous) {
            return Err(MuxError::TimestampRegression);
        }
        match sample.config().as_ref() {
            CodecConfig::H264(c) => {
                let ours = self
                    .config
                    .video
                    .as_ref()
                    .ok_or(MuxError::TrackNotConfigured)?;
                if ours.sps() != c.sps() || ours.pps() != c.pps() {
                    return Err(MuxError::ConfigurationChanged);
                }
                if previous.is_none() && !sample.is_keyframe() {
                    return Err(MuxError::NeedsKeyframe);
                }
            }
            CodecConfig::Aac(c) => {
                let ours = self.config.audio.ok_or(MuxError::TrackNotConfigured)?;
                if ours != *c {
                    return Err(MuxError::ConfigurationChanged);
                }
            }
        }
        let dts = rescale(timing.dts(), timing.timescale());
        let pts = rescale(timing.pts(), timing.timescale());
        let clock = video || self.config.video.is_none();
        if clock
            && self.clock_last.is_some_and(|last| {
                dts.saturating_sub(last) > duration_ticks(self.config.max_clock_gap)
            })
        {
            return Err(MuxError::ClockGap);
        }
        let force_tables = if self.initial_tables || sample.is_keyframe() {
            2
        } else {
            0
        };
        let bytes = sample.payload().len();
        let elementary = Elementary::new(sample)?;
        let total = elementary.remaining;
        self.stats.piece_metadata_bytes =
            elementary.pieces.capacity() * std::mem::size_of::<Piece>();
        self.stats.accepted_samples += 1;
        self.stats.accepted_sample_bytes += bytes as u64;
        self.stats.buffered_sample_bytes = bytes;
        self.stats.buffered_media_duration_ns = end - start;
        self.stats.peak_sample_bytes = self.stats.peak_sample_bytes.max(bytes);
        if video {
            self.video_end = Some(end);
        } else {
            self.audio_end = Some(end);
        }
        if clock {
            self.clock_last = Some(dts);
            if self.next_pcr.is_none() {
                self.next_pcr = Some(dts);
            }
            if self.next_tables.is_none() {
                self.next_tables = Some(dts + duration_ticks(self.config.tables_interval));
            }
        }
        self.initial_tables = false;
        self.pending = Some(Pending {
            elementary,
            first: true,
            clock_target: clock.then_some(dts),
            force_tables,
            pts,
            dts,
            total,
        });
        Ok(())
    }

    /// Returns one owned <=1316-byte chunk, always whole TS packets. The
    /// final partial chunk is returned immediately, so sparse audio cannot
    /// wait indefinitely for another sample. Ownership transfers to caller.
    pub fn poll_chunk(&mut self) -> Result<Option<Vec<u8>>, MuxError> {
        if self.pending.is_none() {
            return Ok(None);
        }
        let mut output = Vec::with_capacity(TS_CHUNK_BYTES);
        self.stats.output_allocations += 1;
        for _ in 0..7 {
            let Some(packet) = self.next_packet()? else {
                break;
            };
            let pcr = packet
                .adaptation_field
                .as_ref()
                .is_some_and(|a| a.pcr.is_some());
            let at = output.len();
            TsPacketWriter::new(&mut output).write_ts_packet(&packet)?;
            if output.len() != at + TS_PACKET_BYTES {
                return Err(MuxError::Writer);
            }
            if pcr {
                fix_pcr_reserved_bits(output.get_mut(at..).ok_or(MuxError::Writer)?)?;
            }
            self.stats.emitted_packets += 1;
            self.stats.emitted_bytes += TS_PACKET_BYTES as u64;
        }
        if output.is_empty() {
            Ok(None)
        } else {
            Ok(Some(output))
        }
    }
    /// Same bounded drain operation as poll_chunk; repeat until None.
    pub fn flush(&mut self) -> Result<Option<Vec<u8>>, MuxError> {
        self.poll_chunk()
    }

    /// Explicit discontinuity: discards an unfinished sample, releases its
    /// Arc, resets track validation/PCR and increments the PSI version.
    /// The adapter must also segment/discontinue any independent recording.
    pub fn reset(&mut self, config: MpegTsConfig) -> Result<(), MuxError> {
        let mut replacement = Self::new(config)?;
        replacement.version = self.version;
        replacement.version.increment();
        replacement.counters = self.counters;
        replacement.payload_seen = self.payload_seen;
        replacement.stats = self.stats;
        replacement.stats.resets += 1;
        replacement.stats.discarded_sample_bytes += self.stats.buffered_sample_bytes as u64;
        replacement.stats.buffered_sample_bytes = 0;
        replacement.stats.buffered_media_duration_ns = 0;
        replacement.stats.piece_metadata_bytes = 0;
        *self = replacement;
        Ok(())
    }

    fn next_packet(&mut self) -> Result<Option<TsPacket>, MuxError> {
        let Some(p) = self.pending.as_mut() else {
            return Ok(None);
        };
        if p.force_tables > 0 {
            let pat = p.force_tables == 2;
            p.force_tables -= 1;
            return Ok(Some(self.table(pat)?));
        }
        if self.periodic_table_stage > 0 {
            self.periodic_table_stage = 0;
            return Ok(Some(self.table(false)?));
        }
        if let Some(target) = p.clock_target {
            let table = self.next_tables.filter(|t| *t <= target);
            let pcr = self.next_pcr.filter(|t| *t <= target);
            if table.is_some_and(|t| pcr.is_none_or(|pcr| t <= pcr)) {
                self.next_tables = self
                    .next_tables
                    .map(|t| t + duration_ticks(self.config.tables_interval));
                self.periodic_table_stage = 1;
                return Ok(Some(self.table(true)?));
            }
            if let Some(pcr) = pcr {
                self.next_pcr = Some(pcr + duration_ticks(self.config.pcr_interval));
                let track = if self.config.video.is_some() { 2 } else { 3 };
                let discontinuity = self.discontinuity.get(track).copied().unwrap_or(false);
                return Ok(Some(TsPacket {
                    header: self.header(track, false)?,
                    adaptation_field: Some(adaptation(
                        discontinuity,
                        false,
                        Some(ClockReference::new((pcr & Timestamp::MAX) * 300)?),
                    )),
                    payload: None,
                }));
            }
        }
        let p = self.pending.as_mut().ok_or(MuxError::Writer)?;
        let track = if p.elementary.sample.is_video() { 2 } else { 3 };
        let first = p.first;
        let random = first && p.elementary.sample.is_keyframe();
        let discontinuity = first && self.discontinuity.get(track).copied().unwrap_or(false);
        let adp = (random || discontinuity).then(|| adaptation(discontinuity, random, None));
        let dts = (p.pts != p.dts).then_some(p.dts);
        let pes_header_len = if dts.is_some() { 19 } else { 14 };
        let max =
            184 - (if adp.is_some() { 2 } else { 0 }) - (if first { pes_header_len } else { 0 });
        let (data, staged) = p.elementary.packet_bytes(max)?;
        let len = data.as_ref().len() as u64;
        if staged {
            self.stats.elementary_staging_copy_bytes += len;
        }
        self.stats.packet_payload_copy_bytes += len;
        self.stats.writer_payload_copy_bytes += len;
        let payload = if first {
            TsPayload::PesStart(Pes {
                header: PesHeader {
                    stream_id: StreamId::new(if track == 2 { 0xe0 } else { 0xc0 }),
                    priority: false,
                    data_alignment_indicator: true,
                    copyright: false,
                    original_or_copy: true,
                    pts: Some(Timestamp::new(p.pts & Timestamp::MAX)?),
                    dts: dts
                        .map(|t| Timestamp::new(t & Timestamp::MAX))
                        .transpose()?,
                    escr: None,
                },
                pes_packet_len: if track == 2 {
                    0
                } else {
                    u16::try_from(p.total + 8).map_err(|_| MuxError::AdtsLength)?
                },
                data,
            })
        } else {
            TsPayload::PesContinuation(data)
        };
        p.first = false;
        let finished = p.elementary.remaining == 0;
        let header = self.header(track, true)?;
        if let Some(discontinuity) = self.discontinuity.get_mut(track) {
            *discontinuity = false;
        }
        if finished {
            self.pending = None;
            self.stats.buffered_sample_bytes = 0;
            self.stats.buffered_media_duration_ns = 0;
            self.stats.piece_metadata_bytes = 0;
        }
        Ok(Some(TsPacket {
            header,
            adaptation_field: adp,
            payload: Some(payload),
        }))
    }
    fn header(&mut self, track: usize, payload: bool) -> Result<TsHeader, MuxError> {
        let pid = match track {
            0 => 0,
            1 => PMT_PID,
            2 => VIDEO_PID,
            3 => AUDIO_PID,
            _ => return Err(MuxError::Writer),
        };
        let next = self.counters.get_mut(track).ok_or(MuxError::Writer)?;
        let seen = self.payload_seen.get_mut(track).ok_or(MuxError::Writer)?;
        let cc = if payload {
            let cc = *next;
            next.increment();
            *seen = true;
            cc
        } else if *seen {
            ContinuityCounter::from_u8(next.as_u8().wrapping_sub(1) & 15)?
        } else {
            *next
        };
        Ok(TsHeader {
            transport_error_indicator: false,
            transport_priority: false,
            pid: Pid::new(pid)?,
            transport_scrambling_control: TransportScramblingControl::NotScrambled,
            continuity_counter: cc,
        })
    }
    fn table(&mut self, pat: bool) -> Result<TsPacket, MuxError> {
        let payload = if pat {
            TsPayload::Pat(Pat {
                transport_stream_id: 1,
                version_number: self.version,
                table: vec![ProgramAssociation {
                    program_num: 1,
                    program_map_pid: Pid::new(PMT_PID)?,
                }],
            })
        } else {
            let mut es_info = Vec::with_capacity(2);
            if self.config.video.is_some() {
                es_info.push(EsInfo {
                    stream_type: StreamType::H264,
                    elementary_pid: Pid::new(VIDEO_PID)?,
                    descriptors: vec![],
                });
            }
            if self.config.audio.is_some() {
                es_info.push(EsInfo {
                    stream_type: StreamType::AdtsAac,
                    elementary_pid: Pid::new(AUDIO_PID)?,
                    descriptors: vec![],
                });
            }
            TsPayload::Pmt(Pmt {
                program_num: 1,
                pcr_pid: Some(Pid::new(if self.config.video.is_some() {
                    VIDEO_PID
                } else {
                    AUDIO_PID
                })?),
                version_number: self.version,
                program_info: vec![],
                es_info,
            })
        };
        Ok(TsPacket {
            header: self.header(if pat { 0 } else { 1 }, true)?,
            adaptation_field: None,
            payload: Some(payload),
        })
    }
}

fn rescale(ticks: i64, timescale: u32) -> u64 {
    (ticks as u128 * 90_000 / u128::from(timescale)) as u64
}
fn duration_ticks(duration: Duration) -> u64 {
    (duration.as_nanos() * 90_000 / 1_000_000_000) as u64
}
fn adaptation(discontinuity: bool, random: bool, pcr: Option<ClockReference>) -> AdaptationField {
    AdaptationField {
        discontinuity_indicator: discontinuity,
        random_access_indicator: random,
        es_priority_indicator: false,
        pcr,
        opcr: None,
        splice_countdown: None,
        transport_private_data: vec![],
        extension: None,
    }
}
fn adts_header(config: AacConfig, payload: usize) -> Result<[u8; 7], MuxError> {
    let len = payload
        .checked_add(7)
        .filter(|len| *len <= 8191)
        .ok_or(MuxError::AdtsLength)?;
    let [a, b] = config.audio_specific_config();
    let rate = ((a & 7) << 1) | (b >> 7);
    let channels = config.channels();
    Ok([
        0xff,
        0xf1,
        0x40 | (rate << 2) | (channels >> 2),
        ((channels & 3) << 6) | ((len >> 11) as u8),
        (len >> 3) as u8,
        ((len & 7) as u8) << 5 | 0x1f,
        0xfc,
    ])
}

/// mpeg2ts 0.6.1 time::ClockReference::write_pcr_to leaves the six reserved
/// PCR bits zero. H.222.0 table 2-6 requires the reserved pattern. Correct
/// only that byte after checking our dedicated adaptation-only packet shape;
/// the dependency still owns all timestamp/TS encoding and remains unchanged.
fn fix_pcr_reserved_bits(packet: &mut [u8]) -> Result<(), MuxError> {
    if packet.len() != 188
        || packet.first() != Some(&0x47)
        || packet.get(3).is_none_or(|b| b & 0x30 != 0x20)
        || packet.get(4) != Some(&183)
        || packet.get(5).is_none_or(|b| !matches!(*b, 0x10 | 0x90))
    {
        return Err(MuxError::Writer);
    }
    *packet.get_mut(10).ok_or(MuxError::Writer)? |= 0x7e;
    Ok(())
}

#[cfg(test)]
#[path = "mpegts_tests.rs"]
mod tests;
