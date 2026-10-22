//! Sequence numbers (31 bits, wrapping) and packet timestamps (32 bits of
//! microseconds, wrapping every 71.6 minutes).

use std::cmp::Ordering;

/// A data packet sequence number: 31 bits that wrap from `0x7FFF_FFFF` to 0.
///
/// Ordering is circular: `a < b` when `b` is less than half the space ahead
/// of `a`, which is the only meaningful order for a window that slides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Seq(u32);

impl Seq {
    /// The largest sequence number, `0x7FFF_FFFF` (2^31 - 1).
    pub const MAX: u32 = 0x7FFF_FFFF;

    /// The low 31 bits of `v`.
    pub const fn new(v: u32) -> Self {
        Self(v & Self::MAX)
    }

    /// The sequence number as a plain integer, at most [`Seq::MAX`].
    pub const fn get(self) -> u32 {
        self.0
    }

    /// `self + n`, wrapping.
    pub const fn add(self, n: u32) -> Self {
        Self::new(self.0.wrapping_add(n))
    }

    /// The next sequence number, wrapping from [`Seq::MAX`] to 0.
    pub const fn next(self) -> Self {
        self.add(1)
    }

    /// The previous sequence number, wrapping from 0 to [`Seq::MAX`].
    pub const fn prev(self) -> Self {
        Self::new(self.0.wrapping_sub(1))
    }

    /// How far `other` is ahead of `self` (negative when behind), in the
    /// range -2^30 ..= 2^30 - 1.
    pub const fn distance_to(self, other: Seq) -> i32 {
        // Shift into the top 31 bits so the sign bit carries the wrap.
        (other.0.wrapping_sub(self.0) << 1) as i32 >> 1
    }
}

impl PartialOrd for Seq {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Seq {
    fn cmp(&self, other: &Self) -> Ordering {
        0.cmp(&self.distance_to(*other))
    }
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Turns 32-bit packet timestamps into a 64-bit microsecond count that does
/// not wrap.
///
/// Each timestamp is placed at the nearest point to the latest one seen:
/// a sender's clock only moves forward, and a retransmission carries an
/// older timestamp, never one more than half the 32-bit range (35 minutes)
/// old.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimestampExtender {
    latest: Option<(u32, i64)>,
}

impl TimestampExtender {
    /// An extender that has seen no timestamp yet: the first one passed to
    /// [`extend`](Self::extend) starts the count at its own value.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the count at `ts` (the handshake's timestamp).
    pub fn starting_at(ts: u32) -> Self {
        Self {
            latest: Some((ts, i64::from(ts))),
        }
    }

    /// Returns `ts` as a microsecond count that does not wrap, placed nearest
    /// the latest timestamp seen (negative for one just before the starting
    /// timestamp). Only a timestamp ahead of the latest moves it on, so a
    /// retransmission's older timestamp never moves the count backwards.
    pub fn extend(&mut self, ts: u32) -> i64 {
        match self.latest {
            None => {
                self.latest = Some((ts, i64::from(ts)));
                i64::from(ts)
            }
            Some((raw, ext)) => {
                let diff = i64::from(ts.wrapping_sub(raw) as i32);
                let v = ext.saturating_add(diff);
                if diff > 0 {
                    self.latest = Some((ts, v));
                }
                v
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_wrap_at_31_bits() {
        let top = Seq::new(Seq::MAX);
        assert_eq!(top.next(), Seq::new(0));
        assert_eq!(Seq::new(0).prev(), top);
        assert_eq!(
            Seq::new(0x8000_0005),
            Seq::new(5),
            "the top bit is not part of it"
        );
        assert_eq!(top.distance_to(Seq::new(2)), 3);
        assert_eq!(Seq::new(2).distance_to(top), -3);
        assert!(top < Seq::new(2));
        assert!(Seq::new(2) > top);
        assert_eq!(top.add(10), Seq::new(9));
    }

    #[test]
    fn distance_is_signed_half_the_space() {
        let a = Seq::new(100);
        assert_eq!(a.distance_to(a.add(0x3FFF_FFFF)), 0x3FFF_FFFF);
        assert_eq!(a.distance_to(a.add(0x4000_0000)), -0x4000_0000);
        assert_eq!(a.distance_to(a), 0);
        assert_eq!(a.cmp(&a), Ordering::Equal);
    }

    #[test]
    fn timestamps_extend_across_the_wrap() {
        let mut x = TimestampExtender::starting_at(u32::MAX - 10);
        assert_eq!(x.extend(u32::MAX - 5), i64::from(u32::MAX) - 5);
        // Wraps: 5 past zero is 2^32 + 5.
        assert_eq!(x.extend(5), (1i64 << 32) + 5);
        // A retransmission from before the wrap stays before it.
        assert_eq!(x.extend(u32::MAX - 3), i64::from(u32::MAX) - 3);
        // And the latest is unchanged by it.
        assert_eq!(x.extend(6), (1i64 << 32) + 6);
    }

    #[test]
    fn timestamps_extend_backwards_from_the_start() {
        let mut x = TimestampExtender::starting_at(3);
        assert_eq!(x.extend(u32::MAX), -1, "just before the handshake");
        assert_eq!(x.extend(1_000_000), 1_000_000);
    }
}
