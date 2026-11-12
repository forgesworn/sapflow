//! Handshake message reassembly (RFC 6347 sections 4.2.2 and 4.2.3), with
//! hard bounds.
//!
//! - A message's declared length is checked against the cap, and an empty
//!   fragment of a message that is not empty is dropped, before anything is
//!   allocated.
//! - A candidate is one version of one message: its seq, type, length and
//!   epoch, and the bytes received so far. A fragment joins every candidate
//!   it agrees with (the same seq, type, length and epoch, and the same
//!   value for every byte both hold), and starts a new candidate if it
//!   agrees with none, so a forged fragment never overwrites genuine bytes
//!   and, by arriving first, does not hold the seq. (Where it splits a
//!   genuine message between two candidates, the retransmission completes
//!   it.)
//! - Each seq has at most [`MAX_VARIANTS`] candidates. Every fragment that
//!   adds coverage counts towards its candidate's fragment cap, while
//!   duplicates are dropped without counting; a candidate over the cap is
//!   removed and its bytes freed.
//! - All candidates together hold at most `max_buffered` bytes. When a new
//!   one does not fit, room is reclaimed from unauthenticated (epoch 0)
//!   candidates that are still incomplete, and only then: first those
//!   started at least `expire_after` ago; then, at most once per
//!   `reclaim_every`, those for later seqs when the newcomer is for the next
//!   message, and the oldest for the newcomer's own seq. An authenticated
//!   newcomer may take any unauthenticated candidate's room, at any time.
//!   So a finite burst of forgeries holds the buffer or the slots for no
//!   longer than `expire_after`, and then the genuine message's
//!   retransmission gets in.
//! - Messages are released strictly in `message_seq` order. Of several
//!   complete candidates the one started first is taken first; the seq
//!   moves on only when the caller accepts it ([`Reassembler::take`],
//!   [`Reassembler::advance`]), so a refused candidate does not take the
//!   others with it.
//!
//! A fragment that would break a bound is [`Push::Over`], with nothing kept
//! for it, and the caller decides what that means: the server drops an
//! unauthenticated one and fails the handshake only for one under the read
//! keys.

/// Candidates kept for one `message_seq`.
pub(crate) const MAX_VARIANTS: usize = 4;

use crate::message::Fragment;
use crate::{Error, Limit};
use std::time::{Duration, Instant};

/// Reassembly bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) max_message: usize,
    pub(crate) max_fragments: usize,
    pub(crate) max_buffered: usize,
    /// How many messages past the next expected one may be buffered.
    pub(crate) ahead: u16,
    /// An unauthenticated candidate still incomplete this long after it was
    /// started gives up its room to a newcomer that needs it. `None`: never.
    pub(crate) expire_after: Option<Duration>,
    /// Room is taken from younger unauthenticated candidates for an
    /// unauthenticated newcomer at most once per this interval. `None`:
    /// never.
    pub(crate) reclaim_every: Option<Duration>,
}

/// A complete handshake message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) msg_type: u8,
    pub(crate) seq: u16,
    pub(crate) epoch: u16,
    pub(crate) body: Vec<u8>,
}

/// What became of a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Push {
    /// A fragment of a message already released: a retransmission.
    Old { seq: u16, offset: u32 },
    /// Stored (it may or may not have completed a candidate).
    Stored,
    /// Dropped: outside the window, empty, a duplicate, or a new candidate
    /// for a seq that already has one, with no room for it.
    Dropped,
    /// Refused because it would break this bound: a declared length over
    /// `max_message`, a first candidate for its seq with no room in
    /// `max_buffered`, or the only candidate for its seq going over
    /// `max_fragments`. Nothing is kept for it; a candidate over the
    /// fragment cap is removed and its bytes freed.
    Over(Limit),
}

#[derive(Debug)]
struct Partial {
    msg_type: u8,
    seq: u16,
    epoch: u16,
    body: Vec<u8>,
    /// Received byte ranges, sorted, merged and non-overlapping.
    covered: Vec<(usize, usize)>,
    fragments: usize,
    /// When its first fragment arrived.
    started: Instant,
}

impl Partial {
    fn complete(&self) -> bool {
        self.body.is_empty() || self.covered.as_slice() == [(0, self.body.len())]
    }

    fn unauthenticated(&self) -> bool {
        self.epoch == 0
    }

    /// Whether `data` at `start` has the value this candidate already holds
    /// for every byte it has received in that range.
    fn agrees(&self, start: usize, data: &[u8]) -> bool {
        let end = start + data.len();
        self.covered.iter().all(|&(s, e)| {
            let (from, to) = (s.max(start), e.min(end));
            from >= to || self.body.get(from..to) == data.get(from - start..to - start)
        })
    }

    /// Whether every byte from `start` to `end` has been received.
    fn covers(&self, start: usize, end: usize) -> bool {
        start == end || self.covered.iter().any(|&(s, e)| s <= start && end <= e)
    }

    fn cover(&mut self, start: usize, end: usize) {
        self.covered.push((start, end));
        self.covered.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(self.covered.len());
        for &(s, e) in &self.covered {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.covered = merged;
    }
}

#[derive(Debug)]
pub(crate) struct Reassembler {
    limits: Limits,
    next_seq: u16,
    partials: Vec<Partial>,
    buffered: usize,
    /// When room was last taken from younger candidates for an
    /// unauthenticated newcomer.
    last_reclaim: Option<Instant>,
}

impl Reassembler {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            next_seq: 0,
            partials: Vec::new(),
            buffered: 0,
            last_reclaim: None,
        }
    }

    /// The next `message_seq` expected.
    pub(crate) fn next_seq(&self) -> u16 {
        self.next_seq
    }

    /// Bytes held by messages in progress.
    pub(crate) fn buffered(&self) -> usize {
        self.buffered
    }

    /// Restarts at `seq`, dropping anything in progress (used after a
    /// HelloVerifyRequest, when the next ClientHello has a higher seq).
    pub(crate) fn restart_at(&mut self, seq: u16) {
        self.next_seq = seq;
        self.partials.clear();
        self.buffered = 0;
    }

    fn variants(&self, seq: u16) -> usize {
        self.partials.iter().filter(|p| p.seq == seq).count()
    }

    fn fits(&self, seq: u16, length: usize) -> bool {
        self.variants(seq) < MAX_VARIANTS && self.buffered + length <= self.limits.max_buffered
    }

    /// Removes every candidate `which` picks, freeing its bytes.
    fn remove_where(&mut self, which: impl Fn(&Partial) -> bool) {
        let mut freed = 0;
        self.partials.retain(|p| {
            let go = which(p);
            if go {
                freed += p.body.len();
            }
            !go
        });
        self.buffered -= freed;
    }

    pub(crate) fn push(
        &mut self,
        epoch: u16,
        f: &Fragment<'_>,
        now: Instant,
    ) -> Result<Push, Error> {
        if f.seq < self.next_seq {
            return Ok(Push::Old {
                seq: f.seq,
                offset: f.offset,
            });
        }
        if u32::from(f.seq) >= u32::from(self.next_seq) + u32::from(self.limits.ahead) {
            return Ok(Push::Dropped);
        }
        let Ok(length) = usize::try_from(f.length) else {
            return Ok(Push::Over(Limit::MessageSize));
        };
        if length > self.limits.max_message {
            return Ok(Push::Over(Limit::MessageSize));
        }
        if f.data.is_empty() && length != 0 {
            // No bytes, so no progress: dropped before a candidate is found
            // or made for it, so that it can hold no budget.
            return Ok(Push::Dropped);
        }
        let start = usize::try_from(f.offset).map_err(|_| Error::Internal)?;
        let end = start + f.data.len();
        if end > length {
            // `message::fragments` never yields one; dropped all the same.
            return Ok(Push::Dropped);
        }
        let max_fragments = self.limits.max_fragments;
        // Every candidate for this message that agrees with the fragment
        // takes what it adds.
        let (mut agreed, mut stored, mut over) = (false, false, false);
        let mut i = 0;
        while let Some(p) = self.partials.get_mut(i) {
            let same = p.seq == f.seq
                && p.msg_type == f.msg_type
                && p.body.len() == length
                && p.epoch == epoch;
            if !same || !p.agrees(start, f.data) {
                i += 1;
                continue;
            }
            agreed = true;
            if p.covers(start, end) {
                // A duplicate for this one: no new bytes, so it neither
                // counts nor stores.
                i += 1;
                continue;
            }
            // Only fragments that add coverage count, so retransmissions
            // and forged duplicates cannot use up the cap.
            p.fragments += 1;
            if p.fragments > max_fragments {
                // It can no longer complete, so it goes and frees its bytes.
                let len = p.body.len();
                self.partials.remove(i);
                self.buffered -= len;
                over = true;
                continue;
            }
            // The bytes it already holds in this range are equal to these,
            // so only new values are written.
            p.body
                .get_mut(start..end)
                .ok_or(Error::Internal)?
                .copy_from_slice(f.data);
            p.cover(start, end);
            stored = true;
            i += 1;
        }
        if stored {
            return Ok(Push::Stored);
        }
        if over {
            // One of several candidates for this seq is simply dropped, so a
            // forged candidate cannot end the handshake; the only one is
            // over the bound, for the caller to judge.
            return Ok(if self.variants(f.seq) > 0 {
                Push::Dropped
            } else {
                Push::Over(Limit::Fragments)
            });
        }
        if agreed {
            return Ok(Push::Dropped);
        }
        self.start(epoch, f, length, now)
    }

    /// A new candidate for `f`, which agrees with none of its seq's.
    fn start(
        &mut self,
        epoch: u16,
        f: &Fragment<'_>,
        length: usize,
        now: Instant,
    ) -> Result<Push, Error> {
        if !self.fits(f.seq, length) {
            self.reclaim(f.seq, epoch, length, now);
        }
        if !self.fits(f.seq, length) {
            // Over the bound only for a message seen for the first time; a
            // further candidate with no room is dropped.
            return Ok(if self.variants(f.seq) > 0 {
                Push::Dropped
            } else {
                Push::Over(Limit::Buffered)
            });
        }
        let start = usize::try_from(f.offset).map_err(|_| Error::Internal)?;
        let end = start + f.data.len();
        let mut body = vec![0; length];
        body.get_mut(start..end)
            .ok_or(Error::Internal)?
            .copy_from_slice(f.data);
        let mut p = Partial {
            msg_type: f.msg_type,
            seq: f.seq,
            epoch,
            body,
            covered: Vec::new(),
            fragments: 1,
            started: now,
        };
        if start < end {
            p.cover(start, end);
        }
        self.buffered += length;
        self.partials.push(p);
        Ok(Push::Stored)
    }

    /// Makes room for a newcomer for `seq` that does not fit, from
    /// unauthenticated candidates that are still incomplete (or any
    /// unauthenticated candidate, for an authenticated newcomer). It never
    /// touches what the caller has accepted, and nothing here sends or
    /// changes a timer.
    fn reclaim(&mut self, seq: u16, epoch: u16, length: usize, now: Instant) {
        // Stalled: started a retransmission interval ago and still
        // incomplete. A genuine message's retransmission brings all of its
        // fragments again, so it loses nothing by this.
        if let Some(after) = self.limits.expire_after {
            self.remove_where(|p| {
                p.unauthenticated()
                    && !p.complete()
                    && now.saturating_duration_since(p.started) >= after
            });
        }
        let authenticated = epoch != 0;
        if !authenticated {
            let Some(every) = self.limits.reclaim_every else {
                return;
            };
            let due = self
                .last_reclaim
                .and_then(|t| t.checked_add(every))
                .is_none_or(|due| now >= due);
            if !due {
                return;
            }
        }
        let mut reclaimed = false;
        while !self.fits(seq, length) {
            let Some(i) = self.victim(seq, authenticated) else {
                break;
            };
            let p = self.partials.remove(i);
            self.buffered -= p.body.len();
            reclaimed = true;
        }
        if reclaimed && !authenticated {
            self.last_reclaim = Some(now);
        }
    }

    /// Which candidate gives up its room to a newcomer for `seq`: a
    /// speculative one for a later seq first, latest first, if the newcomer
    /// is the next message (or authenticated) and the buffer is what is
    /// short; then the oldest for the newcomer's own seq.
    fn victim(&self, seq: u16, authenticated: bool) -> Option<usize> {
        let eligible = |p: &Partial| p.unauthenticated() && (authenticated || !p.complete());
        let slots_full = self.variants(seq) >= MAX_VARIANTS;
        if !slots_full && (seq == self.next_seq || authenticated) {
            let later = self
                .partials
                .iter()
                .enumerate()
                .filter(|(_, p)| p.seq > seq && eligible(p))
                .max_by_key(|&(i, p)| (p.seq, i))
                .map(|(i, _)| i);
            if later.is_some() {
                return later;
            }
        }
        self.partials
            .iter()
            .enumerate()
            .filter(|(_, p)| p.seq == seq && eligible(p))
            .min_by_key(|&(i, p)| (p.started, i))
            .map(|(i, _)| i)
    }

    /// The next message, if a candidate for it is complete: of several, the
    /// one started first. It is removed, but the seq stays, with its other
    /// candidates, until [`Reassembler::advance`]; a caller that refuses it
    /// takes again.
    pub(crate) fn take(&mut self) -> Option<Message> {
        let i = self
            .partials
            .iter()
            .position(|p| p.seq == self.next_seq && p.complete())?;
        // `remove`, not `swap_remove`: candidates stay in the order they
        // were started, so a later forgery cannot overtake an earlier one.
        let p = self.partials.remove(i);
        self.buffered -= p.body.len();
        Some(Message {
            msg_type: p.msg_type,
            seq: p.seq,
            epoch: p.epoch,
            body: p.body,
        })
    }

    /// Moves on to the next `message_seq`, discarding what is left for this
    /// one.
    pub(crate) fn advance(&mut self) {
        let seq = self.next_seq;
        self.remove_where(|p| p.seq == seq);
        self.next_seq = seq.saturating_add(1);
    }

    /// [`Reassembler::take`] and then [`Reassembler::advance`], for a caller
    /// that accepts every message (the test client).
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn pop(&mut self) -> Option<Message> {
        let m = self.take()?;
        self.advance();
        Some(m)
    }

    /// Drops every unauthenticated candidate.
    pub(crate) fn drop_unauthenticated(&mut self) {
        self.remove_where(Partial::unauthenticated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_message: 100,
        max_fragments: 4,
        max_buffered: 150,
        ahead: 3,
        expire_after: None,
        reclaim_every: None,
    };

    /// LIMITS, reclaiming after 1 s and at most once a second.
    const RECLAIMING: Limits = Limits {
        expire_after: Some(Duration::from_secs(1)),
        reclaim_every: Some(Duration::from_secs(1)),
        ..LIMITS
    };

    impl Reassembler {
        /// `push` at one fixed time, for the tests where time plays no part.
        fn put(&mut self, epoch: u16, f: &Fragment<'_>) -> Result<Push, Error> {
            self.push(epoch, f, base())
        }
    }

    fn base() -> Instant {
        use std::sync::OnceLock;
        static T: OnceLock<Instant> = OnceLock::new();
        *T.get_or_init(Instant::now)
    }

    fn frag(seq: u16, length: u32, offset: u32, data: &[u8]) -> Fragment<'_> {
        Fragment {
            msg_type: 1,
            length,
            seq,
            offset,
            data,
        }
    }

    #[test]
    fn out_of_order_and_overlapping_fragments_reassemble() {
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(r.put(0, &frag(0, 6, 3, b"def")), Ok(Push::Stored));
        assert_eq!(r.pop(), None);
        assert_eq!(r.put(0, &frag(0, 6, 1, b"bcd")), Ok(Push::Stored));
        assert_eq!(r.pop(), None);
        assert_eq!(r.put(0, &frag(0, 6, 0, b"a")), Ok(Push::Stored));
        let m = r.pop().expect("complete");
        assert_eq!(m.body, b"abcdef");
        assert_eq!(r.next_seq(), 1);
        assert_eq!(r.buffered(), 0);
        assert_eq!(
            r.put(0, &frag(0, 6, 0, b"a")),
            Ok(Push::Old { seq: 0, offset: 0 })
        );
        // A zero-length message completes on its one empty fragment.
        assert_eq!(r.put(0, &frag(1, 0, 0, b"")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("empty message").body, b"");
    }

    #[test]
    fn messages_release_in_order() {
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(r.put(0, &frag(2, 1, 0, b"c")), Ok(Push::Stored));
        assert_eq!(r.put(0, &frag(1, 1, 0, b"b")), Ok(Push::Stored));
        assert_eq!(
            r.put(0, &frag(3, 1, 0, b"d")),
            Ok(Push::Dropped),
            "beyond the window"
        );
        assert_eq!(r.pop(), None);
        r.put(0, &frag(0, 1, 0, b"a")).expect("push");
        let got: Vec<_> = std::iter::from_fn(|| r.pop()).map(|m| m.body).collect();
        assert_eq!(got, [b"a", b"b", b"c"]);
    }

    #[test]
    fn caps_are_real_bounds() {
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(
            r.put(0, &frag(0, 101, 0, b"x")),
            Ok(Push::Over(Limit::MessageSize))
        );
        assert_eq!(r.buffered(), 0, "nothing allocated");
        r.put(0, &frag(0, 100, 0, b"x")).expect("first");
        assert_eq!(
            r.put(0, &frag(1, 51, 0, b"x")),
            Ok(Push::Over(Limit::Buffered)),
            "100 + 51 > 150"
        );
        assert_eq!(r.buffered(), 100, "nothing allocated");
        // Fragments that add coverage count: four are allowed, and a fifth
        // is over the bound and takes the candidate with it.
        for o in 1..4 {
            assert_eq!(r.put(0, &frag(0, 100, o, b"x")), Ok(Push::Stored));
        }
        assert_eq!(
            r.put(0, &frag(0, 100, 4, b"x")),
            Ok(Push::Over(Limit::Fragments))
        );
        assert_eq!(r.buffered(), 0, "the candidate went");
        assert_eq!(r.pop(), None);
        // The seq starts again from nothing.
        assert_eq!(r.put(0, &frag(0, 2, 0, b"ab")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("complete").body, b"ab");
    }

    #[test]
    fn empty_fragments_allocate_nothing() {
        let mut r = Reassembler::new(LIMITS);
        // Empty fragments of non-empty messages, more than the buffer could
        // hold if each took its declared length.
        for seq in 0..3 {
            assert_eq!(r.put(0, &frag(seq, 100, 0, b"")), Ok(Push::Dropped));
            assert_eq!(r.buffered(), 0);
        }
        assert_eq!(r.pop(), None);
        // A zero-length message still completes on its one empty fragment.
        assert_eq!(r.put(0, &frag(0, 0, 0, b"")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("empty message").body, b"");
    }

    #[test]
    fn duplicates_neither_count_nor_fail() {
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 6, 0, b"abc")).expect("first");
        // Far more repeats than the cap, as heavy loss or a forger could
        // send: dropped, not counted.
        for _ in 0..100 {
            assert_eq!(r.put(0, &frag(0, 6, 0, b"abc")), Ok(Push::Dropped));
            assert_eq!(r.put(0, &frag(0, 6, 1, b"b")), Ok(Push::Dropped));
            assert_eq!(r.put(0, &frag(0, 6, 5, b"")), Ok(Push::Dropped));
        }
        assert_eq!(r.put(0, &frag(0, 6, 3, b"def")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("complete").body, b"abcdef");
    }

    #[test]
    fn a_conflicting_first_fragment_does_not_block_the_message() {
        let mut r = Reassembler::new(LIMITS);
        // A forged fragment arrives first, with another length, epoch and
        // type: each starts its own candidate.
        assert_eq!(r.put(0, &frag(0, 5, 0, b"zz")), Ok(Push::Stored));
        assert_eq!(r.put(1, &frag(0, 4, 0, b"zz")), Ok(Push::Stored));
        let mut other = frag(0, 4, 0, b"zz");
        other.msg_type = 2;
        assert_eq!(r.put(0, &other), Ok(Push::Stored));
        // The real message still assembles and is released; the
        // candidates go with it.
        r.put(0, &frag(0, 4, 0, b"ab")).expect("real");
        assert_eq!(r.pop(), None);
        r.put(0, &frag(0, 4, 2, b"cd")).expect("real");
        let m = r.pop().expect("complete");
        assert_eq!(
            (m.msg_type, m.epoch, m.body.as_slice()),
            (1, 0, &b"abcd"[..])
        );
        assert_eq!(r.buffered(), 0);
        // Candidates are bounded: a fifth is dropped, without failing.
        for len in 10..14 {
            assert_eq!(r.put(0, &frag(1, len, 0, b"x")), Ok(Push::Stored));
        }
        assert_eq!(r.put(0, &frag(1, 20, 0, b"x")), Ok(Push::Dropped));
        // A conflicting candidate that does not fit the buffer is dropped.
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 100, 0, b"x")).expect("first");
        assert_eq!(r.put(0, &frag(0, 60, 0, b"x")), Ok(Push::Dropped));
    }

    #[test]
    fn the_first_started_candidate_wins_after_an_earlier_release() {
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 2, 0, b"a")).expect("seq 0");
        r.put(0, &frag(1, 3, 0, b"abc")).expect("real seq 1");
        let mut forged = frag(1, 3, 0, b"xyz");
        forged.msg_type = 2;
        r.put(0, &forged).expect("forged seq 1");
        r.put(0, &frag(0, 2, 1, b"b")).expect("seq 0 complete");
        assert_eq!(r.pop().map(|m| m.seq), Some(0));
        // Releasing seq 0 must not reorder the seq 1 candidates.
        let m = r.pop().expect("seq 1");
        assert_eq!((m.msg_type, m.body.as_slice()), (1, &b"abc"[..]));
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn a_candidate_over_the_fragment_cap_is_dropped_when_another_exists() {
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 10, 0, b"a")).expect("real");
        let data = [b'x'; 5];
        for i in 0..5u32 {
            let mut forged = frag(0, 10, i, &data[i as usize..=i as usize]);
            forged.msg_type = 2;
            let want = if i < 4 { Push::Stored } else { Push::Dropped };
            assert_eq!(r.put(0, &forged), Ok(want));
        }
        assert_eq!(r.buffered(), 10);
        r.put(0, &frag(0, 10, 1, b"bcdefghij")).expect("real");
        let m = r.pop().expect("complete");
        assert_eq!((m.msg_type, m.body.as_slice()), (1, &b"abcdefghij"[..]));
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn restart_drops_progress() {
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 4, 0, b"ab")).expect("first");
        r.restart_at(1);
        assert_eq!(r.buffered(), 0);
        assert_eq!(
            r.put(0, &frag(0, 4, 2, b"cd")),
            Ok(Push::Old { seq: 0, offset: 2 })
        );
    }

    #[test]
    fn disagreeing_bytes_start_another_candidate() {
        let mut r = Reassembler::new(LIMITS);
        // Genuine [0, 4), then a forgery over [3, 5) that changes byte 3:
        // it does not overwrite byte 3, but starts its own candidate.
        assert_eq!(r.put(0, &frag(0, 8, 0, b"abcd")), Ok(Push::Stored));
        assert_eq!(r.put(0, &frag(0, 8, 3, b"Xe")), Ok(Push::Stored));
        assert_eq!(r.buffered(), 16, "two candidates");
        // The genuine rest agrees with both, so both take it, and the
        // genuine one is complete.
        assert_eq!(r.put(0, &frag(0, 8, 4, b"efgh")), Ok(Push::Stored));
        let m = r.pop().expect("complete");
        assert_eq!(m.body, b"abcdefgh");
        assert_eq!(r.buffered(), 0, "the forgery went with it");
        // The other order: the forged byte first, inside the genuine first
        // fragment's range.
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(r.put(0, &frag(0, 8, 1, b"X")), Ok(Push::Stored));
        assert_eq!(r.put(0, &frag(0, 8, 0, b"abcd")), Ok(Push::Stored));
        assert_eq!(r.put(0, &frag(0, 8, 4, b"efgh")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("complete").body, b"abcdefgh");
        // A fragment that agrees with what is held is a duplicate.
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 8, 0, b"abcd")).expect("first");
        assert_eq!(r.put(0, &frag(0, 8, 1, b"bc")), Ok(Push::Dropped));
        assert_eq!(r.buffered(), 8);
    }

    #[test]
    fn stalled_unauthenticated_candidates_give_way_after_expiry() {
        let t = base();
        let mut r = Reassembler::new(RECLAIMING);
        // Two incomplete forgeries take 141 of the 150 bytes, and one
        // complete candidate for a later seq holds 2.
        r.push(0, &frag(0, 70, 0, b"x"), t).expect("one");
        r.push(0, &frag(0, 71, 0, b"y"), t).expect("two");
        r.push(0, &frag(1, 2, 0, b"ok"), t).expect("complete");
        assert_eq!(r.buffered(), 143);
        // Replacement is allowed once at first; this uses it up.
        assert_eq!(r.push(0, &frag(0, 69, 0, b"z"), t), Ok(Push::Stored));
        assert_eq!(r.buffered(), 142, "the oldest gave way");
        // Before the interval nothing gives way.
        let soon = t + Duration::from_millis(999);
        assert_eq!(r.push(0, &frag(0, 50, 0, b"g"), soon), Ok(Push::Dropped));
        assert_eq!(r.buffered(), 142);
        // After it the stalled ones go, not the complete one, and only
        // because the newcomer needs room.
        let later = t + Duration::from_secs(1);
        assert_eq!(r.push(0, &frag(0, 50, 0, b"g"), later), Ok(Push::Stored));
        assert_eq!(r.buffered(), 52);
        // An authenticated candidate never expires.
        let mut r = Reassembler::new(RECLAIMING);
        r.push(1, &frag(0, 100, 0, b"x"), t).expect("authenticated");
        let much_later = t + Duration::from_secs(60);
        assert_eq!(
            r.push(0, &frag(1, 60, 0, b"x"), much_later),
            Ok(Push::Over(Limit::Buffered))
        );
        assert_eq!(r.buffered(), 100);
    }

    #[test]
    fn replacement_is_rate_limited() {
        let t = base();
        let mut r = Reassembler::new(RECLAIMING);
        // Four slots full.
        for len in 10..14 {
            r.push(0, &frag(0, len, 0, b"x"), t).expect("slot");
        }
        // A fifth replaces the oldest, once.
        assert_eq!(r.push(0, &frag(0, 20, 0, b"x"), t), Ok(Push::Stored));
        assert_eq!(r.buffered(), 11 + 12 + 13 + 20);
        assert_eq!(r.push(0, &frag(0, 21, 0, b"x"), t), Ok(Push::Dropped));
        let half = t + Duration::from_millis(500);
        assert_eq!(r.push(0, &frag(0, 21, 0, b"x"), half), Ok(Push::Dropped));
        // A second later, another is allowed (here none has expired).
        let mut r = Reassembler::new(Limits {
            expire_after: Some(Duration::from_secs(10)),
            ..RECLAIMING
        });
        for len in 10..14 {
            r.push(0, &frag(0, len, 0, b"x"), t).expect("slot");
        }
        assert_eq!(r.push(0, &frag(0, 20, 0, b"x"), t), Ok(Push::Stored));
        assert_eq!(r.push(0, &frag(0, 21, 0, b"x"), t), Ok(Push::Dropped));
        let next = t + Duration::from_secs(1);
        assert_eq!(r.push(0, &frag(0, 21, 0, b"x"), next), Ok(Push::Stored));
        assert_eq!(r.buffered(), 12 + 13 + 20 + 21, "the oldest gave way");
        // Without reclaiming at all (as the test client), none is.
        let mut r = Reassembler::new(LIMITS);
        for len in 10..14 {
            r.put(0, &frag(0, len, 0, b"x")).expect("slot");
        }
        assert_eq!(r.put(0, &frag(0, 20, 0, b"x")), Ok(Push::Dropped));
    }

    #[test]
    fn the_next_message_takes_room_from_later_ones() {
        let t = base();
        // A newcomer that is not the next message cannot take room from a
        // later seq's candidates.
        let mut r = Reassembler::new(RECLAIMING);
        r.push(0, &frag(2, 70, 0, b"x"), t).expect("seq 2");
        r.push(0, &frag(2, 71, 0, b"x"), t).expect("seq 2 again");
        assert_eq!(
            r.push(0, &frag(1, 60, 0, b"y"), t),
            Ok(Push::Over(Limit::Buffered))
        );
        assert_eq!(r.buffered(), 141);
        // The next message (seq 0) can, latest seq first.
        let mut r = Reassembler::new(RECLAIMING);
        r.push(0, &frag(1, 70, 0, b"x"), t).expect("seq 1");
        r.push(0, &frag(2, 70, 0, b"x"), t).expect("seq 2");
        assert_eq!(r.push(0, &frag(0, 60, 0, b"a"), t), Ok(Push::Stored));
        assert_eq!(r.buffered(), 130, "seq 2 gave way");
    }

    #[test]
    fn authenticated_input_comes_first() {
        let t = base();
        let mut r = Reassembler::new(RECLAIMING);
        // The slots full of unauthenticated candidates, one of them
        // complete, and replacement used up.
        r.push(0, &frag(0, 1, 0, b"x"), t).expect("complete");
        for len in [10, 11, 12, 13] {
            r.push(0, &frag(0, len, 0, b"x"), t).expect("slot");
        }
        assert_eq!(r.buffered(), 1 + 11 + 12 + 13, "13 replaced 10");
        assert_eq!(r.push(0, &frag(0, 14, 0, b"x"), t), Ok(Push::Dropped));
        // An authenticated newcomer takes a slot at once, even from the
        // complete one.
        assert_eq!(r.push(1, &frag(0, 8, 0, b"a"), t), Ok(Push::Stored));
        assert_eq!(r.buffered(), 11 + 12 + 13 + 8);
        // Authenticated candidates keep their room: when all of it is
        // theirs, a bound is still reported.
        let mut r = Reassembler::new(RECLAIMING);
        r.push(1, &frag(0, 100, 0, b"x"), t).expect("authenticated");
        assert_eq!(
            r.push(1, &frag(1, 51, 0, b"x"), t),
            Ok(Push::Over(Limit::Buffered))
        );
        for o in 1..4 {
            assert_eq!(r.push(1, &frag(0, 100, o, b"x"), t), Ok(Push::Stored));
        }
        assert_eq!(
            r.push(1, &frag(0, 100, 4, b"x"), t),
            Ok(Push::Over(Limit::Fragments))
        );
    }

    #[test]
    fn a_refused_candidate_leaves_the_others() {
        let mut r = Reassembler::new(LIMITS);
        r.put(0, &frag(0, 2, 0, b"xy")).expect("forged, complete");
        r.put(0, &frag(0, 3, 0, b"abc")).expect("genuine, complete");
        // The first started is taken first; refusing it (not advancing)
        // leaves the genuine one.
        assert_eq!(r.take().expect("forged").body, b"xy");
        assert_eq!(r.next_seq(), 0);
        assert_eq!(r.take().expect("genuine").body, b"abc");
        r.advance();
        assert_eq!(r.next_seq(), 1);
        assert_eq!(r.buffered(), 0);
    }
}
