//! MPEG-TS in, muxer out.
//!
//! TS and PES parsing come from `mpeg2ts`. Its reader pulls from a
//! `std::io::Read` and treats some stream errors as fatal, so it is fed one
//! 188-byte packet at a time from a buffer we own: a bad packet is dropped
//! and the next one parsed, and running out of input is reported as an error
//! that leaves the reader's state intact for the next push.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bytes::Bytes;
use mpeg2ts::es::StreamType;
use mpeg2ts::pes::{PesHeader, PesPacket, PesPacketReader, ReadPesPacket};
use mpeg2ts::ts::{Pid, ReadTsPacket, TsPacket, TsPacketReader, TsPayload};

use crate::codec::{self, AAC_FRAME_SAMPLES, AacConfig, H264Config};
use crate::muxer::{MuxError, Muxer, MuxerConfig, Update};

use crate::TrackSet;
use crate::time::{Unwrapper, signed_delta};

const TS_SIZE: usize = 188;
const SYNC: u8 = 0x47;
/// Cap on unconsumed input (a few seconds at stage bitrates).
const MAX_BUFFER: usize = 8 * 1024 * 1024;
// Limits on assembled payload lengths and bounded PES reservation requests,
// not Vec capacity, allocator overhead, retained HLS data or whole-process RSS.
const MAX_PES_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_PES_BYTES: usize = 16 * 1024 * 1024;
// Separate from the optional video/parameter queue and TS/PES allocations.
const STARTUP_AUDIO_BYTES: usize = 64 * 1024;
const STARTUP_AUDIO_FRAMES: usize = 64;
const STARTUP_AUDIO_TICKS: i64 = 90_000;

#[cfg(test)]
#[path = "pes_limits_tests.rs"]
mod pes_limits_tests;

#[cfg(test)]
#[path = "startup_ts_tests.rs"]
mod startup_ts_tests;

#[derive(Clone, Copy)]
struct StartupLimits {
    aus: usize,
    bytes: usize,
    ticks: i64,
}
impl Default for StartupLimits {
    fn default() -> Self {
        Self {
            aus: 64,
            bytes: 1024 * 1024,
            ticks: 90_000,
        }
    }
}
struct StartupVideo {
    dts: i64,
    pts: i64,
    au: codec::AccessUnit,
    now: SystemTime,
}

/// Why an ingest cannot carry on with the publisher's media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    /// The shared encoded fragment-backing budget is exhausted.
    FragmentLimit,
    /// The program carries a video codec other than H.264.
    UnsupportedVideo,
    /// The held presentation expired before new media recovered it.
    PresentationExpired,
    /// A segment or unsealed media retention exceeded a resource limit.
    SegmentTooLarge,
    /// No supported non-empty track topology can be initialized.
    InvalidTracks,
    /// Media contradicts this connection's explicit immutable declaration.
    TrackConflict,
    /// The AAC configuration is unsupported or changes within the connection.
    InvalidAudio,
    /// An AAC timestamp cannot be represented on the supported timeline.
    InvalidTimestamp,
    /// This connection has explicitly finished; resume with a new ingest.
    InputFinished,
    /// Unread transport input or pending PES exceeds policy.
    /// No more input is accepted; `finish` still seals the accepted prefix.
    InputLimitExceeded,
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FragmentLimit => f.write_str("shared encoded fragment budget exhausted"),
            Self::PresentationExpired => f.write_str("presentation reconnect deadline expired"),
            Self::UnsupportedVideo => f.write_str("video is not H.264"),
            Self::SegmentTooLarge => f.write_str("segment or unsealed media exceeds media limit"),
            Self::InvalidTracks => f.write_str("invalid track set"),
            Self::TrackConflict => f.write_str("track set changed within a connection"),
            Self::InvalidAudio => f.write_str("unsupported or changed AAC configuration"),
            Self::InvalidTimestamp => f.write_str("audio timestamp outside supported range"),
            Self::InputFinished => f.write_str("transport input already finished"),
            Self::InputLimitExceeded => f.write_str("TS input or PES retention limit exceeded"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<MuxError> for IngestError {
    fn from(value: MuxError) -> Self {
        match value {
            MuxError::FragmentLimit => Self::FragmentLimit,
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
    terminal: bool,
    ended: bool,
    limit_exceeded: bool,
}

// Scalar/header metadata only: never a second copy of elementary payloads.
// PID limits the table to the finite transport PID space. Completed multi-TS
// bounded PES leave the table immediately; START-only PES await real EOF.
struct PesBoundary {
    header: PesHeader,
    expected: Option<usize>,
    received: usize,
    reserved: usize,
}

struct Feed {
    input: Arc<Mutex<Input>>,
    slot: SlotReader,
    inner: TsPacketReader<SlotReader>,
    continuity: HashMap<Pid, (u8, [u8; TS_SIZE])>,
    pending: Option<Box<TsPacket>>,
    boundaries: HashMap<Pid, PesBoundary>,
    pending_bytes: usize,
}

impl Feed {
    fn normalize_pes_length(packet: &mut TsPacket, raw: &[u8; TS_SIZE]) -> bool {
        let Some(TsPayload::PesStart(pes)) = &mut packet.payload else {
            return true;
        };
        if !(0xc0..=0xef).contains(&pes.header.stream_id.as_u8()) {
            return true;
        }
        // The maintained TS reader validates the optional header and consumes
        // FF stuffing, but PesHeader does not retain header_data_length. Its
        // PES reader reconstructs only the minimum header length. Preserve the
        // advertised raw payload length by subtracting stuffing from the parsed
        // packet_len before that reader assembles payloads. No payload parsing
        // or copying is introduced here, and unbounded video stays unbounded.
        let at = if raw[3] & 0x20 != 0 {
            5 + usize::from(raw[4])
        } else {
            4
        };
        let Some(&length) = raw.get(at + 8) else {
            return false;
        };
        let actual = 3 + u16::from(length);
        let minimum = 3
            + u16::from(pes.header.pts.is_some()) * 5
            + u16::from(pes.header.dts.is_some()) * 5
            + u16::from(pes.header.escr.is_some()) * 6;
        let Some(stuffing) = actual.checked_sub(minimum) else {
            return false;
        };
        if pes.pes_packet_len != 0 {
            if pes.pes_packet_len < actual {
                return false;
            }
            pes.pes_packet_len -= stuffing;
        }
        true
    }

    // Check BEFORE handing a packet to PesPacketReader: bounded START allocates
    // its advertised size immediately, while an unbounded continuation appends
    // indefinitely. Only scalar accounting is added, never payload copies or a
    // second parser. HashMap keys remain bounded by the transport PID space.
    fn observe_boundary(&mut self, packet: &TsPacket) -> bool {
        let pid = packet.header.pid;
        match &packet.payload {
            Some(TsPayload::PesStart(pes)) => {
                // normalize_pes_length already accounts for raw stuffing;
                // subtracting this retained minimum now gives the exact raw
                // advertised payload length, not a guessed header extent.
                let header_len = 3
                    + usize::from(pes.header.pts.is_some()) * 5
                    + usize::from(pes.header.dts.is_some()) * 5
                    + usize::from(pes.header.escr.is_some()) * 6;
                if pes.pes_packet_len != 0 && usize::from(pes.pes_packet_len) < header_len {
                    // Upstream rejects this START BEFORE allocating/inserting
                    // its replacement. Special stream IDs omit the optional
                    // wire header, so normalize_pes_length does not reject
                    // them. Preserve the old owner's accounting exactly and
                    // let the maintained reader report its existing error.
                    return true;
                }
                let expected = (pes.pes_packet_len != 0)
                    .then(|| usize::from(pes.pes_packet_len).checked_sub(header_len))
                    .flatten();
                let reserved = expected.unwrap_or(0).max(pes.data.len());
                let Some(transient) = self.pending_bytes.checked_add(reserved) else {
                    return false;
                };
                // Upstream allocates the new packet BEFORE releasing the old
                // packet on this PID. Both reservations must fit transiently.
                if reserved > MAX_PES_BYTES || transient > MAX_PENDING_PES_BYTES {
                    return false;
                }
                let old = self.boundaries.insert(
                    pid,
                    PesBoundary {
                        header: pes.header.clone(),
                        expected,
                        received: pes.data.len(),
                        reserved,
                    },
                );
                self.pending_bytes = transient - old.map_or(0, |b| b.reserved);
            }
            Some(TsPayload::PesContinuation(data)) => {
                if let Some(boundary) = self.boundaries.get_mut(&pid) {
                    let Some(received) = boundary.received.checked_add(data.len()) else {
                        return false;
                    };
                    let reserved = boundary.expected.unwrap_or(0).max(received);
                    let Some(total) = self
                        .pending_bytes
                        .checked_sub(boundary.reserved)
                        .and_then(|other| other.checked_add(reserved))
                    else {
                        return false;
                    };
                    if reserved > MAX_PES_BYTES || total > MAX_PENDING_PES_BYTES {
                        return false;
                    }
                    boundary.received = received;
                    boundary.reserved = reserved;
                    self.pending_bytes = total;
                    if boundary.expected.is_some_and(|n| boundary.received >= n) {
                        // The PES reader returns a complete bounded packet (or
                        // rejects an overrun) in this very continuation call.
                        self.pending_bytes -= reserved;
                        self.boundaries.remove(&pid);
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn retention_limit(&self) -> mpeg2ts::Error {
        if let Ok(mut input) = self.input.lock() {
            input.limit_exceeded = true;
            input.dropped = input.dropped.saturating_add(1);
        }
        io::Error::new(io::ErrorKind::InvalidData, "PES retention limit exceeded").into()
    }

    fn terminal_aac(&self, pes: &PesPacket<Vec<u8>>) -> bool {
        // The upstream EOF path also returns unbounded video. Only a pending,
        // exactly bounded audio packet may cross our terminal boundary. Reject
        // ambiguous same-header streams rather than guess their PID identity.
        let mut matches = self
            .boundaries
            .iter()
            .filter(|(_, b)| b.header == pes.header);
        let Some((_, boundary)) = matches.next() else {
            return false;
        };
        matches.next().is_none()
            && (0xc0..=0xdf).contains(&pes.header.stream_id.as_u8())
            && boundary.expected == Some(pes.data.len())
            && boundary.received == pes.data.len()
    }
}

impl ReadTsPacket for Feed {
    fn read_ts_packet(&mut self) -> mpeg2ts::Result<Option<TsPacket>> {
        if let Some(packet) = self.pending.take() {
            if !self.observe_boundary(&packet) {
                return Err(self.retention_limit());
            }
            return Ok(Some(*packet));
        }
        loop {
            let packet = {
                let mut input = self.input.lock().map_err(|_| starved())?;
                let Input { buf, start, .. } = &mut *input;
                // Resynchronise on the sync byte if the stream slipped.
                let before = *start;
                match buf[*start..].iter().position(|&b| b == SYNC) {
                    Some(skip) => *start += skip,
                    None => *start = buf.len(),
                }
                let skipped = input.start != before;
                if skipped {
                    input.dropped = input.dropped.saturating_add(1);
                    input.reset_pes = true;
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "transport sync lost").into(),
                    );
                }
                let remaining = input.buf.len() - input.start;
                if remaining < TS_SIZE {
                    if input.terminal {
                        if remaining != 0 {
                            input.dropped = input.dropped.saturating_add(1);
                            input.start = input.buf.len();
                        }
                        input.ended = true;
                        return Ok(None);
                    }
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
                Ok(Some(mut p)) => {
                    if !Self::normalize_pes_length(&mut p, &packet) {
                        if let Ok(mut input) = self.input.lock() {
                            input.dropped = input.dropped.saturating_add(1);
                            input.reset_pes = true;
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid PES header length",
                        )
                        .into());
                    }
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
                    if !self.observe_boundary(&p) {
                        return Err(self.retention_limit());
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
    fragment_quota: Option<Arc<sapflow_core::fragment::FragmentQuota>>,
}

impl EsIngest {
    /// An ingest with no muxer yet. Audio is expected, so the first keyframe
    /// waits for an AAC configuration (see [`EsIngest::set_expect_audio`]).
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
            fragment_quota: None,
        }
    }

    /// Carries on another connection's stream (see [`Muxer::interrupt`]).
    /// The first supported media decides: with the same parameter sets and audio
    /// config, its media continues the stream after a discontinuity;
    /// otherwise it starts a new initialization epoch on the retained muxer
    /// and the update says `restarted` (for recording rollover).
    pub fn resume(cfg: MuxerConfig, mut muxer: Muxer) -> Self {
        let fragment_quota = muxer.fragment_quota();
        muxer.resume_fragment_admission();
        Self {
            muxer: Some(muxer),
            fragment_quota,
            ..Self::new(cfg)
        }
    }

    /// Uses one shared encoded-backing budget for every codec epoch.
    pub fn with_fragment_quota(
        cfg: MuxerConfig,
        quota: Arc<sapflow_core::fragment::FragmentQuota>,
    ) -> Self {
        Self {
            fragment_quota: Some(quota),
            ..Self::new(cfg)
        }
    }

    /// The muxer, once a video keyframe or explicitly declared audio-only frame
    /// has created one.
    pub fn muxer(&self) -> Option<&Muxer> {
        self.muxer.as_ref()
    }

    /// Mutable access to the muxer after the first supported media creates it.
    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.muxer.as_mut()
    }

    /// Removes and returns the muxer and forgets the timeline origin, for
    /// [`EsIngest::resume`] on a later connection.
    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.origin = None;
        self.muxer.take()
    }

    /// Only an authoritative PMT or explicit RTMP declaration may select
    /// audio-only. Missing video metadata and early AAC do not imply radio.
    /// A declaration alone creates no muxer or servable media.
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

    /// The explicitly declared topology, or `None` for legacy video-led ingest.
    pub fn declared_tracks(&self) -> Option<TrackSet> {
        self.declared
    }

    /// Sets whether the first video keyframe waits for AAC configuration before
    /// fixing the init segment. An explicit track declaration takes precedence.
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

    /// The AAC configuration, once one has been seen.
    pub fn known_audio(&self) -> Option<AacConfig> {
        self.aac
    }

    /// Records the AAC configuration; only the first is kept. A muxer that has
    /// no audio track and nothing servable yet is discarded, so the next
    /// keyframe starts one with both tracks.
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
                m.reconfigure_tracks(Some(video.clone()), audio, now)?;
                restarted = true;
            }
            if self.muxer.is_none() {
                self.muxer = Some(match &self.fragment_quota {
                    Some(quota) => {
                        Muxer::with_fragment_quota(self.cfg, Some(video), audio, quota.clone())?
                    }
                    None => Muxer::new(self.cfg, video, audio),
                });
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
            self.audio(pts, config, payload)?;
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
                self.muxer = Some(match &self.fragment_quota {
                    Some(quota) => {
                        Muxer::with_fragment_quota(self.cfg, None, Some(config), quota.clone())?
                    }
                    None => Muxer::audio_only(self.cfg, config)?,
                });
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

    /// Checked video-led AAC path. This pre-release entry point now returns
    /// resource refusal; consumers must propagate it and preserve incomplete
    /// recording status. Audio-only consumers need the updating `audio_at`.
    pub fn audio(
        &mut self,
        pts: i64,
        config: AacConfig,
        payload: &[u8],
    ) -> Result<(), IngestError> {
        if self.declared == Some(TrackSet::AudioOnly) {
            return Ok(());
        }
        self.audio_config(config);
        let (Some(muxer), Some(origin)) = (self.muxer.as_mut(), self.origin) else {
            return Ok(());
        };
        let t = pts
            .checked_sub(origin)
            .ok_or(IngestError::InvalidTimestamp)?;
        if t >= 0 {
            muxer.check_audio_admission(t, payload.len())?;
            muxer.push_audio(t, Bytes::copy_from_slice(payload))?;
        }
        Ok(())
    }
}

/// One publisher's stream: TS bytes in, LL-HLS muxer state out.
///
/// Unread input and each assembled PES are limited to 8 MiB. Pending PES
/// payload/reservation requests total at most 16 MiB, including simultaneous
/// old/new packets on one PID. These are not limits on Vec capacity or RSS.
/// A limit failure releases pending assemblies and permanently refuses input;
/// `finish` still seals accepted media with `media_loss` set.
/// A startup-prefix budget overflow discards that optional GOP with media
/// loss and waits for a fresh IDR; it does not fail the transport/PES input.
/// Before an authoritative A/V startup knows AAC, complete video AUs from its
/// first valid IDR wait in a separate queue: at most 64 AUs, 1 MiB summed AU
/// and parameter-set capacities, and one second of DTS span. Supplied wall
/// timestamps affect wall association only, never startup admission. There is
/// no wall-time/idle expiry. Refusal/drop releases held payloads.
pub struct TsIngest {
    input: Arc<Mutex<Input>>,
    pes: Option<PesPacketReader<Feed>>,
    clock: Unwrapper,
    es: EsIngest,
    pending: Update,
    waiting_keyframe: bool,
    startup: VecDeque<StartupVideo>,
    startup_bytes: usize,
    startup_limits: StartupLimits,
    startup_error: Option<IngestError>,
    startup_replaying: bool,
    startup_has_successor: bool,
    startup_now: Option<SystemTime>,
}

impl TsIngest {
    /// Starts TS ingest with a shared encoded-backing budget.
    pub fn with_fragment_quota(
        cfg: MuxerConfig,
        quota: Arc<sapflow_core::fragment::FragmentQuota>,
    ) -> Self {
        let mut ingest = Self::new(cfg);
        ingest.es = EsIngest::with_fragment_quota(cfg, quota);
        ingest
    }
    /// Carries on another connection's stream ([`EsIngest::resume`]).
    pub fn resume(cfg: MuxerConfig, muxer: Muxer) -> Self {
        let mut ts = Self::new(cfg);
        ts.es = EsIngest::resume(cfg, muxer);
        ts
    }

    /// The muxer, mutably, once a video keyframe or declared audio-only frame
    /// has created one.
    pub fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        self.es.muxer_mut()
    }

    /// Removes and returns the muxer, for [`TsIngest::resume`] on a later
    /// connection.
    pub fn take_muxer(&mut self) -> Option<Muxer> {
        self.es.take_muxer()
    }

    /// An ingest with no input yet. `cfg` configures the muxer created by the
    /// first supported media after the program's topology is known.
    pub fn new(cfg: MuxerConfig) -> Self {
        let input = Arc::new(Mutex::new(Input::default()));
        let slot = SlotReader::default();
        let feed = Feed {
            input: input.clone(),
            slot: slot.clone(),
            inner: TsPacketReader::new(slot),
            continuity: HashMap::new(),
            pending: None,
            boundaries: HashMap::new(),
            pending_bytes: 0,
        };
        Self {
            input,
            pes: Some(PesPacketReader::new(feed)),
            clock: Unwrapper::default(),
            es: EsIngest::new(cfg),
            pending: Update::default(),
            waiting_keyframe: false,
            startup: VecDeque::new(),
            startup_bytes: 0,
            startup_limits: StartupLimits::default(),
            startup_error: None,
            startup_replaying: false,
            startup_has_successor: false,
            startup_now: None,
        }
    }

    /// The muxer, once a keyframe or explicitly declared audio-only frame has
    /// created one.
    pub fn muxer(&self) -> Option<&Muxer> {
        self.es.muxer()
    }

    /// Invalid transport/PES packets and continuity losses, for diagnostics.
    pub fn dropped_packets(&self) -> u64 {
        self.input.lock().map(|i| i.dropped).unwrap_or(0)
    }

    /// Feeds raw TS bytes (any alignment). `now` stamps new segments.
    ///
    /// Unread input is checked against 8 MiB before copying. Pending assembled
    /// PES payload/reservation requests are limited to 8 MiB per PES and 16 MiB
    /// in aggregate, including same-PID transient replacement. These are logical
    /// limits, not allocator capacity or whole-process memory bounds. Exceeding
    /// one releases pending assemblies and permanently refuses further input;
    /// [`TsIngest::finish`] can still seal the accepted prefix with media loss.
    pub fn push(&mut self, data: &[u8], now: SystemTime) -> Result<Update, IngestError> {
        let exceeded = {
            let Ok(mut input) = self.input.lock() else {
                return Ok(Update::default());
            };
            if input.terminal {
                return Err(IngestError::InputFinished);
            }
            if let Some(error) = &self.startup_error {
                return Err(error.clone());
            }
            if input.limit_exceeded {
                return Err(IngestError::InputLimitExceeded);
            }
            if input.start > 0 {
                let start = input.start;
                input.buf.drain(..start);
                input.start = 0;
            }
            if input
                .buf
                .len()
                .checked_add(data.len())
                .is_none_or(|n| n > MAX_BUFFER)
            {
                input.limit_exceeded = true;
                input.dropped = input.dropped.saturating_add(1);
                true
            } else {
                input.buf.extend_from_slice(data);
                input.starved = false;
                false
            }
        };
        if exceeded {
            if self.waiting_for_startup_audio() {
                return Err(self.refuse_startup(IngestError::InputLimitExceeded));
            }
            if self.startup_replaying {
                self.discard_startup_catchup(now);
            }
            self.release_failed_input();
            return Err(IngestError::InputLimitExceeded);
        }
        self.pending = Update::default();
        if let Err(error) = self.drain(now, false) {
            return Err(
                if self.waiting_for_startup_audio() || self.startup_replaying {
                    self.refuse_startup(error)
                } else {
                    error
                },
            );
        }
        Ok(self.pending)
    }

    /// Ends this connection's input, as distinct from temporary starvation.
    /// Releases only complete bounded AAC still held by the PES reader, then
    /// seals accepted muxer media. An unbounded video tail is never emitted;
    /// its uncertain duration alone is not definite transport loss. Truncated
    /// bounded PES/TS or parser rejection sets `media_loss`. The retained muxer
    /// can be resumed by a new ingest; this parser cannot accept more bytes.
    /// Repeated finish calls are harmless and return an empty update.
    pub fn finish(&mut self, now: SystemTime) -> Result<Update, IngestError> {
        let failed = {
            let mut input = self.input.lock().expect("input lock is private to ingest");
            if input.terminal {
                return Ok(Update::default());
            }
            input.terminal = true;
            input.starved = false;
            input.limit_exceeded
        };
        self.pending = Update::default();
        let result = if let Some(error) = &self.startup_error {
            Err(error.clone())
        } else if failed {
            Ok(())
        } else {
            self.drain(now, true)
        };
        let program = self.program();
        let missing_declared_audio = self.es.origin.is_none()
            && self.es.known_audio().is_none()
            && program.seen
            && program.h264
            && program.aac;
        let result = match result {
            Ok(()) if !failed && (self.waiting_for_startup_audio() || missing_declared_audio) => {
                Err(self.refuse_startup(IngestError::InvalidTracks))
            }
            Err(error) if self.waiting_for_startup_audio() || self.startup_replaying => {
                Err(self.refuse_startup(error))
            }
            other => other,
        };
        self.pending.media_loss |= self.dropped_packets() != 0;
        self.pending.media_loss |= self.startup_error.is_some();
        self.pending.media_loss |= !self.startup.is_empty();
        self.startup = VecDeque::new();
        self.startup_bytes = 0;
        self.startup_replaying = false;
        self.startup_has_successor = false;
        self.startup_now = None;
        if let Some(muxer) = self.es.muxer_mut() {
            self.pending.merge(muxer.interrupt(now));
        }
        result.map(|()| self.pending)
    }

    fn waiting_for_startup_audio(&self) -> bool {
        self.es.origin.is_none()
            && self.es.declared == Some(TrackSet::AudioVideo)
            && self.es.known_audio().is_none()
    }

    fn refuse_startup(&mut self, error: IngestError) -> IngestError {
        if let Some(now) = self.startup_now {
            self.discard_startup_catchup(now);
        }
        self.startup = VecDeque::new();
        self.startup_bytes = 0;
        self.es.sps = None;
        self.es.pps = None;
        self.startup_error = Some(error.clone());
        self.release_failed_input();
        error
    }

    fn restart_startup(&mut self) {
        // The held GOP cannot be published after loss or a configuration
        // change. Keep the bounded known parameter sets, as the original
        // resynchronisation path did, and require a fresh complete IDR.
        // Parameter replacement and its capacity bound are checked below.
        self.startup = VecDeque::new();
        self.startup_bytes = 0;
        self.waiting_keyframe = true;
        self.pending.media_loss = true;
    }

    fn discard_startup_catchup(&mut self, now: SystemTime) {
        self.restart_startup();
        if self.startup_replaying {
            if let Some(muxer) = self.es.muxer_mut() {
                self.pending.merge(muxer.interrupt(now));
            }
            if self.es.muxer().is_some_and(|m| !m.has_content()) {
                self.es.muxer = None;
            }
            self.es.origin = None;
        }
        self.startup_replaying = false;
        self.startup_has_successor = false;
        self.startup_now = None;
    }

    fn replay_startup_video(&mut self) -> Result<(), IngestError> {
        while let Some(frame) = self.startup.front() {
            let ready = self
                .es
                .origin
                .zip(self.es.muxer())
                .is_some_and(|(origin, muxer)| {
                    frame
                        .dts
                        .checked_sub(origin)
                        .is_some_and(|at| muxer.startup_video_replayable(at))
                });
            if !ready {
                break;
            }
            let frame = self.startup.pop_front().expect("checked replay head");
            self.startup_bytes -= frame.au.data.capacity();
            match self.es.video(frame.dts, frame.pts, frame.au, frame.now) {
                Ok(update) => self.pending.merge(update),
                Err(error) => return Err(self.refuse_startup(error)),
            }
            self.startup_has_successor = true;
        }
        if self.startup.is_empty() && self.startup_has_successor {
            self.startup_replaying = false;
            self.startup_now = None;
        }
        Ok(())
    }

    fn startup_video(
        &mut self,
        dts: i64,
        pts: i64,
        mut au: codec::AccessUnit,
        now: SystemTime,
    ) -> Result<Update, IngestError> {
        if !self.waiting_for_startup_audio() && !self.startup_replaying {
            return self.es.video(dts, pts, au, now);
        }
        if self.es.origin.is_none()
            && !self.startup.is_empty()
            && (au
                .sps
                .as_ref()
                .is_some_and(|sps| Some(sps) != self.es.sps.as_ref())
                || au
                    .pps
                    .as_ref()
                    .is_some_and(|pps| Some(pps) != self.es.pps.as_ref()))
        {
            // Nothing has been initialised yet. Discard the old GOP rather
            // than disconnecting an encoder that supplies new parameters.
            // A complete IDR with supported parameters can start over now;
            // other AUs only update bounded parameters and await that IDR.
            self.restart_startup();
        }
        let (sps, pps) = (
            au.sps.as_ref().or(self.es.sps.as_ref()),
            au.pps.as_ref().or(self.es.pps.as_ref()),
        );
        let parameter_bytes = sps
            .map_or(0, Vec::capacity)
            .checked_add(pps.map_or(0, Vec::capacity));
        let Some(parameter_bytes) = parameter_bytes.filter(|n| *n <= self.startup_limits.bytes)
        else {
            self.discard_startup_catchup(now);
            self.es.sps = None;
            self.es.pps = None;
            return Ok(Update::default());
        };
        self.es.parameter_sets(au.sps.take(), au.pps.take());
        if au.data.is_empty() {
            return Ok(Update::default());
        }
        if self.startup.is_empty() && self.es.origin.is_none() {
            if !au.idr {
                return Ok(Update::default());
            }
            let (Some(sps), Some(pps)) = (&self.es.sps, &self.es.pps) else {
                return Ok(Update::default());
            };
            if H264Config::new(sps, pps).is_none() {
                return Ok(Update::default());
            }
            self.waiting_keyframe = false;
        }
        let span_exceeded =
            if let (Some(first), Some(last)) = (self.startup.front(), self.startup.back()) {
                if dts < last.dts {
                    return Err(self.refuse_startup(IngestError::InvalidTimestamp));
                }
                dts.checked_sub(first.dts)
                    .is_none_or(|span| span > self.startup_limits.ticks)
            } else {
                false
            };
        let bytes = self.startup_bytes.checked_add(au.data.capacity());
        if span_exceeded
            || self.startup.len() >= self.startup_limits.aus
            || bytes
                .and_then(|n| n.checked_add(parameter_bytes))
                .is_none_or(|n| n > self.startup_limits.bytes)
        {
            // This optional prefix queue is not the transport/PES budget.
            // Late AAC can still recover at a valid IDR without disconnecting.
            self.discard_startup_catchup(now);
            if au.idr
                && self.startup_limits.aus != 0
                && au
                    .data
                    .capacity()
                    .checked_add(parameter_bytes)
                    .is_some_and(|n| n <= self.startup_limits.bytes)
            {
                // One bounded retry: an empty queue and a fitting single IDR
                // cannot hit the count, span or byte condition again.
                return self.startup_video(dts, pts, au, now);
            }
            return Ok(Update::default());
        }
        self.startup_bytes = bytes.expect("checked startup byte sum");
        self.startup.push_back(StartupVideo { dts, pts, au, now });
        if self.startup_replaying {
            self.replay_startup_video()?;
        }
        Ok(Update::default())
    }

    #[cfg(test)]
    fn release_startup_video(&mut self) -> Result<(), IngestError> {
        while let Some(frame) = self.startup.pop_front() {
            self.startup_bytes -= frame.au.data.capacity();
            match self.es.video(frame.dts, frame.pts, frame.au, frame.now) {
                Ok(update) => self.pending.merge(update),
                Err(error) => return Err(self.refuse_startup(error)),
            }
        }
        self.startup = VecDeque::new();
        Ok(())
    }

    fn startup_audio(&mut self, config: AacConfig) -> Result<(), IngestError> {
        // Match the muxer's supported AAC-LC profile before constructing an
        // immutable A/V init from a held IDR. This is configuration policy,
        // not another ADTS parser.
        if config.object_type != 2
            || !(1..=2).contains(&config.channels)
            || config.sample_rate > u32::from(u16::MAX)
            || AacConfig::from_audio_specific_config(&config.audio_specific_config())
                != Some(config)
        {
            return Err(self.refuse_startup(IngestError::InvalidAudio));
        }
        self.es.audio_config(config);
        if let Some(frame) = self.startup.pop_front() {
            self.startup_replaying = true;
            self.startup_has_successor = false;
            self.startup_now = Some(frame.now);
            self.startup_bytes -= frame.au.data.capacity();
            match self.es.video(frame.dts, frame.pts, frame.au, frame.now) {
                Ok(update) => self.pending.merge(update),
                Err(error) => return Err(self.refuse_startup(error)),
            }
            if let Some(muxer) = self.es.muxer_mut() {
                // The immutable A/V config and one real IDR are known. Open
                // metadata only, so AAC can precede replayed video cuts. No
                // duration or servable part is produced by this preparation.
                muxer.prepare_startup_audio(frame.now);
            }
        }
        Ok(())
    }

    fn release_failed_input(&mut self) {
        // Dropping the reader releases ALL private upstream PES Vecs at once,
        // including previously advertised bounded reservations on other PIDs.
        self.pes = None;
        let mut input = self.input.lock().expect("input lock is private to ingest");
        input.buf = Vec::new();
        input.start = 0;
        input.starved = false;
    }

    fn drain(&mut self, now: SystemTime, terminal: bool) -> Result<(), IngestError> {
        loop {
            match self
                .pes
                .as_mut()
                .expect("PES reader restored after reset")
                .read_pes_packet()
            {
                Ok(Some(pes)) => {
                    if terminal {
                        let feed = self
                            .pes
                            .as_ref()
                            .expect("PES reader present")
                            .ts_packet_reader();
                        // ReadPesPacket has now reached real EOF. All pending
                        // returns, including unbounded video, need validation.
                        let ended = feed.input.lock().map(|i| i.ended).unwrap_or(true);
                        if ended && !feed.terminal_aac(&pes) {
                            self.pending.media_loss |= (0xc0..=0xdf)
                                .contains(&pes.header.stream_id.as_u8())
                                && feed
                                    .boundaries
                                    .values()
                                    .any(|b| b.header == pes.header && b.expected.is_some());
                            continue;
                        }
                        if ended && !complete_adts(&pes.data) {
                            // Keep any individually validated AAC prefix, but
                            // never call a malformed/truncated ADTS tail clean.
                            self.pending.media_loss = true;
                        }
                        if ended && pes.header.pts.is_none() {
                            self.pending.media_loss = true;
                            continue;
                        }
                    }
                    self.on_pes(pes, now)?;
                }
                Ok(None) => break,
                Err(_) => {
                    let limited = self.input.lock().map(|i| i.limit_exceeded).unwrap_or(false);
                    if limited {
                        if self.startup_replaying {
                            self.discard_startup_catchup(now);
                        }
                        self.release_failed_input();
                        return Err(IngestError::InputLimitExceeded);
                    }
                    let reset = self
                        .input
                        .lock()
                        .map(|mut input| std::mem::take(&mut input.reset_pes))
                        .unwrap_or(false);
                    if reset {
                        if self.waiting_for_startup_audio() || self.startup_replaying {
                            self.discard_startup_catchup(now);
                        }
                        let mut feed = self
                            .pes
                            .take()
                            .expect("PES reader present")
                            .into_ts_packet_reader();
                        feed.boundaries.clear();
                        feed.pending_bytes = 0;
                        self.pes = Some(PesPacketReader::new(feed));
                        self.waiting_keyframe = true;
                        // Damage in the stream, not a clock jump: the
                        // muxer holds the last frame across the wait.
                        if let Some(muxer) = self.es.muxer_mut() {
                            muxer.hold_next_gap();
                        }
                        continue;
                    }
                    let starved = self.input.lock().map(|i| i.starved).unwrap_or(true);
                    if starved {
                        break;
                    }
                    // A malformed PES: its data is gone, the next one is fine.
                    if self.waiting_for_startup_audio() || self.startup_replaying {
                        self.discard_startup_catchup(now);
                    }
                    if let Ok(mut input) = self.input.lock() {
                        input.dropped += 1;
                    }
                }
            }
        }
        Ok(())
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
        let up = self.startup_video(dts, pts, au, now)?;
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
        let mut touched_startup = self.startup_replaying;
        let result = (|| {
            let mut elapsed_samples = 0i64;
            for (config, payload) in codec::adts_frames(data) {
                if self.waiting_for_startup_audio() {
                    self.startup_audio(config)?;
                    touched_startup |= self.startup_replaying;
                }
                let offset =
                    i128::from(elapsed_samples) * 90_000 / i128::from(config.sample_rate.max(1));
                let timestamp = pts
                    .checked_add(i64::try_from(offset).map_err(|_| IngestError::InvalidTimestamp)?)
                    .ok_or(IngestError::InvalidTimestamp)?;
                if self.startup_replaying {
                    let fits =
                        self.es
                            .origin
                            .zip(self.es.muxer())
                            .is_some_and(|(origin, muxer)| {
                                timestamp.checked_sub(origin).is_some_and(|relative| {
                                    let (bytes, frames) =
                                        muxer.startup_audio_projection(relative, payload.len());
                                    bytes <= STARTUP_AUDIO_BYTES && frames <= STARTUP_AUDIO_FRAMES
                                })
                            });
                    if !fits {
                        self.discard_startup_catchup(now);
                    }
                }
                self.pending
                    .merge(self.es.audio_at(timestamp, config, payload, now)?);
                if self.startup_replaying {
                    // The real successor AAC timestamp finalises its preceding
                    // frame. Replay immediately before checking the remaining
                    // retained span; no guessed video duration is introduced.
                    self.replay_startup_video()?;
                    if self.startup_replaying
                        && self
                            .es
                            .muxer()
                            .and_then(Muxer::startup_audio_span)
                            .is_some_and(|span| span > STARTUP_AUDIO_TICKS)
                    {
                        self.discard_startup_catchup(now);
                    }
                }
                elapsed_samples += i64::from(AAC_FRAME_SAMPLES);
            }
            Ok::<(), IngestError>(())
        })();
        if let Err(error) = result {
            return Err(if touched_startup {
                self.refuse_startup(error)
            } else {
                error
            });
        }
        Ok(())
    }
}

fn complete_adts(mut data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    while !data.is_empty() {
        let Ok(header) = adts_reader::AdtsHeader::from_bytes(data) else {
            return false;
        };
        let length = usize::from(header.frame_length());
        if length == 0
            || length > data.len()
            || header.number_of_raw_data_blocks_in_frame() != 1
            || header.payload().is_err()
        {
            return false;
        }
        data = &data[length..];
    }
    true
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
        track_samples(ing, 1)
    }

    fn track_samples(ing: &TsIngest, track_id: u32) -> Vec<Vec<u8>> {
        use mp4_atom::Decode;
        let muxer = ing.muxer().expect("muxer");
        let mut samples = Vec::new();
        for id in 0..muxer.next_part_id() {
            let crate::PartLookup::Found(part) = muxer.part(id) else {
                continue;
            };
            let moof = mp4_atom::Moof::decode(&mut part.data.as_ref()).expect("fragment");
            for track in moof
                .traf
                .iter()
                .filter(|track| track.tfhd.track_id == track_id)
            {
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

    /// A lost packet inside the keyframe at 2 s withholds video until the
    /// keyframe at 3 s, a gap of just over a second. The timeline is intact,
    /// so the muxer holds the last good frame: no break, and the audio over
    /// the gap is kept rather than cut out with the video.
    #[test]
    fn video_withheld_after_loss_keeps_the_audio_under_it() {
        let packets: Vec<&[u8]> = LOSS_FIXTURE.chunks_exact(TS_SIZE).collect();
        let video = |p: &[u8]| (u16::from(p[1] & 0x1f) << 8 | u16::from(p[2])) == 256;
        let keyframe =
            |p: &[u8]| video(p) && p[1] & 0x40 != 0 && p[3] & 0x20 != 0 && p[5] & 0x40 != 0;
        let keyframes: Vec<usize> = (0..packets.len())
            .filter(|&i| keyframe(packets[i]))
            .collect();
        assert!(keyframes.len() >= 5, "1 s keyframes over six seconds");
        let lost = (keyframes[2] + 1..packets.len())
            .find(|&i| video(packets[i]))
            .expect("the keyframe spans several packets");
        assert!(lost < keyframes[3]);
        let damaged: Vec<u8> = packets
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != lost)
            .flat_map(|(_, packet)| packet.iter().copied())
            .collect();
        let clean = loss_ingest(LOSS_FIXTURE);
        let ing = loss_ingest(&damaged);
        assert!(ing.dropped_packets() > 0);
        let muxer = ing.muxer().expect("muxer");
        assert_eq!(muxer.breaks(), 0, "a loss gap is held, not broken");
        let (video_kept, video_all) = (video_samples(&ing).len(), video_samples(&clean).len());
        assert!(video_kept + 25 < video_all, "the damaged GOP is withheld");
        // A break would have cut the gap's AAC frames out with the video.
        assert_eq!(track_samples(&ing, 2), track_samples(&clean, 2));
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
