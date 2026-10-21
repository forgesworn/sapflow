//! Configuration from environment variables, overridable by flags.
//!
//! No config file: the stage needs a handful of settings.

use std::net::SocketAddr;

use std::path::PathBuf;
use std::time::Duration;

use sapflow_core::Publisher;
use sapflow_core::recording::{RecordPolicy, parse_age};

pub const HELP: &str = "\
sapflow: SRT in, Low-Latency HLS out, for the v4v live stage.

Every setting is an environment variable; a flag of the same name in
lower case with dashes overrides it (SAPFLOW_HTTP_ADDR = --http-addr).

  SAPFLOW_SRT_ADDR         SRT listener (default 0.0.0.0:8890)
  SAPFLOW_SRT_IMPL         SRT implementation: srt-rs (default) or own
                           (sapflow's own receiver, unencrypted callers only)
  SAPFLOW_RTMP_ADDR        RTMP listener (default 0.0.0.0:1935; off disables)
  SAPFLOW_HTTP_ADDR        HLS over HTTP (default 0.0.0.0:8888)
  SAPFLOW_PUBLISH_USER     the one publisher allowed to push
  SAPFLOW_PUBLISH_PASS     its password
  SAPFLOW_OPEN_PUBLISH     set to 1 to accept any publisher (local testing only)
  SAPFLOW_CALLBACK_URL     stream-health callback; {eventId}, {path} and
                           {token} are substituted
  SAPFLOW_CALLBACK_SECRET  the callback token (appended as token= if the URL
                           has no {token})
  SAPFLOW_SEGMENT_COUNT    segments kept in the playlist (default 24, min 7)
  SAPFLOW_RECORDINGS       record every show to this directory (unset: off)
  SAPFLOW_RECORD_SKIP      paths not to record, comma-separated; a trailing *
                           matches a prefix (live/event-loop-*)
  SAPFLOW_RECORD_DELETE_AFTER  delete recordings older than this (default 7d)
  SAPFLOW_RECORD_MIN_FREE_GB   do not record below this much free disk
                               (default 2; 0 turns the check off)
  SAPFLOW_MAX_PUBLISHERS   simultaneous publishers per protocol (default 16)
  SAPFLOW_MAX_PUBLISH_MBPS drop a publisher above this bitrate, averaged over
                           5 s (default 40)
  SAPFLOW_RECONNECT_GRACE  seconds a stream is held after its encoder drops,
                           so a reconnect carries on the same playlist and
                           recording (default 90, as v4v's hold; 0 disables)
  SAPFLOW_HEALTH_INTERVAL  seconds between health callbacks while a stream is
                           live: bitrate, keyframe interval, audio (default 30;
                           0 disables)
  RUST_LOG                 log filter (default info)

Publish with SRT, streamid=publish:live/<eventId>:<user>:<pass>, or RTMP,
rtmp://<host>:1935/live with key <eventId>?user=<user>&pass=<pass>.
Play at http://<host>:8888/live/<eventId>/index.m3u8.
";

/// Every setting, as an environment variable.
const KEYS: &[&str] = &[
    "SAPFLOW_SRT_ADDR",
    "SAPFLOW_HTTP_ADDR",
    "SAPFLOW_PUBLISH_USER",
    "SAPFLOW_PUBLISH_PASS",
    "SAPFLOW_OPEN_PUBLISH",
    "SAPFLOW_CALLBACK_URL",
    "SAPFLOW_CALLBACK_SECRET",
    "SAPFLOW_SEGMENT_COUNT",
    "SAPFLOW_RTMP_ADDR",
    "SAPFLOW_RECORDINGS",
    "SAPFLOW_RECORD_SKIP",
    "SAPFLOW_RECORD_DELETE_AFTER",
    "SAPFLOW_MAX_PUBLISHERS",
    "SAPFLOW_MAX_PUBLISH_MBPS",
    "SAPFLOW_RECORD_MIN_FREE_GB",
    "SAPFLOW_SRT_IMPL",
    "SAPFLOW_RECONNECT_GRACE",
    "SAPFLOW_HEALTH_INTERVAL",
];

/// Which SRT implementation serves the SRT port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtImpl {
    /// `srt-tokio` with the patched `srt-protocol` (the default until our
    /// own is proven).
    SrtRs,
    /// `sapflow-srt`.
    Own,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub srt_addr: SocketAddr,
    pub srt_impl: SrtImpl,
    pub rtmp_addr: Option<SocketAddr>,
    pub http_addr: SocketAddr,
    /// `None` only when open publishing was asked for explicitly.
    pub publisher: Option<Publisher>,
    pub callback_url: Option<String>,
    pub callback_secret: Option<String>,
    pub segment_count: usize,
    pub recordings: Option<PathBuf>,
    pub record_policy: RecordPolicy,
    pub record_delete_after: Duration,
    pub max_publishers: usize,
    pub max_publish_bps: u64,
    /// Bytes of free disk below which recording does not start or stops.
    pub record_min_free: u64,
    /// How long a dropped publisher's stream is held for a reconnect.
    pub reconnect_grace: Duration,
    /// Between health callbacks; zero: none.
    pub health_interval: Duration,
}

#[derive(Debug)]
pub enum ConfigError {
    Help,
    Version,
    Invalid(String),
}

impl Config {
    /// Reads the environment through `get`, then applies `args`.
    pub fn load(
        get: impl Fn(&str) -> Option<String>,
        args: impl IntoIterator<Item = String>,
    ) -> Result<Self, ConfigError> {
        let mut vals: std::collections::HashMap<String, String> = KEYS
            .iter()
            .filter_map(|k| get(k).filter(|v| !v.is_empty()).map(|v| (k.to_string(), v)))
            .collect();

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => return Err(ConfigError::Help),
                "-V" | "--version" => return Err(ConfigError::Version),
                _ => {}
            }
            let Some(name) = arg.strip_prefix("--") else {
                return Err(ConfigError::Invalid(format!("unexpected argument {arg}")));
            };
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (name, None),
            };
            let key = format!("SAPFLOW_{}", name.to_uppercase().replace('-', "_"));
            if !KEYS.contains(&key.as_str()) {
                return Err(ConfigError::Invalid(format!("unknown option --{name}")));
            }
            let value = match inline {
                Some(v) => v,
                None => args
                    .next()
                    .ok_or_else(|| ConfigError::Invalid(format!("--{name} needs a value")))?,
            };
            vals.insert(key, value);
        }

        let addr = |key: &str, default: &str| -> Result<SocketAddr, ConfigError> {
            let v = vals.get(key).map_or(default, String::as_str);
            v.parse()
                .map_err(|_| ConfigError::Invalid(format!("{key}: not an address: {v}")))
        };
        let open = vals.get("SAPFLOW_OPEN_PUBLISH").is_some_and(|v| v == "1");
        let publisher = match (
            vals.get("SAPFLOW_PUBLISH_USER"),
            vals.get("SAPFLOW_PUBLISH_PASS"),
        ) {
            (Some(u), Some(p)) => Some(Publisher::new(u, p)),
            (None, None) if open => None,
            (None, None) => {
                return Err(ConfigError::Invalid(
                    "set SAPFLOW_PUBLISH_USER and SAPFLOW_PUBLISH_PASS (or SAPFLOW_OPEN_PUBLISH=1 for local testing)".into(),
                ));
            }
            _ => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_PUBLISH_USER and SAPFLOW_PUBLISH_PASS go together".into(),
                ));
            }
        };
        let callback_url = vals.get("SAPFLOW_CALLBACK_URL").cloned();
        if let Some(url) = &callback_url
            && !url.starts_with("http://")
        {
            return Err(ConfigError::Invalid(
                "SAPFLOW_CALLBACK_URL must be http:// (the callback stays on the private network)"
                    .into(),
            ));
        }
        let segment_count = match vals.get("SAPFLOW_SEGMENT_COUNT") {
            None => 24,
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| (7..=1000).contains(n))
                .ok_or_else(|| ConfigError::Invalid("SAPFLOW_SEGMENT_COUNT: 7 to 1000".into()))?,
        };

        let rtmp_addr = match vals.get("SAPFLOW_RTMP_ADDR").map(String::as_str) {
            Some("off") => None,
            _ => Some(addr("SAPFLOW_RTMP_ADDR", "0.0.0.0:1935")?),
        };
        let record_delete_after = match vals.get("SAPFLOW_RECORD_DELETE_AFTER") {
            None => Duration::from_secs(7 * 86_400),
            Some(v) => parse_age(v).ok_or_else(|| {
                ConfigError::Invalid("SAPFLOW_RECORD_DELETE_AFTER: like 7d, 12h or 30m".into())
            })?,
        };
        let max_publishers = match vals.get("SAPFLOW_MAX_PUBLISHERS") {
            None => 16,
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=1000).contains(n))
                .ok_or_else(|| ConfigError::Invalid("SAPFLOW_MAX_PUBLISHERS: 1 to 1000".into()))?,
        };

        let max_publish_bps = match vals.get("SAPFLOW_MAX_PUBLISH_MBPS") {
            None => 40_000_000,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=10_000).contains(n))
                .map(|n| n * 1_000_000)
                .ok_or_else(|| {
                    ConfigError::Invalid("SAPFLOW_MAX_PUBLISH_MBPS: 1 to 10000".into())
                })?,
        };

        let record_min_free = match vals.get("SAPFLOW_RECORD_MIN_FREE_GB") {
            None => 2 << 30,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| *n <= 100_000)
                .map(|n| n << 30)
                .ok_or_else(|| {
                    ConfigError::Invalid("SAPFLOW_RECORD_MIN_FREE_GB: 0 to 100000".into())
                })?,
        };

        let secs = |key: &str, default: u64| match vals.get(key) {
            None => Ok(Duration::from_secs(default)),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| *n <= 3600)
                .map(Duration::from_secs)
                .ok_or_else(|| ConfigError::Invalid(format!("{key}: seconds, 0 to 3600"))),
        };
        let reconnect_grace = secs("SAPFLOW_RECONNECT_GRACE", 90)?;
        let health_interval = secs("SAPFLOW_HEALTH_INTERVAL", 30)?;

        let srt_impl = match vals.get("SAPFLOW_SRT_IMPL").map(String::as_str) {
            None | Some("srt-rs") => SrtImpl::SrtRs,
            Some("own") => SrtImpl::Own,
            Some(v) => {
                return Err(ConfigError::Invalid(format!(
                    "SAPFLOW_SRT_IMPL: srt-rs or own, not {v}"
                )));
            }
        };

        Ok(Self {
            reconnect_grace,
            health_interval,
            srt_impl,
            record_min_free,
            max_publish_bps,
            rtmp_addr,
            recordings: vals.get("SAPFLOW_RECORDINGS").map(PathBuf::from),
            record_policy: RecordPolicy::parse(
                vals.get("SAPFLOW_RECORD_SKIP").map_or("", String::as_str),
            ),
            record_delete_after,
            max_publishers,
            srt_addr: addr("SAPFLOW_SRT_ADDR", "0.0.0.0:8890")?,
            http_addr: addr("SAPFLOW_HTTP_ADDR", "0.0.0.0:8888")?,
            publisher,
            callback_url,
            callback_secret: vals.get("SAPFLOW_CALLBACK_SECRET").cloned(),
            segment_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(env: &[(&str, &str)], args: &[&str]) -> Result<Config, ConfigError> {
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::load(
            |k| env.iter().find(|(e, _)| e == k).map(|(_, v)| v.clone()),
            args.iter().map(|s| s.to_string()),
        )
    }

    #[test]
    fn defaults_with_a_publisher() {
        let c = load(
            &[("SAPFLOW_PUBLISH_USER", "u"), ("SAPFLOW_PUBLISH_PASS", "p")],
            &[],
        )
        .expect("config");
        assert_eq!(c.srt_addr.port(), 8890);
        assert_eq!(c.http_addr.port(), 8888);
        assert!(c.publisher.is_some());
        assert_eq!(c.segment_count, 24);
        assert_eq!(c.reconnect_grace, Duration::from_secs(90));
        assert_eq!(c.health_interval, Duration::from_secs(30));
    }

    #[test]
    fn grace_and_health_interval() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let c = load(
            &[open, ("SAPFLOW_RECONNECT_GRACE", "0")],
            &["--health-interval", "10"],
        )
        .expect("config");
        assert_eq!(c.reconnect_grace, Duration::ZERO);
        assert_eq!(c.health_interval, Duration::from_secs(10));
        for bad in ["-1", "1.5", "3601", "soon"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_RECONNECT_GRACE", bad)], &[]),
                Err(ConfigError::Invalid(_))
            ));
        }
    }

    #[test]
    fn refuses_to_start_without_a_publisher_unless_told() {
        assert!(matches!(load(&[], &[]), Err(ConfigError::Invalid(_))));
        assert!(matches!(
            load(&[("SAPFLOW_PUBLISH_USER", "u")], &[]),
            Err(ConfigError::Invalid(_))
        ));
        let c = load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &[]).expect("open");
        assert!(c.publisher.is_none());
    }

    #[test]
    fn flags_override_the_environment() {
        let c = load(
            &[
                ("SAPFLOW_OPEN_PUBLISH", "1"),
                ("SAPFLOW_HTTP_ADDR", "127.0.0.1:1"),
            ],
            &["--http-addr", "127.0.0.1:2", "--srt-addr=127.0.0.1:3"],
        )
        .expect("config");
        assert_eq!(c.http_addr.port(), 2);
        assert_eq!(c.srt_addr.port(), 3);
        assert!(matches!(
            load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &["--nope", "x"]),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(load(&[], &["--help"]), Err(ConfigError::Help)));
        // A prefix of a real setting is not a setting.
        assert!(matches!(
            load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &["--record", "x"]),
            Err(ConfigError::Invalid(m)) if m.contains("unknown")
        ));
    }

    #[test]
    fn recording_and_rtmp_settings() {
        let c = load(
            &[
                ("SAPFLOW_OPEN_PUBLISH", "1"),
                ("SAPFLOW_RECORDINGS", "/recordings"),
                ("SAPFLOW_RECORD_SKIP", "live/event-loop-*"),
                ("SAPFLOW_RECORD_DELETE_AFTER", "2d"),
                ("SAPFLOW_RTMP_ADDR", "off"),
            ],
            &[],
        )
        .expect("config");
        assert_eq!(
            c.recordings.as_deref(),
            Some(std::path::Path::new("/recordings"))
        );
        assert_eq!(c.record_delete_after, Duration::from_secs(2 * 86_400));
        assert!(c.rtmp_addr.is_none());
        let loop_path = sapflow_core::StreamPath::parse("live/event-loop-1").expect("path");
        assert!(!c.record_policy.records(&loop_path));
        let d = load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &[]).expect("config");
        assert_eq!(d.rtmp_addr.map(|a| a.port()), Some(1935));
        assert!(d.recordings.is_none());
        assert!(matches!(
            load(
                &[
                    ("SAPFLOW_OPEN_PUBLISH", "1"),
                    ("SAPFLOW_RECORD_DELETE_AFTER", "soon")
                ],
                &[]
            ),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn srt_implementation_switch() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let c = load(&[open], &[]).expect("config");
        assert_eq!(
            c.srt_impl,
            SrtImpl::SrtRs,
            "srt-rs until ours is the default"
        );
        let c = load(&[open, ("SAPFLOW_SRT_IMPL", "own")], &[]).expect("config");
        assert_eq!(c.srt_impl, SrtImpl::Own);
        let c = load(&[open], &["--srt-impl", "own"]).expect("config");
        assert_eq!(c.srt_impl, SrtImpl::Own);
        let c = load(&[open, ("SAPFLOW_SRT_IMPL", "own")], &["--srt-impl=srt-rs"]).expect("config");
        assert_eq!(c.srt_impl, SrtImpl::SrtRs);
        assert!(matches!(
            load(&[open, ("SAPFLOW_SRT_IMPL", "libsrt")], &[]),
            Err(ConfigError::Invalid(_))
        ));
        assert!(HELP.contains("SAPFLOW_SRT_IMPL"));
    }

    #[test]
    fn callback_must_stay_on_plain_http() {
        let r = load(
            &[
                ("SAPFLOW_OPEN_PUBLISH", "1"),
                ("SAPFLOW_CALLBACK_URL", "https://x/{eventId}"),
            ],
            &[],
        );
        assert!(matches!(r, Err(ConfigError::Invalid(_))));
    }
}
