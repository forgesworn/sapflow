//! Low-Latency HLS packaging: segments, parts and playlists.
//!
//! The rules here that look arbitrary are Apple's. gohlslib v2.4.0
//! (`muxer_stream.go`, `muxer_segmenter.go`, MIT) is the reference for each;
//! see `docs/reference.md`.

#![warn(missing_docs)]

use std::time::Duration;

pub mod codec;
pub mod flv;
pub mod fmp4;
mod fragment;
pub mod ingest;
pub mod muxer;
pub mod playlist;
pub mod recording_checkpoint;
pub mod recording_programme;
pub mod recording_programme_checkpoint;
pub mod recording_timing;
pub mod time;
pub mod tracks;

pub use flv::FlvIngest;
pub use ingest::{EsIngest, IngestError, TsIngest};
pub use muxer::{Muxer, MuxerConfig, PartLookup, Readiness, Update};
pub use tracks::TrackSet;

#[cfg(test)]
mod audio_only_tests;

#[cfg(test)]
mod terminal_ts_tests;

#[cfg(test)]
mod recording_timing_tests;

#[cfg(test)]
mod recording_checkpoint_tests;

#[cfg(test)]
mod recording_programme_tests;

#[cfg(test)]
mod recording_programme_checkpoint_tests;

/// `EXT-X-TARGETDURATION`, fixed for the life of a stream.
///
/// RFC 8216bis 4.4.3.1: every segment's `EXTINF`, rounded to the nearest
/// second, must be at most the target, and the target must not change;
/// Safari stops a stream whose target rises. So it is chosen once, from the
/// first segment (which follows the encoder's keyframe interval), with a
/// second of headroom for keyframes that come a little late, and segments
/// are kept under `limit_ticks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetDuration {
    secs: u64,
}

impl TargetDuration {
    /// The first segment's duration, rounded, plus one second.
    pub fn for_first_segment(first: Duration) -> Self {
        Self {
            secs: (first.as_secs_f64().round() as u64).max(1) + 1,
        }
    }

    /// `EXT-X-TARGETDURATION` in whole seconds.
    pub fn secs(self) -> u64 {
        self.secs
    }

    /// The longest a segment may run, in 90 kHz ticks: under the target
    /// plus half a second (so it rounds to the target at most), with a
    /// millisecond to spare.
    pub fn limit_ticks(self) -> i64 {
        self.secs as i64 * 90_000 + 45_000 - 90
    }
}

/// How parts are cut, fixed for the life of a stream.
///
/// Apple's rules for parts (RFC 8216bis 4.4.4.9, the HLS authoring
/// specification): no part may be longer than `PART-TARGET`, every part but
/// the last of its segment must be at least 85% of it, and the target must
/// not change. So the target is chosen once, from the frame duration seen
/// over the first segment, and a part is closed at the first frame boundary
/// past `cut` (85% of the target). The target is set so that the nominal
/// part (whole frames, at least `part_min`) lands a millisecond above `cut`,
/// which leaves about a frame of headroom above it for jitter, millisecond
/// timestamps and a dropped frame. A frame that still does not fit (a stall)
/// has its duration clamped by the muxer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartTiming {
    /// `PART-TARGET` in 90 kHz ticks: a whole number of milliseconds, so it
    /// prints exactly.
    pub target: i64,
    /// A part is closed at the first frame boundary at least this many
    /// ticks after its start: 85% of the target, rounded up.
    pub cut: i64,
}

/// 90 kHz ticks.
const TICKS_PER_MS: i64 = 90;
/// Below `cut`, room for jitter on a part of nominal frames (one millisecond
/// covers RTMP's millisecond timestamps).
const JITTER: i64 = TICKS_PER_MS;
/// Fewer frames per part and the 15% between `cut` and the target is less
/// than a frame wide.
const MIN_FRAMES_PER_PART: i64 = 5;

impl PartTiming {
    /// `frame_durations` are the video frame durations seen so far, in 90 kHz
    /// ticks. Long frames (stalls, dropped frames) are left out of the
    /// estimate; with none at all, 30 fps is assumed.
    pub fn new(part_min: Duration, frame_durations: &[u32]) -> Self {
        let frame = typical_frame(frame_durations).clamp(TICKS_PER_MS, 200 * TICKS_PER_MS);
        let min = (part_min.as_micros() as i64 * 9 / 100).max(1);
        // Whole frames, allowing a tenth of a frame of rounding: 30 fps
        // measured as 2999.5 ticks still makes 6-frame parts of 200 ms.
        let frames = ((min - frame / 10).max(1) + frame - 1) / frame;
        let nominal = frames.max(MIN_FRAMES_PER_PART) * frame;
        let target_ms = (nominal - JITTER) * 100 / 85 / TICKS_PER_MS;
        let target = target_ms * TICKS_PER_MS;
        Self {
            target,
            cut: (target * 85 + 99) / 100,
        }
    }

    /// `PART-TARGET` as a duration, to the millisecond.
    pub fn target_duration(self) -> Duration {
        Duration::from_millis((self.target / TICKS_PER_MS) as u64)
    }
}

/// The mean frame duration, leaving out zeros and anything over 1.5 times
/// the median.
fn typical_frame(durations: &[u32]) -> i64 {
    let mut d: Vec<u32> = durations.iter().copied().filter(|&d| d > 0).collect();
    if d.is_empty() {
        return 3000;
    }
    d.sort_unstable();
    let median = i64::from(d[d.len() / 2]);
    let usual: Vec<i64> = d
        .iter()
        .map(|&x| i64::from(x))
        .filter(|&x| x * 2 <= median * 3)
        .collect();
    usual.iter().sum::<i64>() / usual.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_duration_comes_from_the_first_segment_with_headroom() {
        let t = TargetDuration::for_first_segment(Duration::from_millis(2002));
        assert_eq!(t.secs(), 3);
        // 3.49 s rounds to 3; 3.5 s would round to 4.
        assert!(t.limit_ticks() < 315_000 && t.limit_ticks() >= 314_000);
        assert_eq!(
            TargetDuration::for_first_segment(Duration::from_millis(8333)).secs(),
            9
        );
        assert_eq!(
            TargetDuration::for_first_segment(Duration::from_millis(300)).secs(),
            2
        );
    }

    fn ticks(t: i64) -> Duration {
        Duration::from_nanos((t * 1_000_000_000 / 90_000) as u64)
    }

    #[test]
    fn part_timing_at_30fps() {
        let t = PartTiming::new(Duration::from_millis(200), &[3000; 30]);
        assert_eq!(t.target_duration(), Duration::from_millis(234));
        assert_eq!(t.target, 21_060);
        assert_eq!(t.cut, 17_901);
    }

    /// For each frame rate: nominal parts of whole frames close at `cut`,
    /// fit the target, are at least 85% of it, and a frame of jitter or one
    /// dropped frame still fits where the rate allows.
    #[test]
    fn part_timing_suits_common_frame_rates() {
        for (fps_milli, frame) in [
            (10_000, 9000i64),
            (15_000, 6000),
            (23_976, 3754),
            (24_000, 3750),
            (25_000, 3600),
            (29_970, 3003),
            (30_000, 3000),
            (50_000, 1800),
            (59_940, 1502),
            (60_000, 1500),
        ] {
            let t = PartTiming::new(Duration::from_millis(200), &[frame as u32; 60]);
            let n = (1..).find(|n| n * frame >= t.cut).expect("frames");
            let part = n * frame;
            assert!(n >= 5, "{fps_milli}: {n} frames");
            assert!(part <= t.target, "{fps_milli}: {part} > {}", t.target);
            assert!(part * 100 >= t.target * 85, "{fps_milli}");
            assert!(
                ticks(part) >= Duration::from_millis(190),
                "{fps_milli}: {part}"
            );
            assert!(t.cut * 100 >= t.target * 85, "{fps_milli}");
            // A part a millisecond short of nominal still closes on time.
            assert!(part - 90 >= t.cut, "{fps_milli}: no room for jitter");
            assert_eq!(t.target % 90, 0);
        }
    }

    #[test]
    fn part_timing_ignores_stalls_and_rounding() {
        let mut d = vec![3000u32; 40];
        d.extend([33_030, 9000, 6000]);
        assert_eq!(
            PartTiming::new(Duration::from_millis(200), &d),
            PartTiming::new(Duration::from_millis(200), &[3000])
        );
        // RTMP's millisecond timestamps: 33 and 34 ms frames.
        let rtmp: Vec<u32> = (0..60)
            .map(|i| if i % 3 == 2 { 3060 } else { 2970 })
            .collect();
        assert_eq!(
            PartTiming::new(Duration::from_millis(200), &rtmp),
            PartTiming::new(Duration::from_millis(200), &[3000])
        );
        assert_eq!(
            PartTiming::new(Duration::from_millis(200), &[2999]),
            PartTiming::new(Duration::from_millis(200), &[3000])
        );
    }

    #[test]
    fn part_timing_survives_nonsense() {
        for d in [&[][..], &[0, 0], &[1], &[u32::MAX], &[u32::MAX, 1]] {
            let t = PartTiming::new(Duration::from_millis(200), d);
            assert!(t.target > 0 && t.cut > 0 && t.cut <= t.target, "{d:?}");
        }
        let t = PartTiming::new(Duration::ZERO, &[3000]);
        assert!(t.cut <= 5 * 3000 && t.target >= 5 * 3000);
    }
}
