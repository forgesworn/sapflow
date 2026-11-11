//! Reception statistics for the report blocks of sapflow's receiver
//! reports (RFC 3550 section 6.4.1, with the arithmetic of Appendix A.3
//! and the jitter estimator of Appendix A.8), one per media SSRC.
//!
//! What is counted:
//!
//! - Only packets on the media SSRC. RTX packets are a separate RTP stream
//!   (RFC 4588 section 6.2, SSRC multiplexing), so a packet repaired over
//!   RTX still counts as lost here: the sender's loss-based rate control
//!   sees the network's loss before repair. A sender without RTX (OBS)
//!   resends on the media SSRC and sequence number, which cannot be told
//!   from a late original, so its repaired packets count as received.
//! - Each sequence number once. A copy of one already counted, whether a
//!   network duplicate or a needless retransmission, changes nothing, so
//!   cumulative loss never goes negative (Appendix A.3 counts duplicates;
//!   sapflow does not). A late packet never counted before is counted,
//!   even after the reorder buffer declared it lost.
//! - Within [`WINDOW`] sequence numbers of the highest. Older packets are
//!   neither counted nor remembered.
//!
//! Jitter (Appendix A.8, in timestamp units) is sampled only from packets
//! that arrive for the first time and were never NACKed: a retransmission
//! arrives an RTT late by design and would swamp the estimate.

use crate::rtcp::ReportBlock;

/// Where extended sequence numbers start, so that reordering at the start
/// cannot take them below zero. The extended highest sequence number in a
/// report is the offset from here: cycles in the high 16 bits.
const BASE: u64 = 1 << 32;
/// Sequence numbers behind the highest that are remembered, so each is
/// counted once. Above the reorder buffer's default `max_dropout` (3000),
/// so every packet it calls late is within the window.
pub(crate) const WINDOW: u64 = 4096;
const WORDS: usize = (WINDOW / 64) as usize;

/// Which of the last [`WINDOW`] extended sequence numbers have been seen,
/// one bit each, so that each packet is counted or reported once. Fixed
/// size: moving forward reuses the bits of the oldest.
#[derive(Debug, Clone)]
pub(crate) struct SeqWindow {
    seen: [u64; WORDS],
}
impl SeqWindow {
    pub fn new() -> Self {
        Self { seen: [0; WORDS] }
    }

    fn bit(ext: u64) -> (usize, u64) {
        let slot = ext % WINDOW;
        ((slot / 64) as usize, 1 << (slot % 64))
    }

    pub fn is_seen(&self, ext: u64) -> bool {
        let (w, b) = Self::bit(ext);
        self.seen.get(w).is_some_and(|word| word & b != 0)
    }

    pub fn mark(&mut self, ext: u64) {
        let (w, b) = Self::bit(ext);
        if let Some(word) = self.seen.get_mut(w) {
            *word |= b;
        }
    }

    /// Moves the highest from `from` to `to`: forgets the bits of
    /// `(from, to]`, which are about to be reused. Word by word, so a
    /// jump costs at most [`WINDOW`] / 64 steps.
    pub fn advance(&mut self, from: u64, to: u64) {
        if to.saturating_sub(from) >= WINDOW {
            self.seen = [0; WORDS];
            return;
        }
        let mut ext = from + 1;
        while ext <= to {
            let (w, b) = Self::bit(ext);
            let whole = (ext % WINDOW).is_multiple_of(64) && to - ext >= 63;
            if let Some(word) = self.seen.get_mut(w) {
                *word &= if whole { 0 } else { !b };
            }
            ext += if whole { 64 } else { 1 };
        }
    }
}

/// One media SSRC's reception statistics.
#[derive(Debug, Clone)]
pub(crate) struct Reception {
    clock_rate: u32,
    started: bool,
    /// Lowest and highest extended sequence numbers counted.
    base: u64,
    max: u64,
    /// Distinct sequence numbers counted.
    received: u64,
    /// The extended sequence numbers in `(max - WINDOW, max]` counted.
    seen: SeqWindow,
    expected_prior: u64,
    received_prior: u64,
    /// Jitter scaled by 16, as Appendix A.8 keeps it.
    jitter: u64,
    /// The last relative transit time sampled, in timestamp units.
    transit: Option<u32>,
    /// The middle 32 bits of the last SR's NTP timestamp, and when it
    /// arrived (microseconds).
    last_sr: Option<(u32, u64)>,
}

/// How a counted packet was taken by the reorder buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// First seen, never NACKed: counted and sampled for jitter.
    Fresh,
    /// Possibly a retransmission or a copy: counted if new, not sampled.
    Repeat,
}

impl Reception {
    pub fn new(clock_rate: u32) -> Self {
        Self {
            clock_rate,
            started: false,
            base: BASE,
            max: BASE,
            received: 0,
            seen: SeqWindow::new(),
            expected_prior: 0,
            received_prior: 0,
            jitter: 0,
            transit: None,
            last_sr: None,
        }
    }

    /// Whether any packet has been counted.
    pub fn started(&self) -> bool {
        self.started
    }

    fn extend(&self, seq: u16) -> u64 {
        let step = i64::from(seq.wrapping_sub(self.max as u16) as i16);
        self.max.saturating_add_signed(step)
    }

    /// Counts a packet on the media SSRC: its sequence number, RTP
    /// timestamp and arrival time in microseconds. Returns whether it was
    /// counted (false for a copy, or one older than the window).
    pub fn count(&mut self, seq: u16, timestamp: u32, arrival: u64, how: Arrival) -> bool {
        if !self.started {
            self.started = true;
            self.base = BASE + u64::from(seq);
            self.max = self.base;
            self.seen = SeqWindow::new();
            self.seen.mark(self.max);
            self.received = 1;
            self.sample(timestamp, arrival, how);
            return true;
        }
        let ext = self.extend(seq);
        if ext > self.max {
            self.seen.advance(self.max, ext);
            self.max = ext;
        } else if self.max - ext >= WINDOW || self.seen.is_seen(ext) {
            return false;
        }
        self.seen.mark(ext);
        self.base = self.base.min(ext);
        self.received += 1;
        self.sample(timestamp, arrival, how);
        true
    }

    /// The sequence numbers restarted (the reorder buffer confirmed a
    /// jump with two packets in a row): the counts start again from `seq`.
    pub fn restart(&mut self) {
        let last_sr = self.last_sr;
        *self = Self::new(self.clock_rate);
        self.last_sr = last_sr;
    }

    /// Appendix A.8: the transit time in timestamp units against the
    /// arrival clock, and the running mean of its change.
    fn sample(&mut self, timestamp: u32, arrival: u64, how: Arrival) {
        if how != Arrival::Fresh {
            return;
        }
        let ticks = u128::from(arrival) * u128::from(self.clock_rate) / 1_000_000;
        let transit = (ticks as u32).wrapping_sub(timestamp);
        if let Some(prev) = self.transit {
            let d = u64::from((transit.wrapping_sub(prev) as i32).unsigned_abs());
            // J += (|D| - J) / 16, kept scaled by 16.
            self.jitter = self.jitter + d - ((self.jitter + 8) >> 4);
        }
        self.transit = Some(transit);
    }

    /// Records a sender report: the 64-bit NTP timestamp, at `arrival`.
    pub fn sender_report(&mut self, ntp: u64, arrival: u64) {
        self.last_sr = Some(((ntp >> 16) as u32, arrival));
    }

    /// The report block for `ssrc` at `now`, without starting a new
    /// interval.
    pub fn block(&self, ssrc: u32, now: u64) -> ReportBlock {
        let expected = self.max - self.base + 1;
        let lost = i64::try_from(expected)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.received).unwrap_or(i64::MAX));
        let expected_interval = expected.saturating_sub(self.expected_prior);
        let received_interval = self.received.saturating_sub(self.received_prior);
        let lost_interval = i128::from(expected_interval) - i128::from(received_interval);
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / i128::from(expected_interval)).min(255) as u8
        };
        let (last_sr, delay_since_last_sr) = match self.last_sr {
            Some((middle, at)) => {
                let delay = u128::from(now.saturating_sub(at)) * 65_536 / 1_000_000;
                (middle, u32::try_from(delay).unwrap_or(u32::MAX))
            }
            None => (0, 0),
        };
        ReportBlock {
            ssrc,
            fraction_lost,
            // The builder clamps this to 24 bits.
            cumulative_lost: lost.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            highest_sequence: (self.max - BASE) as u32,
            jitter: u32::try_from(self.jitter >> 4).unwrap_or(u32::MAX),
            last_sr,
            delay_since_last_sr,
        }
    }

    /// Starts a new interval for the fraction lost, after a report block
    /// was sent.
    pub fn reported(&mut self) {
        self.expected_prior = self.max - self.base + 1;
        self.received_prior = self.received;
    }

    /// Distinct packets counted.
    #[cfg(any(test, feature = "testing"))]
    pub fn received(&self) -> u64 {
        self.received
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    fn fresh(r: &mut Reception, seq: u16, ts: u32, at: u64) -> bool {
        r.count(seq, ts, at, Arrival::Fresh)
    }

    /// 100 packets expected, 3 never arrive: 3 * 256 / 100 = 7.68, so 7.
    #[test]
    fn fraction_and_cumulative_loss_by_hand() {
        let mut r = Reception::new(48_000);
        for seq in 1000u16..1100 {
            if [1010, 1050, 1051].contains(&seq) {
                continue;
            }
            fresh(&mut r, seq, u32::from(seq) * 960, u64::from(seq) * 20 * MS);
        }
        let b = r.block(9, 0);
        assert_eq!(b.highest_sequence, 1099);
        assert_eq!((b.cumulative_lost, b.fraction_lost), (3, 7));
        r.reported();
        // The next interval: 50 expected, 1 lost; 256 / 50 = 5.12, so 5.
        for seq in 1100u16..1150 {
            if seq != 1120 {
                fresh(&mut r, seq, 0, 0);
            }
        }
        let b = r.block(9, 0);
        assert_eq!((b.cumulative_lost, b.fraction_lost), (4, 5));
        r.reported();
        // A late arrival of a packet lost earlier: the interval expected
        // nothing new, so the fraction is 0.
        assert!(r.count(1010, 0, 0, Arrival::Repeat));
        let b = r.block(9, 0);
        assert_eq!((b.cumulative_lost, b.fraction_lost), (3, 0));
        // Nothing new at all: nothing expected, fraction 0.
        r.reported();
        assert_eq!(r.block(9, 0).fraction_lost, 0);
    }

    /// Appendix A.3 would count the copies and report -2 here.
    #[test]
    fn duplicates_and_retransmissions_count_once() {
        let mut r = Reception::new(90_000);
        for seq in [10u16, 11, 12, 12, 11, 13] {
            r.count(seq, 0, 0, Arrival::Repeat);
        }
        assert_eq!(r.received(), 4);
        let b = r.block(1, 0);
        assert_eq!((b.cumulative_lost, b.highest_sequence), (0, 13));
        // 14 lost, then a needless resend of 13 and the real 15.
        assert!(!r.count(13, 0, 0, Arrival::Repeat));
        assert!(fresh(&mut r, 15, 0, 0));
        assert_eq!(r.block(1, 0).cumulative_lost, 1);
        // Reordering before the first packet extends the range back.
        let mut r = Reception::new(90_000);
        fresh(&mut r, 5, 0, 0);
        fresh(&mut r, 3, 0, 0);
        assert_eq!(r.block(1, 0).cumulative_lost, 1, "4 is missing");
    }

    #[test]
    fn sequence_wrap_extends_the_highest() {
        let mut r = Reception::new(90_000);
        for seq in (65_530u16..=65_535).chain(0..5) {
            fresh(&mut r, seq, 0, 0);
        }
        // Reordered from before the wrap.
        assert!(!r.count(65_534, 0, 0, Arrival::Repeat));
        let b = r.block(1, 0);
        assert_eq!(b.highest_sequence, 0x0001_0004, "one cycle, then 4");
        assert_eq!(b.cumulative_lost, 0);
        // Older than the window: neither counted nor treated as a wrap.
        for seq in 5u16..5000 {
            fresh(&mut r, seq, 0, 0);
        }
        assert!(!r.count(500, 0, 0, Arrival::Repeat));
        assert_eq!(r.block(1, 0).highest_sequence, 0x0001_0000 + 4999);
    }

    /// Opus at 48 kHz, 20 ms apart (960 ticks). The third packet is 5 ms
    /// (240 ticks) late, the fourth on time again: D is 0, 240, 240.
    /// Appendix A.8's integer form: 240 - 0 = 240, so J is 15; then
    /// 240 - ((240 + 8) >> 4) = 225 more, 465 scaled, so J is 29 (the
    /// exact estimator gives 15 + 225 / 16 = 29.06).
    #[test]
    fn jitter_by_hand() {
        let mut r = Reception::new(48_000);
        fresh(&mut r, 1, 0, 1_000 * MS);
        fresh(&mut r, 2, 960, 1_020 * MS);
        assert_eq!(r.block(1, 0).jitter, 0);
        fresh(&mut r, 3, 1920, 1_045 * MS);
        assert_eq!(r.block(1, 0).jitter, 15);
        fresh(&mut r, 4, 2880, 1_060 * MS);
        assert_eq!(r.block(1, 0).jitter, 29);
        // A retransmission an RTT late is not sampled.
        r.count(5, 3840, 1_200 * MS, Arrival::Repeat);
        assert_eq!(r.block(1, 0).jitter, 29);
        // Timestamps across the 32-bit wrap sample correctly.
        let mut r = Reception::new(90_000);
        fresh(&mut r, 1, u32::MAX - 1499, 0);
        fresh(&mut r, 2, 1500, 33_333);
        assert_eq!(r.block(1, 0).jitter, 0, "3000 ticks in 33.333 ms");
    }

    #[test]
    fn lsr_and_dlsr() {
        let mut r = Reception::new(90_000);
        fresh(&mut r, 1, 0, 0);
        assert_eq!(
            (r.block(1, 0).last_sr, r.block(1, 0).delay_since_last_sr),
            (0, 0)
        );
        r.sender_report(0xaabb_ccdd_eeff_0011, 2_000 * MS);
        // 1.5 s later: 1.5 * 65536 = 98 304.
        let b = r.block(1, 3_500 * MS);
        assert_eq!((b.last_sr, b.delay_since_last_sr), (0xccdd_eeff, 98_304));
        // A clock that reads earlier than the SR gives 0, not a wrap.
        assert_eq!(r.block(1, 0).delay_since_last_sr, 0);
        // Saturates after 18 hours.
        assert_eq!(r.block(1, u64::MAX).delay_since_last_sr, u32::MAX);
        r.restart();
        assert!(!r.started());
        assert_eq!(r.block(1, 2_000 * MS).last_sr, 0xccdd_eeff, "kept");
    }

    #[test]
    fn cumulative_loss_clamps_to_24_bits() {
        let mut r = Reception::new(90_000);
        let mut seq = 0u16;
        // 3000 forward each time: 2999 lost per packet, about 8.4 million.
        for _ in 0..2800 {
            fresh(&mut r, seq, 0, 0);
            seq = seq.wrapping_add(3000);
        }
        let b = r.block(1, 0);
        assert_eq!(b.cumulative_lost, 2799 * 2999);
        let mut out = Vec::new();
        crate::rtcp::build::receiver_report(&mut out, 1, &[b]);
        let c = crate::rtcp::Compound::parse(&out).expect("parses");
        let Some(Ok(crate::rtcp::Packet::ReceiverReport(rr))) = c.packets().next() else {
            panic!("an RR");
        };
        let read = rr.reports().next().expect("a block");
        assert_eq!(read.cumulative_lost, 0x7f_ffff);
        assert_eq!(read.fraction_lost, 255, "2999 of 3000 in the interval");
    }
}
