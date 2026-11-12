//! How often a stream's recording may roll to a new file at a codec epoch.
//!
//! Every SPS or PPS change at an IDR starts a codec epoch, and HLS must
//! serve every one: a decoder cannot take new parameter sets under the old
//! init. A recording, though, rolls to a new file at each, and an encoder
//! that changes its parameter sets at every keyframe would make a file and a
//! `recordSegmentComplete` callback a second. So each stream has a budget of
//! rolls: a burst, refilled one roll per period of media. A roll with no
//! budget left finishes the current file as usual and starts none; the
//! epochs after it are served but not recorded. Recording resumes once an
//! epoch has lasted the settle time and a roll is available, from that
//! epoch's first retained part. The file that ran out and the resumed one
//! are marked `epoch-rate`.
//!
//! Time is media time, the muxer's published edge, so a stream's budget
//! does not refill during an outage, and tests can drive it.

use std::time::Duration;

/// The budget's settings (`SAPFLOW_RECORDING_ROLL_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollLimit {
    /// Rolls available at the start and at most at any time; 0 turns the
    /// budget off.
    pub burst: u32,
    /// Media time per roll regained.
    pub refill: Duration,
    /// How long an epoch must last before recording resumes in it.
    pub settle: Duration,
}

impl Default for RollLimit {
    fn default() -> Self {
        Self {
            burst: 8,
            refill: Duration::from_secs(60),
            settle: Duration::from_secs(10),
        }
    }
}

/// What a codec epoch change does to the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Roll {
    /// Roll to a new file, as without a budget.
    Record,
    /// The budget has just run out: the current file finishes, marked, and
    /// the new epoch is not recorded.
    RanOut,
    /// Already out: the new epoch is not recorded either.
    Skip,
}

/// One stream's budget, kept across reconnects with the stream.
#[derive(Debug, Clone)]
pub struct RollBudget {
    limit: RollLimit,
    tokens: u32,
    /// Media time up to which refills have been counted.
    counted: Duration,
    /// Media time at which the current epoch began.
    epoch_start: Duration,
    out: bool,
    /// Epochs served but not recorded.
    skipped: u64,
}

impl RollBudget {
    pub fn new(limit: RollLimit) -> Self {
        Self {
            limit,
            tokens: limit.burst,
            counted: Duration::ZERO,
            epoch_start: Duration::ZERO,
            out: false,
            skipped: 0,
        }
    }

    /// Whether the budget is spent and recording waits to resume.
    pub fn is_out(&self) -> bool {
        self.out
    }

    /// How long an epoch must last before recording resumes in it.
    pub fn settle(&self) -> Duration {
        self.limit.settle
    }

    /// Epochs served but not recorded, over the stream's life.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    fn refill(&mut self, now: Duration) {
        if self.tokens >= self.limit.burst || self.limit.refill.is_zero() {
            self.counted = self.counted.max(now);
            return;
        }
        // Media time can step back a little (a run that ended with its
        // media after a clamped part): nothing is regained then.
        let Some(elapsed) = now.checked_sub(self.counted) else {
            return;
        };
        let gained = elapsed.as_nanos() / self.limit.refill.as_nanos();
        let gained = u32::try_from(gained).unwrap_or(u32::MAX);
        self.tokens = self.tokens.saturating_add(gained).min(self.limit.burst);
        if self.tokens >= self.limit.burst {
            self.counted = now;
        } else {
            self.counted += self.limit.refill * gained;
        }
    }

    /// A codec epoch begins at media time `now`. `charged`: the file it
    /// rolls from was written; a roll from a recording that never started
    /// (an epoch that was never served) makes no more files and is free.
    pub fn epoch(&mut self, now: Duration, charged: bool) -> Roll {
        self.epoch_start = now;
        if self.limit.burst == 0 {
            return Roll::Record;
        }
        self.refill(now);
        if self.out {
            self.skipped += 1;
            return Roll::Skip;
        }
        if !charged {
            return Roll::Record;
        }
        if self.tokens > 0 {
            self.tokens -= 1;
            return Roll::Record;
        }
        self.out = true;
        self.skipped += 1;
        Roll::RanOut
    }

    /// While out: whether recording may resume at media time `now`, the
    /// current epoch having lasted the settle time with a roll available.
    /// [`RollBudget::resume`] then spends it.
    pub fn may_resume(&mut self, now: Duration) -> bool {
        if !self.out {
            return false;
        }
        self.refill(now);
        self.tokens > 0 && now.saturating_sub(self.epoch_start) >= self.limit.settle
    }

    /// Recording resumes in the current epoch.
    pub fn resume(&mut self) {
        debug_assert!(self.out && self.tokens > 0);
        self.tokens = self.tokens.saturating_sub(1);
        self.out = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: u64) -> Duration {
        Duration::from_secs(secs)
    }

    #[test]
    fn a_burst_records_then_runs_out_and_skips() {
        let mut b = RollBudget::new(RollLimit::default());
        for i in 0..8 {
            assert_eq!(b.epoch(s(i), true), Roll::Record, "roll {i}");
        }
        assert_eq!(b.epoch(s(8), true), Roll::RanOut);
        assert_eq!(b.epoch(s(9), true), Roll::Skip);
        assert!(b.is_out());
        assert_eq!(b.skipped(), 2);
    }

    #[test]
    fn recording_resumes_after_the_settle_time_with_a_roll() {
        let mut b = RollBudget::new(RollLimit::default());
        for i in 0..9 {
            b.epoch(s(i), true);
        }
        // The epoch began at 8 s; no roll is back before 60 s of media.
        assert!(!b.may_resume(s(30)));
        assert!(b.may_resume(s(60)), "refilled, and settled since 8 s");
        b.resume();
        assert!(!b.is_out());
        // A later epoch that does not settle keeps it out again.
        assert_eq!(b.epoch(s(61), true), Roll::RanOut);
        assert!(!b.may_resume(s(65)), "under the settle time");
    }

    #[test]
    fn a_settled_epoch_waits_for_a_roll() {
        let mut b = RollBudget::new(RollLimit {
            burst: 1,
            refill: s(60),
            settle: s(10),
        });
        assert_eq!(b.epoch(s(0), true), Roll::Record);
        assert_eq!(b.epoch(s(1), true), Roll::RanOut);
        assert!(!b.may_resume(s(30)), "settled, but no roll");
        assert!(b.may_resume(s(60)));
    }

    #[test]
    fn a_burst_of_zero_turns_the_budget_off() {
        let mut b = RollBudget::new(RollLimit {
            burst: 0,
            ..RollLimit::default()
        });
        for i in 0..1000 {
            assert_eq!(b.epoch(s(i), true), Roll::Record);
        }
        assert!(!b.may_resume(s(2000)));
        assert_eq!(b.skipped(), 0);
    }

    #[test]
    fn the_budget_does_not_bank_beyond_its_burst() {
        let mut b = RollBudget::new(RollLimit::default());
        // An hour of stable media, then a storm: still only the burst.
        let mut rolls = 0;
        for i in 0..20 {
            if b.epoch(s(3600 + i), true) == Roll::Record {
                rolls += 1;
            }
        }
        assert_eq!(rolls, 8);
    }

    #[test]
    fn media_time_stepping_back_regains_nothing() {
        let mut b = RollBudget::new(RollLimit {
            burst: 2,
            refill: s(60),
            settle: s(10),
        });
        b.epoch(s(100), true);
        b.epoch(s(101), true);
        assert_eq!(b.epoch(s(99), true), Roll::RanOut);
        assert!(!b.may_resume(s(98)));
    }

    /// Arbitrary epoch timings: the files started never exceed the burst
    /// plus one per refill period of media (and the free first file), and
    /// an epoch change that finds a roll always records.
    #[test]
    fn rolls_never_exceed_the_burst_plus_refills() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..2000 {
            let limit = RollLimit {
                burst: (next() % 10) as u32,
                refill: Duration::from_millis(1 + next() % 120_000),
                settle: Duration::from_millis(next() % 30_000),
            };
            let mut b = RollBudget::new(limit);
            let mut now = Duration::ZERO;
            let mut files = 1u64; // the stream's first file
            for _ in 0..(next() % 400) {
                now += Duration::from_millis(next() % 20_000);
                let had = b.tokens > 0 && !b.out;
                // Now and then a roll from a recording that never started.
                let charged = next() % 5 != 0;
                match b.epoch(now, charged) {
                    Roll::Record if charged => files += 1,
                    Roll::Record => {}
                    Roll::RanOut | Roll::Skip => {
                        assert!(limit.burst > 0 && !had, "case {case}: a roll was available");
                    }
                }
                if next() % 3 == 0 {
                    now += Duration::from_millis(next() % 40_000);
                    if b.may_resume(now) {
                        b.resume();
                        files += 1;
                    }
                }
            }
            if limit.burst > 0 {
                let refills = (now.as_nanos() / limit.refill.as_nanos()) as u64;
                assert!(
                    files <= 1 + u64::from(limit.burst) + refills,
                    "case {case}: {files} files, burst {}, {refills} refills",
                    limit.burst
                );
            }
        }
    }
}
