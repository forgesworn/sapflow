//! Handshake message reassembly (RFC 6347 sections 4.2.2 and 4.2.3), with
//! hard bounds: a message's declared length is checked against the cap
//! before anything is allocated, every fragment (duplicate, overlapping,
//! empty or inconsistent) counts towards the per-message fragment cap, and
//! all messages in progress together hold at most `max_buffered` bytes.
//! Messages are released strictly in `message_seq` order.

use crate::message::Fragment;
use crate::{Error, Limit};

/// Reassembly bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) max_message: usize,
    pub(crate) max_fragments: usize,
    pub(crate) max_buffered: usize,
    /// How many messages past the next expected one may be buffered.
    pub(crate) ahead: u16,
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
    /// Stored (it may or may not have completed its message).
    Stored,
    /// Dropped: outside the window, empty, or inconsistent with earlier
    /// fragments of the same message.
    Dropped,
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
}

impl Partial {
    fn complete(&self) -> bool {
        self.body.is_empty() || self.covered.as_slice() == [(0, self.body.len())]
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
}

impl Reassembler {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            next_seq: 0,
            partials: Vec::new(),
            buffered: 0,
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

    pub(crate) fn push(&mut self, epoch: u16, f: &Fragment<'_>) -> Result<Push, Error> {
        if f.seq < self.next_seq {
            return Ok(Push::Old {
                seq: f.seq,
                offset: f.offset,
            });
        }
        if u32::from(f.seq) >= u32::from(self.next_seq) + u32::from(self.limits.ahead) {
            return Ok(Push::Dropped);
        }
        let length = usize::try_from(f.length).map_err(|_| Error::Limit(Limit::MessageSize))?;
        if length > self.limits.max_message {
            return Err(Error::Limit(Limit::MessageSize));
        }
        let index = match self.partials.iter().position(|p| p.seq == f.seq) {
            Some(i) => i,
            None => {
                if self.buffered + length > self.limits.max_buffered {
                    return Err(Error::Limit(Limit::Buffered));
                }
                self.buffered += length;
                self.partials.push(Partial {
                    msg_type: f.msg_type,
                    seq: f.seq,
                    epoch,
                    body: vec![0; length],
                    covered: Vec::new(),
                    fragments: 0,
                });
                self.partials.len() - 1
            }
        };
        let max_fragments = self.limits.max_fragments;
        let p = self.partials.get_mut(index).ok_or(Error::Internal)?;
        p.fragments += 1;
        if p.fragments > max_fragments {
            return Err(Error::Limit(Limit::Fragments));
        }
        if p.msg_type != f.msg_type || p.body.len() != length || p.epoch != epoch {
            return Ok(Push::Dropped);
        }
        if f.data.is_empty() {
            // Progress only for a zero-length message, which is complete
            // as soon as it is seen.
            return Ok(if length == 0 {
                Push::Stored
            } else {
                Push::Dropped
            });
        }
        let start = usize::try_from(f.offset).map_err(|_| Error::Internal)?;
        let end = start + f.data.len();
        // `fragments` already checked offset + length against the declared
        // length, and the body has exactly that length.
        p.body
            .get_mut(start..end)
            .ok_or(Error::Decode)?
            .copy_from_slice(f.data);
        p.cover(start, end);
        Ok(Push::Stored)
    }

    /// The next message in order, if it is complete.
    pub(crate) fn pop(&mut self) -> Option<Message> {
        let i = self
            .partials
            .iter()
            .position(|p| p.seq == self.next_seq && p.complete())?;
        let p = self.partials.swap_remove(i);
        self.buffered -= p.body.len();
        self.next_seq = self.next_seq.checked_add(1)?;
        Some(Message {
            msg_type: p.msg_type,
            seq: p.seq,
            epoch: p.epoch,
            body: p.body,
        })
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
    };

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
        assert_eq!(r.push(0, &frag(0, 6, 3, b"def")), Ok(Push::Stored));
        assert_eq!(r.pop(), None);
        assert_eq!(r.push(0, &frag(0, 6, 1, b"bcd")), Ok(Push::Stored));
        assert_eq!(r.pop(), None);
        assert_eq!(r.push(0, &frag(0, 6, 0, b"a")), Ok(Push::Stored));
        let m = r.pop().expect("complete");
        assert_eq!(m.body, b"abcdef");
        assert_eq!(r.next_seq(), 1);
        assert_eq!(r.buffered(), 0);
        assert_eq!(
            r.push(0, &frag(0, 6, 0, b"a")),
            Ok(Push::Old { seq: 0, offset: 0 })
        );
        // A zero-length message completes on its one empty fragment.
        assert_eq!(r.push(0, &frag(1, 0, 0, b"")), Ok(Push::Stored));
        assert_eq!(r.pop().expect("empty message").body, b"");
    }

    #[test]
    fn messages_release_in_order() {
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(r.push(0, &frag(2, 1, 0, b"c")), Ok(Push::Stored));
        assert_eq!(r.push(0, &frag(1, 1, 0, b"b")), Ok(Push::Stored));
        assert_eq!(
            r.push(0, &frag(3, 1, 0, b"d")),
            Ok(Push::Dropped),
            "beyond the window"
        );
        assert_eq!(r.pop(), None);
        r.push(0, &frag(0, 1, 0, b"a")).expect("push");
        let got: Vec<_> = std::iter::from_fn(|| r.pop()).map(|m| m.body).collect();
        assert_eq!(got, [b"a", b"b", b"c"]);
    }

    #[test]
    fn caps_are_real_bounds() {
        let mut r = Reassembler::new(LIMITS);
        assert_eq!(
            r.push(0, &frag(0, 101, 0, b"x")),
            Err(Error::Limit(Limit::MessageSize))
        );
        r.push(0, &frag(0, 100, 0, b"x")).expect("first");
        assert_eq!(
            r.push(0, &frag(1, 51, 0, b"x")),
            Err(Error::Limit(Limit::Buffered)),
            "100 + 51 > 150"
        );
        // Duplicates and empty fragments are not progress, but count.
        r.push(0, &frag(0, 100, 0, b"x")).expect("dup");
        assert_eq!(r.push(0, &frag(0, 100, 5, b"")), Ok(Push::Dropped));
        r.push(0, &frag(0, 100, 0, b"x")).expect("dup");
        assert_eq!(
            r.push(0, &frag(0, 100, 0, b"x")),
            Err(Error::Limit(Limit::Fragments))
        );
    }

    #[test]
    fn inconsistent_fragments_are_dropped() {
        let mut r = Reassembler::new(Limits {
            max_fragments: 8,
            ..LIMITS
        });
        r.push(0, &frag(0, 4, 0, b"ab")).expect("first");
        assert_eq!(
            r.push(0, &frag(0, 5, 2, b"cd")),
            Ok(Push::Dropped),
            "other length"
        );
        assert_eq!(
            r.push(1, &frag(0, 4, 2, b"cd")),
            Ok(Push::Dropped),
            "other epoch"
        );
        let mut other = frag(0, 4, 2, b"cd");
        other.msg_type = 2;
        assert_eq!(r.push(0, &other), Ok(Push::Dropped), "other type");
        assert_eq!(r.pop(), None);
        r.push(0, &frag(0, 4, 2, b"cd")).expect("fits");
        assert_eq!(r.pop().expect("complete").body, b"abcd");
    }

    #[test]
    fn restart_drops_progress() {
        let mut r = Reassembler::new(LIMITS);
        r.push(0, &frag(0, 4, 0, b"ab")).expect("first");
        r.restart_at(1);
        assert_eq!(r.buffered(), 0);
        assert_eq!(
            r.push(0, &frag(0, 4, 2, b"cd")),
            Ok(Push::Old { seq: 0, offset: 2 })
        );
    }
}
