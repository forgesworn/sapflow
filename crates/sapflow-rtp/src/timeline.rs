//! One 90 kHz timeline for a session's streams (`docs/whip-ingest.md`,
//! "One timeline"), from RTCP sender reports (RFC 3550 section 6.4.1: the
//! NTP timestamp and the RTP timestamp of the same instant) or, failing
//! them, from arrival times.
//!
//! - **Start hold.** Media is held from the first media packet until every
//!   stream has media and a sender report, for at most
//!   [`TimelineConfig::start_hold`] (1.5 s), or until the held media
//!   reaches its byte bound or half the event queue. Then the streams are
//!   aligned by their sender reports' NTP times, so that the earliest first
//!   frame is at 0. If a stream with media still has no sender report, or
//!   the reports put the streams' first packets further apart, against
//!   their arrival times, than [`TimelineConfig::max_lead`], every stream
//!   is aligned by arrival time instead, and [`Alignment`] says which and
//!   why.
//! - **After start**, each stream's RTP timestamps are unwrapped in the
//!   order the receiver emits them and advance its timeline by their
//!   deltas, converted to 90 kHz from absolute values (Opus is 48 kHz), so
//!   no rounding accumulates.
//! - **Bounded by arrival time.** A stream's timestamps never go back, and
//!   never run ahead of arrival time by more than `max_lead` (plus 0.1% of
//!   the time since alignment, for clock rate differences between sender
//!   and receiver). A timestamp that would do either re-anchors the stream
//!   on arrival time, continuing from its last timestamp: a discontinuity.
//!   So does a sequence or Opus timestamp discontinuity in the stream.
//! - **Drift.** Each later sender report is checked against where the
//!   stream's RTP timestamps put it. Beyond [`TimelineConfig::max_drift`]
//!   either way is a discontinuity. If the stream is behind, it re-anchors
//!   on the report (moving forward, and still bounded by arrival time). If
//!   it is ahead, it cannot go back, so it keeps its timestamps and takes
//!   the report as its new reference. A stream aligned or re-anchored by
//!   arrival time takes its next report as its reference.
//!
//! A sender report with an NTP time of zero carries no mapping and is
//! ignored here.

use std::time::Duration;

/// The timeline's clock rate.
pub const CLOCK: u32 = 90_000;

/// Settings for the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineConfig {
    /// Longest wait at the start for every stream's first sender report,
    /// from the first media packet. Zero aligns by arrival time at once.
    /// Default 1.5 s (`docs/whip-ingest.md`, proposed).
    pub start_hold: Duration,
    /// Most media payload bytes held during the start hold; reaching it
    /// ends the hold early. Default 4 MiB (1.5 s at about 22 Mbit/s).
    pub max_hold_bytes: usize,
    /// A later sender report further than this from where a stream's RTP
    /// timestamps put it is a discontinuity. Default 100 ms.
    pub max_drift: Duration,
    /// How far a stream's timestamps may run ahead of arrival time, for
    /// network jitter. Default 500 ms.
    pub max_lead: Duration,
}
impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            start_hold: Duration::from_millis(1500),
            max_hold_bytes: 4 << 20,
            max_drift: Duration::from_millis(100),
            max_lead: Duration::from_millis(500),
        }
    }
}

/// How the streams were aligned at the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    /// By sender reports.
    SenderReports,
    /// By arrival time, and why.
    Arrival(Fallback),
}

/// Why the start fell back to arrival time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The start hold ran out (or was zero) before a stream with media had
    /// a sender report.
    Timeout,
    /// The held media reached its byte or event bound first.
    HoldFull,
    /// The sender reports disagreed with arrival times by more than
    /// `max_lead`.
    Inconsistent,
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// Microseconds to 90 kHz ticks.
fn ticks(us: u64) -> i64 {
    i64::try_from(u128::from(us) * 9 / 100).unwrap_or(i64::MAX)
}

/// A 64-bit NTP timestamp to 90 kHz ticks.
fn ntp_ticks(ntp: u64) -> i64 {
    i64::try_from((u128::from(ntp) * u128::from(CLOCK)) >> 32).unwrap_or(i64::MAX)
}

/// An RTP timestamp difference at `rate` to 90 kHz ticks, rounding down.
fn scale(delta: i64, rate: u32) -> i64 {
    let t = (i128::from(delta) * i128::from(CLOCK)).div_euclid(i128::from(rate.max(1)));
    t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[derive(Debug, Clone, Copy)]
struct Sr {
    ntp: u64,
    rtp: u32,
}

#[derive(Debug, Clone, Copy)]
struct Last {
    ext: i64,
    rtp: u32,
    pts: i64,
    arrival: u64,
}

#[derive(Debug)]
struct Track {
    rate: u32,
    /// The first media packet's RTP timestamp and arrival.
    first: Option<(u32, u64)>,
    sr: Option<Sr>,
    /// `(unwrapped RTP timestamp, timeline ticks)` the stream maps from.
    anchor: Option<(i64, i64)>,
    last: Option<Last>,
    /// Timeline ticks minus NTP ticks, the sender report reference.
    offset: Option<i64>,
    /// The next timestamp re-anchors on arrival time.
    reanchor: bool,
    /// Re-anchors on arrival time this timeline made, and sender reports
    /// beyond the drift bound.
    discontinuities: u64,
}

/// What [`Timeline::stamp`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// 90 kHz ticks.
    pub pts: u64,
    /// The stream re-anchored on arrival time for this timestamp.
    pub reanchored: bool,
}

#[derive(Debug)]
pub(crate) struct Timeline {
    hold: u64,
    max_hold_bytes: usize,
    drift: i64,
    lead: i64,
    tracks: Vec<Track>,
    /// The first media packet's arrival.
    since: Option<u64>,
    aligned: Option<Alignment>,
    aligned_at: u64,
    /// NTP ticks at timeline zero, when aligned by sender reports.
    origin: i64,
    /// Timeline ticks minus arrival ticks, for anchoring on arrival time.
    arrival_offset: i64,
}

impl Timeline {
    pub fn new(cfg: TimelineConfig) -> Self {
        Self {
            hold: micros(cfg.start_hold),
            max_hold_bytes: cfg.max_hold_bytes,
            drift: ticks(micros(cfg.max_drift)),
            lead: ticks(micros(cfg.max_lead)),
            tracks: Vec::new(),
            since: None,
            aligned: None,
            aligned_at: 0,
            origin: 0,
            arrival_offset: 0,
        }
    }

    pub fn add_track(&mut self, rate: u32) {
        self.tracks.push(Track {
            rate,
            first: None,
            sr: None,
            anchor: None,
            last: None,
            offset: None,
            reanchor: false,
            discontinuities: 0,
        });
    }

    /// Discontinuities the timeline found in a track.
    pub fn discontinuities(&self, track: usize) -> u64 {
        self.tracks.get(track).map_or(0, |t| t.discontinuities)
    }

    pub fn alignment(&self) -> Option<Alignment> {
        self.aligned
    }

    pub fn holding(&self) -> bool {
        self.aligned.is_none()
    }

    /// A media packet arrived for `track`.
    pub fn note_packet(&mut self, track: usize, rtp: u32, arrival: u64) {
        if let Some(t) = self.tracks.get_mut(track)
            && t.first.is_none()
        {
            t.first = Some((rtp, arrival));
            self.since.get_or_insert(arrival);
        }
    }

    /// When the start hold runs out, while it lasts.
    pub fn hold_deadline(&self) -> Option<u64> {
        if self.aligned.is_some() {
            return None;
        }
        self.since.map(|s| s.saturating_add(self.hold))
    }

    /// Ends the start hold if it is time: `held` media bytes, and whether
    /// the event queue is half full. Returns the alignment chosen.
    pub fn try_align(&mut self, now: u64, held: usize, queue_half_full: bool) -> Option<Alignment> {
        let since = self.since?;
        if self.aligned.is_some() {
            return None;
        }
        let complete = self
            .tracks
            .iter()
            .all(|t| t.first.is_some() && t.sr.is_some());
        let expired = now >= since.saturating_add(self.hold);
        let full = held > self.max_hold_bytes || queue_half_full;
        if !(complete || expired || full) {
            return None;
        }
        let with_media = || self.tracks.iter().filter(|t| t.first.is_some());
        let alignment = if with_media().all(|t| t.sr.is_some()) {
            // Where each stream's first packet was on the sender's clock,
            // and how far that runs ahead of when it arrived.
            let walls: Vec<(i64, i64)> = with_media()
                .filter_map(|t| {
                    let (rtp, arrival) = t.first?;
                    let sr = t.sr?;
                    let wall = ntp_ticks(sr.ntp)
                        .saturating_add(scale(i64::from(rtp.wrapping_sub(sr.rtp) as i32), t.rate));
                    Some((wall, wall.saturating_sub(ticks(arrival))))
                })
                .collect();
            let lo = walls.iter().map(|w| w.1).min().unwrap_or(0);
            let hi = walls.iter().map(|w| w.1).max().unwrap_or(0);
            if hi.saturating_sub(lo) <= self.lead {
                self.origin = walls.iter().map(|w| w.0).min().unwrap_or(0);
                self.arrival_offset = hi.saturating_sub(self.origin);
                Alignment::SenderReports
            } else {
                Alignment::Arrival(Fallback::Inconsistent)
            }
        } else if full && !expired {
            Alignment::Arrival(Fallback::HoldFull)
        } else {
            Alignment::Arrival(Fallback::Timeout)
        };
        if let Alignment::Arrival(_) = alignment {
            let first = with_media()
                .filter_map(|t| t.first.map(|f| f.1))
                .min()
                .unwrap_or(since);
            self.arrival_offset = ticks(first).saturating_neg();
        }
        self.aligned = Some(alignment);
        self.aligned_at = now;
        Some(alignment)
    }

    /// The stream's sequence numbers or timestamps were discontinuous:
    /// its next timestamp re-anchors on arrival time.
    pub fn discontinuity(&mut self, track: usize) {
        if let Some(t) = self.tracks.get_mut(track) {
            t.reanchor = true;
        }
    }

    /// The highest timeline position a timestamp arriving at `arrival` may
    /// have.
    fn bound(&self, arrival: u64) -> i64 {
        let slack = ticks(arrival.saturating_sub(self.aligned_at)) / 1000;
        ticks(arrival)
            .saturating_add(self.arrival_offset)
            .saturating_add(slack)
            .saturating_add(self.lead)
    }

    /// The timeline position of a frame of `track` with RTP timestamp
    /// `rtp`, released from a packet that arrived at `arrival`. Frames
    /// come in the order the receiver emits them. Call only once aligned.
    pub fn stamp(&mut self, track: usize, rtp: u32, arrival: u64) -> Stamp {
        let bound = self.bound(arrival);
        let arrival_pts = ticks(arrival).saturating_add(self.arrival_offset);
        let synced = self.aligned == Some(Alignment::SenderReports);
        let (origin, lead) = (self.origin, self.lead);
        let Some(t) = self.tracks.get_mut(track) else {
            return Stamp {
                pts: 0,
                reanchored: false,
            };
        };
        let ext = match t.last {
            Some(l) => l
                .ext
                .saturating_add(i64::from(rtp.wrapping_sub(l.rtp) as i32)),
            None => i64::from(rtp),
        };
        let mut reanchored = false;
        let pts = match (t.anchor, t.last) {
            (Some((ae, ap)), Some(l)) => {
                let mapped = ap.saturating_add(scale(ext.saturating_sub(ae), t.rate));
                if t.reanchor || mapped < l.pts || mapped > bound {
                    // Continue from the last timestamp by arrival time.
                    let step = ticks(arrival.saturating_sub(l.arrival)).max(1);
                    let pts = l.pts.saturating_add(step);
                    reanchored = !t.reanchor;
                    t.discontinuities += u64::from(reanchored);
                    t.anchor = Some((ext, pts));
                    t.offset = None;
                    pts
                } else {
                    mapped
                }
            }
            _ => {
                // The stream's first frame: on its sender report if aligned
                // by them and the report agrees with arrival time.
                let by_report = t.sr.filter(|_| synced).map(|sr| {
                    ntp_ticks(sr.ntp)
                        .saturating_add(scale(i64::from(rtp.wrapping_sub(sr.rtp) as i32), t.rate))
                        .saturating_sub(origin)
                });
                let pts = match by_report {
                    Some(p) if p.abs_diff(arrival_pts) <= lead.unsigned_abs() => {
                        t.offset = Some(origin.saturating_neg());
                        p
                    }
                    _ => {
                        t.offset = None;
                        arrival_pts
                    }
                }
                .max(0);
                t.anchor = Some((ext, pts));
                pts
            }
        };
        t.reanchor = false;
        t.last = Some(Last {
            ext,
            rtp,
            pts,
            arrival,
        });
        Stamp {
            pts: u64::try_from(pts).unwrap_or(0),
            reanchored,
        }
    }

    /// A sender report for `track`, at `arrival`. Returns the drift in
    /// 90 kHz ticks (positive: the stream was ahead) when it is beyond the
    /// bound, a discontinuity.
    pub fn sender_report(&mut self, track: usize, ntp: u64, rtp: u32) -> Option<i64> {
        let drift_bound = self.drift;
        let t = self.tracks.get_mut(track)?;
        if ntp == 0 {
            return None;
        }
        t.sr = Some(Sr { ntp, rtp });
        let ((ae, ap), l) = (t.anchor?, t.last?);
        let e = l
            .ext
            .saturating_add(i64::from(rtp.wrapping_sub(l.rtp) as i32));
        let predicted = ap.saturating_add(scale(e.saturating_sub(ae), t.rate));
        let n = ntp_ticks(ntp);
        let Some(offset) = t.offset else {
            t.offset = Some(predicted.saturating_sub(n));
            return None;
        };
        let truth = n.saturating_add(offset);
        let drift = predicted.saturating_sub(truth);
        if drift.unsigned_abs() <= drift_bound.unsigned_abs() {
            return None;
        }
        if drift < 0 {
            t.anchor = Some((e, truth));
        } else {
            t.offset = Some(predicted.saturating_sub(n));
        }
        t.discontinuities += 1;
        Some(drift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;
    /// 2026-11-11 in NTP seconds, roughly; any value works.
    const NTP_SECS: u64 = 3_971_808_000;

    /// Rounded up, so that whole 90 kHz ticks come back exactly.
    fn ntp(us: u64) -> u64 {
        (NTP_SECS << 32) + (u128::from(us) * (1u128 << 32)).div_ceil(1_000_000) as u64
    }

    fn two(cfg: TimelineConfig) -> Timeline {
        let mut t = Timeline::new(cfg);
        t.add_track(90_000);
        t.add_track(48_000);
        t
    }

    #[test]
    fn known_offset_aligns_by_sender_reports() {
        let mut t = two(TimelineConfig::default());
        // Video RTP 1000 and audio RTP 50 000 at sender time 0; audio's
        // first packet is captured 300 ms later (14 400 samples).
        t.note_packet(0, 1000, 20 * MS);
        t.note_packet(1, 50_000 + 14_400, 320 * MS);
        assert_eq!(t.try_align(330 * MS, 0, false), None, "still waiting");
        assert_eq!(t.sender_report(0, ntp(0), 1000), None);
        assert_eq!(t.sender_report(1, ntp(0), 50_000), None);
        assert_eq!(
            t.try_align(340 * MS, 0, false),
            Some(Alignment::SenderReports)
        );
        let v = t.stamp(0, 1000, 20 * MS);
        let a = t.stamp(1, 50_000 + 14_400, 320 * MS);
        assert_eq!((v.pts, a.pts), (0, 27_000), "300 ms apart");
        // Deltas from absolute values: 960 samples are 1800 ticks.
        for n in 1..=100u32 {
            let s = t.stamp(1, 50_000 + 14_400 + 960 * n, (320 + 20 * u64::from(n)) * MS);
            assert_eq!(s.pts, 27_000 + 1800 * u64::from(n));
            assert!(!s.reanchored);
        }
    }

    #[test]
    fn the_hold_times_out_to_arrival() {
        let mut t = two(TimelineConfig::default());
        t.note_packet(0, 1000, 100 * MS);
        t.note_packet(1, 7, 150 * MS);
        t.sender_report(0, ntp(0), 1000);
        assert_eq!(t.hold_deadline(), Some(1600 * MS));
        assert_eq!(t.try_align(1599 * MS, 0, false), None);
        assert_eq!(
            t.try_align(1600 * MS, 0, false),
            Some(Alignment::Arrival(Fallback::Timeout))
        );
        assert_eq!(t.hold_deadline(), None);
        assert_eq!(t.stamp(0, 1000, 100 * MS).pts, 0);
        assert_eq!(t.stamp(1, 7, 150 * MS).pts, 4500, "50 ms later");
        // Held bytes end it early.
        let mut t = two(TimelineConfig::default());
        t.note_packet(0, 0, 0);
        assert_eq!(
            t.try_align(MS, (4 << 20) + 1, false),
            Some(Alignment::Arrival(Fallback::HoldFull))
        );
        // A zero hold aligns at the first packet.
        let mut t = two(TimelineConfig {
            start_hold: Duration::ZERO,
            ..TimelineConfig::default()
        });
        assert_eq!(t.try_align(0, 0, false), None, "nothing yet");
        t.note_packet(1, 0, 5 * MS);
        assert_eq!(
            t.try_align(5 * MS, 0, false),
            Some(Alignment::Arrival(Fallback::Timeout))
        );
    }

    #[test]
    fn reports_that_disagree_with_arrival_are_not_used() {
        let mut t = two(TimelineConfig::default());
        t.note_packet(0, 0, 0);
        t.note_packet(1, 0, 0);
        // Audio's report says its first packet was captured 2 s after
        // video's, but they arrived together.
        t.sender_report(0, ntp(0), 0);
        t.sender_report(1, ntp(2_000 * MS), 0);
        assert_eq!(
            t.try_align(MS, 0, false),
            Some(Alignment::Arrival(Fallback::Inconsistent))
        );
    }

    #[test]
    fn drift_and_discontinuity() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(90_000);
        t.note_packet(0, 0, 0);
        t.sender_report(0, ntp(0), 0);
        assert_eq!(t.try_align(0, 0, false), Some(Alignment::SenderReports));
        let frame = |t: &mut Timeline, n: u64| t.stamp(0, (n * 3000) as u32, n * 33_333);
        assert_eq!(frame(&mut t, 0).pts, 0);
        assert_eq!(frame(&mut t, 30).pts, 90_000);
        // 50 ms of drift at 1 s: within the bound.
        assert_eq!(t.sender_report(0, ntp(1_050 * MS), 90_000), None);
        // 150 ms behind: re-anchors forward on the report.
        assert_eq!(t.sender_report(0, ntp(1_150 * MS), 90_000), Some(-13_500));
        assert_eq!(frame(&mut t, 31).pts, 90_000 + 13_500 + 3000);
        // 300 ms ahead: kept, and the report becomes the reference.
        let s = frame(&mut t, 60).pts;
        assert_eq!(t.sender_report(0, ntp(1_850 * MS), 180_000), Some(27_000));
        assert_eq!(frame(&mut t, 61).pts, s + 3000, "never back");
        assert_eq!(
            t.sender_report(0, ntp(1_883 * MS), 183_000),
            None,
            "the new reference"
        );
    }

    #[test]
    fn timestamps_never_go_back_or_run_ahead_of_arrival() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(90_000);
        t.note_packet(0, 0, 0);
        t.try_align(2_000 * MS, 0, false);
        assert_eq!(t.stamp(0, 0, 0).pts, 0);
        assert_eq!(t.stamp(0, 3000, 33 * MS).pts, 3000);
        // Back by a second: continues from the last by arrival time.
        let s = t.stamp(0, 3000u32.wrapping_sub(90_000), 66 * MS);
        assert_eq!((s.pts, s.reanchored), (3000 + 2970, true));
        // Then deltas again from there.
        let next = t.stamp(0, 3000u32.wrapping_sub(87_000), 99 * MS).pts;
        assert_eq!(next, 5970 + 3000);
        // An hour ahead in one step: bounded by arrival time.
        let s = t.stamp(
            0,
            3000u32.wrapping_sub(87_000).wrapping_add(324_000_000),
            132 * MS,
        );
        assert!(s.reanchored);
        assert_eq!(s.pts, next + 2970);
        // Across the 32-bit wrap, ordinary deltas.
        let mut t = Timeline::new(TimelineConfig {
            start_hold: Duration::ZERO,
            ..TimelineConfig::default()
        });
        t.add_track(48_000);
        t.note_packet(0, u32::MAX - 959, 0);
        t.try_align(0, 0, false);
        assert_eq!(t.stamp(0, u32::MAX - 959, 0).pts, 0);
        assert_eq!(t.stamp(0, 0, 20 * MS).pts, 1800);
        // A marked discontinuity re-anchors silently (the event was the
        // depacketiser's).
        t.discontinuity(0);
        let s = t.stamp(0, 5_000_000, 40 * MS);
        assert_eq!((s.pts, s.reanchored), (1800 + 1800, false));
    }

    #[test]
    fn a_hostile_report_cannot_place_media_ahead() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(90_000);
        t.note_packet(0, 0, 0);
        t.sender_report(0, ntp(0), 0);
        t.try_align(0, 0, false);
        assert_eq!(t.stamp(0, 0, 0).pts, 0);
        assert_eq!(t.stamp(0, 3000, 33 * MS).pts, 3000);
        // A report a day ahead: the stream re-anchors forward...
        assert!(t.sender_report(0, ntp(86_400_000 * MS), 3000).is_some());
        // ...but its next frame is bounded by arrival time.
        let s = t.stamp(0, 6000, 66 * MS);
        assert!(s.reanchored);
        assert_eq!(s.pts, 3000 + 2970);
        // NTP zero is no mapping.
        assert_eq!(t.sender_report(0, 0, 0), None);
    }

    #[test]
    fn audio_only_and_late_streams() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(48_000);
        t.note_packet(0, 100, 10 * MS);
        t.sender_report(0, ntp(5_000 * MS), 100);
        assert_eq!(
            t.try_align(10 * MS, 0, false),
            Some(Alignment::SenderReports)
        );
        assert_eq!(t.stamp(0, 100, 10 * MS).pts, 0);
        assert_eq!(t.stamp(0, 1060, 30 * MS).pts, 1800);
        // A video stream that never sends: the hold ends on time, by the
        // audio's report, and video arriving later maps by its own report.
        let mut t = two(TimelineConfig::default());
        t.note_packet(1, 0, 0);
        t.sender_report(1, ntp(0), 0);
        assert_eq!(
            t.try_align(1_500 * MS, 0, false),
            Some(Alignment::SenderReports)
        );
        assert_eq!(t.stamp(1, 0, 0).pts, 0);
        t.note_packet(0, 500, 2_000 * MS);
        t.sender_report(0, ntp(1_990 * MS), 500);
        assert_eq!(t.stamp(0, 500, 2_000 * MS).pts, 179_100, "1.99 s");
        // Without a report it maps by arrival time.
        let mut t = two(TimelineConfig::default());
        t.note_packet(1, 0, 0);
        t.sender_report(1, ntp(0), 0);
        t.try_align(1_500 * MS, 0, false);
        t.stamp(1, 0, 0);
        assert_eq!(t.stamp(0, 500, 2_000 * MS).pts, 180_000);
    }
}
