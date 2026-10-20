//! The LL-HLS segmenter: samples in, parts and segments out.
//!
//! Follows gohlslib v2.4.0 (`muxer_segmenter.go`, `muxer_stream.go`): video
//! leads, segments are cut on a keyframe once the minimum duration has
//! passed, parts are cut on the video timeline, audio goes into whichever
//! part is open when it arrives. Each sample is held until the next one on
//! its track arrives, which gives its duration.
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
use crate::{TargetDuration, compatible_part_duration};

/// fMP4 decode times must not be negative. gohlslib shifts everything by
/// 10 s (`fmp4StartDTS`) because its timeline can start before the first
/// keyframe; ours starts at the first keyframe's DTS and drops anything
/// earlier, so no shift is needed and recordings start at zero.
const START_OFFSET: Duration = Duration::ZERO;

/// iOS needs the playlist to open with this many gap segments.
pub const LEADING_GAPS: u64 = 7;

/// Refuse to buffer a segment larger than this (no keyframes, or abuse).
const MAX_SEGMENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxerConfig {
    pub segment_min: Duration,
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

#[derive(Debug)]
pub struct Part {
    pub id: u64,
    pub duration: Duration,
    pub independent: bool,
    pub data: Bytes,
}

#[derive(Debug)]
pub struct Segment {
    pub id: u64,
    pub duration: Duration,
    pub start: SystemTime,
    pub parts: Vec<Arc<Part>>,
    pub data: Bytes,
}

#[derive(Debug, Clone)]
pub enum Entry {
    Gap { duration: Duration },
    Segment(Arc<Segment>),
}

impl Entry {
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
    pub part: bool,
    pub segment: bool,
}

impl Update {
    pub fn any(self) -> bool {
        self.part || self.segment
    }
}

/// The answer to a media playlist request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Wait,
    BadRequest,
}

/// The answer to a part request.
#[derive(Debug, Clone)]
pub enum PartLookup {
    Found(Arc<Part>),
    /// The part the preload hint points at: wait for it.
    Next,
    Missing,
}

#[derive(Debug, Clone)]
pub enum MuxError {
    SegmentTooLarge,
}

#[derive(Debug)]
struct Pending {
    dts: i64,
    cts: i32,
    sync: bool,
    data: Bytes,
}

#[derive(Debug)]
struct OpenSegment {
    id: u64,
    start_dts: i64,
    start: SystemTime,
    parts: Vec<Arc<Part>>,
    bytes: usize,
}

#[derive(Debug)]
struct OpenPart {
    id: u64,
    start_dts: i64,
    video: Vec<Sample>,
    video_base: u64,
    audio: Vec<Sample>,
    audio_base: u64,
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
            audio_base: 0,
            independent: false,
        }
    }
}

#[derive(Debug)]
pub struct Muxer {
    cfg: MuxerConfig,
    video: H264Config,
    audio: Option<AacConfig>,
    init: Bytes,

    pending_video: Option<Pending>,
    pending_audio: Option<Pending>,

    entries: VecDeque<Entry>,
    media_sequence: u64,
    /// Parts of every real segment in `entries`, oldest first.
    parts: VecDeque<Arc<Part>>,
    next_segment: Option<OpenSegment>,
    part: Option<OpenPart>,
    next_part_id: u64,

    part_duration: Duration,
    frame_durations: Vec<Duration>,
    part_duration_frozen: bool,
    target: TargetDuration,
    part_target: Duration,
}

impl Muxer {
    pub fn new(cfg: MuxerConfig, video: H264Config, audio: Option<AacConfig>) -> Self {
        let init = fmp4::init_segment(&video, audio.as_ref());
        Self {
            cfg: MuxerConfig {
                segment_count: cfg.segment_count.max(LEADING_GAPS as usize),
                ..cfg
            },
            video,
            audio,
            init,
            pending_video: None,
            pending_audio: None,
            entries: VecDeque::new(),
            media_sequence: 0,
            parts: VecDeque::new(),
            next_segment: None,
            part: None,
            next_part_id: 0,
            part_duration: cfg.part_min,
            frame_durations: Vec::new(),
            part_duration_frozen: false,
            target: TargetDuration::default(),
            part_target: Duration::ZERO,
        }
    }

    pub fn video(&self) -> &H264Config {
        &self.video
    }

    pub fn audio(&self) -> Option<&AacConfig> {
        self.audio.as_ref()
    }

    pub fn init(&self) -> &Bytes {
        &self.init
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
        if self.pending_video.is_none() && self.next_segment.is_none() && !sync {
            return Ok(Update::default());
        }
        let offset = duration_to_ticks(START_OFFSET, VIDEO_TIMESCALE);
        let mut next = Pending {
            dts: dts + offset,
            cts: (pts - dts).clamp(0, i64::from(i32::MAX)) as i32,
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
        if next.dts < cur.dts {
            next.dts = cur.dts;
        }
        let duration = (next.dts - cur.dts) as u32;
        let next_dts = next.dts;
        let next_sync = next.sync;
        self.pending_video = Some(next);

        if self.next_segment.is_none() {
            self.next_segment = Some(OpenSegment {
                id: LEADING_GAPS,
                start_dts: cur.dts,
                start: now,
                parts: Vec::new(),
                bytes: 0,
            });
            self.part = Some(OpenPart::new(self.next_part_id, cur.dts));
        }

        if !self.part_duration_frozen && duration > 0 {
            let d = crate::time::ticks_to_duration(i64::from(duration), VIDEO_TIMESCALE);
            if !self.frame_durations.contains(&d) && self.frame_durations.len() < 64 {
                self.frame_durations.push(d);
                self.part_duration =
                    compatible_part_duration(self.cfg.part_min, &self.frame_durations);
            }
        }

        let seg = self
            .next_segment
            .as_mut()
            .ok_or(MuxError::SegmentTooLarge)?;
        seg.bytes += cur.data.len();
        if seg.bytes > MAX_SEGMENT_BYTES {
            return Err(MuxError::SegmentTooLarge);
        }
        let seg_start = seg.start_dts;
        if let Some(part) = self.part.as_mut() {
            if part.video.is_empty() {
                part.video_base = cur.dts as u64;
            }
            if cur.sync {
                part.independent = true;
            }
            part.video.push(Sample {
                duration,
                cts: cur.cts,
                sync: cur.sync,
                data: cur.data,
            });
        }

        let mut update = Update::default();
        let since_segment = ticks_to_duration(next_dts - seg_start, VIDEO_TIMESCALE);
        let part_start = self.part.as_ref().map_or(next_dts, |p| p.start_dts);
        let since_part = ticks_to_duration(next_dts - part_start, VIDEO_TIMESCALE);
        if next_sync && since_segment >= self.cfg.segment_min {
            self.rotate_segment(next_dts, now);
            self.part_duration_frozen = true;
            update.part = true;
            update.segment = true;
        } else if since_part >= self.part_duration {
            self.rotate_part(next_dts);
            update.part = true;
        }
        Ok(update)
    }

    /// Feeds one AAC frame. `pts` is 90 kHz on the same timeline as video.
    pub fn push_audio(&mut self, pts: i64, data: Bytes) {
        let Some(audio) = self.audio else { return };
        if pts < 0 {
            return;
        }
        let rate = audio.sample_rate;
        let offset = duration_to_ticks(START_OFFSET, VIDEO_TIMESCALE);
        let mut next = Pending {
            dts: (pts + offset) * i64::from(rate) / i64::from(VIDEO_TIMESCALE),
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
        let duration = (next.dts - cur.dts) as u32;
        self.pending_audio = Some(next);

        // Audio waits for video to open the first part.
        let Some(part) = self.part.as_mut() else {
            return;
        };
        if part.audio.is_empty() {
            part.audio_base = cur.dts as u64;
        }
        part.audio.push(Sample {
            duration,
            cts: 0,
            sync: true,
            data: cur.data,
        });
    }

    fn rotate_part(&mut self, end_dts: i64) {
        let Some(part) = self.part.take() else { return };
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
                    base_time: part.audio_base,
                    samples: &part.audio,
                },
            ],
        );
        let finished = Arc::new(Part {
            id: part.id,
            duration: ticks_to_duration(end_dts - part.start_dts, VIDEO_TIMESCALE),
            independent: part.independent,
            data,
        });
        if let Some(seg) = self.next_segment.as_mut() {
            seg.parts.push(finished.clone());
        }
        self.parts.push_back(finished);
        self.next_part_id += 1;
        self.part = Some(OpenPart::new(self.next_part_id, end_dts));

        // Like gohlslib: the longest part in the window, rounded up to the
        // millisecond. It is fixed once the part duration is frozen.
        let longest = self
            .parts
            .iter()
            .map(|p| p.duration)
            .max()
            .unwrap_or_default();
        let ms = longest.as_nanos().div_ceil(1_000_000) as u64;
        let target = Duration::from_millis(ms);
        if target > self.part_target {
            self.part_target = target;
        }
    }

    fn rotate_segment(&mut self, end_dts: i64, now: SystemTime) {
        self.rotate_part(end_dts);
        let Some(open) = self.next_segment.take() else {
            return;
        };
        let duration = ticks_to_duration(end_dts - open.start_dts, VIDEO_TIMESCALE);
        let mut data = BytesMut::with_capacity(open.parts.iter().map(|p| p.data.len()).sum());
        for p in &open.parts {
            data.extend_from_slice(&p.data);
        }
        let seg = Arc::new(Segment {
            id: open.id,
            duration,
            start: open.start,
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
            }
            self.media_sequence += 1;
        }
        if self.target.observe(duration) {
            tracing::warn!(
                target_secs = self.target.secs(),
                "segment longer than the target duration; iOS clients may stall (check the encoder's keyframe interval)"
            );
        }

        self.next_segment = Some(OpenSegment {
            id: open.id + 1,
            start_dts: end_dts,
            start: now,
            parts: Vec::new(),
            bytes: 0,
        });
    }

    // ---- the window, for playlists and HTTP ----

    pub fn entries(&self) -> &VecDeque<Entry> {
        &self.entries
    }

    pub fn media_sequence(&self) -> u64 {
        self.media_sequence
    }

    pub fn target_duration(&self) -> u64 {
        self.target.secs().max(1)
    }

    pub fn part_target(&self) -> Duration {
        self.part_target
    }

    /// Parts of the segment still being written.
    pub fn open_parts(&self) -> &[Arc<Part>] {
        self.next_segment.as_ref().map_or(&[], |s| &s.parts)
    }

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
            return Readiness::BadRequest;
        }
        if self.has_content() && self.has_part(msn, part.unwrap_or(0)) {
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

    pub fn segment(&self, id: u64) -> Option<Arc<Segment>> {
        let idx = id.checked_sub(self.media_sequence)?;
        match self.entries.get(idx as usize)? {
            Entry::Segment(s) => Some(s.clone()),
            Entry::Gap { .. } => None,
        }
    }

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
        assert_eq!(m.target_duration(), 1);
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
        assert_eq!(m.part_target(), Duration::from_millis(200));
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
        assert_eq!(m.readiness(Some(7), Some(5)), Readiness::Ready);
        assert!(matches!(m.part(m.next_part_id()), PartLookup::Next));
    }

    #[test]
    fn long_gop_raises_the_target_duration() {
        let mut m = Muxer::new(MuxerConfig::default(), video(), None);
        feed(&mut m, 0..(30 * 9 + 1), 90);
        assert_eq!(m.target_duration(), 3);
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
