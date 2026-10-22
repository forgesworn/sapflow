//! The LL-HLS segmenter: samples in, parts and segments out.
//!
//! For video-led streams, follows gohlslib v2.4.0 (`muxer_segmenter.go`,
//! `muxer_stream.go`) where it keeps Apple's rules: segments are cut on a keyframe once
//! the minimum duration has passed, parts are cut on the video timeline.
//! Each sample is held until the next one on its track arrives, which gives
//! its duration. Unlike gohlslib, `PART-TARGET` is fixed once the first
//! segment is complete (`PartTiming`), parts are closed against it, a frame
//! too long to fit is clamped, and audio is split between parts by its
//! timestamp. `EXT-X-TARGETDURATION` is fixed by the first segment too; a
//! long stall, a late keyframe or a jump in time ends the segment instead
//! (a break): video waits for the next keyframe, output timestamps are
//! rebased so they carry on where the last segment ended, and the next
//! segment is marked as a discontinuity.
//!
//! Explicit AAC-only streams instead use complete 1024-sample frames to cut
//! independent parts and segments on a rational audio sample clock.
//!
//! No I/O and no clock: the caller passes the wall-clock time for
//! `EXT-X-PROGRAM-DATE-TIME`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::{Bytes, BytesMut};

use crate::codec::{AacConfig, H264Config};
use crate::fmp4::{self, AUDIO_TRACK, Sample, TrackRun, VIDEO_TIMESCALE, VIDEO_TRACK};
use crate::time::{duration_to_ticks, ticks_to_duration};
use crate::{PartTiming, TargetDuration, TrackSet};

/// fMP4 decode times must not be negative. gohlslib shifts everything by
/// 10 s (`fmp4StartDTS`) because its timeline can start before the first
/// keyframe; ours starts at the first keyframe's DTS and drops anything
/// earlier, so no shift is needed and recordings start at zero.
const START_OFFSET: Duration = Duration::ZERO;

/// iOS needs the playlist to open with this many gap segments.
pub const LEADING_GAPS: u64 = 7;

/// Refuse to buffer a segment larger than this (no keyframes, or abuse).
const MAX_SEGMENT_BYTES: usize = 64 * 1024 * 1024;

/// Frame durations kept for choosing the part timing.
const MAX_LEARNED_FRAMES: usize = 1024;

/// A gap between video frames (forwards or back) at which the segment ends
/// and video waits for a keyframe. Shorter stalls (one Larix frame lasted
/// 0.367 s over Wi-Fi) are clamped into a held frame instead, which costs
/// no video; this one second is under the headroom `TargetDuration` leaves,
/// so a single clamped stall cannot by itself push a segment past it, and
/// the length rule in `push_video` catches several.
const BREAK_GAP: i64 = 90_000;

/// AAC-LC frames are 1024 samples. A longer gap between two (the audio
/// stopped) is not given to the frame before it.
const AAC_FRAME: i64 = 1024;

/// Settings for a [`Muxer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxerConfig {
    /// A segment is cut at the first keyframe after it has lasted this long,
    /// or at a complete AAC frame boundary for audio-only. Default 1 s.
    pub segment_min: Duration,
    /// The shortest nominal part, from which `PART-TARGET` is derived (see
    /// [`PartTiming`]). Default 200 ms.
    pub part_min: Duration,
    /// Segments kept in the playlist, gaps included. At least 7.
    pub segment_count: usize,
}

impl Default for MuxerConfig {
    fn default() -> Self {
        Self {
            segment_min: Duration::from_secs(1),
            part_min: Duration::from_millis(200),
            segment_count: 24,
        }
    }
}

/// Immutable initialization identity, shared by every part of one codec epoch.
#[derive(Debug)]
pub struct Initialization {
    /// Codec epoch number: 0 for the first, one more after each
    /// reconfiguration. It names the init segment's URI.
    pub id: u64,
    /// The init segment: `ftyp` + `moov`.
    pub data: Bytes,
}

/// One LL-HLS partial segment (`EXT-X-PART`): a `moof` + `mdat` fragment.
#[derive(Debug)]
pub struct Part {
    /// The init segment of the codec epoch this part belongs to.
    pub initialization: Arc<Initialization>,
    /// Counts parts across the whole stream, not within a segment. It names the
    /// part's URI.
    pub id: u64,
    /// What the playlist says: never above `PART-TARGET`.
    pub duration: Duration,
    /// Media time from this part's start to the next part's start. Equal to
    /// `duration` unless a long frame was clamped to fit the part target, in
    /// which case it includes the gap that left before the next frame. The
    /// spans of a segment's parts add up to the segment's duration.
    pub span: Duration,
    /// Whether the part can be decoded independently (`INDEPENDENT=YES`): a
    /// video keyframe or complete audio-only AAC frames.
    pub independent: bool,
    /// The part's `moof` + `mdat` fragment.
    pub data: Bytes,
}

/// One complete segment: its parts, joined.
#[derive(Debug)]
pub struct Segment {
    /// The init segment of the codec epoch this segment belongs to.
    pub initialization: Arc<Initialization>,
    /// The media sequence number. It names the segment's URI; a stream's first
    /// segment is [`LEADING_GAPS`].
    pub id: u64,
    /// How long the segment lasts (`EXTINF`).
    pub duration: Duration,
    /// Wall-clock start of the segment, as `EXT-X-PROGRAM-DATE-TIME` prints it.
    pub start: SystemTime,
    /// Follows a break (a stall, a late keyframe, a jump in time): the
    /// playlist puts `EXT-X-DISCONTINUITY` before it.
    pub discontinuity: bool,
    /// The segment's parts, in order.
    pub parts: Vec<Arc<Part>>,
    /// The whole segment: its parts' fragments, concatenated.
    pub data: Bytes,
}

/// One slot in the playlist window.
#[derive(Debug, Clone)]
pub enum Entry {
    /// A placeholder for a segment that does not exist, written as
    /// `EXT-X-GAP`. iOS needs [`LEADING_GAPS`] of them before the first real
    /// segment.
    Gap {
        /// The `EXTINF` duration; the leading gaps copy the first segment's.
        duration: Duration,
    },
    /// A real segment.
    Segment(Arc<Segment>),
}

impl Entry {
    /// The entry's `EXTINF` duration.
    pub fn duration(&self) -> Duration {
        match self {
            Self::Gap { duration } => *duration,
            Self::Segment(s) => s.duration,
        }
    }
}

/// What a push changed, so the caller knows whether to wake viewers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Update {
    /// A part was closed.
    pub part: bool,
    /// A segment was closed.
    pub segment: bool,
    /// A reconnected encoder starts a new initialization/recording epoch.
    /// The presentation's numbering, timing targets and retained media persist.
    pub restarted: bool,
    /// Definite input continuity failure within a run. Intentional detach or
    /// codec-epoch replacement is not loss; a file containing a failed run
    /// must not later be presented as a complete recording.
    pub media_loss: bool,
}

impl Update {
    /// Whether anything changed.
    pub fn any(self) -> bool {
        self.part || self.segment || self.restarted || self.media_loss
    }

    /// Folds `other` in: each flag is set if it is set in either.
    pub fn merge(&mut self, other: Update) {
        self.part |= other.part;
        self.segment |= other.segment;
        self.restarted |= other.restarted;
        self.media_loss |= other.media_loss;
    }
}

/// The answer to a media playlist request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Serve the playlist now.
    Ready,
    /// Hold the request until the segment or part it names exists, or until
    /// the stream has a first segment to serve.
    Wait,
    /// The request cannot be met: `_HLS_part` without `_HLS_msn`, or an
    /// `_HLS_msn` more than one segment past the one being written.
    BadRequest,
}

/// The answer to a part request.
#[derive(Debug, Clone)]
pub enum PartLookup {
    /// The part, which is still in the window.
    Found(Arc<Part>),
    /// The part the preload hint points at: wait for it.
    Next,
    /// The part is not in the window: too old, or never produced.
    Missing,
}

/// Why the muxer refused media.
#[derive(Debug, Clone)]
pub enum MuxError {
    /// The open segment passed 64 MiB: no keyframe in time, or a hostile
    /// stream.
    SegmentTooLarge,
    /// The requested topology has no supported media tracks.
    InvalidTracks,
    /// The AAC configuration or resulting part timing is unsupported.
    InvalidAudio,
    /// An AAC timestamp or decode-time calculation is outside its valid range.
    Timestamp,
}

#[derive(Debug)]
struct Pending {
    dts: i64,
    cts: i32,
    sync: bool,
    data: Bytes,
}

/// A video frame whose duration is known (the next one has arrived).
#[derive(Debug)]
struct Frame {
    dts: i64,
    next_dts: i64,
    cts: i32,
    sync: bool,
    data: Bytes,
}

#[derive(Debug)]
struct OpenSegment {
    initialization: Arc<Initialization>,
    id: u64,
    start_dts: i64,
    start: SystemTime,
    /// One wall-clock/media-clock association for this continuous run.
    /// Packet arrival jitter must not create a new association per segment.
    wall_origin: (i64, SystemTime),
    parts: Vec<Arc<Part>>,
    bytes: usize,
    discontinuity: bool,
}

#[derive(Debug)]
struct OpenPart {
    id: u64,
    start_dts: i64,
    video: Vec<Sample>,
    video_base: u64,
    audio: Vec<Sample>,
    /// Per audio sample: its start on the 90 kHz timeline, and its decode
    /// time in the audio timescale.
    audio_times: Vec<(i64, u64)>,
    independent: bool,
}

impl OpenPart {
    fn new(id: u64, start_dts: i64) -> Self {
        Self {
            id,
            start_dts,
            video: Vec::new(),
            video_base: 0,
            audio: Vec::new(),
            audio_times: Vec::new(),
            independent: false,
        }
    }
}

/// Audio decode time is an exact count of 1024-sample AAC-LC frames. Input
/// timestamps only detect a break; rounding a 90 kHz timestamp on every frame
/// must not accumulate drift at 44.1 kHz.
#[derive(Debug)]
struct AudioRun {
    input_origin: i64,
    frames: i64,
    next_sample: i64,
}

/// The segmenter for one stream: media samples in, parts and segments out, with
/// a bounded recent window retained for playlists and HTTP. Audio-only requires
/// the explicit checked [`Muxer::audio_only`] constructor.
#[derive(Debug)]
pub struct Muxer {
    cfg: MuxerConfig,
    video: Option<H264Config>,
    audio: Option<AacConfig>,
    audio_run: Option<AudioRun>,
    audio_end: Option<i64>,
    init: Arc<Initialization>,
    epoch_first_part_id: u64,

    pending_video: Option<Pending>,
    pending_audio: Option<Pending>,

    entries: VecDeque<Entry>,
    media_sequence: u64,
    /// Parts of every real segment in `entries`, oldest first.
    parts: VecDeque<Arc<Part>>,
    next_segment: Option<OpenSegment>,
    part: Option<OpenPart>,
    next_part_id: u64,

    /// Set when the first segment completes; never changes after.
    timing: Option<PartTiming>,
    /// The first segment's video frames, held until the timing is chosen
    /// and then cut into parts. Nothing is served before then.
    learning: Vec<Frame>,
    /// Set by the first segment; never changes after.
    target: Option<TargetDuration>,
    /// Added to every input timestamp (90 kHz): changes at each break so
    /// output time carries on where the last segment ended.
    offset: i64,
    /// After a break: the output time the next keyframe is moved to.
    rebase_to: Option<i64>,
    /// After a break: audio before this output time is dropped.
    audio_floor: i64,
    /// The next segment to open follows a break.
    discontinuity_next: bool,
    /// Discontinuities that have left the window.
    discontinuity_sequence: u64,
    /// Breaks since the stream started (stalls, jumps, reconnects).
    breaks: u64,
}

impl Muxer {
    /// A muxer for H.264 video and optional AAC audio. The init segment (codec
    /// epoch 0) is built here, and `segment_count` is raised to at least
    /// [`LEADING_GAPS`].
    pub fn new(cfg: MuxerConfig, video: H264Config, audio: Option<AacConfig>) -> Self {
        let init = fmp4::init_segment(&video, audio.as_ref());
        Self::with_tracks(cfg, Some(video), audio, init)
    }

    /// An explicit AAC-only declaration is required at ingest; an AAC sequence
    /// header alone must not turn an ordinary video publication into radio.
    pub fn audio_only(cfg: MuxerConfig, audio: AacConfig) -> Result<Self, MuxError> {
        Self::validate_audio(audio)?;
        let init = fmp4::init_segment_for_tracks(None, Some(&audio), Duration::ZERO)
            .ok_or(MuxError::InvalidTracks)?;
        Ok(Self::with_tracks(cfg, None, Some(audio), init))
    }

    fn validate_audio(audio: AacConfig) -> Result<(), MuxError> {
        if audio.object_type != 2
            || !(1..=2).contains(&audio.channels)
            || audio.sample_rate > u32::from(u16::MAX)
            || AacConfig::from_audio_specific_config(&audio.audio_specific_config()) != Some(audio)
        {
            return Err(MuxError::InvalidAudio);
        }
        Ok(())
    }

    fn with_tracks(
        cfg: MuxerConfig,
        video: Option<H264Config>,
        audio: Option<AacConfig>,
        init: Bytes,
    ) -> Self {
        Self {
            cfg: MuxerConfig {
                segment_count: cfg.segment_count.max(LEADING_GAPS as usize),
                ..cfg
            },
            video,
            audio,
            audio_run: None,
            audio_end: None,
            init: Arc::new(Initialization { id: 0, data: init }),
            epoch_first_part_id: 0,
            pending_video: None,
            pending_audio: None,
            entries: VecDeque::new(),
            media_sequence: 0,
            parts: VecDeque::new(),
            next_segment: None,
            part: None,
            next_part_id: 0,
            timing: None,
            learning: Vec::new(),
            target: None,
            offset: duration_to_ticks(START_OFFSET, VIDEO_TIMESCALE),
            rebase_to: None,
            audio_floor: 0,
            discontinuity_next: false,
            discontinuity_sequence: 0,
            breaks: 0,
        }
    }

    /// The current video configuration, or `None` for an audio-only epoch.
    pub fn video(&self) -> Option<&H264Config> {
        self.video.as_ref()
    }

    /// The non-empty track topology represented by the current initialization.
    pub fn tracks(&self) -> TrackSet {
        if self.video.is_none() {
            TrackSet::AudioOnly
        } else if self.audio.is_some() {
            TrackSet::AudioVideo
        } else {
            TrackSet::VideoOnly
        }
    }

    /// The audio track's configuration in the current codec epoch, or `None`
    /// for video only.
    pub fn audio(&self) -> Option<&AacConfig> {
        self.audio.as_ref()
    }

    /// The current init segment: `ftyp` + `moov`.
    pub fn init(&self) -> &Bytes {
        &self.init.data
    }

    /// Reconfigure at a random-access boundary without resetting playlist or
    /// decode-time identities. Old initializations live only while referenced
    /// by the bounded segment/part window (or an in-flight response).
    pub fn reconfigure(&mut self, video: H264Config, audio: Option<AacConfig>, now: SystemTime) {
        // Existing H.264 constructor semantics remain unchanged.
        let init = fmp4::init_segment(&video, audio.as_ref());
        self.replace_tracks(Some(video), audio, init, now);
    }

    /// Replaces topology at a connection boundary, preserving playlist and
    /// decode-time identities while sealing accepted media under its old init.
    /// Audio-only configuration and part timing are checked before mutation;
    /// neither track may be absent simultaneously. Forward the returned update
    /// so recorders consume the sealed old parts before a new codec epoch.
    pub fn reconfigure_tracks(
        &mut self,
        video: Option<H264Config>,
        audio: Option<AacConfig>,
        now: SystemTime,
    ) -> Result<Update, MuxError> {
        if video.is_none() {
            let audio = audio.ok_or(MuxError::InvalidTracks)?;
            Self::validate_audio(audio)?;
            self.check_audio_part_timing(audio)?;
        }
        let init = if let Some(video) = &video {
            fmp4::init_segment(video, audio.as_ref())
        } else {
            fmp4::init_segment_for_tracks(None, audio.as_ref(), Duration::ZERO)
                .ok_or(MuxError::InvalidTracks)?
        };
        Ok(self.replace_tracks(video, audio, init, now))
    }

    fn replace_tracks(
        &mut self,
        video: Option<H264Config>,
        audio: Option<AacConfig>,
        data: Bytes,
        now: SystemTime,
    ) -> Update {
        let update = self.interrupt(now);
        self.pending_audio = None;
        self.audio_run = None;
        self.audio_end = None;
        self.epoch_first_part_id = self.next_part_id;
        self.init = Arc::new(Initialization {
            id: self.init.id.checked_add(1).expect("codec epoch exhausted"),
            data,
        });
        self.video = video;
        self.audio = audio;
        update
    }

    /// The id of the first part cut under the current init segment (zero until
    /// the first [`Muxer::reconfigure`]).
    pub fn epoch_first_part_id(&self) -> u64 {
        self.epoch_first_part_id
    }

    /// The init segment of codec epoch `id`, if it is the current one or a part
    /// in the window still uses it.
    pub fn initialization(&self, id: u64) -> Option<&Bytes> {
        if self.init.id == id {
            return Some(&self.init.data);
        }
        self.parts
            .iter()
            .find(|part| part.initialization.id == id)
            .map(|part| &part.initialization.data)
    }

    /// The id of the current codec epoch.
    pub fn current_initialization_id(&self) -> u64 {
        self.init.id
    }

    /// Feeds one video access unit. `dts` and `pts` are 90 kHz, relative to
    /// the first keyframe (never negative for real streams).
    pub fn push_video(
        &mut self,
        dts: i64,
        pts: i64,
        sync: bool,
        data: Bytes,
        now: SystemTime,
    ) -> Result<Update, MuxError> {
        if self.video.is_none() {
            return Err(MuxError::InvalidTracks);
        }
        if self.pending_video.is_none() && self.next_segment.is_none() && !sync {
            return Ok(Update::default());
        }
        let cts = (pts - dts).clamp(0, i64::from(i32::MAX)) as i32;
        let mut dts = dts.saturating_add(self.offset);
        if self.pending_video.is_none() && self.next_segment.is_none() {
            // The keyframe that resumes the stream after a break.
            if let Some(to) = self.rebase_to.take() {
                self.rebase(dts, to);
                dts = to;
            }
        }
        let mut next = Pending {
            dts,
            cts,
            sync,
            data,
        };
        if next.dts < 0 {
            return Ok(Update::default());
        }
        let Some(cur) = self.pending_video.take() else {
            self.pending_video = Some(next);
            return Ok(Update::default());
        };
        if (next.dts - cur.dts).abs() > BREAK_GAP {
            return Ok(self.break_stream(cur.dts, next, now));
        }
        if next.dts < cur.dts {
            next.dts = cur.dts;
        }
        let next_dts = next.dts;
        let next_sync = next.sync;

        if self.next_segment.is_none() {
            let id = if self.entries.is_empty() {
                LEADING_GAPS
            } else {
                self.next_segment_id()
            };
            // A true break/reconnect establishes a new wall-clock association.
            // Retain its real outage gap, but never overlap the preceding run
            // if the host clock stepped backwards or input time ran ahead.
            let start = match self.entries.back() {
                Some(Entry::Segment(previous)) => now.max(previous.start + previous.duration),
                _ => now,
            };
            self.next_segment = Some(OpenSegment {
                initialization: self.init.clone(),
                id,
                start_dts: cur.dts,
                start,
                wall_origin: (cur.dts, start),
                parts: Vec::new(),
                bytes: 0,
                discontinuity: std::mem::take(&mut self.discontinuity_next),
            });
            self.part = Some(OpenPart::new(self.next_part_id, cur.dts));
        }
        // Never let a segment run past the target duration: with no
        // keyframe in time, end it here.
        if let (Some(t), Some(seg)) = (self.target, self.next_segment.as_ref())
            && next_dts - seg.start_dts > t.limit_ticks()
        {
            return Ok(self.break_stream(cur.dts, next, now));
        }
        self.pending_video = Some(next);

        let seg = self
            .next_segment
            .as_mut()
            .ok_or(MuxError::SegmentTooLarge)?;
        seg.bytes += cur.data.len();
        if seg.bytes > MAX_SEGMENT_BYTES {
            return Err(MuxError::SegmentTooLarge);
        }
        let since_segment = ticks_to_duration(next_dts - seg.start_dts, VIDEO_TIMESCALE);
        let ends_segment = next_sync && since_segment >= self.cfg.segment_min;
        let frame = Frame {
            dts: cur.dts,
            next_dts,
            cts: cur.cts,
            sync: cur.sync,
            data: cur.data,
        };

        let mut update = Update::default();
        if self.timing.is_some() {
            update.part = self.place_video(frame, ends_segment);
        } else {
            self.learning.push(frame);
            if ends_segment {
                self.choose_part_timing();
            }
        }
        if ends_segment {
            self.rotate_segment(next_dts, now, true);
            update.part = true;
            update.segment = true;
        }
        Ok(update)
    }

    /// Ends the stream's current run at `end` (output time), dropping the
    /// frame there: a hole or jump of over `BREAK_GAP` before `next`, or a
    /// segment about to outrun the target duration. The open part and
    /// segment close at `end` (a segment's last part may be short), video
    /// waits for a keyframe, and that keyframe is moved to `end` so output
    /// time carries straight on; the playlist marks the new segment as a
    /// discontinuity, since media was lost. A segment cannot be cut on a
    /// frame that is not a keyframe: the multivariant playlist promises
    /// `EXT-X-INDEPENDENT-SEGMENTS`.
    fn break_stream(&mut self, end: i64, next: Pending, now: SystemTime) -> Update {
        if self.timing.is_none() {
            // Nothing is served before the first segment completes: start
            // over, so the targets come from a whole segment.
            tracing::warn!("timestamps broke before the first segment was complete; starting over");
            self.learning.clear();
            self.next_segment = None;
            self.part = None;
            self.pending_video = next.sync.then_some(next);
            return Update::default();
        }
        tracing::warn!(
            gap_ms = (next.dts - end) / 90,
            keyframe = next.sync,
            "media broke off (stall, late keyframe or jump in time); new segment from the next keyframe, marked as a discontinuity"
        );
        let closed = self.close_run(end, now);
        if next.sync {
            let mut next = next;
            self.rebase(next.dts, end);
            next.dts = end;
            self.pending_video = Some(next);
        } else {
            self.rebase_to = Some(end);
        }
        Update {
            part: closed,
            segment: closed,
            ..Update::default()
        }
    }

    /// The encoder went away; another connection may carry on the stream.
    /// Ends the current run as a break does, at the last frame (which is
    /// dropped, its duration unknown): whatever video arrives next, from
    /// any clock, waits for a keyframe, is moved to carry straight on in
    /// output time, and opens a segment marked as a discontinuity.
    /// Audio-only seals every accepted complete AAC frame, whose duration is
    /// known, then rebases the next run after a discontinuity.
    pub fn interrupt(&mut self, now: SystemTime) -> Update {
        if self.video.is_none() {
            self.audio_run = None;
            let Some(end) = self.audio_end.take() else {
                return Update::default();
            };
            let closed = self.close_run(end, now);
            self.rebase_to = Some(end);
            return Update {
                part: closed,
                segment: closed,
                ..Update::default()
            };
        }
        let Some(cur) = self.pending_video.take() else {
            return Update::default();
        };
        if self.timing.is_none() {
            self.learning.clear();
            self.next_segment = None;
            self.part = None;
            return Update::default();
        }
        let closed = self.close_run(cur.dts, now);
        self.rebase_to = Some(cur.dts);
        Update {
            part: closed,
            segment: closed,
            ..Update::default()
        }
    }

    /// Closes the open part and segment at `end`, drops the held frame,
    /// and marks the next segment as a discontinuity. Returns whether a
    /// segment was closed.
    fn close_run(&mut self, end: i64, now: SystemTime) -> bool {
        if self
            .part
            .as_ref()
            .is_some_and(|p| !p.video.is_empty() || (self.video.is_none() && !p.audio.is_empty()))
        {
            self.rotate_part(end, end);
        }
        // An open part with no video (audio alone) is dropped.
        self.part = None;
        let closed = self
            .next_segment
            .as_ref()
            .is_some_and(|s| !s.parts.is_empty());
        if closed {
            self.rotate_segment(end, now, false);
        } else {
            self.next_segment = None;
        }
        self.pending_video = None;
        self.discontinuity_next = true;
        self.breaks += 1;
        closed
    }

    /// Moves output time so that input now at output `from` lands at `to`.
    fn rebase(&mut self, from: i64, to: i64) {
        self.offset += to - from;
        self.audio_floor = to;
        // Its timestamp was taken with the old offset.
        self.pending_audio = None;
    }

    /// Fixes the part timing from the first segment's frames and cuts that
    /// segment into parts.
    fn choose_part_timing(&mut self) {
        let durations: Vec<u32> = self
            .learning
            .iter()
            .take(MAX_LEARNED_FRAMES)
            .map(|f| (f.next_dts - f.dts).clamp(0, i64::from(u32::MAX)) as u32)
            .collect();
        let timing = PartTiming::new(self.cfg.part_min, &durations);
        tracing::debug!(
            part_target_ms = timing.target_duration().as_millis() as u64,
            "part timing fixed"
        );
        self.timing = Some(timing);
        let frames = std::mem::take(&mut self.learning);
        let last = frames.len().saturating_sub(1);
        for (i, f) in frames.into_iter().enumerate() {
            self.place_video(f, i == last);
        }
    }

    /// Adds a frame to the open part and closes the part when it has reached
    /// the cut point, when `ends_segment`, or when the frame had to be
    /// clamped. Returns whether a part was closed.
    ///
    /// A frame longer than the room left before the part target (a Wi-Fi
    /// stall, dropped frames) is written with its duration cut to fit, so
    /// the part is exactly the target; the next part's decode time carries
    /// the true time. The alternatives break Apple's part rules: letting
    /// the part run long exceeds `PART-TARGET`, and closing the part before
    /// the frame leaves it under 85%.
    fn place_video(&mut self, f: Frame, ends_segment: bool) -> bool {
        let (Some(t), Some(part)) = (self.timing, self.part.as_mut()) else {
            return false;
        };
        if part.video.is_empty() {
            part.video_base = f.dts as u64;
        }
        if f.sync {
            part.independent = true;
        }
        let start = part.start_dts;
        let full = f.next_dts - f.dts;
        let room = (start + t.target - f.dts).max(0);
        let (duration, end) = if full > room {
            tracing::debug!(
                frame_ms = full / 90,
                fits_ms = room / 90,
                "long frame clamped to fit the part target"
            );
            (room, start + t.target)
        } else {
            (full, f.next_dts)
        };
        part.video.push(Sample {
            duration: duration as u32,
            cts: f.cts,
            sync: f.sync,
            data: f.data,
        });
        if ends_segment || duration < full || end - start >= t.cut {
            self.rotate_part(end, f.next_dts);
            return true;
        }
        false
    }

    fn check_audio_part_timing(&self, audio: AacConfig) -> Result<PartTiming, MuxError> {
        let rate = i64::from(audio.sample_rate);
        let frame = (AAC_FRAME * i64::from(VIDEO_TIMESCALE) + rate - 1) / rate;
        let timing = self
            .timing
            .unwrap_or_else(|| PartTiming::new(self.cfg.part_min, &[frame as u32]));
        // AAC frames cannot be split or shortened to fit an inherited target.
        let frame_ticks = i128::from(AAC_FRAME) * i128::from(VIDEO_TIMESCALE);
        let count = (i128::from(timing.cut) * i128::from(rate) + frame_ticks - 1) / frame_ticks;
        let end = (count * frame_ticks + i128::from(rate) - 1) / i128::from(rate);
        if end + 1 > i128::from(timing.target) {
            return Err(MuxError::InvalidAudio);
        }
        Ok(timing)
    }

    /// Explicit audio-only mode. Each accepted AAC-LC frame contributes exactly
    /// 1024 sample ticks; no rounded per-frame 90 kHz increment is accumulated.
    /// The wall clock is supplied only for run anchoring, as on the video path.
    pub fn push_audio_at(
        &mut self,
        pts: i64,
        data: Bytes,
        now: SystemTime,
    ) -> Result<Update, MuxError> {
        if self.video.is_some() {
            self.push_audio(pts, data);
            return Ok(Update::default());
        }
        let audio = self.audio.ok_or(MuxError::InvalidTracks)?;
        Self::validate_audio(audio)?;
        if pts < 0 || data.is_empty() {
            return Err(MuxError::Timestamp);
        }
        let rate = i64::from(audio.sample_rate);
        let timing = self.check_audio_part_timing(audio)?;
        let mut update = Update::default();
        if let Some(run) = &self.audio_run {
            let expected = i128::from(run.input_origin)
                + i128::from(run.frames) * i128::from(AAC_FRAME) * i128::from(VIDEO_TIMESCALE)
                    / i128::from(rate);
            // TS tick rounding and RTMP's millisecond timestamps are tolerated;
            // missing/repeated AAC frames start a discontinuous new run.
            if (i128::from(pts) - expected).abs() > 180 {
                update.merge(self.interrupt(now));
                update.media_loss = true;
            }
        }
        if self.audio_run.is_none() {
            let at = self
                .rebase_to
                .take()
                .unwrap_or(pts.checked_add(self.offset).ok_or(MuxError::Timestamp)?);
            if at < 0 {
                return Err(MuxError::Timestamp);
            }
            let next_sample = i64::try_from(
                (i128::from(at) * i128::from(rate) + i128::from(VIDEO_TIMESCALE) - 1)
                    / i128::from(VIDEO_TIMESCALE),
            )
            .map_err(|_| MuxError::Timestamp)?;
            self.audio_run = Some(AudioRun {
                input_origin: pts,
                frames: 0,
                next_sample,
            });
        }
        let run = self.audio_run.as_ref().ok_or(MuxError::Timestamp)?;
        let sample = run.next_sample;
        let next_sample = sample.checked_add(AAC_FRAME).ok_or(MuxError::Timestamp)?;
        let frames = run.frames.checked_add(1).ok_or(MuxError::Timestamp)?;
        let ticks = |n: i64| -> Result<i64, MuxError> {
            i64::try_from(i128::from(n) * i128::from(VIDEO_TIMESCALE) / i128::from(rate))
                .map_err(|_| MuxError::Timestamp)
        };
        let at = ticks(sample)?;
        let end = ticks(next_sample)?;
        if sample < 0 {
            return Err(MuxError::Timestamp);
        }
        if self.next_segment.is_none() {
            let id = if self.entries.is_empty() {
                LEADING_GAPS
            } else {
                self.next_segment_id()
            };
            let start = match self.entries.back() {
                Some(Entry::Segment(previous)) => now.max(previous.start + previous.duration),
                _ => now,
            };
            self.next_segment = Some(OpenSegment {
                initialization: self.init.clone(),
                id,
                start_dts: at,
                start,
                wall_origin: (at, start),
                parts: Vec::new(),
                bytes: 0,
                discontinuity: std::mem::take(&mut self.discontinuity_next),
            });
        }
        if self.part.is_none() {
            self.part = Some(OpenPart::new(self.next_part_id, at));
        }
        let seg = self.next_segment.as_mut().ok_or(MuxError::Timestamp)?;
        let bytes = seg
            .bytes
            .checked_add(data.len())
            .ok_or(MuxError::SegmentTooLarge)?;
        if bytes > MAX_SEGMENT_BYTES {
            return Err(MuxError::SegmentTooLarge);
        }
        let part = self.part.as_mut().ok_or(MuxError::Timestamp)?;
        if end - part.start_dts > timing.target {
            return Err(MuxError::InvalidAudio);
        }
        seg.bytes = bytes;
        part.independent = true;
        part.audio_times.push((at, sample as u64));
        part.audio.push(Sample {
            duration: AAC_FRAME as u32,
            cts: 0,
            sync: true,
            data,
        });
        self.timing = Some(timing);
        self.audio_end = Some(end);
        let run = self.audio_run.as_mut().ok_or(MuxError::Timestamp)?;
        run.next_sample = next_sample;
        run.frames = frames;
        let end_segment =
            ticks_to_duration(end - seg.start_dts, VIDEO_TIMESCALE) >= self.cfg.segment_min;
        if end_segment || end - part.start_dts >= timing.cut {
            self.rotate_part(end, end);
            update.part = true;
        }
        if end_segment {
            self.rotate_segment(end, now, true);
            update.segment = true;
        }
        Ok(update)
    }

    /// Feeds one AAC frame. `pts` is 90 kHz on the same timeline as video.
    pub fn push_audio(&mut self, pts: i64, data: Bytes) {
        // This compatibility path needs video to supply cuts and wall time.
        // Audio-only callers must use the checked, updating `push_audio_at`.
        if self.video.is_none() {
            return;
        }
        let Some(audio) = self.audio else { return };
        if pts < 0 {
            return;
        }
        let rate = audio.sample_rate;
        let pts = pts.saturating_add(self.offset);
        if pts < 0 {
            return;
        }
        let mut next = Pending {
            dts: pts.saturating_mul(i64::from(rate)) / i64::from(VIDEO_TIMESCALE),
            cts: 0,
            sync: true,
            data,
        };
        let Some(cur) = self.pending_audio.take() else {
            self.pending_audio = Some(next);
            return;
        };
        if next.dts < cur.dts {
            next.dts = cur.dts;
        }
        // Timestamp jitter is kept; a gap (the audio stopped) is not.
        let mut duration = next.dts - cur.dts;
        if duration > 3 * AAC_FRAME {
            duration = AAC_FRAME;
        }
        let duration = duration as u32;
        self.pending_audio = Some(next);

        // Audio waits for video to open the first part.
        let Some(part) = self.part.as_mut() else {
            return;
        };
        let at = cur.dts * i64::from(VIDEO_TIMESCALE) / i64::from(rate);
        if at < self.audio_floor {
            return;
        }
        part.audio_times.push((at, cur.dts as u64));
        part.audio.push(Sample {
            duration,
            cts: 0,
            sync: true,
            data: cur.data,
        });
    }

    /// Closes the open part at `end_dts` and opens the next at `next_start`
    /// (later than `end_dts` only after a clamped frame). Audio that starts
    /// at or after the end moves to the next part.
    fn rotate_part(&mut self, end_dts: i64, next_start: i64) {
        let Some(mut part) = self.part.take() else {
            return;
        };
        let split = part
            .audio_times
            .iter()
            .position(|&(at, _)| at >= end_dts)
            .unwrap_or(part.audio.len());
        let later_audio = part.audio.split_off(split);
        let later_times = part.audio_times.split_off(split);
        let data = fmp4::encode_part(
            (part.id + 1) as u32,
            &[
                TrackRun {
                    track_id: VIDEO_TRACK,
                    base_time: part.video_base,
                    samples: &part.video,
                },
                TrackRun {
                    track_id: AUDIO_TRACK,
                    base_time: part.audio_times.first().map_or(0, |t| t.1),
                    samples: &part.audio,
                },
            ],
        );
        let finished = Arc::new(Part {
            initialization: self.init.clone(),
            id: part.id,
            duration: ticks_to_duration(end_dts - part.start_dts, VIDEO_TIMESCALE),
            span: ticks_to_duration(next_start - part.start_dts, VIDEO_TIMESCALE),
            independent: part.independent,
            data,
        });
        if let Some(seg) = self.next_segment.as_mut() {
            seg.parts.push(finished.clone());
        }
        self.parts.push_back(finished);
        self.next_part_id += 1;
        let mut next = OpenPart::new(self.next_part_id, next_start);
        next.audio = later_audio;
        next.audio_times = later_times;
        self.part = Some(next);
    }

    /// Closes the open segment at `end_dts` and, unless the stream broke,
    /// opens the next. Its last part is already closed.
    fn rotate_segment(&mut self, end_dts: i64, _now: SystemTime, open_next: bool) {
        let Some(open) = self.next_segment.take() else {
            return;
        };
        let duration = ticks_to_duration(end_dts - open.start_dts, VIDEO_TIMESCALE);
        let mut data = BytesMut::with_capacity(open.parts.iter().map(|p| p.data.len()).sum());
        for p in &open.parts {
            data.extend_from_slice(&p.data);
        }
        let seg = Arc::new(Segment {
            initialization: open.initialization,
            id: open.id,
            duration,
            start: open.start,
            discontinuity: open.discontinuity,
            parts: open.parts,
            data: data.freeze(),
        });

        if self.entries.is_empty() {
            for _ in 0..LEADING_GAPS {
                self.entries.push_back(Entry::Gap { duration });
            }
        }
        self.entries.push_back(Entry::Segment(seg));
        while self.entries.len() > self.cfg.segment_count {
            if let Some(Entry::Segment(old)) = self.entries.pop_front() {
                for _ in 0..old.parts.len() {
                    self.parts.pop_front();
                }
                if old.discontinuity {
                    self.discontinuity_sequence += 1;
                }
            }
            self.media_sequence += 1;
        }
        if self.target.is_none() {
            let t = TargetDuration::for_first_segment(duration);
            tracing::info!(
                target_secs = t.secs(),
                "target duration fixed from the first segment; a segment longer than this is cut and video waits for the next keyframe (set the encoder's keyframe interval to at most this)"
            );
            self.target = Some(t);
        }

        self.next_segment = open_next.then(|| OpenSegment {
            initialization: self.init.clone(),
            id: open.id + 1,
            start_dts: end_dts,
            // Preserve one affine clock mapping, including fractional ticks;
            // do not accumulate rounded per-segment durations or arrival times.
            start: open.wall_origin.1
                + ticks_to_duration(end_dts - open.wall_origin.0, VIDEO_TIMESCALE),
            wall_origin: open.wall_origin,
            parts: Vec::new(),
            bytes: 0,
            discontinuity: false,
        });
    }

    // ---- the window, for playlists and HTTP ----

    /// The playlist window, oldest first: gaps and segments.
    pub fn entries(&self) -> &VecDeque<Entry> {
        &self.entries
    }

    /// `EXT-X-MEDIA-SEQUENCE`: the sequence number of the oldest entry.
    pub fn media_sequence(&self) -> u64 {
        self.media_sequence
    }

    /// `EXT-X-TARGETDURATION` in seconds; 1 until the first segment fixes it.
    pub fn target_duration(&self) -> u64 {
        self.target.map_or(1, TargetDuration::secs)
    }

    /// `EXT-X-DISCONTINUITY-SEQUENCE`.
    pub fn discontinuity_sequence(&self) -> u64 {
        self.discontinuity_sequence
    }

    /// Whether the segment being written follows a break.
    pub fn open_discontinuity(&self) -> bool {
        self.next_segment.as_ref().is_some_and(|s| s.discontinuity)
    }

    /// `PART-TARGET`: zero until the first segment is complete, then fixed.
    pub fn part_target(&self) -> Duration {
        self.timing
            .map_or(Duration::ZERO, PartTiming::target_duration)
    }

    /// Parts of the segment still being written.
    pub fn open_parts(&self) -> &[Arc<Part>] {
        self.next_segment.as_ref().map_or(&[], |s| &s.parts)
    }

    /// The id of the part now being written, which `EXT-X-PRELOAD-HINT` names.
    pub fn next_part_id(&self) -> u64 {
        self.next_part_id
    }

    fn next_segment_id(&self) -> u64 {
        self.media_sequence + self.entries.len() as u64
    }

    /// True once the first segment exists: the stream is servable.
    pub fn has_content(&self) -> bool {
        self.entries.len() > LEADING_GAPS as usize
    }

    /// Whether a media playlist request can be answered now
    /// (`_HLS_msn`, `_HLS_part` blocking reload).
    pub fn readiness(&self, msn: Option<u64>, part: Option<u64>) -> Readiness {
        let Some(msn) = msn else {
            if part.is_some() {
                return Readiness::BadRequest;
            }
            return if self.has_content() {
                Readiness::Ready
            } else {
                Readiness::Wait
            };
        };
        if msn > self.next_segment_id() + 1 {
            return Readiness::BadRequest;
        }
        if self.has_content() && msn < self.media_sequence {
            // RFC 8216bis 6.2.5.2: an evicted request still receives the
            // entire current playlist, allowing the client to catch up.
            return Readiness::Ready;
        }
        let available = match part {
            // Without _HLS_part, the requested parent must be complete.
            // Its first published part alone does not satisfy the request.
            None => msn < self.next_segment_id(),
            Some(part) => self.has_part(msn, part),
        };
        if self.has_content() && available {
            Readiness::Ready
        } else {
            Readiness::Wait
        }
    }

    fn has_part(&self, mut msn: u64, mut part: u64) -> bool {
        loop {
            let next = self.next_segment_id();
            if msn == next {
                return part < self.open_parts().len() as u64;
            }
            if msn > next {
                return false;
            }
            let Some(idx) = msn.checked_sub(self.media_sequence) else {
                return false;
            };
            match self.entries.get(idx as usize) {
                Some(Entry::Segment(seg)) => {
                    // A part index past the end means part 0 of the next one.
                    if part >= seg.parts.len() as u64 {
                        msn += 1;
                        part = 0;
                        continue;
                    }
                    return true;
                }
                Some(Entry::Gap { .. }) => return true,
                None => return false,
            }
        }
    }

    /// The real segment with media sequence number `id`; `None` for a gap or a
    /// number outside the window.
    pub fn segment(&self, id: u64) -> Option<Arc<Segment>> {
        let idx = id.checked_sub(self.media_sequence)?;
        match self.entries.get(idx as usize)? {
            Entry::Segment(s) => Some(s.clone()),
            Entry::Gap { .. } => None,
        }
    }

    /// Looks up part `id`, parts of the segment still being written included.
    pub fn part(&self, id: u64) -> PartLookup {
        if id == self.next_part_id {
            return PartLookup::Next;
        }
        let Some(first) = self.parts.front().map(|p| p.id) else {
            return PartLookup::Missing;
        };
        let in_window = id
            .checked_sub(first)
            .and_then(|i| self.parts.get(i as usize).cloned());
        // Parts of the open segment are in `parts` too.
        match in_window {
            Some(p) if p.id == id => PartLookup::Found(p),
            _ => PartLookup::Missing,
        }
    }

    /// Peak and average bits per second over the real segments in the window.
    pub fn bandwidth(&self) -> (u64, u64) {
        let (mut peak, mut bytes, mut secs) = (0u64, 0u64, 0f64);
        for e in &self.entries {
            if let Entry::Segment(s) = e {
                let d = s.duration.as_secs_f64();
                if d > 0.0 {
                    let bps = (s.data.len() as f64 * 8.0 / d) as u64;
                    peak = peak.max(bps);
                }
                bytes += s.data.len() as u64;
                secs += d;
            }
        }
        let avg = if secs > 0.0 {
            (bytes as f64 * 8.0 / secs) as u64
        } else {
            0
        };
        (peak.max(1), avg.max(1))
    }

    /// Breaks in the media since the stream started.
    pub fn breaks(&self) -> u64 {
        self.breaks
    }

    /// Over the newest `n` segments: the average bitrate (bits per second)
    /// and the median segment length, which is the encoder's keyframe
    /// interval when that is at least the segment target. `None` before the
    /// first segment.
    pub fn recent(&self, n: usize) -> Option<(u64, Duration)> {
        let segs: Vec<&Segment> = self
            .entries
            .iter()
            .rev()
            .filter_map(|e| match e {
                Entry::Segment(s) if !s.duration.is_zero() => Some(s.as_ref()),
                _ => None,
            })
            .take(n.max(1))
            .collect();
        if segs.is_empty() {
            return None;
        }
        let bytes: u64 = segs.iter().map(|s| s.data.len() as u64).sum();
        let secs: f64 = segs.iter().map(|s| s.duration.as_secs_f64()).sum();
        let mut lens: Vec<Duration> = segs.iter().map(|s| s.duration).collect();
        lens.sort();
        Some(((bytes as f64 * 8.0 / secs) as u64, lens[lens.len() / 2]))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn video() -> H264Config {
        H264Config::new(
            &[
                0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03,
                0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
            ],
            &[0x68, 0xef, 0xbc, 0x80],
        )
        .expect("valid sps")
    }

    pub fn aac() -> AacConfig {
        AacConfig {
            object_type: 2,
            sample_rate_index: 3,
            sample_rate: 48000,
            channels: 2,
        }
    }

    /// Feeds `secs` of 30 fps video with a keyframe every `gop` frames, plus
    /// 48 kHz audio, interleaved in timestamp order.
    pub fn feed(m: &mut Muxer, frames: std::ops::Range<i64>, gop: i64) -> Vec<Update> {
        let mut out = Vec::new();
        let mut next_audio = frames.start * 3000;
        for i in frames {
            let dts = i * 3000;
            while next_audio <= dts {
                m.push_audio(next_audio, Bytes::from_static(b"aac"));
                next_audio += 1920; // 1024 samples at 48 kHz in 90 kHz ticks
            }
            let up = m
                .push_video(
                    dts,
                    dts,
                    i % gop == 0,
                    Bytes::from(vec![0u8; 100]),
                    SystemTime::UNIX_EPOCH,
                )
                .expect("push");
            out.push(up);
        }
        out
    }

    /// Arrival remains monotonic, but its latency ramps between 0 and 145ms.
    /// Video timestamps stay perfectly regular. Segment arrival dates must not
    /// inherit that network latency jitter as overlapping PROGRAM-DATE-TIME.
    fn feed_jittered_arrival(m: &mut Muxer, input_start: i64, wall: SystemTime, frames: i64) {
        for i in 0..frames {
            let latency_ms = if (i / 30) % 2 == 0 {
                (i % 30) * 5
            } else {
                (29 - i % 30) * 5
            };
            let dts = input_start + i * 3000;
            m.push_video(
                dts,
                dts,
                i % 30 == 0,
                Bytes::from_static(b"video"),
                wall + ticks_to_duration(i * 3000, VIDEO_TIMESCALE)
                    + Duration::from_millis(latency_ms as u64),
            )
            .expect("jittered video");
        }
    }

    fn completed_segments(m: &Muxer) -> Vec<&Segment> {
        m.entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Segment(segment) => Some(segment.as_ref()),
                Entry::Gap { .. } => None,
            })
            .collect()
    }

    #[test]
    fn program_dates_follow_one_media_clock_despite_packet_arrival_jitter() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), None);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        feed_jittered_arrival(&mut m, 0, wall, 360);
        let segments = completed_segments(&m);
        assert!(segments.len() >= 10);
        let anchor = wall + ticks_to_duration(3000, VIDEO_TIMESCALE) + Duration::from_millis(5);
        assert_eq!(segments[0].start, anchor);
        for (i, segment) in segments.iter().enumerate() {
            assert_eq!(segment.start, anchor + Duration::from_secs(i as u64));
            assert_eq!(segment.duration, Duration::from_secs(1));
            assert!(!segment.discontinuity);
        }
        let playlist = crate::playlist::media(&m, "p");
        for segment in &segments[segments.len() - 2..] {
            assert!(playlist.contains(&format!(
                "#EXT-X-PROGRAM-DATE-TIME:{}\n",
                crate::time::rfc3339_millis(segment.start)
            )));
        }
    }

    #[test]
    fn program_dates_do_not_accumulate_fractional_tick_rounding() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), None);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        for i in 0..600 {
            let dts = i * 3001;
            m.push_video(
                dts,
                dts,
                i % 30 == 0,
                Bytes::from_static(b"video"),
                wall + ticks_to_duration(dts, VIDEO_TIMESCALE),
            )
            .expect("video");
        }
        let segments = completed_segments(&m);
        assert!(segments.len() >= 10);
        let anchor = wall + ticks_to_duration(3001, VIDEO_TIMESCALE);
        for segment in segments {
            let ticks = (segment.id - LEADING_GAPS) as i64 * 30 * 3001;
            assert_eq!(
                segment.start,
                anchor + ticks_to_duration(ticks, VIDEO_TIMESCALE)
            );
        }
    }

    #[test]
    fn program_dates_reanchor_only_after_a_real_break_or_reconnect() {
        for kind in ["interrupt", "timestamp-gap", "codec-epoch"] {
            let mut m = Muxer::new(MuxerConfig::default(), video(), None);
            let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
            feed_jittered_arrival(&mut m, 0, wall, 180);
            let first_id = m.next_segment_id();
            let input_start = match kind {
                "interrupt" => {
                    m.interrupt(wall + Duration::from_secs(10));
                    0 // The replacement encoder starts its clock again.
                }
                "codec-epoch" => {
                    m.reconfigure(video(), None, wall + Duration::from_secs(10));
                    0
                }
                _ => 8 * 90_000, // A genuine input timestamp hole, no detach.
            };
            let resumed_wall = wall + Duration::from_secs(30);
            feed_jittered_arrival(&mut m, input_start, resumed_wall, 120);
            let segments = completed_segments(&m);
            let resumed: Vec<_> = segments.iter().filter(|s| s.id > first_id).collect();
            assert!(resumed.len() >= 2, "{kind}");
            let anchor =
                resumed_wall + ticks_to_duration(3000, VIDEO_TIMESCALE) + Duration::from_millis(5);
            assert_eq!(resumed[0].start, anchor, "{kind}");
            assert!(resumed[0].discontinuity, "{kind}");
            assert!(!resumed[1].discontinuity, "{kind}");
            assert_eq!(resumed[1].start, anchor + resumed[0].duration, "{kind}");
            let previous = segments
                .iter()
                .find(|s| s.id == first_id)
                .expect("old tail");
            assert!(previous.start + previous.duration < anchor, "{kind}");
            assert_eq!(m.breaks(), 1, "{kind}");
        }
    }

    #[test]
    fn a_new_run_never_overlaps_previous_dates_when_the_wall_clock_steps_back() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), None);
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        feed_jittered_arrival(&mut m, 0, wall, 180);
        m.interrupt(wall + Duration::from_secs(6));
        let (floor, previous_id) = {
            let segments = completed_segments(&m);
            let previous = segments.last().expect("old tail");
            (previous.start + previous.duration, previous.id)
        };
        feed_jittered_arrival(&mut m, 0, wall - Duration::from_secs(60), 120);
        let segments = completed_segments(&m);
        let resumed: Vec<_> = segments.iter().filter(|s| s.id > previous_id).collect();
        assert!(resumed.len() >= 2);
        assert!(resumed[0].discontinuity);
        assert_eq!(resumed[0].start, floor);
        assert_eq!(resumed[1].start, floor + resumed[0].duration);
    }

    /// Pushes video at the given decode times (keyframe flag alongside)
    /// with 48 kHz AAC running underneath, interleaved in timestamp order,
    /// and checks the playlist rules after every push. Audio stops where
    /// video stops for over a second (a network drop loses both). Returns
    /// every playlist rendered.
    pub fn feed_checked(m: &mut Muxer, frames: &[(i64, bool)]) -> Vec<String> {
        let mut rendered = Vec::new();
        let mut rules = crate::playlist::RuleCheck::default();
        let mut next_audio = frames.first().map_or(0, |f| f.0);
        let mut prev = next_audio;
        for &(dts, sync) in frames {
            if (dts - prev).abs() > 90_000 {
                next_audio = dts;
            }
            prev = dts;
            while next_audio <= dts {
                m.push_audio(next_audio, Bytes::from_static(b"aac"));
                next_audio += 1920;
            }
            m.push_video(
                dts,
                dts,
                sync,
                Bytes::from(vec![0u8; 100]),
                SystemTime::UNIX_EPOCH,
            )
            .expect("push");
            if m.has_content() {
                let pl = crate::playlist::media(m, "p");
                if let Err(e) = rules.check(&pl) {
                    panic!("at dts {dts}: {e}\n{pl}");
                }
                rendered.push(pl);
            }
        }
        rendered
    }

    fn part_targets(rendered: &[String]) -> Vec<String> {
        rendered
            .iter()
            .filter_map(|pl| {
                pl.lines()
                    .find_map(|l| l.strip_prefix("#EXT-X-PART-INF:PART-TARGET="))
                    .map(str::to_string)
            })
            .collect()
    }

    /// A small deterministic generator, so the jitter is the same every run.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 33) % n
        }
    }

    /// Larix Broadcaster on an iPhone over Wi-Fi, as recorded on
    /// 2026-10-20: 30 fps whose frames last 2998 to 3004 ticks, a keyframe
    /// every 60 frames, and the occasional long frame from a Wi-Fi stall
    /// (one of 0.367 s, a 9000-tick step) or a dropped frame. Some land in
    /// the first segment, one just before a keyframe, one just after.
    pub fn larix_frames(count: usize) -> Vec<(i64, bool)> {
        let mut rng = Lcg(7);
        let mut dts = 0i64;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            out.push((dts, i % 60 == 0));
            dts += match i {
                20 | 1500 | 2100 | 2701 | 2702 => 6000,
                45 | 381 => 33_030,
                119 | 900 => 9000,
                181 => 21_000,
                _ => 2998 + rng.next(7) as i64,
            };
        }
        out
    }

    /// RTMP carries milliseconds, so 30 fps arrives as 33 and 34 ms frames.
    pub fn rtmp_frames(count: usize) -> Vec<(i64, bool)> {
        (0..count as i64)
            .map(|i| ((i * 1000 + 15) / 30 * 90, i % 60 == 0))
            .collect()
    }

    #[test]
    fn larix_timing_keeps_apples_part_rules() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        // Two minutes: the 24-segment window rolls past every long frame.
        assert!(!feed_checked(&mut m, &larix_frames(3600)).is_empty());
    }

    #[test]
    fn rtmp_millisecond_timing_keeps_apples_part_rules() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        assert!(!feed_checked(&mut m, &rtmp_frames(3600)).is_empty());
    }

    #[test]
    fn part_target_never_changes_once_the_stream_is_servable() {
        for frames in [larix_frames(3600), rtmp_frames(3600)] {
            let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
            let targets = part_targets(&feed_checked(&mut m, &frames));
            assert!(
                targets.iter().all(|t| *t == targets[0]),
                "PART-TARGET changed: {:?}",
                {
                    let mut d = targets.clone();
                    d.dedup();
                    d
                }
            );
        }
    }

    #[test]
    fn regular_30fps_parts_are_six_frames() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        let frames: Vec<(i64, bool)> = (0..600).map(|i| (i * 3000, i % 30 == 0)).collect();
        feed_checked(&mut m, &frames);
        let seg = m.segment(LEADING_GAPS + 3).expect("segment");
        assert_eq!(seg.parts.len(), 5);
        for p in &seg.parts {
            assert_eq!(p.duration, Duration::from_millis(200));
        }
    }

    #[test]
    fn jittered_parts_do_not_alternate_between_six_and_seven_frames() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed_checked(&mut m, &larix_frames(3600));
        let mut lens = std::collections::BTreeMap::<u64, usize>::new();
        for e in m.entries() {
            if let Entry::Segment(s) = e {
                let n = s.parts.len();
                for p in &s.parts[..n - 1] {
                    *lens.entry(p.duration.as_millis() as u64).or_default() += 1;
                }
            }
        }
        // Every whole part is six frames (199 or 200 ms) bar the few that
        // hold a long frame.
        let six: usize = lens.range(199..=200).map(|(_, n)| n).sum();
        let all: usize = lens.values().sum();
        assert!(six * 10 >= all * 9, "part lengths in ms: {lens:?}");
    }

    /// Decode time and summed sample durations of one track in a part.
    fn track_run(part: &Part, track: u32) -> Option<(u64, u64, Vec<u64>)> {
        use mp4_atom::Decode;
        let mut cur = std::io::Cursor::new(&part.data[..]);
        let moof = mp4_atom::Moof::decode(&mut cur).expect("moof");
        let traf = moof.traf.iter().find(|t| t.tfhd.track_id == track)?;
        let base = traf.tfdt.as_ref().expect("tfdt").base_media_decode_time;
        let mut starts = Vec::new();
        let mut t = base;
        for e in &traf.trun[0].entries {
            starts.push(t);
            t += u64::from(e.duration.expect("duration"));
        }
        Some((base, t - base, starts))
    }

    /// Long frames are clamped, not dropped: the media timeline, the
    /// segments' EXTINF and the recording's length stay true, and audio sits
    /// in the part covering its time.
    #[test]
    fn clamped_frames_keep_the_timeline() {
        let cfg = MuxerConfig {
            segment_count: 1000,
            ..MuxerConfig::default()
        };
        let mut m = Muxer::new(cfg, video(), Some(aac()));
        let frames = larix_frames(3600);
        feed_checked(&mut m, &frames);
        let target = m.part_target();
        // Durations hold whole nanoseconds; round back to whole ticks.
        let ticks = |d: Duration| ((d.as_nanos() * 90_000 + 500_000_000) / 1_000_000_000) as u64;
        let mut clamped = 0;
        let mut video_end = 0u64;
        let mut audio_end: Option<u64> = None;
        let mut prev_end = 0u64;
        for e in m.entries() {
            let Entry::Segment(seg) = e else { continue };
            let spans: u64 = seg.parts.iter().map(|p| ticks(p.span)).sum();
            assert_eq!(spans, ticks(seg.duration), "segment {}", seg.id);
            for p in &seg.parts {
                let (base, len, _) = track_run(p, VIDEO_TRACK).expect("video");
                assert_eq!(len, ticks(p.duration), "part {}", p.id);
                if video_end > 0 {
                    assert!(base >= video_end, "part {} overlaps", p.id);
                }
                video_end = base + ticks(p.span);
                // Every part starts on a real frame time.
                assert!(frames.iter().any(|f| f.0 as u64 == base), "part {}", p.id);
                if p.span > p.duration {
                    clamped += 1;
                    assert_eq!(p.duration, target);
                }
                if let Some((abase, alen, starts)) = track_run(p, AUDIO_TRACK) {
                    if let Some(end) = audio_end {
                        assert_eq!(abase, end, "audio continuous into part {}", p.id);
                    }
                    audio_end = Some(abase + alen);
                    let end = base + ticks(p.duration);
                    // An AAC frame is placed when the next one arrives, so
                    // one straddling the cut can land a frame late.
                    for a in starts {
                        let at = a * 90_000 / 48_000 + 1920;
                        assert!(
                            at >= prev_end && at - 1920 < end,
                            "part {}: audio at {at}",
                            p.id
                        );
                    }
                }
                prev_end = base + ticks(p.duration);
            }
        }
        assert!(clamped >= 3, "the stalls were clamped ({clamped})");
    }

    /// Moves every frame from `at` on later by `hole` ticks (negative: a
    /// jump back), as a Wi-Fi drop or an encoder restart does.
    fn with_hole(mut frames: Vec<(i64, bool)>, at: usize, hole: i64) -> Vec<(i64, bool)> {
        for f in &mut frames[at..] {
            f.0 += hole;
        }
        frames
    }

    /// Every part of every segment in the window, in order.
    fn all_parts(m: &Muxer) -> Vec<(bool, Arc<Part>)> {
        let mut out = Vec::new();
        for e in m.entries() {
            if let Entry::Segment(s) = e {
                for (i, p) in s.parts.iter().enumerate() {
                    out.push((i == 0 && s.discontinuity, p.clone()));
                }
            }
        }
        out
    }

    /// Output decode times only move forward and leave no hole at a break
    /// (they are rebased), every segment after a break starts on a
    /// keyframe, and no AAC frame lasts more than one frame's time.
    fn assert_clean_timeline(m: &Muxer) -> usize {
        let mut breaks = 0;
        let (mut video_end, mut audio_end) = (None::<u64>, None::<u64>);
        for (after_break, p) in all_parts(m) {
            let (base, len, _) = track_run(&p, VIDEO_TRACK).expect("video");
            if let Some(end) = video_end {
                assert!(base >= end, "part {} goes back", p.id);
            }
            if after_break {
                breaks += 1;
                assert!(p.independent, "part {} after a break", p.id);
                assert_eq!(Some(base), video_end, "part {}: rebased onto the end", p.id);
            }
            video_end = Some(base + len + (ticks_of(p.span) - ticks_of(p.duration)));
            if let Some((abase, alen, starts)) = track_run(&p, AUDIO_TRACK) {
                if let Some(end) = audio_end {
                    assert!(abase >= end, "audio in part {} goes back", p.id);
                }
                audio_end = Some(abase + alen);
                let mut all = starts.clone();
                all.push(abase + alen);
                for w in all.windows(2) {
                    assert!(
                        w[1] - w[0] <= 1024,
                        "part {}: AAC frame of {}",
                        p.id,
                        w[1] - w[0]
                    );
                }
            }
        }
        breaks
    }

    fn ticks_of(d: Duration) -> u64 {
        ((d.as_nanos() * 90_000 + 500_000_000) / 1_000_000_000) as u64
    }

    fn header(pl: &str, tag: &str) -> u64 {
        pl.lines()
            .find_map(|l| l.strip_prefix(tag))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no {tag}"))
    }

    /// Wi-Fi off for 3 s and later 4 s: both tracks stop, and the frame
    /// after each hole is not a keyframe.
    fn wifi_drop_frames() -> Vec<(i64, bool)> {
        with_hole(with_hole(larix_frames(3600), 1000, 270_000), 2000, 360_000)
    }

    #[test]
    fn a_network_drop_never_changes_the_target_duration() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        let rendered = feed_checked(&mut m, &wifi_drop_frames());
        let first = &rendered[0];
        assert_eq!(header(first, "#EXT-X-TARGETDURATION:"), 3);
        let last = rendered.last().expect("rendered");
        // Both breaks have left the window, and were counted as they went.
        assert_eq!(header(last, "#EXT-X-DISCONTINUITY-SEQUENCE:"), 2);
        assert!(
            rendered
                .iter()
                .any(|pl| pl.contains("#EXT-X-DISCONTINUITY\n"))
        );
    }

    #[test]
    fn a_network_drop_breaks_the_timeline_cleanly() {
        let cfg = MuxerConfig {
            segment_count: 1000,
            ..MuxerConfig::default()
        };
        let mut m = Muxer::new(cfg, video(), Some(aac()));
        feed_checked(&mut m, &wifi_drop_frames());
        assert_eq!(assert_clean_timeline(&m), 2);
    }

    /// No hole, but no keyframe for 6 s: the segment may not run past the
    /// target, so it ends and video waits for the keyframe.
    #[test]
    fn a_late_keyframe_never_changes_the_target_duration() {
        let frames: Vec<(i64, bool)> = (0..3600)
            .map(|i| (i * 3000, i % 60 == 0 && !(660..840).contains(&i)))
            .collect();
        let cfg = MuxerConfig {
            segment_count: 1000,
            ..MuxerConfig::default()
        };
        let mut m = Muxer::new(cfg, video(), Some(aac()));
        feed_checked(&mut m, &frames);
        assert_eq!(assert_clean_timeline(&m), 1);
    }

    /// The encoder restarts its clock: a jump back of 30 s.
    #[test]
    fn a_jump_back_breaks_instead_of_freezing_time() {
        let frames = with_hole(larix_frames(3600), 1200, -2_700_000);
        let cfg = MuxerConfig {
            segment_count: 1000,
            ..MuxerConfig::default()
        };
        let mut m = Muxer::new(cfg, video(), Some(aac()));
        feed_checked(&mut m, &frames);
        assert_eq!(assert_clean_timeline(&m), 1);
        // Media after the jump still made segments.
        let parts = all_parts(&m);
        assert!(parts.len() > 400, "{} parts", parts.len());
    }

    /// The encoder drops and comes back on a new connection: its clock
    /// restarts from zero, or carries on where it was. Either way the
    /// stream resumes at the next keyframe after one discontinuity, and the
    /// playlist rules hold throughout.
    #[test]
    fn an_interrupted_stream_resumes_from_any_clock() {
        let cfg = MuxerConfig {
            segment_count: 1000,
            ..MuxerConfig::default()
        };
        let frames = larix_frames(1200);
        for resumed in [&frames[..600], &frames[600..]] {
            let mut m = Muxer::new(cfg, video(), Some(aac()));
            feed_checked(&mut m, &frames[..600]);
            let before = m.next_part_id();
            m.interrupt(SystemTime::UNIX_EPOCH);
            assert!(m.next_part_id() > before, "the open part was closed");
            // Nothing more until the new encoder's first keyframe.
            assert!(m.interrupt(SystemTime::UNIX_EPOCH) == Update::default());
            feed_checked(&mut m, resumed);
            assert_eq!(assert_clean_timeline(&m), 1);
            let after: usize = all_parts(&m).iter().skip_while(|(brk, _)| !brk).count();
            assert!(after > 80, "{after} parts after the resume");
        }
    }

    /// The health report's measures: keyframe interval from segment
    /// lengths, bitrate from their bytes.
    #[test]
    fn recent_measures_follow_the_keyframe_interval() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        assert_eq!(m.recent(6), None);
        feed(&mut m, 0..600, 60);
        let (bps, len) = m.recent(6).expect("segments");
        assert_eq!(len, Duration::from_secs(2));
        assert!(bps > 0);
        assert_eq!(m.breaks(), 0);
        m.interrupt(SystemTime::UNIX_EPOCH);
        assert_eq!(m.breaks(), 1);
    }

    /// A drop before the first segment is complete: nothing was served, so
    /// the stream starts over and the targets come from a whole segment.
    #[test]
    fn a_drop_in_the_first_segment_starts_over() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        let rendered = feed_checked(&mut m, &with_hole(larix_frames(600), 30, 270_000));
        assert_eq!(header(&rendered[0], "#EXT-X-TARGETDURATION:"), 3);
        assert!(
            !rendered
                .iter()
                .any(|pl| pl.contains("#EXT-X-DISCONTINUITY\n"))
        );
    }

    /// The policy's failure mode: an early extra keyframe (a scene cut)
    /// makes the first segment short, so the target is too small for the
    /// encoder's 4 s keyframe interval and every later segment is cut short
    /// with video dropped to the next keyframe. The rules still hold.
    #[test]
    fn a_short_first_segment_still_keeps_the_rules() {
        let frames: Vec<(i64, bool)> = (0..3600)
            .map(|i| (i * 3000, i == 35 || i % 120 == 0))
            .collect();
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        let rendered = feed_checked(&mut m, &frames);
        assert_eq!(header(&rendered[0], "#EXT-X-TARGETDURATION:"), 2);
    }

    /// While the stream waits for a keyframe after a break, blocking
    /// requests for the next segment wait too, and the preload hint still
    /// names the part that will come.
    #[test]
    fn blocking_reload_waits_across_a_break() {
        let frames = with_hole(larix_frames(1100), 1000, 270_000);
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed_checked(&mut m, &frames[..1001]);
        let next = m.media_sequence() + m.entries().len() as u64;
        // The break closed the segment: nothing is open.
        assert!(m.open_parts().is_empty());
        assert_eq!(m.readiness(Some(next), Some(0)), Readiness::Wait);
        assert!(matches!(m.part(m.next_part_id()), PartLookup::Next));
        let hint = m.next_part_id();
        feed_checked(&mut m, &frames[1001..]);
        let seg = m.segment(next).expect("segment after the break");
        assert!(seg.discontinuity);
        assert_eq!(seg.parts[0].id, hint);
        assert_eq!(m.readiness(Some(next), Some(0)), Readiness::Ready);
    }

    #[test]
    fn waits_for_a_keyframe_and_then_cuts_one_second_segments() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        // Starts on a non-keyframe: ignored until frame 30.
        feed(&mut m, 25..30, 30);
        assert!(!m.has_content());
        assert_eq!(m.readiness(None, None), Readiness::Wait);
        feed(&mut m, 30..61, 30);
        assert!(m.has_content());
        assert_eq!(m.media_sequence(), 0);
        // Seven gaps then the first real segment, msn 7.
        assert_eq!(m.entries().len(), 8);
        let seg = m.segment(LEADING_GAPS).expect("segment 7");
        assert_eq!(seg.duration, Duration::from_secs(1));
        // One second of headroom over the first segment.
        assert_eq!(m.target_duration(), 2);
        for e in m.entries().iter().take(7) {
            assert!(matches!(e, Entry::Gap { duration } if *duration == Duration::from_secs(1)));
        }
    }

    #[test]
    fn parts_are_about_200ms_and_the_first_of_a_segment_is_independent() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..121, 30);
        let seg = m.segment(LEADING_GAPS).expect("segment");
        assert_eq!(seg.parts.len(), 5);
        assert!(seg.parts[0].independent);
        assert!(!seg.parts[1].independent);
        for p in &seg.parts {
            assert_eq!(p.duration, Duration::from_millis(200));
        }
        assert_eq!(m.part_target(), Duration::from_millis(234));
        let joined: Vec<u8> = seg.parts.iter().flat_map(|p| p.data.to_vec()).collect();
        assert_eq!(&seg.data[..], &joined[..]);
    }

    #[test]
    fn keeps_the_configured_window() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..(30 * 40 + 1), 30);
        assert_eq!(m.entries().len(), 24);
        // 40 segments + 7 gaps produced, 24 kept.
        assert_eq!(m.media_sequence(), 47 - 24);
        assert!(m.entries().iter().all(|e| matches!(e, Entry::Segment(_))));
        assert!(m.segment(m.media_sequence() - 1).is_none());
        let first = m.segment(m.media_sequence()).expect("oldest kept");
        match m.part(first.parts[0].id) {
            PartLookup::Found(p) => assert_eq!(p.id, first.parts[0].id),
            other => panic!("expected part, got {other:?}"),
        }
        assert!(matches!(m.part(first.parts[0].id - 1), PartLookup::Missing));
    }

    #[test]
    fn blocking_reload_rules() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        assert_eq!(m.readiness(None, Some(0)), Readiness::BadRequest);
        feed(&mut m, 0..31, 30); // segment 7 done, segment 8 open with 0 parts
        assert_eq!(m.readiness(Some(7), None), Readiness::Ready);
        assert_eq!(m.readiness(Some(7), Some(4)), Readiness::Ready);
        // Part 5 of segment 7 does not exist: it means part 0 of segment 8.
        assert_eq!(m.readiness(Some(7), Some(5)), Readiness::Wait);
        assert_eq!(m.readiness(Some(8), Some(0)), Readiness::Wait);
        assert_eq!(m.readiness(Some(9), None), Readiness::Wait);
        assert_eq!(m.readiness(Some(10), None), Readiness::BadRequest);
        feed(&mut m, 31..37, 30);
        assert_eq!(m.readiness(Some(8), Some(0)), Readiness::Ready);
        assert_eq!(m.readiness(Some(8), None), Readiness::Wait);
        assert_eq!(m.readiness(Some(7), Some(5)), Readiness::Ready);
        assert!(matches!(m.part(m.next_part_id()), PartLookup::Next));
        feed(&mut m, 37..61, 30);
        assert_eq!(m.readiness(Some(8), None), Readiness::Ready);
    }

    #[test]
    fn blocking_reload_of_evicted_media_returns_current_playlist() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..(30 * 40 + 1), 30);
        let evicted = m.media_sequence() - 1;
        assert!(m.segment(evicted).is_none());
        assert_eq!(m.readiness(Some(evicted), None), Readiness::Ready);
        assert_eq!(m.readiness(Some(evicted), Some(0)), Readiness::Ready);
        assert_eq!(m.readiness(Some(evicted), Some(u64::MAX)), Readiness::Ready);
        // Catch-up does not weaken validation of genuinely future requests.
        assert_eq!(
            m.readiness(Some(m.next_segment_id() + 2), None),
            Readiness::BadRequest
        );
    }

    #[test]
    fn long_gop_sets_the_target_duration() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), None);
        feed(&mut m, 0..(30 * 9 + 1), 90);
        assert_eq!(m.target_duration(), 4);
        let seg = m.segment(LEADING_GAPS).expect("segment");
        assert_eq!(seg.duration, Duration::from_secs(3));
    }

    #[test]
    fn decode_times_start_at_zero() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), Some(aac()));
        feed(&mut m, 0..31, 30);
        let seg = m.segment(LEADING_GAPS).expect("segment");
        use mp4_atom::Decode;
        let mut cur = std::io::Cursor::new(&seg.parts[0].data[..]);
        let moof = mp4_atom::Moof::decode(&mut cur).expect("moof");
        let v = moof.traf[0].tfdt.as_ref().map(|t| t.base_media_decode_time);
        assert_eq!(v, Some(0));
        let a = moof.traf[1].tfdt.as_ref().map(|t| t.base_media_decode_time);
        // The first AAC frame arrives before video opens a part, so it is
        // dropped: the audio run starts one frame (1024 samples) later.
        assert_eq!(a, Some(1024));
    }
}
