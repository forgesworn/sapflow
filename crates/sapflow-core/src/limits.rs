//! Publisher limits: current owners, failed-login throttling and bitrate.
//!
//! Pure bookkeeping with the clock passed in; the server does the I/O.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Current publication-owner bookkeeping, separate from transport connections,
/// retained presentations and reconnect grace. One admission domain supplies
/// its configured maximum for every acquisition.
#[derive(Debug, Default)]
pub struct PublicationQuota {
    active: std::sync::atomic::AtomicUsize,
}

/// No additional publication owner can be admitted at this limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationLimit;

/// One current owner. Move this permit on a same-path takeover; do not clone
/// it or keep it on the old connection while its successor publishes.
#[derive(Debug)]
pub struct PublicationPermit {
    quota: std::sync::Arc<PublicationQuota>,
}

impl PublicationQuota {
    /// Creates an empty current-owner counter.
    pub fn new() -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Reserves one owner under the admission domain's configured maximum.
    /// Zero refuses new owners; failed admission changes no count or policy.
    /// The server supplies the same immutable per-protocol maximum each time.
    /// An unpublished owner's dropped permit rolls its reservation back.
    pub fn try_acquire(
        self: &std::sync::Arc<Self>,
        maximum: usize,
    ) -> Result<PublicationPermit, PublicationLimit> {
        use std::sync::atomic::Ordering;
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1).filter(|next| *next <= maximum)
            })
            .map_err(|_| PublicationLimit)?;
        Ok(PublicationPermit {
            quota: self.clone(),
        })
    }

    /// A momentary count of current owner permits, not transport connections.
    pub fn active(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl Drop for PublicationPermit {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.quota
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("publication permit released exactly once");
    }
}

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

#[cfg(test)]
mod publication_quota_tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Barrier};

    #[test]
    fn publication_quota_is_exact_zero_is_closed_and_moved_or_unpublished_permits_release() {
        let closed = Arc::new(PublicationQuota::new());
        assert_eq!(
            closed
                .try_acquire(0)
                .expect_err("zero maximum refuses every owner"),
            PublicationLimit
        );
        assert_eq!(closed.active(), 0);
        let quota = Arc::new(PublicationQuota::new());
        let first = quota.try_acquire(2).expect("first owner");
        let second = quota.try_acquire(2).expect("second owner");
        assert_eq!(
            quota
                .try_acquire(2)
                .expect_err("two owners fill the configured maximum"),
            PublicationLimit
        );
        let successor = Some(first);
        assert_eq!(quota.active(), 2, "a move does not acquire another credit");
        drop(second);
        assert_eq!(quota.active(), 1);
        drop(successor);
        assert_eq!(quota.active(), 0);
        let unpublished: Result<(), ()> = {
            let _draft = quota.try_acquire(2).expect("unpublished draft");
            Err(())
        };
        assert!(unpublished.is_err());
        assert_eq!(quota.active(), 0);
    }

    #[test]
    fn publication_quota_checked_overflow_refuses_without_large_allocation() {
        let quota = Arc::new(PublicationQuota::new());
        quota.active.store(usize::MAX, Ordering::Relaxed);
        assert_eq!(
            quota
                .try_acquire(usize::MAX)
                .expect_err("checked addition refuses the synthetic usize boundary"),
            PublicationLimit
        );
        assert_eq!(quota.active(), usize::MAX);
        quota.active.store(0, Ordering::Relaxed);
        drop(
            quota
                .try_acquire(usize::MAX)
                .expect("still usable after synthetic boundary"),
        );
        assert_eq!(quota.active(), 0);
    }

    #[test]
    fn concurrent_publication_admission_cannot_exceed_the_exact_limit() {
        let quota = Arc::new(PublicationQuota::new());
        let start = Arc::new(Barrier::new(5));
        let held = Arc::new(Barrier::new(5));
        let release = Arc::new(Barrier::new(5));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let (quota, start, held, release) =
                (quota.clone(), start.clone(), held.clone(), release.clone());
            threads.push(std::thread::spawn(move || {
                start.wait();
                let permit = quota.try_acquire(2);
                held.wait();
                release.wait();
                permit.is_ok()
            }));
        }
        start.wait();
        held.wait();
        let observed = quota.active();
        release.wait();
        let accepted = threads
            .into_iter()
            .map(|t| usize::from(t.join().expect("worker")))
            .sum::<usize>();
        assert_eq!(observed, 2);
        assert_eq!(accepted, 2);
        assert_eq!(quota.active(), 0);
    }
}
