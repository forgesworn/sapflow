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
//! - **Bounded by arrival time.** A stream's timestamps strictly increase,
//!   and stay within `max_lead` of arrival time either way, plus a slack of
//!   0.1% of the time since alignment (for clock rate differences between
//!   sender and receiver), itself at most `max_lead`. Above the bound (as
//!   when network delay falls after the start) a timestamp is held to it,
//!   silently. A timestamp that does not advance, or steps back by up to
//!   `max_lead`, continues from the last by arrival time, silently. Both
//!   count as holds. A step back further than that, a jump more than
//!   `max_lead` past the upper bound, or falling behind the lower bound,
//!   re-anchors the stream on arrival time: a discontinuity. A sequence or
//!   Opus timestamp discontinuity in the stream re-anchors it silently (the
//!   event was the depacketiser's).
//! - **Sender reports after the start.** A report is used only when it
//!   comes at least [`TimelineConfig::report_spacing`] (500 ms) after the
//!   stream's previous report and agrees with it: their NTP and RTP times
//!   advance at the same rate within 0.1% and 1 ms. So a forged report, or
//!   reports that alternate, are ignored. The error between where the
//!   stream's timestamps put the report's RTP time and where the report
//!   does is then corrected:
//!   - within `max_lead`, by slewing: the stream's timestamps advance up to
//!     [`TimelineConfig::max_slew_ppm`] (500 ppm) faster or slower until
//!     the error is gone, never backwards, with no discontinuity. A
//!     sender's audio clock running fast or slow against its NTP clock is
//!     followed this way;
//!   - behind by more, by moving forward to the report, no further than
//!     the upper bound: a discontinuity if that moves it more than
//!     `max_lead`, otherwise slewed;
//!   - ahead by more, by taking the report as the stream's new reference,
//!     since it cannot go back: no movement, no discontinuity.
//!
//!   A stream aligned or re-anchored by arrival time takes its next agreeing
//!   report as its reference.
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
    /// How far a stream's timestamps may run ahead of or behind arrival
    /// time, for network jitter, and the largest sender report error that
    /// is slewed rather than stepped. Default 500 ms.
    pub max_lead: Duration,
    /// Fastest correction towards the sender reports, in parts per million
    /// of media time. Default 500.
    pub max_slew_ppm: u32,
    /// Shortest time between two sender reports of a stream that are
    /// compared. Default 500 ms.
    pub report_spacing: Duration,
}
impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            start_hold: Duration::from_millis(1500),
            max_hold_bytes: 4 << 20,
            max_lead: Duration::from_millis(500),
            max_slew_ppm: 500,
            report_spacing: Duration::from_millis(500),
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
    arrival: u64,
}

/// Two reports agree when their NTP and RTP times advance at the same rate
/// within this many parts per million...
const AGREE_PPM: i64 = 1000;
/// ...and this many ticks (1 ms).
const AGREE_TICKS: i64 = 90;

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
    /// The last sender report compared, for the next to agree with.
    compared: Option<Sr>,
    /// Ticks still to slew, and the fraction of a tick carried.
    slew: i64,
    slew_carry: u64,
    /// Timeline ticks minus arrival ticks at the first frame: the lower
    /// bound's reference.
    floor_offset: i64,
    /// The next timestamp re-anchors on arrival time.
    reanchor: bool,
    /// Discontinuities this timeline made: re-anchors on arrival time and
    /// moves forward to a sender report.
    discontinuities: u64,
    /// Timestamps held to the bound or continued by arrival time.
    clamps: u64,
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
    lead: i64,
    slew_ppm: u64,
    spacing: u64,
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
            lead: ticks(micros(cfg.max_lead)),
            slew_ppm: u64::from(cfg.max_slew_ppm),
            spacing: micros(cfg.report_spacing),
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
            compared: None,
            slew: 0,
            slew_carry: 0,
            floor_offset: 0,
            reanchor: false,
            discontinuities: 0,
            clamps: 0,
        });
    }

    /// Timestamps of a track held to the bound or continued by arrival
    /// time.
    pub fn clamps(&self, track: usize) -> u64 {
        self.tracks.get(track).map_or(0, |t| t.clamps)
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

    /// 0.1% of the time since alignment, at most `max_lead`.
    fn slack(&self, arrival: u64) -> i64 {
        (ticks(arrival.saturating_sub(self.aligned_at)) / 1000).min(self.lead)
    }

    /// The highest timeline position a timestamp arriving at `arrival` may
    /// have.
    fn bound(&self, arrival: u64) -> i64 {
        ticks(arrival)
            .saturating_add(self.arrival_offset)
            .saturating_add(self.slack(arrival))
            .saturating_add(self.lead)
    }

    /// The timeline position of a frame of `track` with RTP timestamp
    /// `rtp`, released from a packet that arrived at `arrival`. Frames
    /// come in the order the receiver emits them. Call only once aligned.
    pub fn stamp(&mut self, track: usize, rtp: u32, arrival: u64) -> Stamp {
        let bound = self.bound(arrival);
        let far = bound.saturating_add(self.lead);
        let below = self.lead.saturating_add(self.slack(arrival));
        let arrival_pts = ticks(arrival).saturating_add(self.arrival_offset);
        let synced = self.aligned == Some(Alignment::SenderReports);
        let (origin, lead, slew_ppm) = (self.origin, self.lead, self.slew_ppm);
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
            (Some((ae, mut ap)), Some(l)) => {
                // Slew: move the anchor by up to slew_ppm of the media time
                // since the last frame.
                let advance = scale(ext.saturating_sub(l.ext), t.rate);
                if t.slew != 0 && advance > 0 {
                    let budget = u128::from(advance.unsigned_abs()) * u128::from(slew_ppm)
                        + u128::from(t.slew_carry);
                    t.slew_carry = (budget % 1_000_000) as u64;
                    let budget = i64::try_from(budget / 1_000_000).unwrap_or(i64::MAX);
                    let step = t.slew.clamp(budget.saturating_neg(), budget);
                    ap = ap.saturating_add(step);
                    t.slew -= step;
                    t.anchor = Some((ae, ap));
                }
                let mapped = ap.saturating_add(scale(ext.saturating_sub(ae), t.rate));
                // Continuing from the last frame by arrival time keeps the
                // stream's relation to arrival time exactly.
                let by_arrival = l
                    .pts
                    .saturating_add(ticks(arrival).saturating_sub(ticks(l.arrival)).max(1));
                let floor = ticks(arrival)
                    .saturating_add(t.floor_offset)
                    .saturating_sub(below);
                let (pts, event) = if t.reanchor {
                    (by_arrival, false)
                } else if mapped < l.pts.saturating_sub(lead) || mapped > far {
                    (by_arrival, true)
                } else if mapped <= l.pts {
                    t.clamps += 1;
                    (by_arrival, false)
                } else if mapped < floor {
                    (
                        ticks(arrival)
                            .saturating_add(t.floor_offset)
                            .max(l.pts.saturating_add(1)),
                        true,
                    )
                } else if mapped > bound {
                    // A little ahead of arrival time: held to the bound,
                    // and no slewing further ahead.
                    t.clamps += 1;
                    t.slew = t.slew.min(0);
                    (bound.max(l.pts.saturating_add(1)), false)
                } else {
                    (mapped, false)
                };
                if pts != mapped {
                    t.anchor = Some((ext, pts));
                }
                if t.reanchor || event {
                    t.offset = None;
                    t.slew = 0;
                }
                reanchored = event;
                t.discontinuities += u64::from(event);
                pts
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
                t.floor_offset = pts.saturating_sub(ticks(arrival));
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

    /// A sender report for `track`, arriving at `arrival`. Returns the
    /// error in 90 kHz ticks (negative: the stream was behind) when the
    /// stream moved forward to it, a discontinuity.
    pub fn sender_report(&mut self, track: usize, ntp: u64, rtp: u32, arrival: u64) -> Option<i64> {
        let cap = self.bound(arrival);
        let (lead, spacing) = (self.lead, self.spacing);
        let t = self.tracks.get_mut(track)?;
        if ntp == 0 {
            return None;
        }
        let sr = Sr { ntp, rtp, arrival };
        t.sr = Some(sr);
        let (Some((ae, ap)), Some(l)) = (t.anchor, t.last) else {
            t.compared = Some(sr);
            return None;
        };
        // One report per spacing, and only one that agrees with the last.
        let agrees = match t.compared {
            Some(p) if arrival.saturating_sub(p.arrival) < spacing => return None,
            Some(p) => {
                let dn = ntp_ticks(ntp).saturating_sub(ntp_ticks(p.ntp));
                let dr = scale(i64::from(rtp.wrapping_sub(p.rtp) as i32), t.rate);
                let tolerance = i128::from(dn) * i128::from(AGREE_PPM) / 1_000_000;
                dn > 0 && i128::from(dr.abs_diff(dn)) <= tolerance + i128::from(AGREE_TICKS)
            }
            None => false,
        };
        t.compared = Some(sr);
        if !agrees {
            return None;
        }
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
        let error = predicted.saturating_sub(truth);
        if error.unsigned_abs() <= lead.unsigned_abs() {
            t.slew = error.saturating_neg();
            return None;
        }
        t.slew = 0;
        if error > 0 {
            // Ahead: it cannot go back, so the report is the reference.
            t.offset = Some(predicted.saturating_sub(n));
            return None;
        }
        // Behind: forward to the report, no further than arrival allows,
        // and slewed if that is no further than the lead.
        let to = truth.min(cap).max(predicted);
        t.offset = Some(to.saturating_sub(n));
        let jump = to.saturating_sub(predicted);
        if jump <= lead {
            t.slew = jump;
            return None;
        }
        t.anchor = Some((e, to));
        t.discontinuities += 1;
        Some(error)
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
        assert_eq!(t.sender_report(0, ntp(0), 1000, 30 * MS), None);
        assert_eq!(t.sender_report(1, ntp(0), 50_000, 330 * MS), None);
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
        t.sender_report(0, ntp(0), 1000, 30 * MS);
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
        t.sender_report(0, ntp(0), 0, 0);
        t.sender_report(1, ntp(2_000 * MS), 0, 0);
        assert_eq!(
            t.try_align(MS, 0, false),
            Some(Alignment::Arrival(Fallback::Inconsistent))
        );
    }

    #[test]
    fn reports_slew_small_errors_and_step_large_ones() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(90_000);
        t.note_packet(0, 0, 0);
        t.sender_report(0, ntp(0), 0, 0);
        assert_eq!(t.try_align(0, 0, false), Some(Alignment::SenderReports));
        let frames = |t: &mut Timeline, range: std::ops::RangeInclusive<u64>| {
            range
                .map(|n| t.stamp(0, (n * 3000) as u32, n * 100_000 / 3).pts)
                .last()
        };
        assert_eq!(frames(&mut t, 0..=30), Some(90_000));
        // 50 ms off, alone: it does not agree with the first report.
        assert_eq!(
            t.sender_report(0, ntp(1_050 * MS), 90_000, 1_000 * MS),
            None
        );
        assert_eq!(frames(&mut t, 31..=60), Some(180_000));
        // A second later and agreeing: 4500 ticks behind, slewed at
        // 500 ppm, so 45 ticks over the next second, no discontinuity.
        assert_eq!(
            t.sender_report(0, ntp(2_050 * MS), 180_000, 2_000 * MS),
            None
        );
        assert_eq!(frames(&mut t, 61..=90), Some(270_045));
        // Reports twice as close as the spacing are not compared.
        assert_eq!(
            t.sender_report(0, ntp(2_400 * MS), 210_000, 2_200 * MS),
            None
        );
        assert_eq!(frames(&mut t, 91..=120), Some(360_090));
        // 600 ms off: alone it does not agree; with a second it does.
        assert_eq!(
            t.sender_report(0, ntp(4_650 * MS), 360_000, 4_000 * MS),
            None
        );
        assert_eq!(frames(&mut t, 121..=150), Some(450_135));
        // Behind by more than the lead: forward, as far as the arrival
        // bound allows (5 s, 0.45 ms of slack and the lead).
        let moved = t.sender_report(0, ntp(5_650 * MS), 450_000, 5_000 * MS);
        assert_eq!(moved, Some(450_135 - 508_500));
        assert_eq!(t.discontinuities(0), 1);
        assert_eq!(frames(&mut t, 151..=151), Some(495_450 + 3000));
        // Ahead by more than the lead: it cannot go back, so the report
        // becomes the reference, with nothing moved.
        assert_eq!(frames(&mut t, 152..=180), Some(495_450 + 30 * 3000));
        assert_eq!(
            t.sender_report(0, ntp(5_750 * MS), 540_000, 6_000 * MS),
            None
        );
        assert_eq!(
            t.sender_report(0, ntp(6_750 * MS), 630_000, 7_000 * MS),
            None
        );
        assert_eq!(frames(&mut t, 181..=210), Some(495_450 + 60 * 3000));
        assert_eq!(t.discontinuities(0), 1);
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
        t.sender_report(0, ntp(0), 0, 0);
        t.try_align(0, 0, false);
        let frame = |t: &mut Timeline, n: u64| t.stamp(0, (n * 3000) as u32, n * 100_000 / 3).pts;
        for n in 0..=30 {
            frame(&mut t, n);
        }
        // Reports a day ahead: the first disagrees with the last honest one.
        let day = 86_400_000 * MS;
        assert_eq!(
            t.sender_report(0, ntp(day + 1_000 * MS), 90_000, 1_000 * MS),
            None
        );
        for n in 31..=60 {
            frame(&mut t, n);
        }
        // The second agrees: the stream moves forward once, to the
        // arrival bound (2 s, 0.18 ms of slack and the lead)...
        assert!(
            t.sender_report(0, ntp(day + 2_000 * MS), 180_000, 2_000 * MS)
                .is_some()
        );
        assert_eq!(frame(&mut t, 61), 180_000 + 180 + 45_000 + 3000);
        for n in 62..=90 {
            frame(&mut t, n);
        }
        // ...and later ones, pinned there, move it no more.
        assert_eq!(
            t.sender_report(0, ntp(day + 3_000 * MS), 270_000, 3_000 * MS),
            None
        );
        assert_eq!(t.discontinuities(0), 1);
        // NTP zero is no mapping.
        assert_eq!(t.sender_report(0, 0, 0, 4_000 * MS), None);
    }

    #[test]
    fn audio_only_and_late_streams() {
        let mut t = Timeline::new(TimelineConfig::default());
        t.add_track(48_000);
        t.note_packet(0, 100, 10 * MS);
        t.sender_report(0, ntp(5_000 * MS), 100, 10 * MS);
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
        t.sender_report(1, ntp(0), 0, 0);
        assert_eq!(
            t.try_align(1_500 * MS, 0, false),
            Some(Alignment::SenderReports)
        );
        assert_eq!(t.stamp(1, 0, 0).pts, 0);
        t.note_packet(0, 500, 2_000 * MS);
        t.sender_report(0, ntp(1_990 * MS), 500, 2_000 * MS);
        assert_eq!(t.stamp(0, 500, 2_000 * MS).pts, 179_100, "1.99 s");
        // Without a report it maps by arrival time.
        let mut t = two(TimelineConfig::default());
        t.note_packet(1, 0, 0);
        t.sender_report(1, ntp(0), 0, 0);
        t.try_align(1_500 * MS, 0, false);
        t.stamp(1, 0, 0);
        assert_eq!(t.stamp(0, 500, 2_000 * MS).pts, 180_000);
    }

    /// Network delay that falls by more than the lead after the start (a
    /// queue draining): frames are held to the arrival bound, silently,
    /// never back, with no discontinuity.
    #[test]
    fn a_falling_delay_is_absorbed_without_discontinuities() {
        let mut t = Timeline::new(TimelineConfig {
            start_hold: Duration::ZERO,
            ..TimelineConfig::default()
        });
        t.add_track(90_000);
        // Frames 33.3 ms apart on the sender; 900 ms of delay falling to
        // 50 ms over 3 s, then steady, with 8 ms of jitter either way.
        let sent = |n: u64| n * 100_000 / 3;
        let arrival = |n: u64| {
            let s = sent(n);
            let delay = if s < 3_000_000 {
                900_000 - s * 850 / 3000
            } else {
                50_000
            };
            s + delay + if n.is_multiple_of(2) { 8_000 } else { 0 }
        };
        t.note_packet(0, 0, arrival(0));
        t.try_align(arrival(0), 0, false);
        let mut last = 0;
        for n in 0..600u64 {
            let s = t.stamp(0, (n * 3000) as u32, arrival(n));
            assert!(!s.reanchored, "frame {n}");
            assert!(s.pts >= last);
            last = s.pts;
        }
        assert_eq!(t.discontinuities(0), 0);
        // Clamped while the delay fell past the lead (about 1.8 s to 3 s,
        // some 37 frames); once steady, a clamp on an early frame leaves
        // the rest under the bound, and RTP deltas take over again.
        assert!((20..=60).contains(&t.clamps(0)), "{} clamps", t.clamps(0));
        let before = t.clamps(0);
        for n in 600..700u64 {
            t.stamp(0, (n * 3000) as u32, arrival(n));
        }
        assert_eq!(t.clamps(0), before, "none once steady");
    }

    /// Sender report scenarios: video at 30 frames a second on the sender's
    /// clock and audio at 50 packets a second whose RTP clock runs `ppm`
    /// fast, aligned by sender reports sent every second (or, with
    /// `forged`, audio reports alternately 1 s early and late every 0.5 s),
    /// delivered after `delay(sent)`. Returns the discontinuities and the
    /// largest A/V skew in ticks after the first 2 s.
    fn av_run(ppm: i64, secs: u64, delay: &dyn Fn(u64) -> u64, forged: bool) -> (u64, i64) {
        let mut t = two(TimelineConfig::default());
        enum Item {
            Video(u64),
            Audio(u64),
            Sr(usize, u64, u32),
        }
        let audio_rtp =
            |us: u64| (u128::from(us) * 48 * (1_000_000 + ppm) as u128 / 1_000_000_000) as u32;
        let mut items: Vec<(u64, Item)> = Vec::new();
        let end = secs * 1_000_000;
        let mut n = 0;
        while n * 100_000 / 3 < end {
            let sent = n * 100_000 / 3;
            items.push((sent + delay(sent), Item::Video(n)));
            n += 1;
        }
        for k in 0..end / 20_000 {
            let sent = k * 20_000;
            items.push((sent + delay(sent), Item::Audio(k)));
        }
        for s in 0..secs {
            let sent = s * 1_000_000 + 10_000;
            items.push((
                sent + delay(sent),
                Item::Sr(0, ntp(sent), (sent * 9 / 100) as u32),
            ));
            if forged && s > 2 {
                for (half, skew) in [(0, 1_000_000), (500_000, 0)] {
                    let at = sent + half;
                    let lie = if (s + half / 500_000) % 2 == 0 {
                        at + skew
                    } else {
                        at.saturating_sub(1_000_000 - skew)
                    };
                    items.push((at + delay(at), Item::Sr(1, ntp(lie), audio_rtp(at))));
                }
            } else {
                items.push((sent + delay(sent), Item::Sr(1, ntp(sent), audio_rtp(sent))));
            }
        }
        items.sort_by_key(|i| i.0);
        let (mut video_err, mut skew_max) = (None::<i64>, 0i64);
        let mut aligned = false;
        for (arrival, item) in items {
            match item {
                Item::Video(n) => {
                    if !aligned {
                        t.note_packet(0, 0, arrival);
                    }
                    if aligned {
                        let pts = t.stamp(0, (n * 3000) as u32, arrival).pts as i64;
                        video_err = Some(pts - (n * 3000) as i64);
                    }
                }
                Item::Audio(k) => {
                    if !aligned {
                        t.note_packet(1, 0, arrival);
                    }
                    if aligned {
                        let sent = k * 20_000;
                        let pts = t.stamp(1, audio_rtp(sent), arrival).pts as i64;
                        let err = pts - (sent * 9 / 100) as i64;
                        if let Some(v) = video_err
                            && sent > 2_000_000
                        {
                            skew_max = skew_max.max((err - v).abs());
                        }
                    }
                }
                Item::Sr(k, ntp, rtp) => {
                    t.sender_report(k, ntp, rtp, arrival);
                }
            }
            if !aligned && t.try_align(arrival, 0, false).is_some() {
                aligned = true;
            }
        }
        (t.discontinuities(0) + t.discontinuities(1), skew_max)
    }

    /// The reviewer's drift cases: an hour at +-30 and +-100 ppm. No
    /// discontinuity, and audio stays within 1 ms of video.
    #[test]
    fn audio_clock_drift_is_slewed_not_stepped() {
        let runs: Vec<_> = [30, -30, 100, -100]
            .into_iter()
            .map(|ppm| (ppm, av_run(ppm, 3600, &|_| 40_000, false)))
            .collect();
        for &(ppm, (disc, skew)) in &runs {
            eprintln!("drift {ppm} ppm: {disc} discontinuities, max skew {skew} ticks");
        }
        for (ppm, (disc, skew)) in runs {
            assert_eq!(disc, 0, "{ppm} ppm");
            assert!(skew <= 90, "{ppm} ppm: {skew} ticks");
        }
    }

    /// Forged audio reports alternately a second early and late, every
    /// half second: no storm of discontinuities, and audio keeps to its
    /// RTP timestamps.
    #[test]
    fn forged_alternating_reports_cause_no_storm() {
        let (disc, skew) = av_run(0, 120, &|_| 40_000, true);
        eprintln!("forged: {disc} discontinuities, max skew {skew} ticks");
        assert_eq!(disc, 0);
        assert!(skew <= 90, "{skew} ticks");
    }

    /// The falling delay, with sender reports: no discontinuity.
    #[test]
    fn a_falling_delay_with_reports_is_not_a_discontinuity() {
        let falling = |s: u64| {
            if s < 3_000_000 {
                900_000 - s * 850 / 3000
            } else {
                50_000
            }
        };
        let (disc, skew) = av_run(0, 60, &falling, false);
        eprintln!("falling delay with reports: {disc} discontinuities, max skew {skew} ticks");
        assert_eq!(disc, 0);
        // Both streams are held to the same arrival bound; skew is measured
        // against the latest video frame, and while the delay falls it
        // falls 9.4 ms (850 ticks) a video frame.
        assert!(skew <= 900, "{skew} ticks");
    }

    /// The reviewer's constant-timestamp case: 36 000 video frames (20
    /// minutes at 30 a second) that all carry one RTP timestamp. pts
    /// strictly increases and keeps within the lead of arrival time.
    #[test]
    fn a_constant_timestamp_neither_freezes_nor_crawls() {
        let mut t = Timeline::new(TimelineConfig {
            start_hold: Duration::ZERO,
            ..TimelineConfig::default()
        });
        t.add_track(90_000);
        t.note_packet(0, 7, 0);
        t.try_align(0, 0, false);
        let mut last = None;
        let (mut flat, mut behind) = (0, 0);
        let lead = 45_000 + 45_000;
        for n in 0..36_000u64 {
            let arrival = n * 100_000 / 3;
            let pts = t.stamp(0, 7, arrival).pts;
            flat += u32::from(last.is_some_and(|prev| pts <= prev));
            behind += u32::from((pts as i64) + lead < ticks(arrival));
            last = Some(pts);
        }
        eprintln!(
            "constant timestamp: final pts {} ticks for 1200 s, {} discontinuities, {flat} not increasing, {behind} behind arrival",
            last.unwrap_or(0),
            t.discontinuities(0)
        );
        assert_eq!((flat, behind), (0, 0));
    }
}
