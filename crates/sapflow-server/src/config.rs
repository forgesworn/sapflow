//! Configuration from environment variables, overridable by flags.
//!
//! No config file: the stage needs a handful of settings.

use std::net::SocketAddr;

use std::path::PathBuf;
use std::time::Duration;

use crate::publish_session_wire::Secret;
use sapflow_core::Publisher;
use sapflow_core::recording::{RecordPolicy, parse_age};
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use std::sync::Arc;

pub const HELP: &str = "\
sapflow: SRT in, Low-Latency HLS out, for the v4v live stage.

Every setting is an environment variable; a flag of the same name in
lower case with dashes overrides it (SAPFLOW_HTTP_ADDR = --http-addr).

  SAPFLOW_SRT_ADDR         SRT listener (default 0.0.0.0:8890)
  SAPFLOW_SRT_PASSPHRASE   require SRT encryption, 10..79 non-NUL bytes
                           (separate from the publish password; unset: plaintext)
  SAPFLOW_SRT_PBKEYLEN     AES key length callers must use: 16 (default) or 32
  SAPFLOW_SRT_CIPHER       compatibility alias: aes128 or aes256; when both key
                           settings are present they must agree
  SAPFLOW_SRT_ENCRYPTION   required (default) or optional: with a passphrase,
                           optional also admits callers that send none
  SAPFLOW_RTMP_ADDR        RTMP listener (default 0.0.0.0:1935; off disables)
  SAPFLOW_HTTP_ADDR        HLS over HTTP (default 0.0.0.0:8888)
  SAPFLOW_CONTROL_ADDR     enable scoped SRT publishing: private loopback control
                           listener, behind an authenticated private TLS proxy
  SAPFLOW_CONTROL_TOKEN    dedicated control bearer (32..256 ASCII token bytes)
  SAPFLOW_SESSION_STORE    absolute metadata file in an existing private directory
                           (all three required; disables global/open publishing)
  SAPFLOW_PUBLISH_USER     the one publisher allowed to push
  SAPFLOW_PUBLISH_PASS     its password
  SAPFLOW_OPEN_PUBLISH     set to 1 to accept any publisher (local testing only)
  SAPFLOW_CALLBACK_URL     stream-health callback; {eventId}, {path} and
                           {token} are substituted
  SAPFLOW_CALLBACK_SECRET  the callback token (appended as token= if the URL
                           has no {token})
  SAPFLOW_SEGMENT_COUNT    segments kept in the playlist (default 24, min 7)
  SAPFLOW_HLS_MODE         low-latency (default) or whole; whole omits parts
  SAPFLOW_HTTP_IMPL        own (default) or hyper: choose the viewer HTTP backend
  SAPFLOW_HTTP_THREADS     own event loop threads (default: one per core, max 256)
                           explicit values: 1..256
  SAPFLOW_RECORDINGS       record every show to this directory (unset: off)
  SAPFLOW_RECORD_SKIP      paths not to record, comma-separated; a trailing *
                           matches a prefix (live/event-loop-*)
  SAPFLOW_RECORD_DELETE_AFTER  delete recordings older than this (default 7d)
  SAPFLOW_RECORD_MIN_FREE_GB   do not record below this much free disk
                               (default 2; 0 turns the check off)
  SAPFLOW_MAX_PUBLISHERS   simultaneous publishers per protocol (default 16)
  SAPFLOW_MAX_PUBLISH_MBPS drop a publisher above this bitrate, averaged over
                           5 s (default 40)
  RUST_LOG                 log filter (default info)
  SAPFLOW_RECONNECT_GRACE  seconds to retain a disconnected stream (default 90;
                           0 disables, maximum 3600)
  SAPFLOW_HEALTH_INTERVAL  seconds between health callbacks (default 30;
                           0 disables, maximum 3600)

Publish with SRT, streamid=publish:live/<eventId>:<user>:<pass>, or RTMP,
rtmp://<host>:1935/live with key <eventId>?user=<user>&pass=<pass>.
Play at http://<host>:8888/live/<eventId>/index.m3u8.
";

/// Every setting, as an environment variable.
const KEYS: &[&str] = &[
    "SAPFLOW_CONTROL_ADDR",
    "SAPFLOW_CONTROL_TOKEN",
    "SAPFLOW_SESSION_STORE",
    "SAPFLOW_SRT_ADDR",
    "SAPFLOW_HTTP_ADDR",
    "SAPFLOW_PUBLISH_USER",
    "SAPFLOW_PUBLISH_PASS",
    "SAPFLOW_OPEN_PUBLISH",
    "SAPFLOW_CALLBACK_URL",
    "SAPFLOW_CALLBACK_SECRET",
    "SAPFLOW_SEGMENT_COUNT",
    "SAPFLOW_HLS_MODE",
    "SAPFLOW_HTTP_IMPL",
    "SAPFLOW_HTTP_THREADS",
    "SAPFLOW_RTMP_ADDR",
    "SAPFLOW_RECORDINGS",
    "SAPFLOW_RECORD_SKIP",
    "SAPFLOW_RECORD_DELETE_AFTER",
    "SAPFLOW_MAX_PUBLISHERS",
    "SAPFLOW_MAX_PUBLISH_MBPS",
    "SAPFLOW_RECORD_MIN_FREE_GB",
    "SAPFLOW_SRT_IMPL",
    "SAPFLOW_SRT_PASSPHRASE",
    "SAPFLOW_SRT_CIPHER",
    "SAPFLOW_SRT_PBKEYLEN",
    "SAPFLOW_SRT_ENCRYPTION",
    "SAPFLOW_RECONNECT_GRACE",
    "SAPFLOW_HEALTH_INTERVAL",
];

#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub addr: SocketAddr,
    pub token: Arc<Secret>,
    pub store: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub control: Option<ControlConfig>,
    pub srt_addr: SocketAddr,
    /// Independent transport secret. Shared zeroising ownership, redacted Debug.
    pub srt_crypto: Option<CryptoConfig>,
    /// With `srt_crypto`, also admit SRT callers that do not encrypt.
    pub srt_allow_plaintext: bool,
    pub rtmp_addr: Option<SocketAddr>,
    pub http_addr: SocketAddr,
    /// `None` only when open publishing was asked for explicitly.
    pub publisher: Option<Publisher>,
    pub callback_url: Option<String>,
    pub callback_secret: Option<String>,
    pub segment_count: usize,
    pub low_latency: bool,
    /// Threads for the own event loop (`own_http.rs`); `None` serves viewers
    /// with hyper on tokio.
    pub own_http_threads: Option<usize>,
    pub recordings: Option<PathBuf>,
    pub record_policy: RecordPolicy,
    pub record_delete_after: Duration,
    pub max_publishers: usize,
    pub max_publish_bps: u64,
    /// Bytes of free disk below which recording does not start or stops.
    pub record_min_free: u64,
    pub reconnect_grace: Duration,
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
        // Move the owned environment value directly into zeroising storage,
        // including invalid values. Never place it in the generic String map.
        // Retain a redacted error until overrides have been applied.
        let secret = |value: String| CryptoConfig::new(value.into_bytes(), KeySize::Aes128);
        let mut srt_crypto = get("SAPFLOW_SRT_PASSPHRASE").map(secret);
        let mut control_token = get("SAPFLOW_CONTROL_TOKEN").map(Secret::new);
        let mut vals: std::collections::HashMap<String, String> = KEYS
            .iter()
            .filter(|k| !matches!(**k, "SAPFLOW_SRT_PASSPHRASE" | "SAPFLOW_CONTROL_TOKEN"))
            .filter_map(|k| {
                get(k)
                    .filter(|v| {
                        !v.is_empty() || matches!(*k, "SAPFLOW_SRT_CIPHER" | "SAPFLOW_SRT_PBKEYLEN")
                    })
                    .map(|v| (k.to_string(), v))
            })
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
            if key == "SAPFLOW_SRT_PASSPHRASE" {
                srt_crypto = Some(secret(value));
            } else if key == "SAPFLOW_CONTROL_TOKEN" {
                control_token = Some(Secret::new(value));
            } else {
                vals.insert(key, value);
            }
        }

        let addr = |key: &str, default: &str| -> Result<SocketAddr, ConfigError> {
            let v = vals.get(key).map_or(default, String::as_str);
            v.parse()
                .map_err(|_| ConfigError::Invalid(format!("{key}: not an address: {v}")))
        };
        let control = match (vals.get("SAPFLOW_CONTROL_ADDR"), control_token, vals.get("SAPFLOW_SESSION_STORE")) {
            (None, None, None) => None,
            (Some(address), Some(token), Some(store)) => {
                let addr: SocketAddr = address.parse().map_err(|_| ConfigError::Invalid("SAPFLOW_CONTROL_ADDR: numeric loopback address required".into()))?;
                let value = token.expose();
                if !addr.ip().is_loopback() || !(32..=256).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-'|b'_'|b'.'|b'~')) {
                    return Err(ConfigError::Invalid("control listener requires loopback and a 32..256-byte token".into()));
                }
                let store = PathBuf::from(store);
                if !store.is_absolute() || store.file_name().is_none() { return Err(ConfigError::Invalid("SAPFLOW_SESSION_STORE: absolute file path required".into())); }
                Some(ControlConfig { addr, token: Arc::new(token), store })
            }
            _ => return Err(ConfigError::Invalid("SAPFLOW_CONTROL_ADDR, SAPFLOW_CONTROL_TOKEN and SAPFLOW_SESSION_STORE must be configured together".into())),
        };
        let open = vals.get("SAPFLOW_OPEN_PUBLISH").is_some_and(|v| v == "1");
        if control.is_some()
            && (open
                || vals.contains_key("SAPFLOW_PUBLISH_USER")
                || vals.contains_key("SAPFLOW_PUBLISH_PASS")
                || srt_crypto.is_some()
                || vals.contains_key("SAPFLOW_SRT_CIPHER")
                || vals.contains_key("SAPFLOW_SRT_PBKEYLEN")
                || vals.contains_key("SAPFLOW_SRT_ENCRYPTION")
                || vals.get("SAPFLOW_RTMP_ADDR").is_some_and(|v| v != "off"))
        {
            return Err(ConfigError::Invalid("scoped control mode requires per-session encrypted SRT; global/open publishing, global SRT keys and RTMP are disabled".into()));
        }
        let publisher = match (
            vals.get("SAPFLOW_PUBLISH_USER"),
            vals.get("SAPFLOW_PUBLISH_PASS"),
        ) {
            (Some(u), Some(p)) => Some(Publisher::new(u, p)),
            (None, None) if open || control.is_some() => None,
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
            _ if control.is_some() => None,
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

        let seconds = |key: &str, default| -> Result<Duration, ConfigError> {
            match vals.get(key) {
                None => Ok(Duration::from_secs(default)),
                Some(value) => value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n <= 3600)
                    .map(Duration::from_secs)
                    .ok_or_else(|| ConfigError::Invalid(format!("{key}: seconds, 0 to 3600"))),
            }
        };
        let reconnect_grace = seconds("SAPFLOW_RECONNECT_GRACE", 90)?;
        let health_interval = seconds("SAPFLOW_HEALTH_INTERVAL", 30)?;
        // Keep recognising the removed implementation switch so obsolete
        // deployments fail explicitly rather than silently changing transport.
        match vals.get("SAPFLOW_SRT_IMPL").map(String::as_str) {
            None | Some("own") => {}
            Some(v) => {
                return Err(ConfigError::Invalid(format!(
                    "SAPFLOW_SRT_IMPL={v}: srt-rs was removed; unset SAPFLOW_SRT_IMPL"
                )));
            }
        }
        let key_size = match vals.get("SAPFLOW_SRT_PBKEYLEN").map(String::as_str) {
            None => None,
            Some("16") => Some(KeySize::Aes128),
            Some("32") => Some(KeySize::Aes256),
            Some(_) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_SRT_PBKEYLEN: 16 or 32".into(),
                ));
            }
        };
        let cipher_alias = match vals.get("SAPFLOW_SRT_CIPHER").map(String::as_str) {
            None => None,
            Some("aes128") => Some(KeySize::Aes128),
            Some("aes256") => Some(KeySize::Aes256),
            Some(_) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_SRT_CIPHER: aes128 or aes256".into(),
                ));
            }
        };
        if cipher_alias.is_some() && srt_crypto.is_none() {
            return Err(ConfigError::Invalid(
                "SAPFLOW_SRT_CIPHER requires SAPFLOW_SRT_PASSPHRASE".into(),
            ));
        }
        if let (Some(key_size), Some(alias)) = (key_size, cipher_alias)
            && key_size != alias
        {
            return Err(ConfigError::Invalid(
                "SAPFLOW_SRT_PBKEYLEN and SAPFLOW_SRT_CIPHER disagree".into(),
            ));
        }
        let cipher = key_size.or(cipher_alias).unwrap_or(KeySize::Aes128);
        let srt_crypto = srt_crypto
            .transpose()
            .map_err(|_| {
                ConfigError::Invalid("SAPFLOW_SRT_PASSPHRASE: 10..79 non-NUL bytes required".into())
            })?
            .map(|mut config| {
                config.key_size = cipher;
                config
            });

        let srt_allow_plaintext = match vals.get("SAPFLOW_SRT_ENCRYPTION").map(String::as_str) {
            None | Some("required") => false,
            Some("optional") if srt_crypto.is_some() => true,
            Some("optional") => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_SRT_ENCRYPTION=optional requires SAPFLOW_SRT_PASSPHRASE".into(),
                ));
            }
            Some(_) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_SRT_ENCRYPTION: required or optional".into(),
                ));
            }
        };

        let own_http_threads = match vals.get("SAPFLOW_HTTP_IMPL").map(String::as_str) {
            Some("hyper") => None,
            None | Some("own") => Some(match vals.get("SAPFLOW_HTTP_THREADS") {
                None => std::thread::available_parallelism().map_or(4, |n| n.get().min(256)),
                Some(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=256).contains(n))
                    .ok_or_else(|| ConfigError::Invalid("SAPFLOW_HTTP_THREADS: 1 to 256".into()))?,
            }),
            Some(_) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_HTTP_IMPL: hyper or own".into(),
                ));
            }
        };

        Ok(Self {
            own_http_threads,
            low_latency: match vals.get("SAPFLOW_HLS_MODE").map(String::as_str) {
                None | Some("low-latency") => true,
                Some("whole") => false,
                Some(_) => {
                    return Err(ConfigError::Invalid(
                        "SAPFLOW_HLS_MODE: low-latency or whole".into(),
                    ));
                }
            },
            control,
            reconnect_grace,
            health_interval,
            srt_crypto,
            srt_allow_plaintext,
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

    #[test]
    fn http_impl_defaults_to_own_and_hyper_is_explicit() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert!(
            load(&[open], &[])
                .expect("default")
                .own_http_threads
                .is_some_and(|n| (1..=256).contains(&n))
        );
        assert_eq!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "hyper")], &[])
                .expect("hyper")
                .own_http_threads,
            None
        );
        assert!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "own")], &[])
                .expect("own")
                .own_http_threads
                .is_some_and(|n| n >= 1)
        );
        assert_eq!(
            load(
                &[
                    open,
                    ("SAPFLOW_HTTP_IMPL", "own"),
                    ("SAPFLOW_HTTP_THREADS", "3")
                ],
                &[]
            )
            .expect("own, 3 threads")
            .own_http_threads,
            Some(3)
        );
        assert_eq!(
            load(&[open, ("SAPFLOW_HTTP_THREADS", "3")], &[])
                .expect("default backend, explicit thread count")
                .own_http_threads,
            Some(3)
        );
        assert_eq!(
            load(
                &[open, ("SAPFLOW_HTTP_IMPL", "own")],
                &["--http-impl=hyper"]
            )
            .expect("explicit reference backend overrides environment")
            .own_http_threads,
            None
        );
        assert!(load(&[open, ("SAPFLOW_HTTP_IMPL", "tokio")], &[]).is_err());
        // Empty means unset, as for every setting.
        assert!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "")], &[])
                .expect("empty")
                .own_http_threads
                .is_some_and(|n| (1..=256).contains(&n))
        );
        for threads in ["0", "257", "many"] {
            assert!(load(&[open, ("SAPFLOW_HTTP_THREADS", threads)], &[]).is_err());
            assert!(
                load(
                    &[
                        open,
                        ("SAPFLOW_HTTP_IMPL", "own"),
                        ("SAPFLOW_HTTP_THREADS", threads)
                    ],
                    &[]
                )
                .is_err()
            );
        }
    }

    #[test]
    fn hls_delivery_mode_is_explicit_and_defaults_to_low_latency() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert!(load(&[open], &[]).expect("default config").low_latency);
        assert!(
            !load(&[open, ("SAPFLOW_HLS_MODE", "whole")], &[])
                .expect("whole config")
                .low_latency
        );
        assert!(load(&[open, ("SAPFLOW_HLS_MODE", "invalid")], &[]).is_err());
        assert!(
            !load(&[open], &["--hls-mode", "whole"])
                .expect("flag override")
                .low_latency
        );
    }

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
    }

    #[test]
    fn grace_and_health_settings_are_bounded_and_do_not_change_crypto() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let config = load(&[open], &[]).expect("valid bounded settings");
        assert_eq!(config.reconnect_grace, Duration::from_secs(90));
        assert_eq!(config.health_interval, Duration::from_secs(30));
        let config = load(&[open], &["--reconnect-grace=0", "--health-interval=10"])
            .expect("valid bounded settings");
        assert_eq!(config.reconnect_grace, Duration::ZERO);
        assert_eq!(config.health_interval, Duration::from_secs(10));
        for invalid in ["-1", "3601", "1.5"] {
            assert!(load(&[open, ("SAPFLOW_RECONNECT_GRACE", invalid)], &[]).is_err());
            assert!(load(&[open, ("SAPFLOW_HEALTH_INTERVAL", invalid)], &[]).is_err());
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
    fn srt_rs_is_refused_by_name() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert!(load(&[open, ("SAPFLOW_SRT_IMPL", "own")], &[]).is_ok());
        assert!(load(&[open], &["--srt-impl", "own"]).is_ok());
        for v in ["srt-rs", "libsrt"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_SRT_IMPL", v)], &[]),
                Err(ConfigError::Invalid(m)) if m.contains("removed")
            ));
        }
        assert!(!HELP.contains("SAPFLOW_SRT_IMPL"));
    }

    #[test]
    fn srt_passphrase() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let own = ("SAPFLOW_SRT_IMPL", "own");
        assert!(load(&[open], &[]).expect("config").srt_crypto.is_none());
        let c = load(
            &[open, own, ("SAPFLOW_SRT_PASSPHRASE", "correct horse")],
            &[],
        )
        .expect("config");
        let crypto = c.srt_crypto.expect("crypto");
        assert_eq!(crypto.key_size, KeySize::Aes128);
        // Never printed.
        assert!(!format!("{crypto:?}").contains("horse"));
        let c = load(
            &[
                open,
                own,
                ("SAPFLOW_SRT_PASSPHRASE", "correct horse"),
                ("SAPFLOW_SRT_PBKEYLEN", "32"),
            ],
            &[],
        )
        .expect("config");
        assert_eq!(c.srt_crypto.expect("crypto").key_size, KeySize::Aes256);
        for bad in [
            vec![open, own, ("SAPFLOW_SRT_PASSPHRASE", "short")],
            vec![
                open,
                own,
                ("SAPFLOW_SRT_PASSPHRASE", "correct horse"),
                ("SAPFLOW_SRT_PBKEYLEN", "24"),
            ],
        ] {
            assert!(matches!(load(&bad, &[]), Err(ConfigError::Invalid(_))));
        }
    }

    #[test]
    fn srt_encryption_can_be_optional_only_with_a_passphrase() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let pass = ("SAPFLOW_SRT_PASSPHRASE", "correct horse");
        assert!(
            !load(&[open, pass], &[])
                .expect("config")
                .srt_allow_plaintext
        );
        let c = load(&[open, pass, ("SAPFLOW_SRT_ENCRYPTION", "optional")], &[]).expect("config");
        assert!(c.srt_allow_plaintext && c.srt_crypto.is_some());
        assert!(
            !load(&[open, pass], &["--srt-encryption=required"])
                .expect("config")
                .srt_allow_plaintext
        );
        for bad in [
            vec![open, ("SAPFLOW_SRT_ENCRYPTION", "optional")],
            vec![open, pass, ("SAPFLOW_SRT_ENCRYPTION", "maybe")],
        ] {
            assert!(matches!(load(&bad, &[]), Err(ConfigError::Invalid(_))));
        }
    }

    #[test]
    fn srt_key_settings_must_agree_and_never_ignore_invalid_values() {
        let base = [
            ("SAPFLOW_OPEN_PUBLISH", "1"),
            ("SAPFLOW_SRT_PASSPHRASE", "transport credential"),
        ];
        // PBKEYLEN preserves main's configuration semantics: selecting a key
        // size alone never promises encryption or requires a passphrase.
        assert!(
            load(
                &[
                    ("SAPFLOW_OPEN_PUBLISH", "1"),
                    ("SAPFLOW_SRT_PBKEYLEN", "32"),
                ],
                &[],
            )
            .expect("key size without secret")
            .srt_crypto
            .is_none()
        );
        for (length, alias, expected) in [
            ("16", "aes128", KeySize::Aes128),
            ("32", "aes256", KeySize::Aes256),
        ] {
            let mut env = base.to_vec();
            env.extend([
                ("SAPFLOW_SRT_PBKEYLEN", length),
                ("SAPFLOW_SRT_CIPHER", alias),
            ]);
            assert_eq!(
                load(&env, &[])
                    .expect("matching key settings")
                    .srt_crypto
                    .expect("crypto")
                    .key_size,
                expected
            );
        }
        for (length, alias) in [("16", "aes256"), ("32", "aes128")] {
            let mut env = base.to_vec();
            env.extend([
                ("SAPFLOW_SRT_PBKEYLEN", length),
                ("SAPFLOW_SRT_CIPHER", alias),
            ]);
            assert!(
                matches!(load(&env, &[]), Err(ConfigError::Invalid(m)) if m.contains("disagree"))
            );
        }
        for invalid in ["", "24", "aes256"] {
            let mut env = base.to_vec();
            env.push(("SAPFLOW_SRT_PBKEYLEN", invalid));
            assert!(matches!(load(&env, &[]), Err(ConfigError::Invalid(_))));
        }
        // Existing native pilot recipes used the named cipher before main
        // introduced PBKEYLEN; both explicit aliases retain their meaning.
        for (alias, expected) in [("aes128", KeySize::Aes128), ("aes256", KeySize::Aes256)] {
            assert_eq!(
                load(&base, &[&format!("--srt-cipher={alias}")])
                    .expect("cipher alias")
                    .srt_crypto
                    .expect("crypto")
                    .key_size,
                expected
            );
        }
    }

    #[test]
    fn own_srt_encryption_is_explicit_separate_and_redacted() {
        let base = [
            ("SAPFLOW_PUBLISH_USER", "publisher"),
            ("SAPFLOW_PUBLISH_PASS", "publish credential"),
            ("SAPFLOW_SRT_IMPL", "own"),
            ("SAPFLOW_SRT_PASSPHRASE", "transport credential"),
            ("SAPFLOW_SRT_PBKEYLEN", "32"),
        ];
        let config = load(&base, &[]).expect("encrypted own");
        assert_eq!(
            config.srt_crypto.as_ref().expect("crypto").key_size,
            KeySize::Aes256
        );
        assert!(config.publisher.is_some());
        let publish = |password| {
            sapflow_core::PublishRequest::parse_srt_stream_id(&format!(
                "publish:live/event-test:publisher:{password}"
            ))
            .expect("request")
            .authorise(config.publisher.as_ref().expect("publisher"))
        };
        assert!(publish("publish credential").is_ok());
        assert!(publish("transport credential").is_err());
        let debug = format!("{config:?}");
        assert!(!debug.contains("transport credential"));
        assert!(!debug.contains("publish credential"));
        let config = load(
            &base,
            &[
                "--srt-pbkeylen=16",
                "--srt-cipher=aes128",
                "--srt-passphrase",
                "replacement secret",
            ],
        )
        .expect("override");
        assert_eq!(config.srt_crypto.expect("crypto").key_size, KeySize::Aes128);
        assert!(
            load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &[])
                .expect("default")
                .srt_crypto
                .is_none()
        );
    }

    #[test]
    fn own_srt_encryption_rejects_invalid_and_ignored_configuration() {
        let base = [("SAPFLOW_OPEN_PUBLISH", "1"), ("SAPFLOW_SRT_IMPL", "own")];
        for secret in [
            "".to_owned(),
            "short".into(),
            "x".repeat(80),
            "has\0a nul byte".into(),
        ] {
            let mut env = base.to_vec();
            env.push(("SAPFLOW_SRT_PASSPHRASE", &secret));
            let Err(ConfigError::Invalid(error)) = load(&env, &[]) else {
                panic!("bad secret accepted")
            };
            assert_eq!(
                error,
                "SAPFLOW_SRT_PASSPHRASE: 10..79 non-NUL bytes required"
            );
        }
        for n in [10, 79] {
            let secret = "x".repeat(n);
            let mut env = base.to_vec();
            env.push(("SAPFLOW_SRT_PASSPHRASE", &secret));
            assert!(load(&env, &[]).is_ok());
        }
        for cipher in ["", "aes192", "aes256-gcm", "off"] {
            let mut env = base.to_vec();
            env.extend([
                ("SAPFLOW_SRT_PASSPHRASE", "transport credential"),
                ("SAPFLOW_SRT_CIPHER", cipher),
            ]);
            assert!(matches!(load(&env, &[]), Err(ConfigError::Invalid(_))));
        }
        assert!(matches!(
            load(&base, &["--srt-cipher=aes128"]),
            Err(ConfigError::Invalid(_))
        ));
        assert!(matches!(
            load(&base, &["--srt-passphrase="]),
            Err(ConfigError::Invalid(_))
        ));
        // Explicitly switching implementations must not silently drop crypto.
        assert!(matches!(
            load(
                &base,
                &["--srt-passphrase=transport credential", "--srt-impl=srt-rs"]
            ),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn scoped_mode_is_explicit_private_and_cannot_fall_back_to_global_credentials() {
        let base = [
            ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:9000"),
            (
                "SAPFLOW_CONTROL_TOKEN",
                "independent-control-token-32-bytes-minimum",
            ),
            ("SAPFLOW_SESSION_STORE", "/private/session-authority.json"),
        ];
        let config = load(&base, &[]).expect("scoped config");
        assert!(config.control.is_some());
        assert!(config.publisher.is_none());
        assert!(config.rtmp_addr.is_none());
        assert!(config.srt_crypto.is_none());
        assert!(!format!("{config:?}").contains("independent-control-token"));
        for (key, value) in [
            ("SAPFLOW_OPEN_PUBLISH", "1"),
            ("SAPFLOW_PUBLISH_USER", "publisher"),
            ("SAPFLOW_PUBLISH_PASS", "password"),
            ("SAPFLOW_SRT_PASSPHRASE", "transport-secret"),
            ("SAPFLOW_SRT_PBKEYLEN", "32"),
            ("SAPFLOW_SRT_ENCRYPTION", "optional"),
            ("SAPFLOW_RTMP_ADDR", "127.0.0.1:1935"),
        ] {
            let mut values = base.to_vec();
            values.push((key, value));
            assert!(load(&values, &[]).is_err(), "{key}");
        }
        for values in [
            vec![base[0], base[1]],
            vec![base[1], base[2]],
            vec![base[0], base[2]],
        ] {
            assert!(load(&values, &[]).is_err());
        }
        for args in [
            vec!["--control-addr=0.0.0.0:9000"],
            vec!["--control-token="],
            vec!["--control-token=short"],
            vec!["--session-store=relative.json"],
        ] {
            assert!(load(&base, &args).is_err());
        }
        assert!(load(&base, &["--control-addr=[::1]:9000", "--rtmp-addr=off"]).is_ok());
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
