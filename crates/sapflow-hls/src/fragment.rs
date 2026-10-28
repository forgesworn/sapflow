//! Allocation ownership for encoded media shared through `Bytes` aliases.

use bytes::Bytes;
use sapflow_core::fragment::{FragmentLimit, FragmentPermit, FragmentQuota};
use std::sync::Arc;

// Fields drop in order: free storage before returning its byte reservation.
struct Backing {
    data: Vec<u8>,
    _permit: FragmentPermit,
}

impl AsRef<[u8]> for Backing {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

pub(crate) fn retain(
    data: Vec<u8>,
    quota: Option<&Arc<FragmentQuota>>,
) -> Result<Bytes, FragmentLimit> {
    match quota {
        Some(quota) => {
            let permit = quota.try_acquire(data.capacity())?;
            Ok(Bytes::from_owner(Backing {
                data,
                _permit: permit,
            }))
        }
        None => Ok(Bytes::from(data)),
    }
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
