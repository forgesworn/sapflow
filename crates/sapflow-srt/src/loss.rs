//! The receiver's loss list: sequence numbers not yet received, as ranges,
//! each with when it was last asked for.
//!
//! When to ask again follows what `docs/interop.md` measured against
//! libsrt's own receiver: every round trip, and every quarter round trip
//! (at most eight times) once the packet's delivery deadline is under three
//! round trips away, but never once a retransmission could no longer arrive
//! before the deadline.

use crate::seq::Seq;

/// Re-requests closer together than one round trip, per loss.
pub const MAX_FAST_REQUESTS: u32 = 8;
/// Within this many round trips of the deadline, ask every quarter round trip.
const NEAR_DEADLINE_RTTS: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loss {
    pub first: Seq,
    pub last: Seq,
    /// When the packet that revealed the loss is to be delivered: the lost
    /// ones are due no later (microseconds, the listener's clock).
    pub deadline: i64,
    pub last_request: i64,
    pub requests: u32,
    pub fast_requests: u32,
}

#[derive(Debug, Default, Clone)]
pub struct LossList {
    /// Sorted, not overlapping.
    entries: Vec<Loss>,
}

impl LossList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

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

    pub fn contains(&self, s: Seq) -> bool {
        self.entries.iter().any(|e| e.first <= s && s <= e.last)
    }

    pub fn first(&self) -> Option<Seq> {
        self.entries.first().map(|e| e.first)
    }

    /// Records `first..=last` as lost, asked for once at `now`.
    pub fn add(&mut self, first: Seq, last: Seq, now: i64, deadline: i64) {
        if last < first {
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

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The losses to ask for again at `now`, marked as asked; at most
    /// `limit` ranges, earliest first.
    pub fn due(&mut self, now: i64, rtt: i64, limit: usize) -> Vec<(Seq, Seq)> {
        let rtt = rtt.max(1);
        let mut out = Vec::new();
        for e in &mut self.entries {
            if out.len() >= limit {
                break;
            }
            // A retransmission needs a round trip: the NAK there, the
            // packet back.
            if now.saturating_add(rtt) > e.deadline {
                continue;
            }
            let near = e.deadline - now < NEAR_DEADLINE_RTTS * rtt;
            let fast = near && e.fast_requests < MAX_FAST_REQUESTS;
            let interval = if fast { rtt / 4 } else { rtt };
            if now - e.last_request < interval {
                continue;
            }
            e.last_request = now;
            e.requests = e.requests.saturating_add(1);
            if fast {
                e.fast_requests += 1;
            }
            out.push((e.first, e.last));
        }
        out
    }

    /// When [`due`](Self::due) would next have something to send.
    pub fn next_due(&self, rtt: i64) -> Option<i64> {
        let rtt = rtt.max(1);
        self.entries
            .iter()
            .filter_map(|e| {
                let last_useful = e.deadline - rtt;
                // Due at the end of the ordinary interval, or sooner once near.
                let ordinary = e.last_request + rtt;
                let near_from = e.deadline - NEAR_DEADLINE_RTTS * rtt + 1;
                let fast = if e.fast_requests < MAX_FAST_REQUESTS {
                    (e.last_request + rtt / 4).max(near_from)
                } else {
                    ordinary
                };
                let at = ordinary.min(fast);
                (at <= last_useful).then_some(at)
            })
            .min()
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
        assert!(l.due(RTT - 1, RTT, 10).is_empty());
        assert_eq!(l.next_due(RTT), Some(RTT));
        assert_eq!(l.due(RTT, RTT, 10), vec![(s(1), s(1))]);
        assert!(l.due(RTT + 1, RTT, 10).is_empty());
        assert_eq!(l.due(2 * RTT, RTT, 10).len(), 1);
    }

    #[test]
    fn asks_every_quarter_round_trip_near_the_deadline_up_to_a_cap() {
        let mut l = LossList::new();
        let deadline = 1_000_000;
        l.add(s(1), s(1), 0, deadline);
        let near = deadline - 3 * RTT + 1;
        assert_eq!(l.due(near, RTT, 10).len(), 1);
        let mut t = near;
        let mut asked = 1;
        loop {
            t += RTT / 4;
            if t + RTT > deadline {
                break;
            }
            asked += l.due(t, RTT, 10).len();
        }
        // Eight quick ones at most, and none once it cannot arrive in time.
        assert!(asked <= 1 + MAX_FAST_REQUESTS as usize, "{asked}");
        assert!(asked >= 7, "{asked}");
        assert!(l.due(deadline - RTT + 1, RTT, 10).is_empty());
        assert_eq!(l.next_due(RTT), None);
    }

    #[test]
    fn never_asks_when_a_retransmission_would_be_too_late() {
        let mut l = LossList::new();
        l.add(s(1), s(3), 0, RTT / 2);
        assert!(l.due(RTT, RTT, 10).is_empty());
        assert_eq!(l.next_due(RTT), None);
    }

    #[test]
    fn limit_keeps_the_earliest() {
        let mut l = LossList::new();
        for i in 0..10 {
            l.add(s(i * 2), s(i * 2), 0, 10_000_000);
        }
        let d = l.due(RTT, RTT, 3);
        assert_eq!(d, vec![(s(0), s(0)), (s(2), s(2)), (s(4), s(4))]);
        // The rest are still due.
        assert_eq!(l.due(RTT, RTT, 100).len(), 7);
    }
}
