//! Whether the retained-media cap fits the process's memory limit.
//!
//! `SAPFLOW_MAX_FRAGMENT_BYTES` bounds encoded HLS and recorder backing, not
//! the whole process. A cap at or above a container's memory limit lets one
//! high-bitrate publisher get the server killed instead of refused. These are
//! pure parsers for Linux cgroup files; the caller reads them.

/// Largest share of the memory limit the fragment cap may take, as a
/// fraction. The rest covers the process itself, parser and socket buffers,
/// and allocator overhead. A heuristic, not a measured bound.
pub const FRAGMENT_SHARE: (u64, u64) = (3, 4);

/// The largest fragment cap that fits `limit` bytes of memory.
pub fn max_fragment_bytes(limit: u64) -> u64 {
    limit / FRAGMENT_SHARE.1 * FRAGMENT_SHARE.0
}

/// Whether a `cap`-byte fragment cap leaves too little of `limit`.
pub fn fragment_cap_exceeds(cap: u64, limit: u64) -> bool {
    cap > max_fragment_bytes(limit)
}

/// Parses a cgroup v2 `memory.max` or v1 `memory.limit_in_bytes` value.
/// `max` and v1's near-`i64::MAX` "unlimited" value mean no limit.
pub fn parse_limit(contents: &str) -> Option<u64> {
    let value = contents.trim().parse::<u64>().ok()?;
    (value > 0 && value < 1 << 62).then_some(value)
}

/// Limit files to check, relative to `/sys/fs/cgroup`, from the process's own
/// cgroup up to the root, given `/proc/self/cgroup`. The effective limit is
/// the lowest of those that exist. Paths with `..` are ignored.
pub fn limit_files(proc_self_cgroup: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in proc_self_cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(_), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (prefix, file) = if controllers.is_empty() {
            ("", "memory.max")
        } else if controllers.split(',').any(|c| c == "memory") {
            ("/memory", "memory.limit_in_bytes")
        } else {
            continue;
        };
        if !path.starts_with('/') || path.split('/').any(|part| part == "..") {
            continue;
        }
        let mut dir = path.trim_end_matches('/');
        loop {
            files.push(format!("{prefix}{dir}/{file}"));
            match dir.rfind('/') {
                Some(at) => dir = &dir[..at],
                None => break,
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn default_cap_on_a_small_container_is_flagged_but_a_reduced_one_is_not() {
        // The soak that was OOM-killed: 512 MiB default cap, 384 MiB limit.
        assert!(fragment_cap_exceeds(512 * MIB, 384 * MIB));
        assert!(fragment_cap_exceeds(384 * MIB, 384 * MIB));
        assert!(!fragment_cap_exceeds(256 * MIB, 384 * MIB));
        assert_eq!(max_fragment_bytes(384 * MIB), 288 * MIB);
        assert!(!fragment_cap_exceeds(512 * MIB, 4096 * MIB));
    }

    #[test]
    fn limit_values_parse_bytes_and_treat_max_or_v1_unlimited_as_none() {
        assert_eq!(parse_limit("402653184\n"), Some(384 * MIB));
        assert_eq!(parse_limit("max\n"), None);
        assert_eq!(parse_limit("9223372036854771712\n"), None);
        assert_eq!(parse_limit("0"), None);
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("-1"), None);
    }

    #[test]
    fn limit_files_walk_from_the_own_cgroup_to_the_root() {
        // A container with its own cgroup namespace sees itself at the root.
        assert_eq!(limit_files("0::/\n"), vec!["/memory.max"]);
        assert_eq!(
            limit_files("0::/system.slice/sapflow.service\n"),
            vec![
                "/system.slice/sapflow.service/memory.max",
                "/system.slice/memory.max",
                "/memory.max",
            ]
        );
        assert_eq!(
            limit_files("12:cpu,cpuacct:/x\n4:memory:/docker/abc\n"),
            vec![
                "/memory/docker/abc/memory.limit_in_bytes",
                "/memory/docker/memory.limit_in_bytes",
                "/memory/memory.limit_in_bytes",
            ]
        );
        assert!(limit_files("0::/../etc\n4:cpu:/x\nnonsense\n").is_empty());
        assert!(limit_files("").is_empty());
    }
}
