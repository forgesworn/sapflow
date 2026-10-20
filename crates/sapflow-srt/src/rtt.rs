//! Round-trip time from ACK/ACKACK pairs (section 4.10).

/// Microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rtt {
    rtt: i64,
    var: i64,
    sampled: bool,
}

impl Default for Rtt {
    fn default() -> Self {
        Self::new()
    }
}

impl Rtt {
    /// The specification's starting values: 100 ms, variance 50 ms.
    pub const fn new() -> Self {
        Self {
            rtt: 100_000,
            var: 50_000,
            sampled: false,
        }
    }

    pub fn rtt(&self) -> i64 {
        self.rtt
    }

    pub fn var(&self) -> i64 {
        self.var
    }

    /// Adds one measurement. The first replaces the starting values (as
    /// libsrt's receiver does, observed: its ACK after the first ACKACK
    /// reports that sample as the RTT and half of it as the variance);
    /// later ones are smoothed, RTT by 1/8 and variance by 1/4.
    pub fn sample(&mut self, rtt: i64) {
        // Nothing real takes a minute; a stale ACKACK could.
        let rtt = rtt.clamp(1, 60_000_000);
        if !self.sampled {
            self.sampled = true;
            self.rtt = rtt;
            self.var = rtt / 2;
            return;
        }
        self.var = (3 * self.var + (self.rtt - rtt).abs()) / 4;
        self.rtt = (7 * self.rtt + rtt) / 8;
    }

    /// How often periodic NAK reports go out: (RTT + 4 * RTTVar) / 2, at
    /// least 20 ms.
    pub fn nak_interval(&self) -> i64 {
        ((self.rtt + 4 * self.var) / 2).max(20_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_replaces_then_smooths() {
        let mut r = Rtt::new();
        assert_eq!((r.rtt(), r.var()), (100_000, 50_000));
        r.sample(63_697);
        assert_eq!((r.rtt(), r.var()), (63_697, 31_848));
        r.sample(63_697);
        assert_eq!(r.rtt(), 63_697);
        assert_eq!(r.var(), 31_848 * 3 / 4);
        r.sample(71_697);
        assert_eq!(r.rtt(), 63_697 + 1_000);
    }

    #[test]
    fn nak_interval_has_a_floor() {
        let mut r = Rtt::new();
        r.sample(500);
        assert_eq!(r.nak_interval(), 20_000);
        let r = Rtt::new();
        assert_eq!(r.nak_interval(), 150_000);
    }

    #[test]
    fn absurd_samples_are_clamped() {
        let mut r = Rtt::new();
        r.sample(-5);
        assert_eq!(r.rtt(), 1);
        r.sample(i64::MAX);
        assert!(r.rtt() < 10_000_000);
    }
}
