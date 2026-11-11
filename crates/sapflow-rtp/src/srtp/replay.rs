//! Packet index estimation (RFC 3711 section 3.3.1 and Appendix A) and the
//! replay list (section 3.3.2).
//!
//! A stream's state is the highest authenticated index. RFC 3711 keeps
//! `ROC` and `s_l` separately, but after its update rule `2^16 * ROC + s_l`
//! is always that highest index, so one value holds both.
//!
//! The replay list is a ring of bits, one per index in the window behind
//! the highest. An index ahead of the window, or inside it and never
//! received, is accepted; a repeat, or one behind the window, is not.
//! Nothing changes until the packet has authenticated.

use super::SrtpError;

/// Highest valid 48-bit SRTP index.
const MAX_INDEX: u64 = (1 << 48) - 1;

/// The index of a packet with sequence number `seq`, given the highest
/// authenticated index so far (RFC 3711 Appendix A). An estimate of
/// `ROC - 1` while ROC is 0 is refused as too old; one past `ROC = 2^32 -
/// 1` as exhausted.
pub(crate) fn estimate(highest: u64, seq: u16) -> Result<u64, SrtpError> {
    let roc = i64::try_from(highest >> 16).map_err(|_| SrtpError::Exhausted)?;
    let s_l = i64::from(highest as u16);
    let seq_i = i64::from(seq);
    let v = if s_l < 32_768 {
        if seq_i - s_l > 32_768 { roc - 1 } else { roc }
    } else if s_l - 32_768 > seq_i {
        roc + 1
    } else {
        roc
    };
    if v < 0 {
        return Err(SrtpError::TooOld);
    }
    let index = (v as u64) << 16 | u64::from(seq);
    if index > MAX_INDEX {
        return Err(SrtpError::Exhausted);
    }
    Ok(index)
}

/// A sliding replay window over indices.
pub(crate) struct ReplayWindow {
    /// One bit per index modulo `size`.
    bits: Vec<u64>,
    size: u64,
    /// Highest index accepted; `None` until the first.
    top: Option<u64>,
}

impl ReplayWindow {
    /// `size` is a multiple of 64, checked by the caller.
    pub(crate) fn new(size: u32) -> Self {
        Self {
            bits: vec![0; (size / 64) as usize],
            size: u64::from(size),
            top: None,
        }
    }

    pub(crate) fn top(&self) -> Option<u64> {
        self.top
    }

    fn slot(&self, index: u64) -> (usize, u64) {
        let at = index % self.size;
        ((at / 64) as usize, 1u64 << (at % 64))
    }

    fn is_set(&self, index: u64) -> bool {
        let (word, bit) = self.slot(index);
        self.bits.get(word).is_some_and(|w| w & bit != 0)
    }

    fn set(&mut self, index: u64, on: bool) {
        let (word, bit) = self.slot(index);
        if let Some(w) = self.bits.get_mut(word) {
            if on {
                *w |= bit;
            } else {
                *w &= !bit;
            }
        }
    }

    /// Whether `index` may be accepted, before authentication.
    pub(crate) fn check(&self, index: u64) -> Result<(), SrtpError> {
        let Some(top) = self.top else {
            return Ok(());
        };
        if index > top {
            return Ok(());
        }
        if top - index >= self.size {
            return Err(SrtpError::TooOld);
        }
        if self.is_set(index) {
            return Err(SrtpError::Replayed);
        }
        Ok(())
    }

    /// Records an authenticated `index`, moving the window ahead if needed.
    /// Call [`check`](Self::check) first.
    pub(crate) fn accept(&mut self, index: u64) {
        match self.top {
            Some(top) if index <= top => {}
            Some(top) if index - top < self.size => {
                // Forget the indices the window slides over.
                let mut i = top + 1;
                while i < index {
                    self.set(i, false);
                    i += 1;
                }
                self.top = Some(index);
            }
            _ => {
                self.bits.fill(0);
                self.top = Some(index);
            }
        }
        self.set(index, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_a_estimation() {
        // Same ROC.
        assert_eq!(estimate(100, 101), Ok(101));
        assert_eq!(estimate(100, 50), Ok(50));
        // Wrap forward: s_l near the top, seq small.
        assert_eq!(estimate(65_530, 3), Ok(65_536 + 3));
        // A late packet from before the wrap.
        assert_eq!(estimate(65_536 + 3, 65_530), Ok(65_530));
        // ROC - 1 while ROC is 0.
        assert_eq!(estimate(3, 65_530), Err(SrtpError::TooOld));
        // Boundaries from Appendix A's strict comparisons.
        assert_eq!(estimate(0, 32_768), Ok(32_768));
        assert_eq!(estimate(65_536, 32_769), Ok(32_769));
        assert_eq!(estimate(32_768, 0), Ok(0));
        assert_eq!(estimate(32_769, 0), Ok(65_536));
        // The 48-bit index cannot pass 2^48 - 1.
        assert_eq!(estimate(MAX_INDEX, 1), Err(SrtpError::Exhausted));
        assert_eq!(estimate(MAX_INDEX - 1, 0xffff), Ok(MAX_INDEX));
    }

    #[test]
    fn window_accepts_late_unseen_and_refuses_repeats_and_old() {
        let mut w = ReplayWindow::new(1024);
        assert_eq!(w.check(5000), Ok(()));
        w.accept(5000);
        assert_eq!(w.check(5000), Err(SrtpError::Replayed));
        // Never received, inside the window: accepted once.
        for late in [4999, 4000, 3977] {
            assert_eq!(w.check(late), Ok(()), "{late}");
            w.accept(late);
            assert_eq!(w.check(late), Err(SrtpError::Replayed), "{late}");
        }
        assert_eq!(w.check(3976), Err(SrtpError::TooOld));
        // Moving ahead forgets the slots it slides over.
        w.accept(5000 + 1023);
        assert_eq!(w.check(4999), Err(SrtpError::TooOld));
        assert_eq!(w.check(5001), Ok(()));
        assert_eq!(w.check(5000), Err(SrtpError::Replayed));
        // A jump past the whole window clears it.
        w.accept(100_000);
        assert_eq!(w.check(100_000 - 1023), Ok(()));
        assert_eq!(w.check(100_000 - 1024), Err(SrtpError::TooOld));
        assert_eq!(w.top(), Some(100_000));
    }

    #[test]
    fn ring_slots_are_reused_without_false_replays() {
        let mut w = ReplayWindow::new(64);
        for i in 0..10_000u64 {
            assert_eq!(w.check(i), Ok(()), "{i}");
            w.accept(i);
        }
        for i in 10_000 - 64..10_000 {
            assert_eq!(w.check(i), Err(SrtpError::Replayed));
        }
        // Every other index, then the gaps late.
        let mut w = ReplayWindow::new(128);
        for i in (0..1000u64).step_by(2) {
            w.accept(i);
        }
        for i in (1000 - 127..1000).filter(|i| i % 2 == 1) {
            assert_eq!(w.check(i), Ok(()), "{i}");
        }
    }
}
