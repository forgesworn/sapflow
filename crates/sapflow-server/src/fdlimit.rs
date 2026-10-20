//! Raise the open-file limit at startup, as Go's runtime does for mediamtx.
//!
//! Every viewer holds a connection or two. A service started by systemd or
//! over SSH often gets a soft limit of 256 or 1,024 files, and past it new
//! connections fail ("Too many open files"): found in the cross-machine
//! benchmark, where 250 viewers ran into macOS's 256.

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// Never ask for more than this, whatever the hard limit says.
const CEILING: u64 = 1 << 20;

/// The soft limits to try, highest first: the hard limit (capped), then
/// values macOS accepts when the hard limit is "unlimited".
pub fn candidates(soft: u64, hard: Option<u64>) -> Vec<u64> {
    let top = hard.unwrap_or(CEILING).min(CEILING);
    [top, 65_536, 10_240]
        .into_iter()
        .filter(|&n| n > soft && n <= top)
        .collect()
}

/// Raises the soft limit as far as allowed. Returns (before, after).
pub fn raise() -> (u64, u64) {
    let Rlimit { current, maximum } = getrlimit(Resource::Nofile);
    let soft = current.unwrap_or(CEILING);
    for n in candidates(soft, maximum) {
        let new = Rlimit {
            current: Some(n),
            maximum,
        };
        if setrlimit(Resource::Nofile, new).is_ok() {
            return (soft, n);
        }
    }
    (soft, soft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_respect_the_hard_limit() {
        assert_eq!(candidates(256, None), vec![CEILING, 65_536, 10_240]);
        assert_eq!(candidates(256, Some(4_096)), vec![4_096]);
        assert_eq!(candidates(1_024, Some(20_000)), vec![20_000, 10_240]);
        assert!(candidates(CEILING, None).is_empty());
    }

    #[test]
    fn raising_never_lowers() {
        let (before, after) = raise();
        assert!(after >= before);
    }
}
