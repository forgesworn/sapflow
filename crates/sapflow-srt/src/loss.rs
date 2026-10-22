//! The receiver's loss list: sequence numbers not yet received, as ranges,
//! each with when it was last asked for.
//!
//! When to ask again follows what `docs/interop.md` measured against
//! libsrt's own receiver: after a round trip and its variance, and every
//! quarter round trip (at most eight times) once the packet's delivery
//! deadline is under three round trips away, but never once a
//! retransmission could no longer arrive before the deadline.

use crate::seq::Seq;

/// Re-requests closer together than one round trip, per loss.
pub const MAX_FAST_REQUESTS: u32 = 8;
/// Ranges tracked at most. The list is scanned on every packet, so a
/// sender that leaves every other packet missing must not grow it with the
/// buffer; gaps beyond this are reported once and then left to the
/// too-late drop.
pub const MAX_RANGES: usize = 1024;
/// Within this many round trips of the deadline, ask every quarter round trip.
const NEAR_DEADLINE_RTTS: i64 = 3;
/// However short the round trip, ask again no more often than this
/// (microseconds; the ACK period)...
const MIN_INTERVAL: i64 = 10_000;
/// ...or this near the deadline.
const MIN_FAST_INTERVAL: i64 = 2_500;

/// A run of lost sequence numbers, `first..=last`, and the state of asking
/// for it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loss {
    /// First lost sequence number.
    pub first: Seq,
    /// Last lost sequence number, inclusive.
    pub last: Seq,
    /// When the packet that revealed the loss is to be delivered: the lost
    /// ones are due no later (microseconds, the listener's clock).
    pub deadline: i64,
    /// When the range was last asked for (microseconds, the listener's
    /// clock).
    pub last_request: i64,
    /// How many times the range has been asked for, the first report
    /// included.
    pub requests: u32,
    /// How many requests were made within three round trips of the deadline,
    /// counted up to [`MAX_FAST_REQUESTS`]; after that the ordinary interval
    /// applies.
    pub fast_requests: u32,
}

/// The round trip and how long to wait before asking again (at least the
/// round trip; more allows for the retransmission's own delay), in
/// microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The round-trip time, in microseconds.
    pub rtt: i64,
    /// How long to wait after a request before asking again, in microseconds;
    /// used as at least `rtt`.
    pub retry: i64,
}

impl Timing {
    /// Ask again exactly one round trip later.
    pub const fn rtt(rtt: i64) -> Self {
        Self { rtt, retry: rtt }
    }
}

/// The losses to ask for: sorted, non-overlapping ranges, at most
/// [`MAX_RANGES`].
#[derive(Debug, Default, Clone)]
pub struct LossList {
    /// Sorted, not overlapping.
    entries: Vec<Loss>,
}

impl LossList {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing is missing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of ranges on the list; [`count`](Self::count) gives the number
    /// of sequence numbers.
    pub fn ranges(&self) -> usize {
        self.entries.len()
    }

    /// Sequence numbers missing in all.
    pub fn count(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.first.distance_to(e.last) as u64 + 1)
            .sum()
    }

    /// Whether `s` is a missing sequence number on the list.
    pub fn contains(&self, s: Seq) -> bool {
        self.entries.iter().any(|e| e.first <= s && s <= e.last)
    }

    /// The earliest missing sequence number, if any.
    pub fn first(&self) -> Option<Seq> {
        self.entries.first().map(|e| e.first)
    }

    /// Records `first..=last` as lost, asked for once at `now`.
    pub fn add(&mut self, first: Seq, last: Seq, now: i64, deadline: i64) {
        if last < first || self.entries.len() >= MAX_RANGES {
            return;
        }
        let e = Loss {
            first,
            last,
            deadline,
            last_request: now,
            requests: 1,
            fast_requests: 0,
        };
        // Gaps are found at the leading edge, so this is nearly always a push.
        let at = self
            .entries
            .iter()
            .rposition(|x| x.last < first)
            .map_or(0, |i| i + 1);
        if self.entries.get(at).is_some_and(|x| x.first <= last) {
            // Overlaps what is already known; keep the existing record.
            return;
        }
        self.entries.insert(at, e);
    }

    /// `s` arrived. Returns whether it was on the list.
    pub fn remove(&mut self, s: Seq) -> bool {
        let Some(i) = self
            .entries
            .iter()
            .position(|e| e.first <= s && s <= e.last)
        else {
            return false;
        };
        let Some(e) = self.entries.get_mut(i) else {
            return false;
        };
        match (e.first == s, e.last == s) {
            (true, true) => {
                self.entries.remove(i);
            }
            (true, false) => e.first = s.next(),
            (false, true) => e.last = s.prev(),
            (false, false) => {
                let mut tail = *e;
                e.last = s.prev();
                tail.first = s.next();
                self.entries.insert(i + 1, tail);
                // At the limit the latest losses are no longer tracked.
                self.entries.truncate(MAX_RANGES);
            }
        }
        true
    }

    /// Forgets everything before `s` (delivered or given up on).
    pub fn remove_before(&mut self, s: Seq) {
        self.entries.retain_mut(|e| {
            if e.last < s {
                return false;
            }
            if e.first < s {
                e.first = s;
            }
            true
        });
    }

    /// Forgets `first..=last` (the sender said it no longer has them).
    pub fn remove_range(&mut self, first: Seq, last: Seq) {
        if last < first {
            return;
        }
        let mut kept = Vec::with_capacity(self.entries.len() + 1);
        for e in self.entries.drain(..) {
            if e.last < first || last < e.first {
                kept.push(e);
                continue;
            }
            if e.first < first {
                kept.push(Loss {
                    last: first.prev(),
                    ..e
                });
            }
            if last < e.last {
                kept.push(Loss {
                    first: last.next(),
                    ..e
                });
            }
        }
        kept.truncate(MAX_RANGES);
        self.entries = kept;
    }

    /// Forgets every loss.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The losses to ask for again at `now`, marked as asked; at most
    /// `limit` ranges, earliest first.
    pub fn due(&mut self, now: i64, t: Timing, limit: usize) -> Vec<(Seq, Seq)> {
        let rtt = t.rtt.max(1);
        let mut out = Vec::new();
        for e in &mut self.entries {
            if out.len() >= limit {
                break;
            }
            if !matches!(e.next_request(now, t), Some(at) if at <= now) {
                continue;
            }
            if e.near(now, rtt) && e.fast_requests < MAX_FAST_REQUESTS {
                e.fast_requests += 1;
            }
            e.last_request = now;
            e.requests = e.requests.saturating_add(1);
            out.push((e.first, e.last));
        }
        out
    }

    /// When [`due`](Self::due) next has something to send, given that it
    /// has just run at `now`: never earlier than `now`, and nothing for a
    /// loss a retransmission could no longer fill in time.
    pub fn next_due(&self, now: i64, t: Timing) -> Option<i64> {
        self.entries
            .iter()
            .filter_map(|e| e.next_request(now, t))
            .map(|at| at.max(now))
            .min()
    }
}

impl Loss {
    /// Within three round trips of the deadline at `t`.
    fn near(&self, t: i64, rtt: i64) -> bool {
        self.deadline.saturating_sub(t) < NEAR_DEADLINE_RTTS * rtt
    }

    /// When to ask for this loss next, seen from `now`: a round trip after
    /// the last request, or a quarter round trip once near the deadline;
    /// `None` once a retransmission asked for then could not arrive in time
    /// (it needs a round trip: the NAK there, the packet back).
    fn next_request(&self, now: i64, t: Timing) -> Option<i64> {
        let rtt = t.rtt.max(1);
        let ordinary = self
            .last_request
            .saturating_add(t.retry.max(rtt).max(MIN_INTERVAL));
        let at = if self.fast_requests < MAX_FAST_REQUESTS {
            let near_from = self.deadline.saturating_sub(NEAR_DEADLINE_RTTS * rtt) + 1;
            let fast = self
                .last_request
                .saturating_add((rtt / 4).max(MIN_FAST_INTERVAL))
                .max(near_from);
            ordinary.min(fast)
        } else {
            ordinary
        };
        let useful = |t: i64| t.saturating_add(rtt) <= self.deadline;
        (useful(now) && useful(at)).then_some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTT: i64 = 60_000;

    fn s(v: u32) -> Seq {
        Seq::new(v)
    }

    #[test]
    fn add_remove_and_split() {
        let mut l = LossList::new();
        l.add(s(10), s(14), 0, 1_000_000);
        l.add(s(20), s(20), 0, 1_000_000);
        assert_eq!(l.count(), 6);
        assert!(l.remove(s(12)));
        assert!(!l.remove(s(12)));
        assert!(l.remove(s(10)));
        assert!(l.remove(s(14)));
        assert_eq!(l.count(), 3);
        assert!(l.contains(s(11)) && l.contains(s(13)) && l.contains(s(20)));
        assert!(!l.contains(s(12)));
        l.remove_before(s(13));
        assert_eq!(l.first(), Some(s(13)));
        l.remove_before(s(21));
        assert!(l.is_empty());
    }

    #[test]
    fn ranges_across_the_wrap() {
        let mut l = LossList::new();
        l.add(s(Seq::MAX - 1), s(1), 0, 1_000_000);
        assert_eq!(l.count(), 4);
        assert!(l.remove(s(0)));
        assert_eq!(l.ranges(), 2);
        l.remove_before(s(1));
        assert_eq!(l.count(), 1);
        assert_eq!(l.first(), Some(s(1)));
    }

    #[test]
    fn the_list_is_bounded() {
        let mut l = LossList::new();
        for i in 0..(MAX_RANGES as u32 + 10) {
            l.add(s(i * 2), s(i * 2), 0, 1_000_000);
        }
        assert_eq!(l.ranges(), MAX_RANGES);
        // Splitting does not grow it past the limit either.
        let mut l = LossList::new();
        l.add(s(0), s(10_000), 0, 1_000_000);
        for i in 0..5000 {
            l.remove(s(i * 2 + 1));
        }
        assert!(l.ranges() <= MAX_RANGES);
    }

    #[test]
    fn remove_a_range() {
        let mut l = LossList::new();
        l.add(s(10), s(20), 0, 1_000_000);
        l.add(s(30), s(30), 0, 1_000_000);
        l.add(s(40), s(45), 0, 1_000_000);
        l.remove_range(s(15), s(42));
        assert_eq!(l.count(), 5 + 3);
        assert!(l.contains(s(14)) && !l.contains(s(15)));
        assert!(!l.contains(s(30)) && !l.contains(s(42)) && l.contains(s(43)));
        l.remove_range(s(12), s(11));
        assert_eq!(l.count(), 8, "an empty range removes nothing");
        l.remove_range(s(11), s(11));
        assert_eq!(l.ranges(), 3, "a hole splits a range");
    }

    #[test]
    fn overlapping_adds_are_ignored() {
        let mut l = LossList::new();
        l.add(s(5), s(9), 0, 10);
        l.add(s(7), s(12), 0, 10);
        assert_eq!(l.count(), 5);
        l.add(s(3), s(1), 0, 10);
        assert_eq!(l.count(), 5, "an empty range is nothing");
        l.add(s(1), s(2), 0, 10);
        assert_eq!(l.first(), Some(s(1)), "sorted");
    }

    #[test]
    fn asks_again_every_round_trip() {
        let mut l = LossList::new();
        let deadline = 2_000_000;
        l.add(s(1), s(1), 0, deadline);
        assert!(l.due(RTT - 1, Timing::rtt(RTT), 10).is_empty());
        assert_eq!(l.next_due(RTT - 1, Timing::rtt(RTT)), Some(RTT));
        assert_eq!(l.due(RTT, Timing::rtt(RTT), 10), vec![(s(1), s(1))]);
        assert!(l.due(RTT + 1, Timing::rtt(RTT), 10).is_empty());
        assert_eq!(l.due(2 * RTT, Timing::rtt(RTT), 10).len(), 1);
    }

    #[test]
    fn asks_every_quarter_round_trip_near_the_deadline_up_to_a_cap() {
        let mut l = LossList::new();
        let deadline = 1_000_000;
        l.add(s(1), s(1), 0, deadline);
        let near = deadline - 3 * RTT + 1;
        assert_eq!(l.due(near, Timing::rtt(RTT), 10).len(), 1);
        let mut t = near;
        let mut asked = 1;
        loop {
            t += RTT / 4;
            if t + RTT > deadline {
                break;
            }
            asked += l.due(t, Timing::rtt(RTT), 10).len();
        }
        // Eight quick ones at most, and none once it cannot arrive in time.
        assert!(asked <= 1 + MAX_FAST_REQUESTS as usize, "{asked}");
        assert!(asked >= 7, "{asked}");
        assert!(l.due(deadline - RTT + 1, Timing::rtt(RTT), 10).is_empty());
        assert_eq!(l.next_due(deadline - RTT + 1, Timing::rtt(RTT)), None);
    }

    #[test]
    fn never_asks_when_a_retransmission_would_be_too_late() {
        let mut l = LossList::new();
        l.add(s(1), s(3), 0, RTT / 2);
        assert!(l.due(RTT, Timing::rtt(RTT), 10).is_empty());
        assert_eq!(l.next_due(RTT, Timing::rtt(RTT)), None);
    }

    #[test]
    fn the_next_request_is_never_in_the_past_nor_useless() {
        let mut l = LossList::new();
        let deadline = 1_000_000;
        l.add(s(1), s(1), 0, deadline);
        // A timer that fires late: the next request is now, not before.
        assert_eq!(l.next_due(500_000, Timing::rtt(RTT)), Some(500_000));
        // Late enough that a retransmission cannot make it: no timer at all.
        assert_eq!(l.next_due(deadline - RTT + 1, Timing::rtt(RTT)), None);
        // A request whose turn would come too late is not scheduled.
        let mut l = LossList::new();
        l.add(s(1), s(1), 0, RTT + RTT / 8);
        assert_eq!(l.next_due(0, Timing::rtt(RTT)), None);
    }

    #[test]
    fn tiny_round_trips_do_not_flood() {
        let mut l = LossList::new();
        l.add(s(1), s(1), 0, 10_000_000);
        assert_eq!(l.next_due(0, Timing::rtt(3)), Some(MIN_INTERVAL));
        let mut asked = 0;
        for t in 0..=100_000 {
            asked += l.due(t, Timing::rtt(3), 10).len();
        }
        assert_eq!(asked, 10, "every 10 ms over 100 ms");
    }

    #[test]
    fn limit_keeps_the_earliest() {
        let mut l = LossList::new();
        for i in 0..10 {
            l.add(s(i * 2), s(i * 2), 0, 10_000_000);
        }
        let d = l.due(RTT, Timing::rtt(RTT), 3);
        assert_eq!(d, vec![(s(0), s(0)), (s(2), s(2)), (s(4), s(4))]);
        // The rest are still due.
        assert_eq!(l.due(RTT, Timing::rtt(RTT), 100).len(), 7);
    }
}
