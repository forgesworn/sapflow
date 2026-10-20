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

/// Whether a file name is one of ours (`<digits>.mp4`), so expiry never
/// touches anything else in the directory.
pub fn is_recording_name(name: &str) -> bool {
    name.strip_suffix(".mp4")
        .is_some_and(|n| !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()))
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
