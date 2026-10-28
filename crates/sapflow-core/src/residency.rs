//! Final-owner admission for resident media presentations, separate from publishers.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// One admission domain's immutable presentation limit and live reservations.
#[derive(Debug)]
pub struct PresentationQuota {
    maximum: usize,
    active: AtomicUsize,
}

/// A new presentation would exceed the resident limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationLimit;

/// Held by the presentation itself until its final owner releases it.
/// Registry removal and publisher retirement must not release this reservation.
#[derive(Debug)]
pub struct PresentationPermit {
    quota: Arc<PresentationQuota>,
}

/// Initial server policy: four presentations per publisher, capped at 4,096.
/// This is admission bookkeeping, not a qualified memory or audience budget.
pub fn default_presentation_limit(publishers: usize) -> usize {
    publishers.checked_mul(4).unwrap_or(4096).min(4096)
}

impl PresentationQuota {
    /// Creates an empty domain. Zero refuses every new presentation.
    pub fn new(maximum: usize) -> Self {
        Self {
            maximum,
            active: AtomicUsize::new(0),
        }
    }

    /// Reserves one presentation before it becomes visible. Failure changes nothing.
    pub fn try_acquire(self: &Arc<Self>) -> Result<PresentationPermit, PresentationLimit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1).filter(|next| *next <= self.maximum)
            })
            .map_err(|_| PresentationLimit)?;
        Ok(PresentationPermit {
            quota: self.clone(),
        })
    }

    /// A momentary count through final presentation owners, including retired ones.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

impl Drop for PresentationPermit {
    fn drop(&mut self) {
        self.quota
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("presentation permit released exactly once");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_caps_before_overflow() {
        for (publishers, expected) in [(0, 0), (1, 4), (16, 64), (1024, 4096), (usize::MAX, 4096)] {
            assert_eq!(default_presentation_limit(publishers), expected);
        }
    }

    #[test]
    fn exact_capacity_refusal_and_final_release() {
        let quota = Arc::new(PresentationQuota::new(2));
        let first = quota.try_acquire().expect("first");
        let second = quota.try_acquire().expect("second");
        assert_eq!(quota.active(), 2);
        assert!(quota.try_acquire().is_err());
        assert_eq!(quota.active(), 2);
        drop(first);
        let replacement = quota.try_acquire().expect("released reservation");
        drop(second);
        drop(replacement);
        assert_eq!(quota.active(), 0);
        assert!(Arc::new(PresentationQuota::new(0)).try_acquire().is_err());
    }

    #[test]
    fn an_unrepresentable_count_is_refused_without_mutation() {
        let quota = Arc::new(PresentationQuota {
            maximum: usize::MAX,
            active: AtomicUsize::new(usize::MAX),
        });
        assert!(quota.try_acquire().is_err());
        assert_eq!(quota.active(), usize::MAX);
    }

    #[test]
    fn concurrent_admission_and_release_respect_one_domain() {
        let quota = Arc::new(PresentationQuota::new(4));
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let quota = quota.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let permit = quota.try_acquire().ok();
                    barrier.wait();
                    barrier.wait();
                    permit.is_some()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(quota.active(), 4);
        barrier.wait();
        let admitted = threads
            .into_iter()
            .map(|t| t.join().expect("worker"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 4);
        assert_eq!(quota.active(), 0);
    }
}
