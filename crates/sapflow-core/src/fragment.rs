//! Checked byte admission for immutable encoded backing allocations.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// One shared byte budget. Charges describe allocations, not aliases or RSS.
#[derive(Debug)]
pub struct FragmentQuota {
    maximum: usize,
    used: AtomicUsize,
}

/// The entire encoded backing cannot be admitted under this budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentLimit;

/// One whole allocation's charge, held until its final payload owner drops.
#[derive(Debug)]
pub struct FragmentPermit {
    quota: Arc<FragmentQuota>,
    capacity: usize,
}

impl FragmentQuota {
    /// Creates an empty shared budget. Zero admits only zero-capacity storage.
    pub fn new(maximum: usize) -> Self {
        Self {
            maximum,
            used: AtomicUsize::new(0),
        }
    }

    /// Reserves a whole allocation capacity before publishing any alias.
    /// Exact capacity is accepted; overflow and one-over refusal change nothing.
    pub fn try_acquire(self: &Arc<Self>, capacity: usize) -> Result<FragmentPermit, FragmentLimit> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(capacity)
                    .filter(|next| *next <= self.maximum)
            })
            .map_err(|_| FragmentLimit)?;
        Ok(FragmentPermit {
            quota: self.clone(),
            capacity,
        })
    }

    /// Logical charged allocation capacity, including backings outside caches.
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

impl Drop for FragmentPermit {
    fn drop(&mut self) {
        self.quota
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_sub(self.capacity)
            })
            .expect("fragment allocation released exactly once");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_capacity_one_over_and_final_release() {
        let quota = Arc::new(FragmentQuota::new(8));
        let first = quota.try_acquire(3).expect("first allocation");
        let second = quota.try_acquire(5).expect("exact capacity");
        assert!(quota.try_acquire(1).is_err());
        assert_eq!(quota.used(), 8);
        drop(first);
        let replacement = quota.try_acquire(3).expect("released capacity");
        drop(second);
        drop(replacement);
        assert_eq!(quota.used(), 0);
        let zero = Arc::new(FragmentQuota::new(0));
        assert!(zero.try_acquire(1).is_err());
        assert!(zero.try_acquire(0).is_ok());
    }

    #[test]
    fn overflow_never_wraps_or_changes_the_charge() {
        let quota = Arc::new(FragmentQuota::new(usize::MAX));
        let full = quota
            .try_acquire(usize::MAX)
            .expect("representable bookkeeping");
        assert!(quota.try_acquire(1).is_err());
        assert_eq!(quota.used(), usize::MAX);
        drop(full);
        assert_eq!(quota.used(), 0);
    }

    #[test]
    fn concurrent_allocations_cannot_exceed_one_shared_budget() {
        let quota = Arc::new(FragmentQuota::new(12));
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let quota = quota.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let permit = quota.try_acquire(3).ok();
                    barrier.wait();
                    barrier.wait();
                    permit.is_some()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(quota.used(), 12);
        barrier.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().expect("allocation worker"))
                .filter(|admitted| *admitted)
                .count(),
            4
        );
        assert_eq!(quota.used(), 0);
    }
}
