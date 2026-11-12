//! Allocation ownership for encoded media shared through `Bytes` aliases.

use bytes::Bytes;
use sapflow_core::fragment::{FragmentLimit, FragmentPermit, FragmentQuota};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Which budget one allocation is charged to, shared by every alias of it:
/// the live fragment budget, or a retention pool once the muxer keeps it
/// only for RFC 8216bis 6.2.2. The charge goes when the last alias does.
#[derive(Debug)]
pub(crate) struct Charge {
    capacity: usize,
    /// The permit, and whether it is the pool's.
    permit: Mutex<(FragmentPermit, bool)>,
}

impl Charge {
    /// Moves the charge from the live budget to `pool`, taking the pool's
    /// permit before returning the live one, so the allocation is never
    /// uncharged. False, and no change, if the pool cannot take it.
    pub(crate) fn keep_in(&self, pool: &Arc<FragmentQuota>) -> bool {
        let mut permit = self.permit.lock().unwrap_or_else(PoisonError::into_inner);
        if permit.1 {
            return true;
        }
        match pool.try_acquire(self.capacity) {
            Ok(kept) => {
                *permit = (kept, true);
                true
            }
            Err(FragmentLimit) => false,
        }
    }

    /// Whether the pool holds the charge.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn kept(&self) -> bool {
        self.permit.lock().unwrap_or_else(PoisonError::into_inner).1
    }
}

// Fields drop in order: free storage before returning its byte reservation.
struct Backing {
    data: Vec<u8>,
    _charge: Arc<Charge>,
}

impl AsRef<[u8]> for Backing {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

#[cfg(test)]
pub(crate) fn retain(
    data: Vec<u8>,
    quota: Option<&Arc<FragmentQuota>>,
) -> Result<Bytes, FragmentLimit> {
    retain_charged(data, quota).map(|(bytes, _)| bytes)
}

/// Wraps encoded bytes, charging them to `quota` when there is one, and returns the charge.
pub(crate) fn retain_charged(
    data: Vec<u8>,
    quota: Option<&Arc<FragmentQuota>>,
) -> Result<(Bytes, Option<Arc<Charge>>), FragmentLimit> {
    match quota {
        Some(quota) => {
            let charge = Arc::new(Charge {
                capacity: data.capacity(),
                permit: Mutex::new((quota.try_acquire(data.capacity())?, false)),
            });
            let bytes = Bytes::from_owner(Backing {
                data,
                _charge: charge.clone(),
            });
            Ok((bytes, Some(charge)))
        }
        None => Ok((Bytes::from(data), None)),
    }
}

/// The retention pool of a live fragment budget: one per budget, shared by
/// every muxer on it, so that what they keep is bounded together without
/// the server handing a pool over. Keyed by the budget's allocation; a
/// budget's entry goes once the budget does.
pub(crate) fn retention_pool(live: &Arc<FragmentQuota>) -> Arc<FragmentQuota> {
    type Pools = Vec<(Weak<FragmentQuota>, Arc<FragmentQuota>)>;
    static POOLS: Mutex<Pools> = Mutex::new(Vec::new());
    let mut pools = POOLS.lock().unwrap_or_else(PoisonError::into_inner);
    pools.retain(|(budget, _)| budget.strong_count() > 0);
    if let Some((_, pool)) = pools
        .iter()
        .find(|(budget, _)| std::ptr::eq(budget.as_ptr(), Arc::as_ptr(live)))
    {
        return pool.clone();
    }
    let pool = Arc::new(FragmentQuota::new(crate::muxer::RETENTION_POOL_BYTES));
    pools.push((Arc::downgrade(live), pool.clone()));
    pool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_capacity_is_charged_once_across_clones_and_slices() {
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(b"generated fragment");
        let capacity = data.capacity();
        let pointer = data.as_ptr();
        let quota = Arc::new(FragmentQuota::new(capacity));
        let bytes = retain(data, Some(&quota)).expect("exact capacity");
        assert_eq!(bytes.as_ptr(), pointer, "no payload copy");
        assert_eq!(quota.used(), capacity, "include spare encoded capacity");
        let alias = bytes.clone();
        let suffix = bytes.slice(10..);
        assert!(retain(vec![1], Some(&quota)).is_err());
        drop(bytes);
        drop(alias);
        assert_eq!(
            quota.used(),
            capacity,
            "short slice retains the whole allocation"
        );
        assert_eq!(suffix.as_ref(), b"fragment");
        drop(suffix);
        assert_eq!(quota.used(), 0);
    }

    #[test]
    fn a_kept_charge_moves_to_the_pool_and_goes_with_the_last_alias() {
        let data = vec![7u8; 100];
        let capacity = data.capacity();
        let live = Arc::new(FragmentQuota::new(capacity));
        let pool = Arc::new(FragmentQuota::new(capacity));
        let (bytes, charge) = retain_charged(data, Some(&live)).expect("fits");
        let charge = charge.expect("charged");
        assert_eq!((live.used(), pool.used()), (capacity, 0));
        assert!(charge.keep_in(&pool));
        assert!(charge.kept());
        assert_eq!((live.used(), pool.used()), (0, capacity));
        assert!(
            retain(vec![0; 100], Some(&live)).is_ok(),
            "live room is back"
        );
        // A full pool takes nothing and changes nothing.
        let (other, other_charge) = retain_charged(vec![1u8; 100], Some(&live)).expect("fits");
        assert!(!other_charge.as_ref().expect("charged").keep_in(&pool));
        assert_eq!(live.used(), capacity);
        drop((other, other_charge));
        let alias = bytes.slice(10..);
        drop((bytes, charge));
        assert_eq!(pool.used(), capacity, "an alias still holds it");
        drop(alias);
        assert_eq!((live.used(), pool.used()), (0, 0));
    }

    #[test]
    fn one_live_budget_has_one_retention_pool() {
        let live = Arc::new(FragmentQuota::new(1));
        let other = Arc::new(FragmentQuota::new(1));
        let pool = retention_pool(&live);
        assert!(Arc::ptr_eq(&pool, &retention_pool(&live)));
        assert!(!Arc::ptr_eq(&pool, &retention_pool(&other)));
    }

    #[test]
    fn one_over_capacity_and_empty_spare_capacity_are_not_hidden() {
        let data = Vec::<u8>::with_capacity(129);
        let quota = Arc::new(FragmentQuota::new(data.capacity() - 1));
        assert!(retain(data, Some(&quota)).is_err());
        assert_eq!(quota.used(), 0);
        let data = Vec::<u8>::with_capacity(64);
        let capacity = data.capacity();
        let quota = Arc::new(FragmentQuota::new(capacity));
        let empty = retain(data, Some(&quota)).expect("empty backing still owns capacity");
        assert!(empty.is_empty());
        assert_eq!(quota.used(), capacity);
        drop(empty);
        assert_eq!(quota.used(), 0);
    }
}
