//! MPEG-TS in, muxer out.
//!
//! TS and PES parsing come from `mpeg2ts`. Its reader pulls from a
//! `std::io::Read` and treats some stream errors as fatal, so it is fed one
//! 188-byte packet at a time from a buffer we own: a bad packet is dropped
//! and the next one parsed, and running out of input is reported as an error
//! that leaves the reader's state intact for the next push.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bytes::Bytes;
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{Pid, ReadTsPacket, TsPacket, TsPacketReader, TsPayload};

use crate::codec::{self, AAC_FRAME_SAMPLES, AacConfig, H264Config};
use crate::muxer::{MuxError, Muxer, MuxerConfig, Update};

use crate::TrackSet;
use crate::time::{Unwrapper, signed_delta};

const TS_SIZE: usize = 188;
const SYNC: u8 = 0x47;
/// Cap on unconsumed input (a few seconds at stage bitrates).
const MAX_BUFFER: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    /// The program carries a video codec other than H.264.
    UnsupportedVideo,
    /// The held presentation expired before new media recovered it.
    PresentationExpired,
    /// No keyframe for too long, or a segment beyond the size limit.
    SegmentTooLarge,
    InvalidTracks,
    TrackConflict,
    InvalidAudio,
    InvalidTimestamp,
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PresentationExpired => f.write_str("presentation reconnect deadline expired"),
            Self::UnsupportedVideo => f.write_str("video is not H.264"),
            Self::SegmentTooLarge => f.write_str("segment too large (keyframe interval?)"),
            Self::InvalidTracks => f.write_str("invalid track set"),
            Self::TrackConflict => f.write_str("track set changed within a connection"),
            Self::InvalidAudio => f.write_str("unsupported or changed AAC configuration"),
            Self::InvalidTimestamp => f.write_str("audio timestamp outside supported range"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<MuxError> for IngestError {
    fn from(value: MuxError) -> Self {
        match value {
            MuxError::SegmentTooLarge => Self::SegmentTooLarge,
            MuxError::InvalidTracks => Self::InvalidTracks,
            MuxError::InvalidAudio => Self::InvalidAudio,
            MuxError::Timestamp => Self::InvalidTimestamp,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Program {
    seen: bool,
    h264: bool,
    other_video: bool,
    aac: bool,
}

#[derive(Default)]
struct Slot {
    buf: Vec<u8>,
    pos: usize,
}

#[derive(Clone, Default)]
struct SlotReader(Arc<Mutex<Slot>>);

impl Read for SlotReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut slot = self.0.lock().map_err(|_| io::ErrorKind::Other)?;
        let n = out.len().min(slot.buf.len() - slot.pos);
        let start = slot.pos;
        out[..n].copy_from_slice(&slot.buf[start..start + n]);
        slot.pos += n;
        Ok(n)
    }
}

#[derive(Default)]
struct Input {
    buf: Vec<u8>,
    start: usize,
    starved: bool,
    program: Program,
    dropped: u64,
    reset_pes: bool,
}

struct Feed {
    input: Arc<Mutex<Input>>,
    slot: SlotReader,
    inner: TsPacketReader<SlotReader>,
    continuity: HashMap<Pid, (u8, [u8; TS_SIZE])>,
    pending: Option<Box<TsPacket>>,
}

impl ReadTsPacket for Feed {
    fn read_ts_packet(&mut self) -> mpeg2ts::Result<Option<TsPacket>> {
        if let Some(packet) = self.pending.take() {
            return Ok(Some(*packet));
        }
        loop {
            let packet = {
                let mut input = self.input.lock().map_err(|_| starved())?;
                let Input { buf, start, .. } = &mut *input;
                // Resynchronise on the sync byte if the stream slipped.
                match buf[*start..].iter().position(|&b| b == SYNC) {
                    Some(skip) => *start += skip,
                    None => *start = buf.len(),
                }
                if buf.len() - *start < TS_SIZE {
                    input.starved = true;
                    return Err(starved());
                }
                let mut p = [0; TS_SIZE];
                p.copy_from_slice(&input.buf[input.start..input.start + TS_SIZE]);
                input.start += TS_SIZE;
                p
            };
            {
                let mut slot = self.slot.0.lock().map_err(|_| starved())?;
                slot.buf.clear();
                slot.buf.extend_from_slice(&packet);
                slot.pos = 0;
            }
            match self.inner.read_ts_packet() {
                Ok(Some(p)) => {
                    let pid = p.header.pid;
                    let carries_pes = matches!(
                        p.payload,
                        Some(TsPayload::PesStart(_) | TsPayload::PesContinuation(_))
                    );
                    let discontinuity = p
                        .adaptation_field
                        .as_ref()
                        .is_some_and(|field| field.discontinuity_indicator);
                    let previous = self.continuity.get(&pid);
                    let counter = p.header.continuity_counter.as_u8();
                    let gap = carries_pes
                        && previous.is_some_and(|(last, _)| counter != ((last + 1) & 0x0f));
                    // An identical retransmission must not append its payload
                    // twice, including at the start of an unbounded video PES.
                    if carries_pes
                        && !discontinuity
                        && !p.header.transport_error_indicator
                        && previous.is_some_and(|(_, bytes)| *bytes == packet)
                    {
                        continue;
                    }
                    let reset = p.header.transport_error_indicator
                        || gap
                        || (discontinuity && previous.is_some());
                    if carries_pes || (discontinuity && previous.is_some()) {
                        self.continuity.insert(pid, (counter, packet));
                    }
                    if reset {
                        // mpeg2ts does not check continuity and cannot validate
                        // the length of an unbounded video PES. Reset its PES
                        // assembly before a truncated frame can be returned.
                        // Keep its TS/PMT reader and the first clean packet.
                        self.pending = (!p.header.transport_error_indicator).then(|| Box::new(p));
                        if let Ok(mut input) = self.input.lock() {
                            input.reset_pes = true;
                            input.dropped += 1;
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "transport continuity lost",
                        )
                        .into());
                    }
                    if let Some(TsPayload::Pmt(pmt)) = &p.payload
                        && let Ok(mut input) = self.input.lock()
                    {
                        let mut program = Program {
                            seen: true,
                            ..Program::default()
                        };
                        for es in &pmt.es_info {
                            match es.stream_type {
                                StreamType::H264 => program.h264 = true,
                                StreamType::AdtsAac => program.aac = true,
                                StreamType::H265
                                | StreamType::Mpeg1Video
                                | StreamType::Mpeg2Video
                                | StreamType::Mpeg4H263BasedVideo => program.other_video = true,
                                _ => {}
                            }
                        }
                        input.program = program;
                    }
                    return Ok(Some(p));
                }
                Ok(None) => {}
                Err(_) => {
                    if let Ok(mut input) = self.input.lock() {
                        input.dropped += 1;
                        input.reset_pes = true;
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed transport packet",
                    )
                    .into());
                }
            }
        }
    }
}

fn starved() -> mpeg2ts::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "need more input").into()
}

/// Elementary streams in, muxer out: shared by the TS (SRT) and FLV (RTMP)
/// front ends. Timestamps are 90 kHz on one monotonic timeline.
pub struct EsIngest {
    cfg: MuxerConfig,
    origin: Option<i64>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    aac: Option<AacConfig>,
    /// Whether to hold the first keyframe until the audio config is known.
    expect_audio: bool,
    declared: Option<TrackSet>,
    muxer: Option<Muxer>,
}

impl EsIngest {
    pub fn new(cfg: MuxerConfig) -> Self {
        Self {
            cfg,
            origin: None,
            sps: None,
            pps: None,
            aac: None,
            expect_audio: true,
            declared: None,
            muxer: None,
        }
    }

    /// Carries on another connection's stream (see [`Muxer::interrupt`]).
    /// The first keyframe decides: with the same parameter sets and audio
    /// config, its media continues the stream after a discontinuity;
    /// otherwise it starts a new initialization epoch on the retained muxer
    /// and the update says `restarted` (for recording rollover).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        Self {
            muxer: Some(muxer),
            ..Self::new(cfg)
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        self.muxer.as_ref()
    }

    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.muxer.as_mut()
    }

    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.origin = None;
        self.muxer.take()
    }

    /// Only an authoritative PMT or explicit RTMP declaration may select
    /// audio-only. Missing video metadata and early AAC do not imply radio.
    pub fn declare_tracks(
        &mut self,
        tracks: TrackSet,
        _now: SystemTime,
    ) -> Result<Update, IngestError> {
        if self.declared.is_some_and(|old| old != tracks)
            || (self.origin.is_some() && self.muxer.as_ref().is_some_and(|m| m.tracks() != tracks))
        {
            return Err(IngestError::TrackConflict);
        }
        self.declared = Some(tracks);
        self.expect_audio = tracks.has_audio();
        Ok(Update::default())
    }

    pub fn declared_tracks(&self) -> Option<TrackSet> {
        self.declared
    }

    pub fn set_expect_audio(&mut self, expect: bool) {
        if self.declared.is_none() {
            self.expect_audio = expect;
        }
    }

    /// Parameter sets from out of band (RTMP's sequence header) or in band.
    pub fn parameter_sets(&mut self, sps: Option<Vec<u8>>, pps: Option<Vec<u8>>) {
        if let Some(sps) = sps {
            if let Some(m) = &self.muxer
                && self.origin.is_some()
                && m.video().is_some_and(|video| video.sps != sps)
                && self.sps.as_ref() != Some(&sps)
            {
                // The init segment is fixed at the first keyframe.
                tracing::warn!(
                    "encoder changed its SPS mid-stream (resolution or profile?); players keep the first one"
                );
            }
            self.sps = Some(sps);
        }
        if let Some(pps) = pps {
            self.pps = Some(pps);
        }
    }

    pub fn known_audio(&self) -> Option<AacConfig> {
        self.aac
    }

    pub fn audio_config(&mut self, config: AacConfig) {
        if self.aac.is_none() {
            if let Some(muxer) = &self.muxer
                && muxer.audio().is_none()
            {
                if muxer.has_content() {
                    // A served init segment and the recorder's track identity
                    // cannot acquire a new track in place.
                    tracing::warn!(
                        "AAC configuration arrived after video-only content became servable; audio cannot be added to this init segment"
                    );
                } else {
                    // Separate hardware encoders may deliver AVC + first IDR
                    // before AAC's sequence header. No content was published,
                    // so discard the provisional video-only muxer and start
                    // with both tracks at the next IDR.
                    self.muxer = None;
                    self.origin = None;
                }
            }
            self.aac = Some(config);
        }
    }

    /// One access unit, already length-prefixed (4 bytes).
    pub fn video(
        &mut self,
        dts: i64,
        pts: i64,
        au: codec::AccessUnit,
        now: SystemTime,
    ) -> Result<Update, IngestError> {
        if self.declared == Some(TrackSet::AudioOnly) {
            return Err(IngestError::TrackConflict);
        }
        self.parameter_sets(au.sps, au.pps);
        if au.data.is_empty() {
            return Ok(Update::default());
        }
        let mut restarted = false;
        if self.origin.is_none() {
            if !au.idr {
                return Ok(Update::default());
            }
            let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else {
                return Ok(Update::default());
            };
            let Some(video) = H264Config::new(sps, pps) else {
                return Ok(Update::default());
            };
            // If audio is expected, wait for its config before the init
            // segment is fixed (or compared); the next keyframe will do.
            if self.expect_audio && self.aac.is_none() {
                return Ok(Update::default());
            }
            let audio = if self.expect_audio { self.aac } else { None };
            if let Some(m) = &mut self.muxer
                && (m.video() != Some(&video) || m.audio().copied() != audio)
            {
                tracing::warn!(
                    "reconnected encoder changed configuration; starting a new codec epoch on the existing timeline"
                );
                m.reconfigure(video.clone(), audio, now);
                restarted = true;
            }
            if self.muxer.is_none() {
                self.muxer = Some(Muxer::new(self.cfg, video, audio));
            }
            self.origin = Some(dts);
        }
        let (Some(muxer), Some(origin)) = (self.muxer.as_mut(), self.origin) else {
            return Ok(Update::default());
        };
        let mut update = muxer
            .push_video(
                dts - origin,
                pts - origin,
                au.idr,
                Bytes::from(au.data),
                now,
            )
            .map_err(IngestError::from)?;
        update.restarted |= restarted;
        Ok(update)
    }

    /// One raw AAC frame plus caller-owned wall time. The returned update is
    /// essential for waking audio-only playlists and beginning origin recording.
    pub fn audio_at(
        &mut self,
        pts: i64,
        config: AacConfig,
        payload: &[u8],
        now: SystemTime,
    ) -> Result<Update, IngestError> {
        if self.declared == Some(TrackSet::VideoOnly) {
            return Err(IngestError::TrackConflict);
        }
        if (self.origin.is_some() || self.declared == Some(TrackSet::AudioOnly))
            && self.aac.is_some_and(|old| old != config)
        {
            return Err(IngestError::InvalidAudio);
        }
        self.audio_config(config);
        if payload.is_empty() {
            return Ok(Update::default());
        }
        if self.declared != Some(TrackSet::AudioOnly) {
            self.audio(pts, config, payload);
            return Ok(Update::default());
        }
        let mut update = Update::default();
        if self.origin.is_none() {
            if let Some(muxer) = &mut self.muxer {
                if muxer.tracks() != TrackSet::AudioOnly || muxer.audio() != Some(&config) {
                    update.merge(muxer.reconfigure_tracks(None, Some(config), now)?);
                    update.restarted = true;
                }
            } else {
                self.muxer = Some(Muxer::audio_only(self.cfg, config)?);
            }
            self.origin = Some(pts);
        }
        let origin = self.origin.ok_or(IngestError::InvalidTimestamp)?;
        let relative = pts
            .checked_sub(origin)
            .ok_or(IngestError::InvalidTimestamp)?;
        update.merge(
            self.muxer
                .as_mut()
                .ok_or(IngestError::InvalidTracks)?
                .push_audio_at(relative, Bytes::copy_from_slice(payload), now)?,
        );
        Ok(update)
    }

    /// Legacy video-led AAC path. Audio-only consumers need `audio_at` and
    /// must forward its updates to waiters/recorders.
    pub fn audio(&mut self, pts: i64, config: AacConfig, payload: &[u8]) {
        if self.declared == Some(TrackSet::AudioOnly) {
            return;
        }
        self.audio_config(config);
        let (Some(muxer), Some(origin)) = (self.muxer.as_mut(), self.origin) else {
            return;
        };
        let t = pts - origin;
        if t >= 0 {
            muxer.push_audio(t, Bytes::copy_from_slice(payload));
        }
    }
}

/// One publisher's stream: TS bytes in, LL-HLS muxer state out.
pub struct TsIngest {
    input: Arc<Mutex<Input>>,
    pes: Option<PesPacketReader<Feed>>,
    clock: Unwrapper,
    es: EsIngest,
    pending: Update,
    waiting_keyframe: bool,
}

impl TsIngest {
    /// Carries on another connection's stream ([`EsIngest::resume`]).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        let mut ts = Self::new(cfg);
        ts.es = EsIngest::resume(cfg, muxer);
        ts
    }

    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.es.muxer_mut()
    }

    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.es.take_muxer()
    }

    pub fn new(cfg: MuxerConfig) -> Self {
        let input = Arc::new(Mutex::new(Input::default()));
        let slot = SlotReader::default();
        let feed = Feed {
            input: input.clone(),
            slot: slot.clone(),
            inner: TsPacketReader::new(slot),
            continuity: HashMap::new(),
            pending: None,
        };
        Self {
            input,
            pes: Some(PesPacketReader::new(feed)),
            clock: Unwrapper::default(),
            es: EsIngest::new(cfg),
            pending: Update::default(),
            waiting_keyframe: false,
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        self.es.muxer()
    }

    /// Invalid transport/PES packets and continuity losses, for diagnostics.
    pub fn dropped_packets(&self) -> u64 {
        self.input.lock().map(|i| i.dropped).unwrap_or(0)
    }

    /// Feeds raw TS bytes (any alignment). `now` stamps new segments.
    pub fn push(&mut self, data: &[u8], now: SystemTime) -> Result<Update, IngestError> {
        {
            let Ok(mut input) = self.input.lock() else {
                return Ok(Update::default());
            };
            if input.start > 0 {
                let start = input.start;
                input.buf.drain(..start);
                input.start = 0;
            }
            if input.buf.len() + data.len() > MAX_BUFFER {
                input.buf.clear();
            }
            input.buf.extend_from_slice(data);
            input.starved = false;
        }
        self.pending = Update::default();
        loop {
            match self
                .pes
                .as_mut()
                .expect("PES reader restored after reset")
                .read_pes_packet()
            {
                Ok(Some(pes)) => self.on_pes(pes, now)?,
                Ok(None) => break,
                Err(_) => {
                    let reset = self
                        .input
                        .lock()
                        .map(|mut input| std::mem::take(&mut input.reset_pes))
                        .unwrap_or(false);
                    if reset {
                        let feed = self
                            .pes
                            .take()
                            .expect("PES reader present")
                            .into_ts_packet_reader();
                        self.pes = Some(PesPacketReader::new(feed));
                        self.waiting_keyframe = true;
                        continue;
                    }
                    let starved = self.input.lock().map(|i| i.starved).unwrap_or(true);
                    if starved {
                        break;
                    }
                    // A malformed PES: its data is gone, the next one is fine.
                    if let Ok(mut input) = self.input.lock() {
                        input.dropped += 1;
                    }
                }
            }
        }
        Ok(self.pending)
    }

    fn program(&self) -> Program {
        self.input.lock().map(|i| i.program).unwrap_or_default()
    }

    fn on_pes(
        &mut self,
        pes: mpeg2ts::pes::PesPacket<Vec<u8>>,
        now: SystemTime,
    ) -> Result<(), IngestError> {
        let id = pes.header.stream_id.as_u8();
        let Some(pts) = pes.header.pts.map(|t| t.as_u64()) else {
            return Ok(());
        };
        match id {
            0xE0..=0xEF => self.on_video(pts, pes.header.dts.map(|t| t.as_u64()), &pes.data, now),
            0xC0..=0xDF => self.on_audio(pts, &pes.data, now),
            _ => Ok(()),
        }
    }

    fn on_video(
        &mut self,
        pts_raw: u64,
        dts_raw: Option<u64>,
        data: &[u8],
        now: SystemTime,
    ) -> Result<(), IngestError> {
        let program = self.program();
        if program.other_video {
            return Err(IngestError::UnsupportedVideo);
        }
        self.declare_program(program, now)?;
        self.es.set_expect_audio(program.aac);
        let dts_raw = dts_raw.unwrap_or(pts_raw);
        let dts = self.clock.unwrap(dts_raw);
        let pts = dts + signed_delta(pts_raw, dts_raw);
        let au = codec::access_unit(data);
        if self.waiting_keyframe && !au.idr {
            return Ok(());
        }
        self.waiting_keyframe = false;
        let up = self.es.video(dts, pts, au, now)?;
        self.pending.merge(up);
        Ok(())
    }

    fn declare_program(&mut self, program: Program, now: SystemTime) -> Result<(), IngestError> {
        if !program.seen {
            return Ok(());
        }
        if program.other_video {
            return Err(IngestError::UnsupportedVideo);
        }
        let tracks = match (program.h264, program.aac) {
            (true, true) => TrackSet::AudioVideo,
            (true, false) => TrackSet::VideoOnly,
            (false, true) => TrackSet::AudioOnly,
            (false, false) => return Err(IngestError::InvalidTracks),
        };
        self.pending.merge(self.es.declare_tracks(tracks, now)?);
        Ok(())
    }

    fn on_audio(&mut self, pts_raw: u64, data: &[u8], now: SystemTime) -> Result<(), IngestError> {
        self.declare_program(self.program(), now)?;
        let pts = self.clock.unwrap(pts_raw);
        let mut elapsed_samples = 0i64;
        for (config, payload) in codec::adts_frames(data) {
            let offset =
                i128::from(elapsed_samples) * 90_000 / i128::from(config.sample_rate.max(1));
            let timestamp = pts
                .checked_add(i64::try_from(offset).map_err(|_| IngestError::InvalidTimestamp)?)
                .ok_or(IngestError::InvalidTimestamp)?;
            self.pending
                .merge(self.es.audio_at(timestamp, config, payload, now)?);
            elapsed_samples += i64::from(AAC_FRAME_SAMPLES);
        }
        Ok(())
    }
}

/// Demuxes a clean TS buffer into elementary events, for other modules' tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub enum Es {
        Video {
            dts: i64,
            pts: i64,
            annexb: Vec<u8>,
        },
        Audio {
            pts: i64,
            config: AacConfig,
            payload: Vec<u8>,
        },
    }

    pub fn elementary(ts: &[u8], mut f: impl FnMut(Es)) {
        let mut reader = PesPacketReader::new(TsPacketReader::new(ts));
        let mut clock = Unwrapper::default();
        while let Ok(Some(pes)) = reader.read_pes_packet() {
            let Some(pts_raw) = pes.header.pts.map(|t| t.as_u64()) else {
                continue;
            };
            match pes.header.stream_id.as_u8() {
                0xE0..=0xEF => {
                    let dts_raw = pes.header.dts.map_or(pts_raw, |t| t.as_u64());
                    let dts = clock.unwrap(dts_raw);
                    let pts = dts + signed_delta(pts_raw, dts_raw);
                    f(Es::Video {
                        dts,
                        pts,
                        annexb: pes.data,
                    });
                }
                0xC0..=0xDF => {
                    let pts = clock.unwrap(pts_raw);
                    for (i, (config, payload)) in
                        codec::adts_frames(&pes.data).into_iter().enumerate()
                    {
                        let step =
                            i64::from(AAC_FRAME_SAMPLES) * 90_000 / i64::from(config.sample_rate);
                        f(Es::Audio {
                            pts: pts + step * i as i64,
                            config,
                            payload: payload.to_vec(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muxer::LEADING_GAPS;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/testsrc-3s.ts");

    fn ingest_in_chunks(chunk: usize) -> TsIngest {
        let mut ing = TsIngest::new(MuxerConfig::default());
        for c in FIXTURE.chunks(chunk) {
            ing.push(c, SystemTime::UNIX_EPOCH).expect("push");
        }
        ing
    }

    #[test]
    fn real_ffmpeg_stream_becomes_segments() {
        // SRT payloads are 1316 bytes (7 TS packets).
        let ing = ingest_in_chunks(1316);
        let m = ing.muxer().expect("muxer");
        assert!(m.has_content());
        assert_eq!(m.video().expect("video").width, 320);
        assert_eq!(m.video().expect("video").height, 180);
        assert_eq!(m.audio().map(|a| a.sample_rate), Some(48000));
        let seg = m.segment(LEADING_GAPS).expect("first segment");
        assert_eq!(seg.duration.as_millis(), 1000);
        assert_eq!(ing.dropped_packets(), 0);
    }

    #[test]
    fn odd_chunking_gives_the_same_result() {
        let a = ingest_in_chunks(1316);
        let b = ingest_in_chunks(1000);
        let (ma, mb) = (a.muxer().expect("a"), b.muxer().expect("b"));
        assert_eq!(ma.entries().len(), mb.entries().len());
        let (sa, sb) = (
            ma.segment(LEADING_GAPS).expect("a"),
            mb.segment(LEADING_GAPS).expect("b"),
        );
        assert_eq!(sa.data, sb.data);
    }

    fn video_samples(ing: &TsIngest) -> Vec<Vec<u8>> {
        use mp4_atom::Decode;
        let muxer = ing.muxer().expect("muxer");
        let mut samples = Vec::new();
        for id in 0..muxer.next_part_id() {
            let crate::PartLookup::Found(part) = muxer.part(id) else {
                continue;
            };
            let moof = mp4_atom::Moof::decode(&mut part.data.as_ref()).expect("fragment");
            for track in moof.traf.iter().filter(|track| track.tfhd.track_id == 1) {
                for run in &track.trun {
                    let mut offset = run.data_offset.expect("sample offset") as usize;
                    for entry in &run.entries {
                        let end = offset + entry.size.expect("sample size") as usize;
                        samples.push(part.data[offset..end].to_vec());
                        offset = end;
                    }
                }
            }
        }
        samples
    }

    // Entirely generated testsrc2 + sine media. Six seconds leaves enough
    // keyframes to verify recovery as well as rejection of the damaged frame.
    const LOSS_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/transport-loss-6s.ts");

    fn loss_start_packet() -> usize {
        let mut probe = TsIngest::new(MuxerConfig::default());
        let packets: Vec<_> = LOSS_FIXTURE.chunks_exact(TS_SIZE).collect();
        for (index, pair) in packets.windows(2).enumerate() {
            probe.push(pair[0], SystemTime::UNIX_EPOCH).expect("probe");
            let video = |p: &[u8]| (u16::from(p[1] & 0x1f) << 8 | u16::from(p[2])) == 256;
            if probe.muxer().is_some_and(|muxer| muxer.has_content())
                && video(pair[0])
                && video(pair[1])
                && pair[0][1] & 0x40 != 0
                && pair[1][1] & 0x40 == 0
            {
                return index;
            }
        }
        panic!("fixture must contain a multi-packet video PES after playback starts");
    }

    fn loss_ingest(data: &[u8]) -> TsIngest {
        let mut ing = TsIngest::new(MuxerConfig::default());
        for chunk in data.chunks(1000) {
            ing.push(chunk, SystemTime::UNIX_EPOCH).expect("ingest");
        }
        ing
    }

    #[test]
    fn lost_transport_packet_never_becomes_a_corrupt_video_sample() {
        // Generated ffmpeg test pattern, not camera footage. Drop a packet
        // inside a multi-packet video PES after playback has begun.
        let lost = loss_start_packet() + 1;
        let damaged: Vec<u8> = LOSS_FIXTURE
            .chunks_exact(TS_SIZE)
            .enumerate()
            .filter(|(index, _)| *index != lost)
            .flat_map(|(_, packet)| packet.iter().copied())
            .collect();
        let clean = video_samples(&loss_ingest(LOSS_FIXTURE));
        assert!(clean.len() > 20, "exercise actual video samples");
        let ing = loss_ingest(&damaged);
        let emitted = video_samples(&ing);
        assert!(!emitted.is_empty(), "playback must recover after the loss");
        assert!(
            emitted.iter().all(|sample| clean.contains(sample)),
            "a truncated PES must never reach a viewer's decoder"
        );
        assert_eq!(
            emitted.last(),
            clean.last(),
            "recover through the final complete part"
        );
        assert!(ing.dropped_packets() > 0, "report transport loss");
    }

    #[test]
    fn retransmitted_packets_and_adaptation_only_packets_preserve_media() {
        let baseline = ingest_in_chunks(1316);
        let mut data = Vec::new();
        for (index, packet) in FIXTURE.chunks_exact(TS_SIZE).enumerate() {
            data.extend_from_slice(packet);
            if index == 3 || index == 133 {
                // Duplicate both a PES start and a PES continuation.
                data.extend_from_slice(packet);
            }
            if index == 134 {
                // Adaptation-only packets retain the preceding payload's
                // counter; they do not consume a payload counter value.
                let mut adaptation = [0xff; TS_SIZE];
                adaptation[..4].copy_from_slice(&packet[..4]);
                adaptation[1] &= !0x40;
                adaptation[3] = 0x20 | (packet[3] & 0x0f);
                adaptation[4] = 183;
                adaptation[5] = 0;
                data.extend_from_slice(&adaptation);
            }
        }
        let mut ing = TsIngest::new(MuxerConfig::default());
        for chunk in data.chunks(1000) {
            ing.push(chunk, SystemTime::UNIX_EPOCH).expect("ingest");
        }
        assert_eq!(video_samples(&ing), video_samples(&baseline));
        assert_eq!(ing.dropped_packets(), 0);
        assert_eq!(
            ing.muxer()
                .expect("muxer")
                .segment(LEADING_GAPS)
                .expect("segment")
                .data,
            baseline
                .muxer()
                .expect("baseline")
                .segment(LEADING_GAPS)
                .expect("segment")
                .data
        );
    }

    #[test]
    fn flagged_transport_errors_do_not_reach_the_decoder() {
        let clean = video_samples(&loss_ingest(LOSS_FIXTURE));
        let start = loss_start_packet();
        for lost_start in [false, true] {
            let mut damaged = LOSS_FIXTURE.to_vec();
            if lost_start {
                damaged.drain(start * TS_SIZE..(start + 1) * TS_SIZE);
            } else {
                damaged[(start + 1) * TS_SIZE + 1] |= 0x80;
            }
            let ing = loss_ingest(&damaged);
            let emitted = video_samples(&ing);
            assert!(emitted.iter().all(|sample| clean.contains(sample)));
            assert_eq!(emitted.last(), clean.last(), "recover without a reconnect");
            assert!(ing.dropped_packets() > 0);
        }
    }

    #[test]
    fn garbage_and_truncation_do_not_derail_it() {
        let mut ing = TsIngest::new(MuxerConfig::default());
        ing.push(&[0x47, 1, 2, 3, 0xff, 0x00], SystemTime::UNIX_EPOCH)
            .expect("push");
        ing.push(&[0u8; 5000], SystemTime::UNIX_EPOCH)
            .expect("push");
        // Corrupt some bytes in the middle of the real stream.
        let mut data = FIXTURE.to_vec();
        for i in (20_000..data.len()).step_by(9_973) {
            data[i] ^= 0x5a;
        }
        for c in data.chunks(1316) {
            ing.push(c, SystemTime::UNIX_EPOCH).expect("push");
        }
        assert!(ing.muxer().is_some_and(|m| m.has_content()));
    }
}

#[cfg(test)]
mod epoch_window_tests {
    use super::*;
    use crate::{muxer::Entry, playlist};
    #[test]
    fn codec_epochs_retain_old_maps_only_for_the_bounded_window_and_inflight_owners() {
        let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
        let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
        let cfg = MuxerConfig {
            segment_count: 8,
            ..MuxerConfig::default()
        };
        let mut ingest = TsIngest::new(cfg);
        let mut checker = playlist::RuleCheck::default();
        let mut old = None;
        let mut video_end = 0;
        let mut last_part = None;
        for epoch in 0..16 {
            let bytes = if epoch % 2 == 0 {
                portrait.as_slice()
            } else {
                landscape.as_slice()
            };
            for chunk in bytes.chunks(1316) {
                ingest
                    .push(chunk, SystemTime::UNIX_EPOCH)
                    .expect("valid epoch regression fixture");
                let Some(muxer) = ingest.muxer() else {
                    continue;
                };
                if muxer.has_content() {
                    checker
                        .check(&playlist::media(muxer, "fixed"))
                        .expect("valid epoch regression fixture");
                }
            }
            let muxer = ingest.muxer().expect("valid epoch regression fixture");
            assert_eq!(muxer.current_initialization_id(), epoch);
            assert_eq!(muxer.target_duration(), 2);
            assert!(muxer.entries().len() <= cfg.segment_count);
            for id in last_part.map_or(0, |id| id + 1)..muxer.next_part_id() {
                if let crate::PartLookup::Found(part) = muxer.part(id) {
                    use mp4_atom::Decode;
                    let mut input = part.data.as_ref();
                    let moof =
                        mp4_atom::Moof::decode(&mut input).expect("valid epoch regression fixture");
                    let video = moof
                        .traf
                        .iter()
                        .find(|track| track.tfhd.track_id == 1)
                        .expect("valid epoch regression fixture");
                    let at = video
                        .tfdt
                        .as_ref()
                        .expect("valid epoch regression fixture")
                        .base_media_decode_time;
                    assert!(
                        at >= video_end,
                        "HLS decode time must not reset at a new epoch"
                    );
                    video_end = at
                        + video
                            .trun
                            .iter()
                            .flat_map(|run| &run.entries)
                            .map(|sample| {
                                u64::from(sample.duration.expect("valid epoch regression fixture"))
                            })
                            .sum::<u64>();
                    last_part = Some(id);
                }
            }
            if epoch == 0 {
                old = Some(muxer.segment(7).expect("valid epoch regression fixture"));
            }
            if epoch > 5 {
                assert!(muxer.initialization(0).is_none());
            }
            let epochs: std::collections::BTreeSet<_> = muxer
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Segment(segment) => Some(segment.initialization.id),
                    _ => None,
                })
                .chain(muxer.open_parts().iter().map(|part| part.initialization.id))
                .collect();
            assert!(epochs.len() <= cfg.segment_count + 1);
            let mut held = ingest.take_muxer().expect("valid epoch regression fixture");
            held.interrupt(SystemTime::UNIX_EPOCH);
            ingest = TsIngest::resume(cfg, held);
        }
        let old = old.expect("valid epoch regression fixture"); // a response already holding this Arc can finish
        assert_eq!(old.initialization.id, 0);
        assert!(!old.initialization.data.is_empty() && !old.data.is_empty());
        let latest = playlist::media(
            ingest.muxer().expect("valid epoch regression fixture"),
            "fixed",
        );
        assert!(
            checker
                .check(&latest.replace("fixed_seg", "changed_seg"))
                .is_err(),
            "same MSN must never be relabelled"
        );
    }
}

#[cfg(test)]
mod declared_topology_tests {
    use super::*;

    #[test]
    fn no_pmt_is_not_an_audio_only_declaration() {
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .declare_program(
                Program {
                    seen: false,
                    aac: true,
                    ..Program::default()
                },
                SystemTime::UNIX_EPOCH,
            )
            .expect("an unseen PMT leaves topology undeclared");
        assert_eq!(ingest.es.declared_tracks(), None);
    }

    #[test]
    fn known_unsupported_video_cannot_be_hidden_by_aac_or_supported_video() {
        for h264 in [false, true] {
            let mut ingest = TsIngest::new(MuxerConfig::default());
            assert_eq!(
                ingest.declare_program(
                    Program {
                        seen: true,
                        aac: true,
                        h264,
                        other_video: true
                    },
                    SystemTime::UNIX_EPOCH
                ),
                Err(IngestError::UnsupportedVideo)
            );
            assert_eq!(ingest.es.declared_tracks(), None);
        }
    }

    #[test]
    fn a_second_pmt_cannot_change_connection_topology() {
        let mut ingest = TsIngest::new(MuxerConfig::default());
        ingest
            .declare_program(
                Program {
                    seen: true,
                    aac: true,
                    ..Program::default()
                },
                SystemTime::UNIX_EPOCH,
            )
            .expect("first authoritative AAC-only PMT is accepted");
        assert_eq!(
            ingest.declare_program(
                Program {
                    seen: true,
                    aac: true,
                    h264: true,
                    other_video: false
                },
                SystemTime::UNIX_EPOCH
            ),
            Err(IngestError::TrackConflict)
        );
    }
}
