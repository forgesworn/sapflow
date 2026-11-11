//! A bounded per-stream reorder buffer (RFC 3550 section 5.1 sequence
//! numbers, with the extension of Appendix A.1), and the state of each
//! missing packet that NACK scheduling (RFC 4585 section 6.2.1) reads.
//!
//! Times are microseconds on the receiver's own clock. Sequence numbers
//! are extended to 64 bits against the highest one seen, so the 16-bit
//! wrap is invisible here.
//!
//! Packets come out in sequence order. A gap is waited for until the
//! jitter deadline, counted from when the gap was first seen; then it is
//! declared lost and the buffer moves past it. When the buffer is full in
//! packets or bytes, it releases from the front (declaring what is
//! missing there lost) rather than refuse new packets.

use std::collections::VecDeque;

/// Where extended sequence numbers start, so that reordering at the start
/// of a stream cannot take them below zero.
const BASE: u64 = 1 << 32;

/// Limits and timings for one stream's buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    pub max_packets: usize,
    pub max_bytes: usize,
    pub jitter: u64,
    pub reorder_delay: u64,
    pub max_dropout: u64,
    pub max_nacks: u8,
}

/// A packet held for release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stored {
    pub seq: u64,
    pub timestamp: u32,
    pub marker: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
enum Slot {
    Held(Stored),
    Missing {
        /// When the gap was first seen.
        since: u64,
        nacks: u8,
        last_nack: u64,
    },
}

/// What leaves the buffer, in sequence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Released {
    Packet(Stored),
    /// `count` packets from `first` were never received in time.
    Lost {
        first: u64,
        count: u64,
    },
    /// The sequence numbers restarted; what follows is a new run.
    Discontinuity,
}

/// What happened to an inserted packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inserted {
    /// A packet not seen before.
    New,
    /// It filled a gap; `nacked` if a NACK had been sent for it, and an
    /// RTT sample if exactly one had.
    Recovered { nacked: bool, rtt: Option<u64> },
    /// Already held.
    Duplicate,
    /// Older than what has been released or declared lost.
    Late,
    /// So far from the stream that it may be a restart; held aside until
    /// its successor confirms it.
    Probe,
    /// It confirmed a restart: the buffer was flushed and starts again.
    Restart,
    /// Its payload alone is larger than the byte bound; refused.
    TooLarge,
}

#[derive(Debug)]
pub(crate) struct Reorder {
    s: Settings,
    /// Extended sequence number of `slots[0]`, or of the next packet to
    /// release when `slots` is empty.
    head: u64,
    /// The highest extended sequence number seen.
    highest: u64,
    started: bool,
    released_any: bool,
    first_arrival: u64,
    slots: VecDeque<Slot>,
    bytes: usize,
    missing: usize,
    ready: VecDeque<Released>,
    probe: Option<(u16, u32, bool, Vec<u8>)>,
}

impl Reorder {
    pub fn new(s: Settings) -> Self {
        Self {
            s,
            head: BASE,
            highest: BASE,
            started: false,
            released_any: false,
            first_arrival: 0,
            slots: VecDeque::new(),
            bytes: 0,
            missing: 0,
            ready: VecDeque::new(),
            probe: None,
        }
    }

    /// Packets and bytes held.
    pub fn held(&self) -> (usize, usize) {
        (self.slots.len() - self.missing, self.bytes)
    }

    /// The 16-bit sequence number, extended against the highest seen.
    fn extend(&self, seq: u16) -> u64 {
        let step = i64::from(seq.wrapping_sub(self.highest as u16) as i16);
        self.highest.saturating_add_signed(step)
    }

    fn start(&mut self, now: u64, seq: u16, timestamp: u32, marker: bool, payload: Vec<u8>) {
        let ext = BASE + u64::from(seq);
        self.started = true;
        self.released_any = false;
        self.first_arrival = now;
        self.head = ext;
        self.highest = ext;
        self.bytes = payload.len();
        self.slots.push_back(Slot::Held(Stored {
            seq: ext,
            timestamp,
            marker,
            payload,
        }));
    }

    /// Whether `seq` (16-bit) is a gap being waited for.
    pub fn is_missing(&self, seq: u16) -> bool {
        if !self.started {
            return false;
        }
        let ext = self.extend(seq);
        ext.checked_sub(self.head)
            .and_then(|d| usize::try_from(d).ok())
            .and_then(|d| self.slots.get(d))
            .is_some_and(|s| matches!(s, Slot::Missing { .. }))
    }

    pub fn insert(
        &mut self,
        now: u64,
        seq: u16,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Inserted {
        if payload.len() > self.s.max_bytes {
            return Inserted::TooLarge;
        }
        if !self.started {
            self.start(now, seq, timestamp, marker, payload.to_vec());
            return Inserted::New;
        }
        let ext = self.extend(seq);
        // A packet far from the stream, either way, may be a restart.
        let far = if ext < self.head {
            self.head - ext > self.s.max_dropout
        } else {
            ext > self.highest && ext - self.highest > self.s.max_dropout
        };
        if far {
            return self.probe(now, seq, timestamp, marker, payload);
        }
        self.probe = None;
        if let Some(Slot::Held(_)) = self.slot(ext) {
            return Inserted::Duplicate;
        }
        // Room in bytes first; this may release the front.
        while self.bytes + payload.len() > self.s.max_bytes && !self.slots.is_empty() {
            self.pop_front();
        }
        if ext < self.head {
            let back = self.head - ext;
            let room = self.s.max_packets.saturating_sub(self.slots.len()) as u64;
            if self.released_any || back > room {
                return Inserted::Late;
            }
            // Reordering at the very start: grow the front backwards.
            for _ in 1..back {
                self.slots.push_front(Slot::Missing {
                    since: now,
                    nacks: 0,
                    last_nack: 0,
                });
                self.missing += 1;
            }
            self.slots.push_front(Slot::Held(Stored {
                seq: ext,
                timestamp,
                marker,
                payload: payload.to_vec(),
            }));
            self.bytes += payload.len();
            self.head = ext;
            return Inserted::New;
        }
        if let Some(Slot::Missing {
            nacks, last_nack, ..
        }) = self.slot(ext)
        {
            let (nacks, last_nack) = (*nacks, *last_nack);
            let rtt = (nacks == 1).then(|| now.saturating_sub(last_nack));
            if let Some(slot) = self.slot_mut(ext) {
                *slot = Slot::Held(Stored {
                    seq: ext,
                    timestamp,
                    marker,
                    payload: payload.to_vec(),
                });
            }
            self.missing -= 1;
            self.bytes += payload.len();
            return Inserted::Recovered {
                nacked: nacks > 0,
                rtt,
            };
        }
        // Beyond the highest: the packets in between are missing. Release
        // from the front until the whole span fits.
        let next = self.head + self.slots.len() as u64;
        let mut gap = ext - next;
        let max = self.s.max_packets.max(1) as u64;
        while self.slots.len() as u64 + gap + 1 > max && !self.slots.is_empty() {
            self.pop_front();
        }
        if gap + 1 > max {
            // Even empty, the span does not fit: the oldest part of the
            // gap is lost at once.
            let skip = gap + 1 - max;
            self.push_lost(self.head, skip);
            self.head += skip;
            self.released_any = true;
            gap -= skip;
        }
        for _ in 0..gap {
            self.slots.push_back(Slot::Missing {
                since: now,
                nacks: 0,
                last_nack: 0,
            });
            self.missing += 1;
        }
        self.slots.push_back(Slot::Held(Stored {
            seq: ext,
            timestamp,
            marker,
            payload: payload.to_vec(),
        }));
        self.bytes += payload.len();
        self.highest = self.highest.max(ext);
        Inserted::New
    }

    fn probe(
        &mut self,
        now: u64,
        seq: u16,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Inserted {
        let confirmed = self
            .probe
            .as_ref()
            .is_some_and(|p| p.0.wrapping_add(1) == seq);
        if !confirmed {
            self.probe = Some((seq, timestamp, marker, payload.to_vec()));
            return Inserted::Probe;
        }
        // Flush everything in order, then start again from the probe.
        while !self.slots.is_empty() {
            self.pop_front();
        }
        self.ready.push_back(Released::Discontinuity);
        if let Some((pseq, pts, pmarker, ppayload)) = self.probe.take() {
            self.start(now, pseq, pts, pmarker, ppayload);
            self.released_any = true;
            let _ = self.insert(now, seq, timestamp, marker, payload);
        }
        Inserted::Restart
    }

    fn slot(&self, ext: u64) -> Option<&Slot> {
        let d = usize::try_from(ext.checked_sub(self.head)?).ok()?;
        self.slots.get(d)
    }
    fn slot_mut(&mut self, ext: u64) -> Option<&mut Slot> {
        let d = usize::try_from(ext.checked_sub(self.head)?).ok()?;
        self.slots.get_mut(d)
    }

    fn push_lost(&mut self, first: u64, count: u64) {
        if let Some(Released::Lost { first: f, count: c }) = self.ready.back_mut()
            && *f + *c == first
        {
            *c += count;
            return;
        }
        self.ready.push_back(Released::Lost { first, count });
    }

    /// Releases the front slot into `ready`.
    fn pop_front(&mut self) {
        let Some(slot) = self.slots.pop_front() else {
            return;
        };
        let seq = self.head;
        self.head += 1;
        self.released_any = true;
        match slot {
            Slot::Held(p) => {
                self.bytes -= p.payload.len();
                self.ready.push_back(Released::Packet(p));
            }
            Slot::Missing { .. } => {
                self.missing -= 1;
                self.push_lost(seq, 1);
            }
        }
    }

    /// The next release due at `now`.
    pub fn poll(&mut self, now: u64) -> Option<Released> {
        if let Some(r) = self.ready.pop_front() {
            return Some(r);
        }
        if !self.released_any && now < self.first_arrival.saturating_add(self.s.reorder_delay) {
            // Hold the first packets briefly, in case earlier ones are
            // only reordered.
            return None;
        }
        match self.slots.front()? {
            Slot::Held(_) => {
                self.pop_front();
            }
            Slot::Missing { .. } => {
                while let Some(Slot::Missing { since, .. }) = self.slots.front()
                    && now >= since.saturating_add(self.s.jitter)
                {
                    self.pop_front();
                }
            }
        }
        self.ready.pop_front()
    }

    /// When [`poll`](Self::poll) next has something, if it is waiting.
    pub fn next_release(&self) -> Option<u64> {
        if !self.ready.is_empty() {
            return Some(0);
        }
        if !self.released_any && !self.slots.is_empty() {
            return Some(self.first_arrival.saturating_add(self.s.reorder_delay));
        }
        match self.slots.front()? {
            Slot::Held(_) => Some(0),
            Slot::Missing { since, .. } => Some(since.saturating_add(self.s.jitter)),
        }
    }

    /// When a missing packet is next due a NACK, by the rules of
    /// [`due_nacks`](Self::due_nacks).
    fn nack_time(&self, since: u64, nacks: u8, last_nack: u64, rtt: u64) -> Option<u64> {
        let deadline = since.saturating_add(self.s.jitter);
        let at = if nacks == 0 {
            since.saturating_add(self.s.reorder_delay)
        } else if nacks < self.s.max_nacks {
            let at = last_nack.saturating_add(retry(rtt));
            // A repeat whose answer cannot arrive before the deadline is
            // not worth sending.
            if at.saturating_add(rtt) >= deadline {
                return None;
            }
            at
        } else {
            return None;
        };
        (at < deadline).then_some(at)
    }

    /// The missing packets due a NACK at `now`, oldest first: the first
    /// after the reorder delay, then repeats every RTT and a quarter, up
    /// to the per-packet cap and only while an answer can still arrive
    /// before the jitter deadline.
    pub fn due_nacks(&self, now: u64, rtt: u64) -> Vec<u64> {
        if self.missing == 0 {
            return Vec::new();
        }
        let mut due = Vec::new();
        for (n, slot) in self.slots.iter().enumerate() {
            if let Slot::Missing {
                since,
                nacks,
                last_nack,
            } = *slot
                && self
                    .nack_time(since, nacks, last_nack, rtt)
                    .is_some_and(|at| at <= now)
            {
                due.push(self.head + n as u64);
            }
        }
        due
    }

    /// The earliest time a missing packet is due a NACK.
    pub fn next_nack(&self, rtt: u64) -> Option<u64> {
        if self.missing == 0 {
            return None;
        }
        self.slots
            .iter()
            .filter_map(|slot| match *slot {
                Slot::Missing {
                    since,
                    nacks,
                    last_nack,
                } => self.nack_time(since, nacks, last_nack, rtt),
                Slot::Held(_) => None,
            })
            .min()
    }

    /// Records a NACK sent at `now` for these extended sequence numbers.
    pub fn mark_nacked(&mut self, seqs: &[u64], now: u64) {
        for &ext in seqs {
            if let Some(Slot::Missing {
                nacks, last_nack, ..
            }) = self.slot_mut(ext)
            {
                *nacks = nacks.saturating_add(1);
                *last_nack = now;
            }
        }
    }
}

/// Time between NACKs for the same packet: the RTT and a quarter, at
/// least 5 ms.
pub(crate) fn retry(rtt: u64) -> u64 {
    (rtt + rtt / 4).max(5_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    fn settings() -> Settings {
        Settings {
            max_packets: 64,
            max_bytes: 64 * 100,
            jitter: 200 * MS,
            reorder_delay: 10 * MS,
            max_dropout: 3000,
            max_nacks: 4,
        }
    }

    fn put(r: &mut Reorder, now: u64, seq: u16) -> Inserted {
        r.insert(now, seq, u32::from(seq) * 10, false, &[seq as u8; 10])
    }

    /// Everything released up to `now`, as 16-bit numbers or lost runs.
    fn drain(r: &mut Reorder, now: u64) -> Vec<String> {
        std::iter::from_fn(|| r.poll(now))
            .map(|x| match x {
                Released::Packet(p) => format!("{}", p.seq as u16),
                Released::Lost { first, count } => format!("lost {}+{count}", first as u16),
                Released::Discontinuity => "restart".into(),
            })
            .collect()
    }

    #[test]
    fn in_order_after_the_start_hold() {
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 100);
        put(&mut r, 1, 101);
        assert!(
            drain(&mut r, 9 * MS).is_empty(),
            "held for the reorder delay"
        );
        assert_eq!(r.next_release(), Some(10 * MS));
        assert_eq!(drain(&mut r, 10 * MS), ["100", "101"]);
        put(&mut r, 11 * MS, 102);
        assert_eq!(drain(&mut r, 11 * MS), ["102"]);
        assert_eq!(r.next_release(), None);
    }

    #[test]
    fn reordering_at_the_start_and_later() {
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 5);
        // Earlier packets inside the start hold extend the front.
        assert_eq!(put(&mut r, 1, 3), Inserted::New);
        assert_eq!(
            put(&mut r, 2, 4),
            Inserted::Recovered {
                nacked: false,
                rtt: None
            }
        );
        assert_eq!(drain(&mut r, 10 * MS), ["3", "4", "5"]);
        // After a release, older packets are late.
        assert_eq!(put(&mut r, 11 * MS, 2), Inserted::Late);
        put(&mut r, 11 * MS, 7);
        assert_eq!(drain(&mut r, 11 * MS), Vec::<String>::new());
        assert!(r.is_missing(6));
        put(&mut r, 12 * MS, 6);
        assert_eq!(drain(&mut r, 12 * MS), ["6", "7"]);
        assert_eq!(put(&mut r, 13 * MS, 7), Inserted::Late);
        put(&mut r, 13 * MS, 9);
        assert_eq!(put(&mut r, 13 * MS, 9), Inserted::Duplicate);
    }

    #[test]
    fn gaps_wait_for_the_jitter_deadline() {
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 65_534);
        assert_eq!(drain(&mut r, 10 * MS), ["65534"]);
        // 65535 and 0 missing; 1 arrives across the wrap.
        put(&mut r, 20 * MS, 1);
        put(&mut r, 30 * MS, 3);
        assert_eq!(r.next_release(), Some(220 * MS));
        assert!(drain(&mut r, 219 * MS).is_empty());
        assert_eq!(drain(&mut r, 220 * MS), ["lost 65535+2", "1"]);
        // 2 was seen missing at 30 ms.
        assert_eq!(drain(&mut r, 229 * MS), Vec::<String>::new());
        assert_eq!(drain(&mut r, 230 * MS), ["lost 2+1", "3"]);
        assert_eq!(put(&mut r, 231 * MS, 2), Inserted::Late);
    }

    #[test]
    fn nack_schedule() {
        let mut r = Reorder::new(settings());
        let rtt = 30 * MS;
        put(&mut r, 0, 10);
        drain(&mut r, 10 * MS);
        put(&mut r, 20 * MS, 13);
        // 11 and 12 missing since 20 ms; first NACK after 10 ms.
        assert!(r.due_nacks(29 * MS, rtt).is_empty());
        assert_eq!(r.next_nack(rtt), Some(30 * MS));
        let due = r.due_nacks(30 * MS, rtt);
        assert_eq!(due.len(), 2);
        r.mark_nacked(&due, 30 * MS);
        // Repeats every 37.5 ms while an answer can still arrive by 220.
        assert_eq!(r.next_nack(rtt), Some(30 * MS + 37_500));
        let mut sent = vec![30 * MS];
        let mut now = 30 * MS;
        while let Some(at) = r.next_nack(rtt) {
            now = at;
            let due = r.due_nacks(now, rtt);
            r.mark_nacked(&due, now);
            sent.push(now);
        }
        assert_eq!(sent, [30_000, 67_500, 105_000, 142_500], "capped at 4");
        // A recovered packet NACKed more than once gives no RTT sample.
        assert_eq!(
            put(&mut r, now, 11),
            Inserted::Recovered {
                nacked: true,
                rtt: None
            }
        );
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 1);
        put(&mut r, 0, 3);
        r.mark_nacked(&r.due_nacks(10 * MS, rtt), 10 * MS);
        assert_eq!(
            put(&mut r, 35 * MS, 2),
            Inserted::Recovered {
                nacked: true,
                rtt: Some(25 * MS)
            }
        );
        // A long RTT: the first NACK still goes, repeats do not.
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 1);
        put(&mut r, 0, 3);
        let slow = 150 * MS;
        assert_eq!(r.due_nacks(10 * MS, slow).len(), 1);
        r.mark_nacked(&[BASE + 2], 10 * MS);
        assert_eq!(r.next_nack(slow), None);
    }

    #[test]
    fn bounds_release_from_the_front() {
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 0);
        drain(&mut r, 10 * MS);
        // A jump of 100 with 64 slots: the oldest part of the gap is lost
        // at once.
        put(&mut r, 20 * MS, 100);
        assert_eq!(drain(&mut r, 20 * MS), ["lost 1+36"]);
        assert_eq!(r.held(), (1, 10));
        assert!(r.is_missing(37) && !r.is_missing(36));
        // Bytes: 64 slots of 10 bytes, 6400 allowed; fill with 120-byte
        // packets until the front is pushed out.
        let mut r = Reorder::new(Settings {
            max_bytes: 1000,
            ..settings()
        });
        for s in 0..20u16 {
            r.insert(0, s, 0, false, &[0; 120]);
            assert!(r.held().1 <= 1000);
        }
        assert_eq!(r.held(), (8, 960));
        assert_eq!(r.insert(0, 99, 0, false, &[0; 1001]), Inserted::TooLarge);
        // Once anything has been forced out, the start hold is over.
        assert_eq!(drain(&mut r, 0).len(), 20);
    }

    #[test]
    fn restarts_need_two_packets() {
        let mut r = Reorder::new(settings());
        put(&mut r, 0, 10);
        put(&mut r, 0, 11);
        // A lone far packet is set aside.
        assert_eq!(put(&mut r, 1, 30_000), Inserted::Probe);
        assert_eq!(put(&mut r, 1, 12), Inserted::New);
        assert_eq!(put(&mut r, 2, 40_000), Inserted::Probe);
        assert_eq!(put(&mut r, 2, 40_001), Inserted::Restart);
        assert_eq!(
            drain(&mut r, 2),
            ["10", "11", "12", "restart", "40000", "40001"]
        );
        // Backwards too.
        assert_eq!(put(&mut r, 3, 20_000), Inserted::Probe);
        assert_eq!(put(&mut r, 3, 20_001), Inserted::Restart);
        assert_eq!(drain(&mut r, 3), ["restart", "20000", "20001"]);
    }
}
