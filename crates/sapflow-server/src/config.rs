//! Configuration from environment variables, overridable by flags.
//!
//! No config file: the stage needs a handful of settings.

use std::net::{IpAddr, SocketAddr};

use std::path::PathBuf;
use std::time::Duration;

use crate::http_lite::Endpoint;
use crate::publish_session_wire::Secret;
use sapflow_core::Publisher;
use sapflow_core::recording::{RecordPolicy, parse_age};
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use sapflow_whip::Transport;
use sapflow_whip::resource::PublicUrl;
use sapflow_whip::sdp::Candidate;
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
                           optional also admits callers that send none. Set
                           to required with no passphrase only with
                           SAPFLOW_SRT_APP_KEYS=1: callers must then encrypt
  SAPFLOW_SRT_APP_KEYS     set to 1 to let the app give each SRT stream its own
                           passphrase (needs SAPFLOW_PUBLISH_AUTH_URL; not with
                           scoped mode): requests offer \"srtKey\":\"accepted\",
                           and a 2xx carrying sapflow-srt-passphrase admits
                           only a caller keyed with it, checked before the
                           path is claimed. A caller's key material belongs
                           to the first to connect with it; anyone else
                           sending it (a replayed handshake) is refused.
                           Needs SAPFLOW_SRT_REPLAY_STORE
  SAPFLOW_SRT_REPLAY_STORE absolute file in an existing private (0700)
                           directory on a persistent volume, required with
                           SAPFLOW_SRT_APP_KEYS=1: digests of admitted
                           callers' key material, kept across restarts so a
                           recorded handshake and its media cannot be
                           replayed after one. In scoped mode it defaults
                           to srt-replay beside SAPFLOW_SESSION_STORE.
                           Unreadable or malformed at start refuses to start
  SAPFLOW_SRT_REORDER_WAIT_MAX_MS  once a caller's path reorders packets, hold
                           a new gap's first NAK up to this long, so a packet
                           that is only late is not resent (default 50, 0 to
                           1000; never over an eighth of the latency; 0 NAKs
                           every gap at once, as libsrt does)
  SAPFLOW_RTMP_ADDR        RTMP listener (default 0.0.0.0:1935; off disables)
  SAPFLOW_RTMP_PROXY_ADDR  a second RTMP listener, for a TLS front that sends
                           PROXY protocol v2 (RTMPS through Caddy layer4 or
                           HAProxy): each connection must come from
                           SAPFLOW_RTMP_TRUSTED_PROXY and start with the header,
                           whose source is the publisher's address (default off)
  SAPFLOW_RTMP_TRUSTED_PROXY  the front's address as this server sees it, the
                           only peer SAPFLOW_RTMP_PROXY_ADDR accepts; keep that
                           port reachable from the front only (under Docker,
                           publish it on 127.0.0.1)
  SAPFLOW_HTTP_ADDR        HLS over HTTP (default 0.0.0.0:8888)
  SAPFLOW_CONTROL_ADDR     enable scoped SRT publishing: private loopback control
                           listener, behind an authenticated private TLS proxy
  SAPFLOW_CONTROL_TOKEN    dedicated control bearer (32..256 ASCII token bytes)
  SAPFLOW_SESSION_STORE    absolute metadata file in an existing private directory
                           (all three required; disables global/open publishing)
  Outside scoped mode, choose exactly one way to authorise publishers:
  SAPFLOW_PUBLISH_USER     the one publisher allowed to push
  SAPFLOW_PUBLISH_PASS     its password
  SAPFLOW_PUBLISH_AUTH_URL ask the app about each publisher instead: an http://
                           URL (no TLS; keep it on the private network) that
                           gets mediamtx's HTTP auth request; 2xx admits
  SAPFLOW_OPEN_PUBLISH     set to 1 to accept any publisher (local testing only)
  SAPFLOW_CALLBACK_URL     stream-health callback; {eventId}, {path} and
                           {token} are substituted
  SAPFLOW_CALLBACK_SECRET  the callback token (appended as token= if the URL
                           has no {token})
  SAPFLOW_SEGMENT_COUNT    segments the playlist keeps (default 24, 7 to 1000);
                           more while that many would last under three target
                           durations: up to 256 entries, with at most 64 MiB of
                           media past the count
  SAPFLOW_WINDOW_SECONDS   the most media the playlist keeps, beside the
                           segment count (default 2 s per segment, 48 at the
                           default count; 0 for no limit): a long keyframe
                           interval makes long segments, and the count alone
                           would hold minutes of them. Never under seven
                           entries or three target durations
  SAPFLOW_HLS_MODE         low-latency (default) or whole; whole omits parts
  SAPFLOW_HTTP_THREADS     viewer event loop threads (default: one per core, max 256)
                           explicit values: 1..256
  SAPFLOW_HTTP_BODY_TIMEOUT absolute ready-response transfer seconds, including
                           headers (default 120, 1..3600); excludes parked waits
  SAPFLOW_RECORDINGS       record every show to this directory (unset: off)
  SAPFLOW_RECORD_SKIP      paths not to record, comma-separated; a trailing *
                           matches a prefix (live/event-loop-*)
  SAPFLOW_RECORD_DELETE_AFTER  delete recordings older than this (default 7d)
  SAPFLOW_RECORD_MIN_FREE_GB   do not record below this much free disk
                               (default 2; 0 turns the check off)
  SAPFLOW_RECORDING_ROLL_BURST  new recording files a stream may start at
                           codec changes before it must wait (default 8,
                           0 to 1000; 0 turns the limit off). Past it, codec
                           epochs are still served but not recorded, and the
                           files on either side are marked epoch-rate
  SAPFLOW_RECORDING_ROLL_REFILL  seconds per file regained, passed in both
                           media and wall time (default 60, 1 to 3600)
  SAPFLOW_RECORDING_ROLL_SETTLE  seconds a codec epoch must last before
                           recording resumes in it (default 10, 0 to 3600)
  SAPFLOW_MAX_PUBLISHERS   simultaneous publishers per protocol (default 16)
  SAPFLOW_MAX_PRESENTATIONS resident HLS presentations, including reconnect hold
                           and removed streams with owners (default 4x publishers;
                           64 at default, explicit values 1..4096)
  SAPFLOW_MAX_FRAGMENT_BYTES shared encoded init/fragment backing capacity
                           (default 536870912, 512 MiB; positive byte count);
                           a new stream is refused once the streams' expected
                           windows would fill 80% of it, or while eight are
                           starting (a reconnect to a held one is not)
  SAPFLOW_MAX_RETAINED_BYTES media kept past the live window so parts and
                           segments stay fetchable (RFC 8216bis 6.2.2),
                           charged apart from the fragment bytes and never
                           refusing live media (default 134217728, 128 MiB;
                           positive byte count); fragment memory is
                           SAPFLOW_MAX_FRAGMENT_BYTES plus this
  SAPFLOW_MAX_PUBLISH_MBPS drop a publisher above this bitrate, averaged over
                           5 s (default 40)
  RUST_LOG                 log filter (default info)
  SAPFLOW_RECONNECT_GRACE  seconds to retain a disconnected stream (default 90;
                           0 disables, maximum 3600)
  SAPFLOW_HEALTH_INTERVAL  seconds between health callbacks (default 30;
                           0 disables, maximum 3600)

WHIP (browser and OBS publishing over WebRTC) is off unless SAPFLOW_WHIP_ADDR
is set. It should not face the internet until the qualification gates in
docs/whip-ingest.md (W4) pass. With it set, the next three are required:
  SAPFLOW_WHIP_ADDR        WHIP signalling over plain HTTP, behind a TLS proxy
  SAPFLOW_WHIP_PUBLIC_URL  the public base URL of that listener, for Location
  SAPFLOW_WHIP_UDP_ADDR    the media UDP socket every session shares
  SAPFLOW_WHIP_CANDIDATES  advertised candidates, comma-separated: udp <ip>:<port>,
                           and tcp <ip>:<port> (passive ICE-TCP) with the next
  SAPFLOW_WHIP_TCP_ADDR    the ICE-TCP listener (RFC 4571 frames), for a layer-4
                           front proxy on TCP 443 when UDP is blocked
  SAPFLOW_WHIP_TCP_PROXY_PROTOCOL  v2: every ICE-TCP connection starts with a
                           PROXY protocol v2 header from SAPFLOW_WHIP_TRUSTED_PROXY,
                           whose source is the publisher; other peers are dropped
  SAPFLOW_WHIP_ALLOW_ORIGINS  browser origins allowed by CORS, comma-separated
  SAPFLOW_WHIP_TRUSTED_PROXY  the proxy address whose X-Forwarded-For (and, with
                           v2 above, PROXY header) is believed
  SAPFLOW_WHIP_KEYFRAME_INTERVAL  seconds between keyframe requests (default 2;
                           0 asks only when needed, maximum 60)
  SAPFLOW_WHIP_OPUS_MAX_BITRATE   Opus maxaveragebitrate offered (default 128000;
                           6000 to 510000)
  SAPFLOW_WHIP_STALE       seconds without media after which a new POST takes a
                           WHIP session's path over (default 2, 1 to 60)

Publish with SRT, streamid=publish:live/<eventId>:<user>:<pass>, or RTMP,
rtmp://<host>:1935/live with key <eventId>?user=<user>&pass=<pass>, or WHIP,
POST <SAPFLOW_WHIP_PUBLIC_URL>/whip/live/<eventId> with bearer <user>:<pass>.
Play at http://<host>:8888/live/<eventId>/index.m3u8.
";

/// Whether two stores in one directory would use the same file: each keeps
/// `<name>`, `<name>.lock` and `.<name>.tmp-*` beside it.
fn stores_share_files(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a.parent() != b.parent() {
        return false;
    }
    let (Some(a), Some(b)) = (
        a.file_name().and_then(|n| n.to_str()),
        b.file_name().and_then(|n| n.to_str()),
    ) else {
        return a == b;
    };
    let one_way = |x: &str, y: &str| {
        x == y || x == format!("{y}.lock") || x.starts_with(&format!(".{y}.tmp-"))
    };
    one_way(a, b) || one_way(b, a)
}

/// Every setting, as an environment variable.
const KEYS: &[&str] = &[
    "SAPFLOW_CONTROL_ADDR",
    "SAPFLOW_CONTROL_TOKEN",
    "SAPFLOW_SESSION_STORE",
    "SAPFLOW_SRT_ADDR",
    "SAPFLOW_HTTP_ADDR",
    "SAPFLOW_PUBLISH_USER",
    "SAPFLOW_PUBLISH_PASS",
    "SAPFLOW_PUBLISH_AUTH_URL",
    "SAPFLOW_OPEN_PUBLISH",
    "SAPFLOW_CALLBACK_URL",
    "SAPFLOW_CALLBACK_SECRET",
    "SAPFLOW_SEGMENT_COUNT",
    "SAPFLOW_WINDOW_SECONDS",
    "SAPFLOW_HLS_MODE",
    "SAPFLOW_HTTP_IMPL",
    "SAPFLOW_HTTP_THREADS",
    "SAPFLOW_HTTP_BODY_TIMEOUT",
    "SAPFLOW_RTMP_ADDR",
    "SAPFLOW_RTMP_PROXY_ADDR",
    "SAPFLOW_RTMP_TRUSTED_PROXY",
    "SAPFLOW_RECORDINGS",
    "SAPFLOW_RECORD_SKIP",
    "SAPFLOW_RECORD_DELETE_AFTER",
    "SAPFLOW_MAX_PUBLISHERS",
    "SAPFLOW_MAX_PRESENTATIONS",
    "SAPFLOW_MAX_FRAGMENT_BYTES",
    "SAPFLOW_MAX_RETAINED_BYTES",
    "SAPFLOW_MAX_PUBLISH_MBPS",
    "SAPFLOW_RECORD_MIN_FREE_GB",
    "SAPFLOW_RECORDING_ROLL_BURST",
    "SAPFLOW_RECORDING_ROLL_REFILL",
    "SAPFLOW_RECORDING_ROLL_SETTLE",
    "SAPFLOW_SRT_IMPL",
    "SAPFLOW_SRT_PASSPHRASE",
    "SAPFLOW_SRT_CIPHER",
    "SAPFLOW_SRT_PBKEYLEN",
    "SAPFLOW_SRT_ENCRYPTION",
    "SAPFLOW_SRT_REORDER_WAIT_MAX_MS",
    "SAPFLOW_SRT_APP_KEYS",
    "SAPFLOW_SRT_REPLAY_STORE",
    "SAPFLOW_RECONNECT_GRACE",
    "SAPFLOW_HEALTH_INTERVAL",
    "SAPFLOW_WHIP_ADDR",
    "SAPFLOW_WHIP_PUBLIC_URL",
    "SAPFLOW_WHIP_UDP_ADDR",
    "SAPFLOW_WHIP_CANDIDATES",
    "SAPFLOW_WHIP_ALLOW_ORIGINS",
    "SAPFLOW_WHIP_TRUSTED_PROXY",
    "SAPFLOW_WHIP_KEYFRAME_INTERVAL",
    "SAPFLOW_WHIP_OPUS_MAX_BITRATE",
    "SAPFLOW_WHIP_STALE",
    "SAPFLOW_WHIP_TCP_ADDR",
    "SAPFLOW_WHIP_TCP_PROXY_PROTOCOL",
];

/// WHIP ingest, when `SAPFLOW_WHIP_ADDR` is set.
#[derive(Debug, Clone)]
pub struct WhipConfig {
    /// The signalling listener.
    pub addr: SocketAddr,
    /// For `Location`.
    pub public_url: PublicUrl,
    /// The media socket.
    pub udp_addr: SocketAddr,
    /// What the answer advertises: UDP, and passive TCP with `tcp_addr`.
    pub candidates: Vec<Candidate>,
    /// The passive ICE-TCP listener, for a front proxy.
    pub tcp_addr: Option<SocketAddr>,
    /// Whether every connection to `tcp_addr` starts with a PROXY
    /// protocol v2 header from `trusted_proxy`.
    pub tcp_proxy_protocol: bool,
    /// Exact origins CORS allows.
    pub allow_origins: Vec<String>,
    /// The proxy whose `X-Forwarded-For` gives the publisher's address.
    pub trusted_proxy: Option<IpAddr>,
    pub keyframe_interval: Option<Duration>,
    pub opus_max_bitrate: u32,
    /// Media silence after which a new `POST` may supersede a session.
    pub stale: Duration,
}

impl WhipConfig {
    fn load(vals: &std::collections::HashMap<String, String>) -> Result<Option<Self>, ConfigError> {
        let invalid = |m: &str| ConfigError::Invalid(m.into());
        let Some(addr) = vals.get("SAPFLOW_WHIP_ADDR") else {
            if let Some(key) = KEYS
                .iter()
                .find(|k| k.starts_with("SAPFLOW_WHIP_") && vals.contains_key(**k))
            {
                return Err(ConfigError::Invalid(format!(
                    "{key} needs SAPFLOW_WHIP_ADDR: WHIP is off without it"
                )));
            }
            return Ok(None);
        };
        let socket = |key: &str, v: &str| -> Result<SocketAddr, ConfigError> {
            v.parse()
                .map_err(|_| ConfigError::Invalid(format!("{key}: not an address: {v}")))
        };
        let addr = socket("SAPFLOW_WHIP_ADDR", addr)?;
        let required = |key: &str| {
            vals.get(key).ok_or_else(|| {
                ConfigError::Invalid(format!("{key} is required with SAPFLOW_WHIP_ADDR"))
            })
        };
        let public_url = PublicUrl::parse(required("SAPFLOW_WHIP_PUBLIC_URL")?).ok_or_else(|| {
            invalid("SAPFLOW_WHIP_PUBLIC_URL: an http:// or https:// base URL with a host, no query")
        })?;
        let udp_addr = socket("SAPFLOW_WHIP_UDP_ADDR", required("SAPFLOW_WHIP_UDP_ADDR")?)?;
        let tcp_addr = vals
            .get("SAPFLOW_WHIP_TCP_ADDR")
            .map(|v| socket("SAPFLOW_WHIP_TCP_ADDR", v))
            .transpose()?;
        if tcp_addr == Some(addr) {
            return Err(invalid(
                "SAPFLOW_WHIP_TCP_ADDR: the ICE-TCP listener needs its own address, not SAPFLOW_WHIP_ADDR's",
            ));
        }
        let mut candidates = Vec::new();
        for item in required("SAPFLOW_WHIP_CANDIDATES")?.split(',') {
            let item = item.trim();
            let (transport, address) = match item.split_once(char::is_whitespace) {
                Some((kind, address)) if kind.eq_ignore_ascii_case("udp") => {
                    (Transport::Udp, address.trim())
                }
                Some((kind, address)) if kind.eq_ignore_ascii_case("tcp") => {
                    (Transport::Tcp, address.trim())
                }
                _ => {
                    return Err(invalid(
                        "SAPFLOW_WHIP_CANDIDATES: udp|tcp <ip>:<port>, comma-separated",
                    ));
                }
            };
            candidates.push(Candidate {
                transport,
                address: socket("SAPFLOW_WHIP_CANDIDATES", address)?,
            });
        }
        if candidates.is_empty() || candidates.len() > sapflow_whip::sdp::MAX_CANDIDATES {
            return Err(invalid("SAPFLOW_WHIP_CANDIDATES: 1 to 8 candidates"));
        }
        let tcp_candidates = candidates.iter().any(|c| c.transport == Transport::Tcp);
        match (tcp_addr.is_some(), tcp_candidates) {
            (false, true) => {
                return Err(invalid(
                    "SAPFLOW_WHIP_CANDIDATES: a tcp candidate needs SAPFLOW_WHIP_TCP_ADDR, the listener it reaches",
                ));
            }
            (true, false) => {
                return Err(invalid(
                    "SAPFLOW_WHIP_TCP_ADDR: no tcp candidate in SAPFLOW_WHIP_CANDIDATES advertises it",
                ));
            }
            _ => {}
        }
        let allow_origins = match vals.get("SAPFLOW_WHIP_ALLOW_ORIGINS") {
            None => Vec::new(),
            Some(v) => v
                .split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(|o| {
                    let rest = o.strip_prefix("https://").or_else(|| o.strip_prefix("http://"));
                    match rest {
                        Some(host) if !host.is_empty() && !host.contains(['/', '?', '#', ' ', '*']) => {
                            Ok(o.to_owned())
                        }
                        _ => Err(invalid(
                            "SAPFLOW_WHIP_ALLOW_ORIGINS: exact origins like https://example.org, comma-separated",
                        )),
                    }
                })
                .collect::<Result<_, _>>()?,
        };
        let trusted_proxy = match vals.get("SAPFLOW_WHIP_TRUSTED_PROXY") {
            None => None,
            Some(v) => Some(
                v.parse::<IpAddr>()
                    .map_err(|_| invalid("SAPFLOW_WHIP_TRUSTED_PROXY: an IP address"))?,
            ),
        };
        let tcp_proxy_protocol = match vals
            .get("SAPFLOW_WHIP_TCP_PROXY_PROTOCOL")
            .map(String::as_str)
        {
            None => false,
            Some(v) if v.eq_ignore_ascii_case("v2") => {
                if tcp_addr.is_none() {
                    return Err(invalid(
                        "SAPFLOW_WHIP_TCP_PROXY_PROTOCOL needs SAPFLOW_WHIP_TCP_ADDR",
                    ));
                }
                if trusted_proxy.is_none() {
                    return Err(invalid(
                        "SAPFLOW_WHIP_TCP_PROXY_PROTOCOL=v2 needs SAPFLOW_WHIP_TRUSTED_PROXY, the only peer whose header is believed",
                    ));
                }
                true
            }
            Some(_) => {
                return Err(invalid(
                    "SAPFLOW_WHIP_TCP_PROXY_PROTOCOL: v2, or unset for none",
                ));
            }
        };
        let keyframe_interval = match vals.get("SAPFLOW_WHIP_KEYFRAME_INTERVAL") {
            None => Some(Duration::from_secs(2)),
            Some(v) => match v.parse::<u64>() {
                Ok(0) => None,
                Ok(n) if n <= 60 => Some(Duration::from_secs(n)),
                _ => return Err(invalid("SAPFLOW_WHIP_KEYFRAME_INTERVAL: seconds, 0 to 60")),
            },
        };
        let opus_max_bitrate = match vals.get("SAPFLOW_WHIP_OPUS_MAX_BITRATE") {
            None => 128_000,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|n| (6_000..=510_000).contains(n))
                .ok_or_else(|| invalid("SAPFLOW_WHIP_OPUS_MAX_BITRATE: 6000 to 510000"))?,
        };
        let stale = match vals.get("SAPFLOW_WHIP_STALE") {
            None => Duration::from_secs(2),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=60).contains(n))
                .map(Duration::from_secs)
                .ok_or_else(|| invalid("SAPFLOW_WHIP_STALE: seconds, 1 to 60"))?,
        };
        Ok(Some(Self {
            stale,
            addr,
            public_url,
            udp_addr,
            candidates,
            tcp_addr,
            tcp_proxy_protocol,
            allow_origins,
            trusted_proxy,
            keyframe_interval,
            opus_max_bitrate,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub addr: SocketAddr,
    pub token: Arc<Secret>,
    pub store: PathBuf,
}

/// The RTMP listener for a TLS front (RTMPS) that sends PROXY protocol v2, so
/// that limits, logs and the app's publish check see each publisher's own
/// address rather than the front's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtmpProxy {
    /// Where it listens.
    pub addr: SocketAddr,
    /// The front's address as this server sees it: the only peer accepted,
    /// and the only one whose header is believed.
    pub trusted: IpAddr,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub control: Option<ControlConfig>,
    pub srt_addr: SocketAddr,
    /// Independent transport secret. Shared zeroising ownership, redacted Debug.
    pub srt_crypto: Option<CryptoConfig>,
    /// With `srt_crypto`, also admit SRT callers that do not encrypt.
    pub srt_allow_plaintext: bool,
    /// Refuse SRT callers that send no key material: a global passphrase
    /// without `optional`, or an explicit `required` with app keys.
    pub srt_require_encryption: bool,
    /// The app may give each SRT stream its own passphrase in its publish
    /// authorisation answer (`SAPFLOW_SRT_APP_KEYS`).
    pub srt_app_keys: bool,
    /// Where the SRT replay guard keeps admitted key material digests
    /// across restarts (`SAPFLOW_SRT_REPLAY_STORE`); required with app keys.
    pub srt_replay_store: Option<PathBuf>,
    /// The longest an SRT connection holds a new gap's first NAK while its
    /// path reorders packets; zero reports every gap at once.
    pub srt_reorder_wait_max: Duration,
    pub rtmp_addr: Option<SocketAddr>,
    /// A second RTMP listener behind a PROXY protocol v2 front
    /// (`SAPFLOW_RTMP_PROXY_ADDR`), and the one peer it accepts.
    pub rtmp_proxy: Option<RtmpProxy>,
    pub http_addr: SocketAddr,
    /// `None` when open publishing was asked for explicitly, in scoped mode,
    /// or when the app authorises each publisher (`publish_auth`).
    pub publisher: Option<Publisher>,
    /// Ask the app about each publisher (`SAPFLOW_PUBLISH_AUTH_URL`). The URL
    /// carries the app's token; `Endpoint`'s Debug never prints it.
    pub publish_auth: Option<Endpoint>,
    pub callback_url: Option<String>,
    pub callback_secret: Option<String>,
    pub segment_count: usize,
    /// The most media the playlist keeps (`SAPFLOW_WINDOW_SECONDS`), beside
    /// `segment_count`; `None` for no limit.
    pub window: Option<Duration>,
    pub low_latency: bool,
    /// Threads for the viewer event loop (`own_http.rs`).
    pub http_threads: usize,
    pub http_body_timeout: Duration,
    pub recordings: Option<PathBuf>,
    pub record_policy: RecordPolicy,
    pub record_delete_after: Duration,
    pub max_publishers: usize,
    pub max_presentations: usize,
    pub max_fragment_bytes: usize,
    /// Bytes of media kept past the live window for RFC 8216bis 6.2.2, in a
    /// pool beside the fragment bytes.
    pub max_retained_bytes: usize,
    pub max_publish_bps: u64,
    /// Bytes of free disk below which recording does not start or stops.
    pub record_min_free: u64,
    /// How often a stream's recording may roll to a new file at a codec
    /// epoch (`SAPFLOW_RECORDING_ROLL_*`).
    pub recording_roll: crate::roll_budget::RollLimit,
    pub reconnect_grace: Duration,
    pub health_interval: Duration,
    /// WHIP ingest; `None` binds nothing.
    pub whip: Option<WhipConfig>,
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
        let publish_auth_url = vals.get("SAPFLOW_PUBLISH_AUTH_URL");
        let srt_app_keys = match vals.get("SAPFLOW_SRT_APP_KEYS").map(String::as_str) {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => {
                return Err(ConfigError::Invalid("SAPFLOW_SRT_APP_KEYS: 1 or 0".into()));
            }
        };
        if control.is_some()
            && (open
                || publish_auth_url.is_some()
                || srt_app_keys
                || vals.contains_key("SAPFLOW_PUBLISH_USER")
                || vals.contains_key("SAPFLOW_PUBLISH_PASS")
                || srt_crypto.is_some()
                || vals.contains_key("SAPFLOW_SRT_CIPHER")
                || vals.contains_key("SAPFLOW_SRT_PBKEYLEN")
                || vals.contains_key("SAPFLOW_SRT_ENCRYPTION")
                || vals.get("SAPFLOW_RTMP_ADDR").is_some_and(|v| v != "off")
                || vals
                    .get("SAPFLOW_RTMP_PROXY_ADDR")
                    .is_some_and(|v| v != "off"))
        {
            return Err(ConfigError::Invalid("scoped control mode requires per-session encrypted SRT; global/open/app-authorised publishing, global SRT keys and RTMP are disabled".into()));
        }
        // The URL carries the app's token: no error below repeats it.
        let publish_auth = match publish_auth_url {
            None => None,
            Some(url)
                if url
                    .get(..8)
                    .is_some_and(|s| s.eq_ignore_ascii_case("https://")) =>
            {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_PUBLISH_AUTH_URL must be http:// (sapflow has no TLS client; keep it on the private network)".into(),
                ));
            }
            Some(url) => Some(Endpoint::parse(url).ok_or_else(|| {
                ConfigError::Invalid("SAPFLOW_PUBLISH_AUTH_URL: not a usable http:// URL".into())
            })?),
        };
        let srt_replay_store = match vals.get("SAPFLOW_SRT_REPLAY_STORE") {
            None => None,
            Some(path) => {
                let path = PathBuf::from(path);
                if !path.is_absolute() || path.file_name().is_none() {
                    return Err(ConfigError::Invalid(
                        "SAPFLOW_SRT_REPLAY_STORE: absolute file path required".into(),
                    ));
                }
                Some(path)
            }
        };
        if srt_app_keys && publish_auth.is_none() {
            return Err(ConfigError::Invalid(
                "SAPFLOW_SRT_APP_KEYS=1 requires SAPFLOW_PUBLISH_AUTH_URL: the app returns each stream's passphrase".into(),
            ));
        }
        // Scoped sessions keep their digests beside the session store, whose
        // private directory already outlives restarts, unless told otherwise.
        let srt_replay_store = match (srt_replay_store, &control) {
            (None, Some(control)) => Some(control.store.with_file_name("srt-replay")),
            (store, _) => store,
        };
        if let (Some(replay), Some(control)) = (&srt_replay_store, &control)
            && stores_share_files(replay, &control.store)
        {
            return Err(ConfigError::Invalid(
                "SAPFLOW_SRT_REPLAY_STORE and SAPFLOW_SESSION_STORE would share a file (the store, its .lock or its temporary files); set SAPFLOW_SRT_REPLAY_STORE to another name".into(),
            ));
        }
        if srt_app_keys && srt_replay_store.is_none() {
            return Err(ConfigError::Invalid(
                "SAPFLOW_SRT_APP_KEYS=1 requires SAPFLOW_SRT_REPLAY_STORE: an absolute file path in a private (0700) directory on a persistent volume, where admitted callers' key material digests survive restarts".into(),
            ));
        }
        if publish_auth.is_some()
            && (open
                || vals.contains_key("SAPFLOW_PUBLISH_USER")
                || vals.contains_key("SAPFLOW_PUBLISH_PASS"))
        {
            return Err(ConfigError::Invalid(
                "SAPFLOW_PUBLISH_AUTH_URL replaces SAPFLOW_PUBLISH_USER/PASS and SAPFLOW_OPEN_PUBLISH; set only one".into(),
            ));
        }
        let publisher = match (
            vals.get("SAPFLOW_PUBLISH_USER"),
            vals.get("SAPFLOW_PUBLISH_PASS"),
        ) {
            (Some(u), Some(p)) => Some(Publisher::new(u, p)),
            (None, None) if open || control.is_some() || publish_auth.is_some() => None,
            (None, None) => {
                return Err(ConfigError::Invalid(
                    "set SAPFLOW_PUBLISH_USER and SAPFLOW_PUBLISH_PASS, or SAPFLOW_PUBLISH_AUTH_URL (or SAPFLOW_OPEN_PUBLISH=1 for local testing)".into(),
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
        // Two seconds a segment: what the count holds for an encoder sending
        // a keyframe every 2 s or more often, so only longer intervals are cut.
        let window = match vals.get("SAPFLOW_WINDOW_SECONDS") {
            None => Some(Duration::from_secs(2 * segment_count as u64)),
            Some(v) => match v.parse::<u64>() {
                Ok(0) => None,
                Ok(n) if n <= 86_400 => Some(Duration::from_secs(n)),
                _ => {
                    return Err(ConfigError::Invalid(
                        "SAPFLOW_WINDOW_SECONDS: 0 (no limit) to 86400".into(),
                    ));
                }
            },
        };

        let rtmp_addr = match vals.get("SAPFLOW_RTMP_ADDR").map(String::as_str) {
            _ if control.is_some() => None,
            Some("off") => None,
            _ => Some(addr("SAPFLOW_RTMP_ADDR", "0.0.0.0:1935")?),
        };
        let rtmp_proxy = match (
            vals.get("SAPFLOW_RTMP_PROXY_ADDR").map(String::as_str),
            vals.get("SAPFLOW_RTMP_TRUSTED_PROXY"),
        ) {
            (None | Some("off"), None) => None,
            (None | Some("off"), Some(_)) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_RTMP_TRUSTED_PROXY needs SAPFLOW_RTMP_PROXY_ADDR".into(),
                ));
            }
            (Some(_), None) => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_RTMP_PROXY_ADDR needs SAPFLOW_RTMP_TRUSTED_PROXY, the only peer whose PROXY header is believed".into(),
                ));
            }
            (Some(_), Some(trusted)) => {
                let proxy = RtmpProxy {
                    addr: addr("SAPFLOW_RTMP_PROXY_ADDR", "")?,
                    trusted: trusted.parse().map_err(|_| {
                        ConfigError::Invalid("SAPFLOW_RTMP_TRUSTED_PROXY: an IP address".into())
                    })?,
                };
                if rtmp_addr == Some(proxy.addr) {
                    return Err(ConfigError::Invalid(
                        "SAPFLOW_RTMP_PROXY_ADDR must differ from SAPFLOW_RTMP_ADDR: one listener takes PROXY headers, the other plain RTMP".into(),
                    ));
                }
                Some(proxy)
            }
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

        let max_presentations = match vals.get("SAPFLOW_MAX_PRESENTATIONS") {
            None => sapflow_core::residency::default_presentation_limit(max_publishers),
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=4096).contains(n))
                .ok_or_else(|| {
                    ConfigError::Invalid("SAPFLOW_MAX_PRESENTATIONS: 1 to 4096".into())
                })?,
        };

        let max_fragment_bytes = match vals.get("SAPFLOW_MAX_FRAGMENT_BYTES") {
            None => 512 * 1024 * 1024,
            Some(v) => v.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| {
                ConfigError::Invalid(
                    "SAPFLOW_MAX_FRAGMENT_BYTES: positive representable byte count".into(),
                )
            })?,
        };

        let max_retained_bytes = match vals.get("SAPFLOW_MAX_RETAINED_BYTES") {
            None => 128 * 1024 * 1024,
            Some(v) => v.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| {
                ConfigError::Invalid(
                    "SAPFLOW_MAX_RETAINED_BYTES: positive representable byte count".into(),
                )
            })?,
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
        let recording_roll = crate::roll_budget::RollLimit {
            burst: match vals.get("SAPFLOW_RECORDING_ROLL_BURST") {
                None => 8,
                Some(v) => v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n <= 1000)
                    .ok_or_else(|| {
                        ConfigError::Invalid("SAPFLOW_RECORDING_ROLL_BURST: 0 to 1000".into())
                    })?,
            },
            refill: seconds("SAPFLOW_RECORDING_ROLL_REFILL", 60)?,
            settle: seconds("SAPFLOW_RECORDING_ROLL_SETTLE", 10)?,
        };
        if recording_roll.refill.is_zero() {
            return Err(ConfigError::Invalid(
                "SAPFLOW_RECORDING_ROLL_REFILL: seconds, 1 to 3600".into(),
            ));
        }
        let health_interval = seconds("SAPFLOW_HEALTH_INTERVAL", 30)?;
        let http_body_timeout = seconds("SAPFLOW_HTTP_BODY_TIMEOUT", 120)?;
        if http_body_timeout.is_zero() {
            return Err(ConfigError::Invalid(
                "SAPFLOW_HTTP_BODY_TIMEOUT: seconds, 1 to 3600".into(),
            ));
        }
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

        let encryption = vals.get("SAPFLOW_SRT_ENCRYPTION").map(String::as_str);
        let srt_allow_plaintext = match encryption {
            None => false,
            // Explicitly required with no key to require: refused rather
            // than left meaning plaintext, unless the app holds the keys.
            Some("required") if srt_crypto.is_none() && !srt_app_keys => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_SRT_ENCRYPTION=required needs SAPFLOW_SRT_PASSPHRASE or SAPFLOW_SRT_APP_KEYS=1".into(),
                ));
            }
            Some("required") => false,
            Some("optional") if srt_crypto.is_some() => true,
            // App keys without a global passphrase admit plaintext callers
            // already; optional says so.
            Some("optional") if srt_app_keys => false,
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

        let srt_require_encryption = (srt_crypto.is_some() && !srt_allow_plaintext)
            || (srt_app_keys && encryption == Some("required"));

        let srt_reorder_wait_max = match vals.get("SAPFLOW_SRT_REORDER_WAIT_MAX_MS") {
            None => Duration::from_millis(50),
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|n| *n <= 1000)
                .map(Duration::from_millis)
                .ok_or_else(|| {
                    ConfigError::Invalid("SAPFLOW_SRT_REORDER_WAIT_MAX_MS: 0 to 1000".into())
                })?,
        };

        // Keep recognising the removed viewer backend switch so obsolete
        // deployments fail explicitly rather than silently changing backend.
        match vals.get("SAPFLOW_HTTP_IMPL").map(String::as_str) {
            None | Some("own") => {}
            Some("hyper") => {
                return Err(ConfigError::Invalid(
                    "SAPFLOW_HTTP_IMPL=hyper: the Hyper viewer backend was removed; \
                     viewers are served by the own event loop, so unset SAPFLOW_HTTP_IMPL"
                        .into(),
                ));
            }
            Some(v) => {
                return Err(ConfigError::Invalid(format!(
                    "SAPFLOW_HTTP_IMPL={v}: unknown; the Hyper viewer backend was removed, \
                     so unset SAPFLOW_HTTP_IMPL"
                )));
            }
        }
        let http_threads = match vals.get("SAPFLOW_HTTP_THREADS") {
            None => std::thread::available_parallelism().map_or(4, |n| n.get().min(256)),
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=256).contains(n))
                .ok_or_else(|| ConfigError::Invalid("SAPFLOW_HTTP_THREADS: 1 to 256".into()))?,
        };

        let whip = WhipConfig::load(&vals)?;
        if whip.is_some() && control.is_some() {
            return Err(ConfigError::Invalid(
                "WHIP needs global or app-authorised publishing; scoped control mode is SRT only"
                    .into(),
            ));
        }
        Ok(Self {
            whip,
            http_threads,
            http_body_timeout,
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
            srt_require_encryption,
            srt_app_keys,
            srt_replay_store,
            srt_reorder_wait_max,
            record_min_free,
            recording_roll,
            max_publish_bps,
            rtmp_addr,
            rtmp_proxy,
            recordings: vals.get("SAPFLOW_RECORDINGS").map(PathBuf::from),
            record_policy: RecordPolicy::parse(
                vals.get("SAPFLOW_RECORD_SKIP").map_or("", String::as_str),
            ),
            record_delete_after,
            max_publishers,
            max_presentations,
            max_fragment_bytes,
            max_retained_bytes,
            srt_addr: addr("SAPFLOW_SRT_ADDR", "0.0.0.0:8890")?,
            http_addr: addr("SAPFLOW_HTTP_ADDR", "0.0.0.0:8888")?,
            publisher,
            publish_auth,
            callback_url,
            callback_secret: vals.get("SAPFLOW_CALLBACK_SECRET").cloned(),
            segment_count,
            window,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_backend_is_the_own_event_loop_and_hyper_is_refused() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert!((1..=256).contains(&load(&[open], &[]).expect("default").http_threads));
        assert!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "own")], &[])
                .expect("own")
                .http_threads
                >= 1
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
            .http_threads,
            3
        );
        assert_eq!(
            load(&[open, ("SAPFLOW_HTTP_THREADS", "3")], &[])
                .expect("default backend, explicit thread count")
                .http_threads,
            3
        );
        assert!(load(&[open], &["--http-impl=own"]).is_ok());
        // The removed backend stops startup and says why, from either source.
        for config in [
            load(&[open, ("SAPFLOW_HTTP_IMPL", "hyper")], &[]),
            load(&[open], &["--http-impl=hyper"]),
            load(
                &[open, ("SAPFLOW_HTTP_IMPL", "own")],
                &["--http-impl=hyper"],
            ),
            // Not excused by a valid thread count.
            load(
                &[
                    open,
                    ("SAPFLOW_HTTP_IMPL", "hyper"),
                    ("SAPFLOW_HTTP_THREADS", "3"),
                ],
                &[],
            ),
        ] {
            assert!(matches!(
                config,
                Err(ConfigError::Invalid(m)) if m.contains("SAPFLOW_HTTP_IMPL=hyper") && m.contains("removed")
            ));
        }
        assert!(matches!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "tokio")], &[]),
            Err(ConfigError::Invalid(m)) if m.contains("SAPFLOW_HTTP_IMPL=tokio") && m.contains("removed")
        ));
        // Empty means unset, as for every setting.
        assert!(
            load(&[open, ("SAPFLOW_HTTP_IMPL", "")], &[])
                .expect("empty")
                .http_threads
                >= 1
        );
        // The removed switch is not advertised.
        assert!(!HELP.contains("SAPFLOW_HTTP_IMPL"));
        assert!(HELP.contains("SAPFLOW_HTTP_THREADS"));
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

    /// `base` with `changes` applied: a value replaces its key's, `None`
    /// removes it.
    fn with<'a>(
        base: &[(&'a str, &'a str)],
        changes: &[(&'a str, Option<&'a str>)],
    ) -> Vec<(&'a str, &'a str)> {
        let mut out: Vec<(&str, &str)> = base
            .iter()
            .filter(|(k, _)| !changes.iter().any(|(c, _)| c == k))
            .copied()
            .collect();
        out.extend(changes.iter().filter_map(|(k, v)| v.map(|v| (*k, v))));
        out
    }

    #[test]
    fn whip_is_off_unless_its_address_is_set() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert!(load(&[open], &[]).expect("defaults").whip.is_none());
        let whip = [
            open,
            ("SAPFLOW_WHIP_ADDR", "127.0.0.1:8443"),
            ("SAPFLOW_WHIP_PUBLIC_URL", "https://whip.example"),
            ("SAPFLOW_WHIP_UDP_ADDR", "0.0.0.0:8189"),
            (
                "SAPFLOW_WHIP_CANDIDATES",
                "udp 203.0.113.5:8189, UDP 198.51.100.7:8189",
            ),
        ];
        let c = load(&whip, &[]).expect("WHIP").whip.expect("on");
        assert_eq!(c.candidates.len(), 2);
        assert_eq!(c.keyframe_interval, Some(Duration::from_secs(2)));
        assert_eq!(c.opus_max_bitrate, 128_000);
        assert_eq!(c.stale, Duration::from_secs(2));
        assert!(c.allow_origins.is_empty() && c.trusted_proxy.is_none());
        let c = load(
            &with(
                &whip,
                &[
                    (
                        "SAPFLOW_WHIP_ALLOW_ORIGINS",
                        Some("https://a.example, http://b.example:8080"),
                    ),
                    ("SAPFLOW_WHIP_TRUSTED_PROXY", Some("10.0.0.1")),
                    ("SAPFLOW_WHIP_KEYFRAME_INTERVAL", Some("0")),
                    ("SAPFLOW_WHIP_STALE", Some("5")),
                    ("SAPFLOW_WHIP_OPUS_MAX_BITRATE", Some("64000")),
                ],
            ),
            &[],
        )
        .expect("WHIP")
        .whip
        .expect("on");
        assert_eq!(
            c.allow_origins,
            ["https://a.example", "http://b.example:8080"]
        );
        assert_eq!(c.trusted_proxy, Some("10.0.0.1".parse().expect("ip")));
        assert_eq!((c.keyframe_interval, c.opus_max_bitrate), (None, 64_000));
        assert_eq!(c.stale, Duration::from_secs(5));
        // A WHIP setting without the address is refused, not ignored.
        assert!(load(&[open, ("SAPFLOW_WHIP_UDP_ADDR", "0.0.0.0:8189")], &[]).is_err());
        for missing in [
            "SAPFLOW_WHIP_PUBLIC_URL",
            "SAPFLOW_WHIP_UDP_ADDR",
            "SAPFLOW_WHIP_CANDIDATES",
        ] {
            assert!(
                load(&with(&whip, &[(missing, None)]), &[]).is_err(),
                "{missing}"
            );
        }
        // ICE-TCP: a tcp candidate with its listener, and PROXY v2 from the
        // trusted proxy.
        let tcp = with(
            &whip,
            &[
                (
                    "SAPFLOW_WHIP_CANDIDATES",
                    Some("udp 203.0.113.5:8189, tcp 203.0.113.5:443"),
                ),
                ("SAPFLOW_WHIP_TCP_ADDR", Some("0.0.0.0:8444")),
            ],
        );
        let c = load(&tcp, &[]).expect("TCP").whip.expect("on");
        assert_eq!(c.tcp_addr, Some("0.0.0.0:8444".parse().expect("addr")));
        assert!(!c.tcp_proxy_protocol);
        assert_eq!(
            c.candidates.iter().map(|c| c.transport).collect::<Vec<_>>(),
            [Transport::Udp, Transport::Tcp]
        );
        let v2 = with(
            &tcp,
            &[
                ("SAPFLOW_WHIP_TCP_PROXY_PROTOCOL", Some("v2")),
                ("SAPFLOW_WHIP_TRUSTED_PROXY", Some("10.0.0.1")),
            ],
        );
        let c = load(&v2, &[]).expect("PROXY v2").whip.expect("on");
        assert!(c.tcp_proxy_protocol);
        assert_eq!(c.trusted_proxy, Some("10.0.0.1".parse().expect("ip")));
        for (base, changes) in [
            // PROXY v2 with no trusted proxy, or no listener, or another value.
            (&v2, vec![("SAPFLOW_WHIP_TRUSTED_PROXY", None)]),
            (&v2, vec![("SAPFLOW_WHIP_TCP_ADDR", None)]),
            (&v2, vec![("SAPFLOW_WHIP_TCP_PROXY_PROTOCOL", Some("v1"))]),
            (&v2, vec![("SAPFLOW_WHIP_TCP_PROXY_PROTOCOL", Some("yes"))]),
            // A listener no candidate advertises, or one on the signalling
            // address.
            (
                &tcp,
                vec![("SAPFLOW_WHIP_CANDIDATES", Some("udp 203.0.113.5:8189"))],
            ),
            (
                &tcp,
                vec![("SAPFLOW_WHIP_TCP_ADDR", Some("127.0.0.1:8443"))],
            ),
            (&tcp, vec![("SAPFLOW_WHIP_TCP_ADDR", Some("somewhere"))]),
        ] {
            assert!(load(&with(base, &changes), &[]).is_err(), "{changes:?}");
        }
        // Without SAPFLOW_WHIP_ADDR, the TCP settings are refused too.
        for key in ["SAPFLOW_WHIP_TCP_ADDR", "SAPFLOW_WHIP_TCP_PROXY_PROTOCOL"] {
            assert!(load(&[open, (key, "v2")], &[]).is_err(), "{key}");
        }
        for (key, bad) in [
            // A tcp candidate needs the listener.
            ("SAPFLOW_WHIP_CANDIDATES", "tcp 203.0.113.5:443"),
            ("SAPFLOW_WHIP_CANDIDATES", "udp somewhere"),
            ("SAPFLOW_WHIP_CANDIDATES", "quic 203.0.113.5:443"),
            ("SAPFLOW_WHIP_CANDIDATES", ""),
            // A listener with no tcp candidate.
            ("SAPFLOW_WHIP_TCP_ADDR", "0.0.0.0:8443"),
            ("SAPFLOW_WHIP_TCP_PROXY_PROTOCOL", "v2"),
            ("SAPFLOW_WHIP_PUBLIC_URL", "whip.example"),
            ("SAPFLOW_WHIP_ALLOW_ORIGINS", "*"),
            ("SAPFLOW_WHIP_ALLOW_ORIGINS", "https://a.example/page"),
            ("SAPFLOW_WHIP_TRUSTED_PROXY", "proxy.example"),
            ("SAPFLOW_WHIP_KEYFRAME_INTERVAL", "61"),
            ("SAPFLOW_WHIP_OPUS_MAX_BITRATE", "5999"),
            ("SAPFLOW_WHIP_STALE", "0"),
            ("SAPFLOW_WHIP_STALE", "61"),
        ] {
            assert!(
                load(&with(&whip, &[(key, Some(bad))]), &[]).is_err(),
                "{key}={bad}"
            );
        }
        assert_eq!(
            load(
                &with(
                    &whip,
                    &[(
                        "SAPFLOW_WHIP_CANDIDATES",
                        Some(&["udp 192.0.2.1:1"; 9].join(","))
                    )]
                ),
                &[]
            )
            .map(|_| ())
            .map_err(|_| ()),
            Err(()),
            "at most eight candidates"
        );
        // Scoped control mode is SRT only.
        let scoped = with(
            &whip,
            &[
                ("SAPFLOW_OPEN_PUBLISH", None),
                ("SAPFLOW_RTMP_ADDR", Some("off")),
                ("SAPFLOW_CONTROL_ADDR", Some("127.0.0.1:9000")),
                (
                    "SAPFLOW_CONTROL_TOKEN",
                    Some("0123456789abcdef0123456789abcdef"),
                ),
                (
                    "SAPFLOW_SESSION_STORE",
                    Some("/var/lib/sapflow/sessions.json"),
                ),
            ],
        );
        assert!(load(&scoped, &[]).is_err());
        assert!(
            load(
                &with(
                    &scoped,
                    &[
                        ("SAPFLOW_WHIP_ADDR", None),
                        ("SAPFLOW_WHIP_PUBLIC_URL", None),
                        ("SAPFLOW_WHIP_UDP_ADDR", None),
                        ("SAPFLOW_WHIP_CANDIDATES", None)
                    ]
                ),
                &[]
            )
            .is_ok()
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
    fn the_window_bound_follows_the_count_unless_set() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let window = |env: &[(&str, &str)]| {
            let mut env = env.to_vec();
            env.push(open);
            load(&env, &[]).map(|c| c.window)
        };
        let secs = |n| Some(Duration::from_secs(n));
        let valid = |env: &[(&str, &str)]| window(env).expect("valid");
        assert_eq!(valid(&[]), secs(48));
        assert_eq!(valid(&[("SAPFLOW_SEGMENT_COUNT", "48")]), secs(96));
        assert_eq!(valid(&[("SAPFLOW_WINDOW_SECONDS", "30")]), secs(30));
        assert_eq!(valid(&[("SAPFLOW_WINDOW_SECONDS", "0")]), None);
        for bad in ["-1", "30s", "86401"] {
            assert!(window(&[("SAPFLOW_WINDOW_SECONDS", bad)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_rtmp_proxy_listener_needs_its_trusted_front() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let c = load(
            &[
                open,
                ("SAPFLOW_RTMP_PROXY_ADDR", "0.0.0.0:1936"),
                ("SAPFLOW_RTMP_TRUSTED_PROXY", "172.18.0.1"),
            ],
            &[],
        )
        .expect("a proxied listener with its front");
        assert_eq!(
            c.rtmp_proxy,
            Some(RtmpProxy {
                addr: "0.0.0.0:1936".parse().expect("address"),
                trusted: "172.18.0.1".parse().expect("address"),
            })
        );
        assert_eq!(
            c.rtmp_addr.map(|a| a.port()),
            Some(1935),
            "plain RTMP stays"
        );
        let off = load(&[open, ("SAPFLOW_RTMP_PROXY_ADDR", "off")], &[]).expect("off");
        assert!(off.rtmp_proxy.is_none());
        for (env, why) in [
            (
                vec![("SAPFLOW_RTMP_PROXY_ADDR", "0.0.0.0:1936")],
                "no trusted front",
            ),
            (
                vec![("SAPFLOW_RTMP_TRUSTED_PROXY", "172.18.0.1")],
                "no listener",
            ),
            (
                vec![
                    ("SAPFLOW_RTMP_PROXY_ADDR", "0.0.0.0:1936"),
                    ("SAPFLOW_RTMP_TRUSTED_PROXY", "172.18.0.0/16"),
                ],
                "a range, not an address",
            ),
            (
                vec![
                    ("SAPFLOW_RTMP_PROXY_ADDR", "1936"),
                    ("SAPFLOW_RTMP_TRUSTED_PROXY", "172.18.0.1"),
                ],
                "not a socket address",
            ),
            (
                vec![
                    ("SAPFLOW_RTMP_PROXY_ADDR", "0.0.0.0:1935"),
                    ("SAPFLOW_RTMP_TRUSTED_PROXY", "172.18.0.1"),
                ],
                "the plain listener's address",
            ),
        ] {
            let mut env = env;
            env.push(open);
            assert!(load(&env, &[]).is_err(), "{why}");
        }
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
        assert_eq!(c.max_presentations, 64);
    }

    #[test]
    fn resident_presentations_have_an_explicit_bounded_domain() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert_eq!(
            load(&[open, ("SAPFLOW_MAX_PUBLISHERS", "1")], &[])
                .expect("derived limit")
                .max_presentations,
            4
        );
        assert_eq!(
            load(&[open, ("SAPFLOW_MAX_PUBLISHERS", "1000")], &[])
                .expect("large publisher limit")
                .max_presentations,
            4000
        );
        for limit in ["1", "4096"] {
            assert_eq!(
                load(&[open, ("SAPFLOW_MAX_PRESENTATIONS", limit)], &[])
                    .expect("explicit limit")
                    .max_presentations
                    .to_string(),
                limit
            );
        }
        for limit in ["0", "4097", "-1", "invalid", "18446744073709551616"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_MAX_PRESENTATIONS", limit)], &[]),
                Err(ConfigError::Invalid(_))
            ));
        }
        assert_eq!(
            load(
                &[open, ("SAPFLOW_MAX_PRESENTATIONS", "4")],
                &["--max-presentations", "2"]
            )
            .expect("flag override")
            .max_presentations,
            2
        );
    }

    #[test]
    fn fragment_byte_domain_is_explicit_checked_and_independent_of_publication_count() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert_eq!(
            load(&[open], &[]).expect("defaults").max_fragment_bytes,
            512 * 1024 * 1024
        );
        for limit in ["1".to_string(), usize::MAX.to_string()] {
            assert_eq!(
                load(&[open, ("SAPFLOW_MAX_FRAGMENT_BYTES", &limit)], &[])
                    .expect("explicit byte domain")
                    .max_fragment_bytes
                    .to_string(),
                limit
            );
        }
        for limit in ["0", "-1", "invalid", "18446744073709551616"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_MAX_FRAGMENT_BYTES", limit)], &[]),
                Err(ConfigError::Invalid(_))
            ));
        }
        let config = load(
            &[
                open,
                ("SAPFLOW_MAX_FRAGMENT_BYTES", "1000"),
                ("SAPFLOW_MAX_PUBLISHERS", "1"),
            ],
            &["--max-fragment-bytes", "64"],
        )
        .expect("flag override");
        assert_eq!(config.max_fragment_bytes, 64);
        assert_eq!(config.max_presentations, 4);
    }

    #[test]
    fn retained_byte_pool_is_explicit_and_checked() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert_eq!(
            load(&[open], &[]).expect("defaults").max_retained_bytes,
            128 * 1024 * 1024
        );
        for limit in ["1".to_string(), usize::MAX.to_string()] {
            assert_eq!(
                load(&[open, ("SAPFLOW_MAX_RETAINED_BYTES", &limit)], &[])
                    .expect("explicit pool")
                    .max_retained_bytes
                    .to_string(),
                limit
            );
        }
        for limit in ["0", "-1", "invalid", "18446744073709551616"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_MAX_RETAINED_BYTES", limit)], &[]),
                Err(ConfigError::Invalid(_))
            ));
        }
        let config = load(
            &[open, ("SAPFLOW_MAX_RETAINED_BYTES", "1000")],
            &["--max-retained-bytes", "64"],
        )
        .expect("flag override");
        assert_eq!(config.max_retained_bytes, 64);
    }

    #[test]
    fn the_recording_roll_budget_has_defaults_a_switch_and_bounds() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert_eq!(
            load(&[open], &[]).expect("default").recording_roll,
            crate::roll_budget::RollLimit::default()
        );
        let set = load(
            &[
                open,
                ("SAPFLOW_RECORDING_ROLL_BURST", "0"),
                ("SAPFLOW_RECORDING_ROLL_REFILL", "1"),
                ("SAPFLOW_RECORDING_ROLL_SETTLE", "0"),
            ],
            &["--recording-roll-burst", "3"],
        )
        .expect("set")
        .recording_roll;
        assert_eq!(
            set,
            crate::roll_budget::RollLimit {
                burst: 3,
                refill: Duration::from_secs(1),
                settle: Duration::ZERO,
            }
        );
        for (key, value) in [
            ("SAPFLOW_RECORDING_ROLL_BURST", "1001"),
            ("SAPFLOW_RECORDING_ROLL_BURST", "-1"),
            ("SAPFLOW_RECORDING_ROLL_REFILL", "0"),
            ("SAPFLOW_RECORDING_ROLL_REFILL", "3601"),
            ("SAPFLOW_RECORDING_ROLL_SETTLE", "3601"),
            ("SAPFLOW_RECORDING_ROLL_SETTLE", "x"),
        ] {
            assert!(
                matches!(
                    load(&[open, (key, value)], &[]),
                    Err(ConfigError::Invalid(_))
                ),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn absolute_http_body_timeout_is_positive_bounded_and_overridable() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        assert_eq!(
            load(&[open], &[]).expect("default").http_body_timeout,
            Duration::from_secs(120)
        );
        for value in ["1", "3600"] {
            assert_eq!(
                load(&[open, ("SAPFLOW_HTTP_BODY_TIMEOUT", value)], &[])
                    .expect("selected deadline")
                    .http_body_timeout
                    .as_secs()
                    .to_string(),
                value
            );
        }
        for value in ["0", "3601", "-1", "invalid", "18446744073709551616"] {
            assert!(matches!(
                load(&[open, ("SAPFLOW_HTTP_BODY_TIMEOUT", value)], &[]),
                Err(ConfigError::Invalid(_))
            ));
        }
        assert_eq!(
            load(
                &[open, ("SAPFLOW_HTTP_BODY_TIMEOUT", "10")],
                &["--http-body-timeout", "2"]
            )
            .expect("override")
            .http_body_timeout,
            Duration::from_secs(2)
        );
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
        assert_eq!(d.window, Some(Duration::from_secs(48)));
        assert!(d.rtmp_proxy.is_none());
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
    fn srt_reorder_wait_max() {
        let open = ("SAPFLOW_OPEN_PUBLISH", "1");
        let key = "SAPFLOW_SRT_REORDER_WAIT_MAX_MS";
        let wait =
            |env: &[(&str, &str)], args: &[&str]| load(env, args).map(|c| c.srt_reorder_wait_max);
        assert_eq!(wait(&[open], &[]).ok(), Some(Duration::from_millis(50)));
        assert_eq!(wait(&[open, (key, "0")], &[]).ok(), Some(Duration::ZERO));
        assert_eq!(
            wait(&[open, (key, "1000")], &[]).ok(),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            wait(&[open, (key, "20")], &["--srt-reorder-wait-max-ms=30"]).ok(),
            Some(Duration::from_millis(30))
        );
        for bad in ["1001", "-1", "5ms", "x"] {
            assert!(
                matches!(wait(&[open, (key, bad)], &[]), Err(ConfigError::Invalid(m)) if m.contains(key)),
                "{bad}"
            );
        }
        assert!(HELP.contains(key));
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
        assert!(config.rtmp_proxy.is_none());
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
            ("SAPFLOW_RTMP_PROXY_ADDR", "127.0.0.1:1936"),
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
    fn app_authorisation_is_its_own_mode_and_never_prints_its_token() {
        let url = (
            "SAPFLOW_PUBLISH_AUTH_URL",
            "http://app:5181/api/media/publish-auth?token=app-secret-token&server=sapflow",
        );
        let config = load(&[url], &[]).expect("app-authorised config");
        let endpoint = config.publish_auth.as_ref().expect("endpoint");
        assert_eq!((endpoint.host.as_str(), endpoint.port), ("app", 5181));
        assert_eq!(
            endpoint.target(),
            "/api/media/publish-auth?token=app-secret-token&server=sapflow"
        );
        assert!(config.publisher.is_none());
        assert!(config.control.is_none());
        assert!(config.rtmp_addr.is_some());
        assert!(!format!("{config:?}").contains("app-secret-token"));
        // The flag form, and an empty value means unset as for every setting.
        assert!(
            load(&[], &["--publish-auth-url=http://app/auth?token=t"])
                .expect("flag")
                .publish_auth
                .is_some()
        );
        assert!(
            load(
                &[
                    ("SAPFLOW_OPEN_PUBLISH", "1"),
                    ("SAPFLOW_PUBLISH_AUTH_URL", "")
                ],
                &[]
            )
            .expect("empty")
            .publish_auth
            .is_none()
        );
        assert!(load(&[("SAPFLOW_PUBLISH_AUTH_URL", "")], &[]).is_err());
        // Exactly one of the three publisher modes.
        for extra in [
            vec![("SAPFLOW_OPEN_PUBLISH", "1")],
            vec![("SAPFLOW_PUBLISH_USER", "u"), ("SAPFLOW_PUBLISH_PASS", "p")],
            vec![("SAPFLOW_PUBLISH_USER", "u")],
            vec![("SAPFLOW_PUBLISH_PASS", "p")],
        ] {
            let mut env = vec![url];
            env.extend(extra);
            let Err(ConfigError::Invalid(error)) = load(&env, &[]) else {
                panic!("conflicting publisher modes accepted: {env:?}")
            };
            assert!(
                error.contains("set only one") || error.contains("go together"),
                "{error}"
            );
            assert!(!error.contains("app-secret-token"));
        }
        // An unusable or TLS URL stops startup without echoing the token.
        for bad in [
            "https://app/auth?token=app-secret-token",
            "HTTPS://app/auth?token=app-secret-token",
            "ftp://app/auth?token=app-secret-token",
            "http://app:0/auth?token=app-secret-token",
            "http://app/auth?token=app secret token",
            "app/auth?token=app-secret-token",
        ] {
            let Err(ConfigError::Invalid(error)) = load(&[("SAPFLOW_PUBLISH_AUTH_URL", bad)], &[])
            else {
                panic!("bad URL accepted: {bad}")
            };
            assert!(error.starts_with("SAPFLOW_PUBLISH_AUTH_URL"), "{error}");
            assert!(!error.contains("app-secret-token"), "{error}");
        }
        assert!(HELP.contains("SAPFLOW_PUBLISH_AUTH_URL"));
    }

    #[test]
    fn srt_app_keys_need_the_app_and_refuse_scoped_mode() {
        let url = ("SAPFLOW_PUBLISH_AUTH_URL", "http://app/auth?token=t");
        let on = ("SAPFLOW_SRT_APP_KEYS", "1");
        let store = ("SAPFLOW_SRT_REPLAY_STORE", "/var/lib/sapflow-srt/replay");
        let off = load(&[url], &[]).expect("default");
        assert!(!off.srt_app_keys && !off.srt_require_encryption);
        assert!(
            !load(&[url, ("SAPFLOW_SRT_APP_KEYS", "0")], &[])
                .expect("off")
                .srt_app_keys
        );
        let c = load(&[url, on, store], &[]).expect("app keys");
        // On, with nothing else: plaintext callers are still admitted.
        assert!(c.srt_app_keys && c.srt_crypto.is_none() && !c.srt_require_encryption);
        assert!(
            load(&[url, store], &["--srt-app-keys=1"])
                .expect("flag")
                .srt_app_keys
        );
        let Err(ConfigError::Invalid(error)) = load(&[url, ("SAPFLOW_SRT_APP_KEYS", "yes")], &[])
        else {
            panic!("bad value accepted")
        };
        assert_eq!(error, "SAPFLOW_SRT_APP_KEYS: 1 or 0");
        for env in [
            vec![on, ("SAPFLOW_OPEN_PUBLISH", "1")],
            vec![
                on,
                ("SAPFLOW_PUBLISH_USER", "u"),
                ("SAPFLOW_PUBLISH_PASS", "p"),
            ],
        ] {
            let Err(ConfigError::Invalid(error)) = load(&env, &[]) else {
                panic!("app keys without the app accepted: {env:?}")
            };
            assert!(
                error.contains("requires SAPFLOW_PUBLISH_AUTH_URL"),
                "{error}"
            );
        }
        let scoped = [
            ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:9000"),
            (
                "SAPFLOW_CONTROL_TOKEN",
                "independent-control-token-32-bytes-minimum",
            ),
            ("SAPFLOW_SESSION_STORE", "/private/session-authority.json"),
            on,
        ];
        assert!(matches!(
            load(&scoped, &[]),
            Err(ConfigError::Invalid(m)) if m.contains("scoped control mode")
        ));
        assert!(HELP.contains("SAPFLOW_SRT_APP_KEYS"));
    }

    #[test]
    fn scoped_mode_keeps_the_replay_store_beside_the_session_store() {
        let scoped = |extra: &[(&'static str, &'static str)]| {
            let mut env = vec![
                ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:9000"),
                (
                    "SAPFLOW_CONTROL_TOKEN",
                    "independent-control-token-32-bytes-minimum",
                ),
                ("SAPFLOW_SESSION_STORE", "/var/lib/sapflow/sessions.json"),
            ];
            env.extend_from_slice(extra);
            load(&env, &[])
        };
        let c = scoped(&[]).expect("scoped");
        assert_eq!(
            c.srt_replay_store.as_deref(),
            Some(std::path::Path::new("/var/lib/sapflow/srt-replay"))
        );
        let c = scoped(&[("SAPFLOW_SRT_REPLAY_STORE", "/srv/replay/digests")]).expect("own");
        assert_eq!(
            c.srt_replay_store.as_deref(),
            Some(std::path::Path::new("/srv/replay/digests"))
        );
        for replay in [
            "/var/lib/sapflow/sessions.json",
            "/var/lib/sapflow/sessions.json.lock",
            "/var/lib/sapflow/.sessions.json.tmp-0011",
        ] {
            let Err(ConfigError::Invalid(error)) = scoped(&[("SAPFLOW_SRT_REPLAY_STORE", replay)])
            else {
                panic!("{replay} accepted beside the session store")
            };
            assert!(error.contains("would share a file"), "{error}");
        }
        // A session store named for the default replay store's own files.
        for session in [
            "/var/lib/sapflow/srt-replay",
            "/var/lib/sapflow/srt-replay.lock",
            "/var/lib/sapflow/.srt-replay.tmp-0011223344556677",
        ] {
            let env = [
                ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:9000"),
                (
                    "SAPFLOW_CONTROL_TOKEN",
                    "independent-control-token-32-bytes-minimum",
                ),
                ("SAPFLOW_SESSION_STORE", session),
            ];
            let Err(ConfigError::Invalid(error)) = load(&env, &[]) else {
                panic!("session store {session} accepted")
            };
            assert!(error.contains("would share a file"), "{error}");
        }
        // Another directory is fine whatever the names.
        assert!(scoped(&[("SAPFLOW_SRT_REPLAY_STORE", "/srv/r/sessions.json")]).is_ok());
        // Outside scoped mode and app keys there is no store by default.
        let open = load(&[("SAPFLOW_OPEN_PUBLISH", "1")], &[]).expect("open");
        assert!(open.srt_replay_store.is_none());
    }

    #[test]
    fn srt_app_keys_need_an_absolute_replay_store() {
        let url = ("SAPFLOW_PUBLISH_AUTH_URL", "http://app/auth?token=t");
        let on = ("SAPFLOW_SRT_APP_KEYS", "1");
        let Err(ConfigError::Invalid(error)) = load(&[url, on], &[]) else {
            panic!("app keys without a replay store accepted")
        };
        assert!(
            error.starts_with("SAPFLOW_SRT_APP_KEYS=1 requires SAPFLOW_SRT_REPLAY_STORE"),
            "{error}"
        );
        for relative in ["replay", "srt/replay", "/"] {
            let Err(ConfigError::Invalid(error)) =
                load(&[url, on, ("SAPFLOW_SRT_REPLAY_STORE", relative)], &[])
            else {
                panic!("{relative} accepted")
            };
            assert_eq!(
                error,
                "SAPFLOW_SRT_REPLAY_STORE: absolute file path required"
            );
        }
        let c = load(
            &[url, on, ("SAPFLOW_SRT_REPLAY_STORE", "/var/lib/s/replay")],
            &[],
        )
        .expect("with a store");
        assert_eq!(
            c.srt_replay_store.as_deref(),
            Some(std::path::Path::new("/var/lib/s/replay"))
        );
        assert!(HELP.contains("SAPFLOW_SRT_REPLAY_STORE"));
    }

    #[test]
    fn required_encryption_without_a_passphrase_needs_app_keys() {
        let url = ("SAPFLOW_PUBLISH_AUTH_URL", "http://app/auth?token=t");
        let on = ("SAPFLOW_SRT_APP_KEYS", "1");
        let store = ("SAPFLOW_SRT_REPLAY_STORE", "/var/lib/sapflow-srt/replay");
        let required = ("SAPFLOW_SRT_ENCRYPTION", "required");
        let c = load(&[url, on, store, required], &[]).expect("app keys, required");
        assert!(c.srt_require_encryption && c.srt_crypto.is_none() && !c.srt_allow_plaintext);
        let listener = crate::srt_own::listener_config_for_test(&c);
        assert!(listener.require_encryption && listener.app_keys && listener.crypto.is_none());
        // Without app keys an explicit `required` with nothing to require is
        // refused, rather than left meaning plaintext.
        for env in [
            vec![url, required],
            vec![("SAPFLOW_OPEN_PUBLISH", "1"), required],
        ] {
            let Err(ConfigError::Invalid(error)) = load(&env, &[]) else {
                panic!("required without a key accepted: {env:?}")
            };
            assert_eq!(
                error,
                "SAPFLOW_SRT_ENCRYPTION=required needs SAPFLOW_SRT_PASSPHRASE or SAPFLOW_SRT_APP_KEYS=1"
            );
        }
        assert!(load(&[url], &["--srt-encryption=required"]).is_err());
        // Unset still means plaintext without a passphrase, app keys or not.
        let listener =
            crate::srt_own::listener_config_for_test(&load(&[url, on, store], &[]).expect("on"));
        assert!(!listener.require_encryption && listener.app_keys);
        let listener = crate::srt_own::listener_config_for_test(&load(&[url], &[]).expect("off"));
        assert!(!listener.require_encryption && !listener.app_keys);
        // Optional, with app keys and no passphrase, says the same.
        let c = load(
            &[url, on, store, ("SAPFLOW_SRT_ENCRYPTION", "optional")],
            &[],
        )
        .expect("optional");
        assert!(!c.srt_require_encryption && !c.srt_allow_plaintext);
        // A global passphrase alongside app keys keeps its meaning.
        let pass = ("SAPFLOW_SRT_PASSPHRASE", "deployment secret");
        let c = load(
            &[url, on, store, pass, ("SAPFLOW_SRT_ENCRYPTION", "optional")],
            &[],
        )
        .expect("both");
        assert!(c.srt_allow_plaintext && !c.srt_require_encryption && c.srt_app_keys);
        let c = load(&[url, on, store, pass], &[]).expect("both, required");
        assert!(c.srt_require_encryption);
    }

    #[test]
    fn scoped_mode_refuses_app_authorisation() {
        let env = [
            ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:9000"),
            (
                "SAPFLOW_CONTROL_TOKEN",
                "independent-control-token-32-bytes-minimum",
            ),
            ("SAPFLOW_SESSION_STORE", "/private/session-authority.json"),
            ("SAPFLOW_PUBLISH_AUTH_URL", "http://app/auth?token=t"),
        ];
        assert!(matches!(
            load(&env, &[]),
            Err(ConfigError::Invalid(m)) if m.contains("scoped control mode")
        ));
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
