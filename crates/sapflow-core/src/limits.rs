//! Limits on publishers: failed-login throttling and a bitrate cap.
//!
//! Pure bookkeeping with the clock passed in; the server does the I/O.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Blocks an address after too many failed publish attempts in a window,
/// which makes guessing the publisher password slow.
#[derive(Debug)]
pub struct AuthThrottle {
    max_failures: u32,
    window: Duration,
    failures: HashMap<IpAddr, (u32, Instant)>,
}

/// Addresses tracked at once; past this the table is cleared (an attacker
/// with that many addresses is not slowed by this anyway).
const MAX_TRACKED: usize = 10_000;

impl AuthThrottle {
    /// Creates a throttle that refuses an address after `max_failures`
    /// failures, until `window` has passed since the first of them.
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            max_failures,
            window,
            failures: HashMap::new(),
        }
    }

    /// Whether `ip` may try to publish now.
    pub fn allowed(&mut self, ip: IpAddr, now: Instant) -> bool {
        match self.failures.get(&ip) {
            Some(&(n, since)) if now.duration_since(since) < self.window => n < self.max_failures,
            Some(_) => {
                self.failures.remove(&ip);
                true
            }
            None => true,
        }
    }

    /// Records a failed publish attempt from `ip`, starting a new window if
    /// the last one has ended.
    pub fn failed(&mut self, ip: IpAddr, now: Instant) {
        if self.failures.len() >= MAX_TRACKED && !self.failures.contains_key(&ip) {
            let window = self.window;
            self.failures
                .retain(|_, (_, since)| now.duration_since(*since) < window);
            if self.failures.len() >= MAX_TRACKED {
                self.failures.clear();
            }
        }
        let entry = self.failures.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= self.window {
            *entry = (0, now);
        }
        entry.0 += 1;
    }
}

/// Caps one publisher's bitrate, averaged over a few seconds so keyframe
/// bursts pass.
#[derive(Debug)]
pub struct RateCap {
    max_bytes_per_window: u64,
    window: Duration,
    start: Option<Instant>,
    bytes: u64,
}

impl RateCap {
    /// Creates a cap of `max_bits_per_sec`, averaged over each `window`. The
    /// byte budget uses whole seconds of the window, with a minimum of one.
    pub fn new(max_bits_per_sec: u64, window: Duration) -> Self {
        Self {
            max_bytes_per_window: max_bits_per_sec / 8 * window.as_secs().max(1),
            window,
            start: None,
            bytes: 0,
        }
    }

    /// Counts `n` bytes; false once the current window is over the cap.
    pub fn add(&mut self, n: usize, now: Instant) -> bool {
        let start = *self.start.get_or_insert(now);
        if now.duration_since(start) >= self.window {
            self.start = Some(now);
            self.bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(n as u64);
        self.bytes <= self.max_bytes_per_window
    }
}

/// Lets through at most `max` events per window and counts the rest, so a
/// stream of bad requests cannot flood the log.
#[derive(Debug)]
pub struct LogLimiter {
    max: u32,
    window: Duration,
    start: Option<Instant>,
    seen: u32,
    suppressed: u64,
}

impl LogLimiter {
    /// Creates a limiter that lets through `max` events in each `window`.
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            start: None,
            seen: 0,
            suppressed: 0,
        }
    }

    /// `Some(n)` if this event should be logged, where `n` is how many were
    /// suppressed since the last one logged; `None` to stay quiet.
    pub fn allow(&mut self, now: Instant) -> Option<u64> {
        let start = *self.start.get_or_insert(now);
        if now.duration_since(start) >= self.window {
            self.start = Some(now);
            self.seen = 0;
        }
        if self.seen < self.max {
            self.seen += 1;
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_repeated_failures_then_forgets() {
        let mut t = AuthThrottle::new(3, Duration::from_secs(60));
        let ip: IpAddr = "192.0.2.1".parse().expect("ip");
        let other: IpAddr = "192.0.2.2".parse().expect("ip");
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(t.allowed(ip, t0));
            t.failed(ip, t0);
        }
        assert!(!t.allowed(ip, t0 + Duration::from_secs(30)));
        assert!(t.allowed(other, t0));
        assert!(t.allowed(ip, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn table_stays_bounded() {
        let mut t = AuthThrottle::new(1, Duration::from_secs(60));
        let now = Instant::now();
        for i in 0..(MAX_TRACKED as u32 + 10) {
            t.failed(IpAddr::from(i.to_be_bytes()), now);
        }
        assert!(t.failures.len() <= MAX_TRACKED);
    }

    #[test]
    fn log_limiter_counts_what_it_drops() {
        let mut l = LogLimiter::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert_eq!(l.allow(t0), Some(0));
        assert_eq!(l.allow(t0), Some(0));
        assert_eq!(l.allow(t0), None);
        assert_eq!(l.allow(t0), None);
        assert_eq!(l.allow(t0 + Duration::from_secs(61)), Some(2));
    }

    #[test]
    fn rate_cap_allows_bursts_within_the_window() {
        let mut r = RateCap::new(8_000_000, Duration::from_secs(5)); // 5 MB per 5 s
        let t0 = Instant::now();
        assert!(r.add(4_000_000, t0));
        assert!(r.add(1_000_000, t0 + Duration::from_secs(1)));
        assert!(!r.add(1, t0 + Duration::from_secs(2)));
        assert!(r.add(1_000_000, t0 + Duration::from_secs(5)));
    }
}
