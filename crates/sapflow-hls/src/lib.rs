//! Low-Latency HLS packaging: segments, parts and playlists.
//!
//! The rules here that look arbitrary are Apple's. gohlslib v2.4.0
//! (`muxer_stream.go`, `muxer_segmenter.go`, MIT) is the reference for each;
//! see `docs/reference.md`.

use std::time::Duration;

pub mod codec;
pub mod flv;
pub mod fmp4;
pub mod ingest;
pub mod muxer;
pub mod playlist;
pub mod rtmp;
pub mod time;

pub use flv::FlvIngest;
pub use ingest::{EsIngest, IngestError, TsIngest};
pub use muxer::{Muxer, MuxerConfig, PartLookup, Readiness, Update};

/// Tracks `EXT-X-TARGETDURATION`, which may only ever go up during a stream.
///
/// Every segment's duration, rounded to the nearest second, must be at or
/// below the target. iOS fails if the target changes, so a long segment
/// raises it for the rest of the stream; it never comes back down.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TargetDuration {
    secs: u64,
}

impl TargetDuration {
    pub fn secs(self) -> u64 {
        self.secs
    }

    /// Feeds a finished segment. Returns true when the target had to rise,
    /// which the caller should log: iOS clients may stall at that moment.
    pub fn observe(&mut self, segment: Duration) -> bool {
        // A target of 0 is invalid, so very short segments count as 1 s.
        let rounded = (segment.as_secs_f64().round() as u64).max(1);
        let first = self.secs == 0;
        if rounded > self.secs {
            self.secs = rounded;
            return !first;
        }
        false
    }
}

/// iOS fails if a part is shorter than 85% of the longest part. Given the
/// frame durations seen so far, find the shortest part duration at or above
/// `min` that every frame duration divides into closely enough.
pub fn compatible_part_duration(min: Duration, frame_durations: &[Duration]) -> Duration {
    let step = Duration::from_millis(5);
    let mut candidate = min;
    while candidate < Duration::from_secs(5) {
        if frame_durations.iter().all(|&f| part_fits(candidate, f)) {
            return candidate;
        }
        candidate += step;
    }
    candidate
}

fn part_fits(part: Duration, frame: Duration) -> bool {
    if frame.is_zero() {
        return true;
    }
    if frame > part {
        return false;
    }
    let part_n = part.as_nanos();
    let frame_n = frame.as_nanos();
    let frames = part_n.div_ceil(frame_n);
    part_n * 100 > frames * frame_n * 85
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_duration_only_rises() {
        let mut t = TargetDuration::default();
        assert!(!t.observe(Duration::from_millis(1000)));
        assert_eq!(t.secs(), 1);
        assert!(!t.observe(Duration::from_millis(1400)));
        assert_eq!(t.secs(), 1);
        assert!(t.observe(Duration::from_millis(3600)));
        assert_eq!(t.secs(), 4);
        assert!(!t.observe(Duration::from_millis(1000)));
        assert_eq!(t.secs(), 4);
    }

    #[test]
    fn target_duration_is_never_zero() {
        let mut t = TargetDuration::default();
        t.observe(Duration::from_millis(300));
        assert_eq!(t.secs(), 1);
    }

    #[test]
    fn part_duration_fits_30fps_video() {
        let frame = Duration::from_nanos(33_333_333);
        let part = compatible_part_duration(Duration::from_millis(200), &[frame]);
        assert!(part >= Duration::from_millis(200));
        assert!(part_fits(part, frame));
    }

    #[test]
    fn part_duration_fits_aac_and_video_together() {
        let video = Duration::from_nanos(33_333_333);
        let aac_48k = Duration::from_nanos(21_333_333);
        let part = compatible_part_duration(Duration::from_millis(200), &[video, aac_48k]);
        assert!(part_fits(part, video) && part_fits(part, aac_48k));
    }
}
