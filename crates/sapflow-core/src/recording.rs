//! Recording policy: which paths record, where the file goes, when it expires.
//!
//! No filesystem access here; the server does the I/O.

use std::time::Duration;

use crate::StreamPath;

/// Paths that must not be recorded (the looped-videos case).
///
/// Each pattern is a full path (`live/event-loop`) or a prefix ending in `*`
/// (`live/event-loop-*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordPolicy {
    skip: Vec<String>,
}

impl RecordPolicy {
    /// Parses a comma-separated list of patterns. Blank entries are ignored.
    pub fn parse(list: &str) -> Self {
        Self {
            skip: list
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Whether a stream at `path` should be recorded: false when it matches a
    /// skip pattern.
    pub fn records(&self, path: &StreamPath) -> bool {
        let p = path.as_path();
        !self.skip.iter().any(|pat| match pat.strip_suffix('*') {
            Some(prefix) => p.starts_with(prefix),
            None => p == *pat,
        })
    }
}

/// `live/<eventId>/<unix-start>.mp4`, relative to the recordings directory
/// (mediamtx's `recordPath: /recordings/%path/%s`).
pub fn recording_file(path: &StreamPath, unix_start: u64) -> String {
    format!("{}/{unix_start}.mp4", path.as_path())
}

/// A scoped recording retains its real start second even when another codec
/// epoch starts in the same second. The callback identity supplies uniqueness.
pub fn scoped_recording_file(
    path: &StreamPath,
    unix_start: u64,
    recording_id: &str,
) -> Option<String> {
    is_recording_id(recording_id)
        .then(|| format!("{}/{unix_start}-{recording_id}.mp4", path.as_path()))
}

fn is_recording_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Only legacy `<digits>.mp4` and scoped `<digits>-<32 lower hex>.mp4`
/// names belong to expiry. Other files in the directory remain untouched.
pub fn is_recording_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".mp4") else {
        return false;
    };
    let start = match stem.split_once('-') {
        Some((start, id)) if is_recording_id(id) => start,
        Some(_) => return false,
        None => stem,
    };
    !start.is_empty() && start.len() <= 20 && start.bytes().all(|b| b.is_ascii_digit())
}

/// Parses an age like `7d`, `12h`, `30m` or `90s`.
pub fn parse_age(s: &str) -> Option<Duration> {
    let s = s.trim();
    let unit = s.chars().last()?;
    let n = &s[..s.len() - unit.len_utf8()];
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        'd' => n.checked_mul(86_400)?,
        'h' => n.checked_mul(3_600)?,
        'm' => n.checked_mul(60)?,
        's' => n,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> StreamPath {
        StreamPath::parse(p).expect("valid")
    }

    #[test]
    fn records_everything_by_default() {
        assert!(RecordPolicy::default().records(&path("live/event-a")));
        assert!(RecordPolicy::parse(" , ").records(&path("live/event-a")));
    }

    #[test]
    fn skips_exact_paths_and_prefixes() {
        let p = RecordPolicy::parse("live/event-loop, live/event-bg-*");
        assert!(!p.records(&path("live/event-loop")));
        assert!(p.records(&path("live/event-loop2")));
        assert!(!p.records(&path("live/event-bg-1")));
        assert!(p.records(&path("live/event-gig")));
    }

    #[test]
    fn file_names() {
        assert_eq!(
            recording_file(&path("live/event-a"), 1_792_500_000),
            "live/event-a/1792500000.mp4"
        );
        assert!(is_recording_name("1792500000.mp4"));
        for bad in [".mp4", "x.mp4", "1792500000.mp4.part", "../1.mp4", "1.MP4"] {
            assert!(!is_recording_name(bad), "{bad}");
        }
    }

    #[test]
    fn scoped_file_names_keep_the_start_and_require_exact_recording_identity() {
        let p = path("live/event-a");
        let id = "0123456789abcdef0123456789abcdef";
        let file = scoped_recording_file(&p, 1_792_500_000, id).expect("scoped file");
        assert_eq!(
            file,
            "live/event-a/1792500000-0123456789abcdef0123456789abcdef.mp4"
        );
        assert!(is_recording_name(file.rsplit('/').next().expect("name")));
        for bad in [
            "",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
            "../0123456789abcdef0123456789abcdef",
        ] {
            assert_eq!(scoped_recording_file(&p, 1, bad), None, "{bad}");
        }
    }

    #[test]
    fn expiry_refuses_near_matches_of_scoped_names() {
        for bad in [
            "-0123456789abcdef0123456789abcdef.mp4",
            "1-0123456789abcdef0123456789abcde.mp4",
            "1-0123456789ABCDEF0123456789ABCDEF.mp4",
            "1-0123456789abcdef0123456789abcdef-extra.mp4",
            "1-0123456789abcdef0123456789abcdef.mp4.tmp",
            "../1-0123456789abcdef0123456789abcdef.mp4",
            "1-0123456789abcdef0123456789abcdef.MP4",
            "123456789012345678901-0123456789abcdef0123456789abcdef.mp4",
        ] {
            assert!(!is_recording_name(bad), "{bad}");
        }
    }

    #[test]
    fn ages() {
        assert_eq!(parse_age("7d"), Some(Duration::from_secs(7 * 86_400)));
        assert_eq!(parse_age("12h"), Some(Duration::from_secs(43_200)));
        assert_eq!(parse_age("90s"), Some(Duration::from_secs(90)));
        for bad in [
            "",
            "d",
            "0d",
            "7",
            "7w",
            "-1d",
            "99999999999999999999d",
            "7\u{fffd}",
            "\u{fffd}",
        ] {
            assert_eq!(parse_age(bad), None, "{bad}");
        }
    }
}
