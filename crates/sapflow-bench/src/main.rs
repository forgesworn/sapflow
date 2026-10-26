//! Simulated LL-HLS viewers, for the M4 comparison with mediamtx.
//!
//! Each viewer behaves like hls.js in low-latency mode: it follows the
//! media playlist with blocking reloads (`_HLS_msn`, `_HLS_part`) and fetches
//! each preload-hinted part, on its own connection. Every few reloads it
//! also makes a plain playlist request and fetches the newest whole segment.
//!
//! Reports response times for plain playlist and segment requests, and the
//! fan-out spread: for each part, how long after the first viewer received
//! it the last one did.
//!
//!     sapflow-bench --url http://127.0.0.1:8888/live/event-x/index.m3u8 \
//!         --viewers 250 --secs 60
//!
//! Behind a TLS front such as Caddy, as production runs: an `https://` URL
//! with the CA that signed the front's certificate. Add `--http2` for one
//! HTTP/2 connection per viewer carrying all its requests as streams, as
//! Safari and hls.js use; without it the viewers speak HTTP/1.1 over TLS.
//!
//!     sapflow-bench --url https://localhost:8943/live-hls/live/event-x/index.m3u8 \
//!         --ca caddy-root.crt --http2 --viewers 250 --secs 60
//!
//! `--slow N --slow-rate BYTES_PER_SEC` makes N of the full-media viewers slow
//! consumers. Each reads its response bodies frame by frame, never faster than
//! the rate, so the server sees TCP backpressure. They behave as the other
//! viewers otherwise, are counted in `--viewers`, and are reported in a
//! section of their own; the figures for the other viewers exclude them.
//!
//!     sapflow-bench --url http://127.0.0.1:8888/live/event-x/index.m3u8 \
//!         --viewers 250 --secs 60 --slow 5 --slow-rate 50000

mod tls;

#[cfg(test)]
mod front_tests;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, Response, StatusCode, Version};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connect, HttpConnector, HttpInfo};
use hyper_util::rt::TokioExecutor;

#[derive(Default)]
struct Stats {
    window: Option<CollectionWindow>,
    excluded_requests: u64,
    excluded_errors: u64,
    drained_requests: u64,
    max_drain: Duration,
    playlist: Vec<Duration>,
    segment: Vec<Duration>,
    /// When each viewer finished each part, by part URI.
    parts: HashMap<String, Vec<Instant>>,
    /// When each viewer's blocking reload for (msn, part) returned.
    playlist_updates: HashMap<(u64, u64), Vec<Instant>>,
    errors: u64,
    /// Errors by kind and the kind of URL, for diagnosis.
    error_kinds: HashMap<String, u64>,
    requests: u64,
    bytes: u64,
    /// Responses by HTTP version, to show what the viewers really spoke.
    versions: BTreeMap<&'static str, u64>,
    /// Local addresses of the connections responses arrived on: one per
    /// connection used, so how many connections the viewers needed.
    connections: HashSet<SocketAddr>,
}

/// Admit by request start; retain completions after the interval to avoid clipping tails.
#[derive(Clone, Copy)]
struct CollectionWindow {
    start: Instant,
    end: Instant,
}

impl CollectionWindow {
    fn admits(self, started: Instant) -> bool {
        started >= self.start && started < self.end
    }
}

struct Download {
    body: Bytes,
    took: Duration,
    url: String,
    finished: Instant,
    measured: bool,
}

#[derive(Debug)]
struct Args {
    url: String,
    viewers: usize,
    secs: u64,
    ramp: u64,
    /// Viewers that fetch media; the rest follow the playlist only.
    full: Option<usize>,
    /// PEM file with the CA that signed the front's certificate (`https://`).
    ca: Option<String>,
    /// HTTP/2 over TLS, one connection per viewer (`https://`).
    http2: bool,
    /// Full-media viewers that read their response bodies slowly. They are
    /// counted in `viewers` and reported apart from the others.
    slow: usize,
    /// The most bytes a second a slow viewer reads from one response body.
    slow_rate: Option<u64>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        url: String::new(),
        viewers: 10,
        secs: 30,
        ramp: 5,
        full: None,
        ca: None,
        http2: false,
        slow: 0,
        slow_rate: None,
    };
    let mut it = args.into_iter();
    while let Some(k) = it.next() {
        if k == "--http2" {
            a.http2 = true;
            continue;
        }
        let v = it.next().ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--url" => a.url = v,
            "--viewers" => a.viewers = v.parse().map_err(|_| "--viewers: a number")?,
            "--secs" => a.secs = v.parse().map_err(|_| "--secs: a number")?,
            "--ramp" => a.ramp = v.parse().map_err(|_| "--ramp: a number")?,
            "--full" => a.full = Some(v.parse().map_err(|_| "--full: a number")?),
            "--ca" => a.ca = Some(v),
            "--slow" => a.slow = v.parse().map_err(|_| "--slow: a number")?,
            "--slow-rate" => a.slow_rate = Some(v.parse().map_err(|_| "--slow-rate: a number")?),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if a.viewers == 0 || a.secs == 0 {
        return Err("--viewers and --secs must be positive".into());
    }
    let tls = a.url.starts_with("https://");
    if !tls && !a.url.starts_with("http://") {
        return Err("--url http(s)://.../index.m3u8 is required".into());
    }
    if tls && a.ca.is_none() {
        return Err(
            "an https:// URL needs --ca <pem>, the CA that signed the front's certificate \
             (Caddy's internal root, or a system bundle such as /etc/ssl/cert.pem)"
                .into(),
        );
    }
    if !tls && a.http2 {
        return Err(
            "--http2 needs an https:// URL: sapflow itself serves HTTP/1.1, HTTP/2 comes from the TLS front"
                .into(),
        );
    }
    if !tls && a.ca.is_some() {
        return Err("--ca only applies to an https:// URL".into());
    }
    let full_viewers = a.full.map_or(a.viewers, |n| n.min(a.viewers));
    if a.slow > full_viewers {
        return Err(format!(
            "--slow {} exceeds the {full_viewers} full-media viewers",
            a.slow
        ));
    }
    match (a.slow, a.slow_rate) {
        (_, Some(0)) => return Err("--slow-rate must be positive".into()),
        (0, Some(_)) => return Err("--slow-rate only applies with --slow N (N > 0)".into()),
        (_, None) if a.slow > 0 => {
            return Err("--slow needs --slow-rate <bytes per second>".into());
        }
        _ => {}
    }
    Ok(a)
}

#[tokio::main]
async fn main() {
    let a = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sapflow-bench: {e}");
            std::process::exit(2);
        }
    };
    match run(&a).await {
        Ok(out) => report(&a, &out),
        Err(e) => {
            eprintln!("sapflow-bench: {e}");
            std::process::exit(2);
        }
    }
}

/// What the viewers saw: the ordinary viewers, and the slow consumers kept
/// apart so they cannot colour the figures the older receipts compare.
#[derive(Default)]
struct Outcome {
    normal: Stats,
    slow: Stats,
}

/// Runs the viewers to the end of the measurement and returns what they saw.
async fn run(a: &Args) -> Result<Outcome, String> {
    let start = Instant::now()
        .checked_add(Duration::from_secs(a.ramp))
        .ok_or("ramp is too long")?;
    let window = CollectionWindow {
        start,
        end: start
            .checked_add(Duration::from_secs(a.secs))
            .ok_or("measurement is too long")?,
    };
    let cohort = || {
        Arc::new(Mutex::new(Stats {
            window: Some(window),
            ..Stats::default()
        }))
    };
    let stats = Cohorts {
        normal: cohort(),
        slow: cohort(),
    };
    let tasks = match &a.ca {
        Some(path) => {
            let pem = std::fs::read(path).map_err(|e| format!("cannot read --ca {path}: {e}"))?;
            let config = tls::client_config(&pem).map_err(|e| format!("--ca {path}: {e}"))?;
            let http2 = a.http2;
            spawn_viewers(a, &stats, window, move || tls::https_client(&config, http2))
        }
        None => spawn_viewers(a, &stats, window, plain_client),
    };
    for t in tasks {
        let _ = t.await;
    }
    Ok(Outcome {
        normal: std::mem::take(&mut *lock(&stats.normal)),
        slow: std::mem::take(&mut *lock(&stats.slow)),
    })
}

/// Where each kind of viewer records what it sees.
struct Cohorts {
    normal: Arc<Mutex<Stats>>,
    slow: Arc<Mutex<Stats>>,
}

/// HTTP/1.1 over plain TCP, as the benchmarks have always run.
fn plain_client() -> Client<HttpConnector, Empty<Bytes>> {
    Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(4)
        .build_http()
}

/// Starts the viewers, ramped in, each with a client of its own: its own
/// connection, as a player has.
fn spawn_viewers<C>(
    a: &Args,
    stats: &Cohorts,
    window: CollectionWindow,
    make_client: impl Fn() -> Client<C, Empty<Bytes>>,
) -> Vec<tokio::task::JoinHandle<()>>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    let deadline = window.end;
    let mut tasks = Vec::new();
    for i in 0..a.viewers {
        // The first `slow` viewers are the slow ones: `--slow` is at most the
        // full-media count, and the full-media viewers are the first ones.
        let slow = i < a.slow;
        let stats = if slow { &stats.slow } else { &stats.normal }.clone();
        let pace = if slow { a.slow_rate } else { None };
        let url = a.url.clone();
        let client = make_client();
        let full = a.full.is_none_or(|n| i < n);
        let delay = Duration::from_millis(a.ramp * 1000 * i as u64 / a.viewers.max(1) as u64);
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            viewer(&client, &url, deadline, &stats, full, pace).await;
        }));
    }
    tasks
}

fn lock(stats: &Mutex<Stats>) -> std::sync::MutexGuard<'_, Stats> {
    stats.lock().unwrap_or_else(|e| e.into_inner())
}

async fn get<C>(
    client: &Client<C, Empty<Bytes>>,
    url: &str,
    stats: &Mutex<Stats>,
    pace: Option<u64>,
) -> Option<Download>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    let mut url = url.to_owned();
    // Follow at most one redirect (mediamtx's session check).
    for _ in 0..2 {
        let start = Instant::now();
        if lock(stats).window.is_some_and(|window| start >= window.end) {
            return None;
        }
        let req = Request::get(&url).body(Empty::new()).ok()?;
        let resp = tokio::time::timeout(Duration::from_secs(15), client.request(req)).await;
        let resp = match resp {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                record_request(
                    stats,
                    start,
                    Instant::now(),
                    None,
                    0,
                    Some((&url, &format!("client error: {}", describe(&e)))),
                );
                return None;
            }
            Err(_) => {
                record_request(
                    stats,
                    start,
                    Instant::now(),
                    None,
                    0,
                    Some((&url, "timeout")),
                );
                return None;
            }
        };
        let (parts, body) = resp.into_parts();
        let resp = Response::from_parts(parts, ());
        if resp.status().is_redirection() {
            record_request(stats, start, Instant::now(), Some(&resp), 0, None);
            let loc = resp.headers().get("location")?.to_str().ok()?.to_owned();
            url = join(&url, &loc);
            continue;
        }
        if resp.status() != StatusCode::OK {
            record_request(
                stats,
                start,
                Instant::now(),
                Some(&resp),
                0,
                Some((&url, &format!("HTTP {}", resp.status().as_u16()))),
            );
            return None;
        }
        let body = match tokio::time::timeout(Duration::from_secs(15), read_body(body, pace)).await
        {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                record_request(
                    stats,
                    start,
                    Instant::now(),
                    Some(&resp),
                    0,
                    Some((&url, &format!("body error: {}", describe(&e)))),
                );
                return None;
            }
            Err(_) => {
                record_request(
                    stats,
                    start,
                    Instant::now(),
                    Some(&resp),
                    0,
                    Some((&url, "body timeout")),
                );
                return None;
            }
        };
        let finished = Instant::now();
        let measured = record_request(stats, start, finished, Some(&resp), body.len() as u64, None);
        return Some(Download {
            body,
            took: finished.duration_since(start),
            url,
            finished,
            measured,
        });
    }
    None
}

/// Reads a response body. With a `pace` in bytes a second it reads frame by
/// frame and, after each, waits until the bytes so far are due at that rate, so
/// a slow consumer leaves the rest in the connection and the server sees the
/// backpressure. The caller's timeout covers the waits as well.
async fn read_body<B>(mut body: B, pace: Option<u64>) -> Result<Bytes, B::Error>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let Some(rate) = pace else {
        return Ok(body.collect().await?.to_bytes());
    };
    let started = Instant::now();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            bytes.extend_from_slice(&data);
            tokio::time::sleep(pace_delay(bytes.len() as u64, rate, started.elapsed())).await;
        }
    }
    Ok(Bytes::from(bytes))
}

/// How much longer a paced reader waits, `elapsed` into a body of which it has
/// read `bytes`, for `bytes` to be due at `rate` bytes a second. Zero once it
/// is on or behind schedule. A zero rate is treated as one byte a second.
fn pace_delay(bytes: u64, rate: u64, elapsed: Duration) -> Duration {
    let rate = u128::from(rate.max(1));
    let nanos = u128::from(bytes) * 1_000_000_000 / rate;
    let due = Duration::new(
        u64::try_from(nanos / 1_000_000_000).unwrap_or(u64::MAX),
        (nanos % 1_000_000_000) as u32,
    );
    due.saturating_sub(elapsed)
}

fn record_request(
    stats: &Mutex<Stats>,
    started: Instant,
    finished: Instant,
    response: Option<&Response<()>>,
    bytes: u64,
    failure: Option<(&str, &str)>,
) -> bool {
    let mut st = lock(stats);
    let measured = st.window.is_none_or(|window| window.admits(started));
    if measured {
        st.requests += 1;
        st.bytes += bytes;
        if let Some(window) = st.window
            && finished > window.end
        {
            st.drained_requests += 1;
            st.max_drain = st.max_drain.max(finished.duration_since(window.end));
        }
        if let Some(response) = response {
            note_response(&mut st, response);
        }
    } else {
        st.excluded_requests += 1;
    }
    if let Some((url, kind)) = failure {
        if measured {
            st.errors += 1;
        } else {
            st.excluded_errors += 1;
        }
        fail(&mut st, url, kind, measured);
    }
    measured
}

/// An error with its causes, innermost last: hyper's own text for a failed
/// connection is only "client error (Connect)", and the TLS reason is below.
fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

fn version_label(v: Version) -> &'static str {
    match v {
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_10 => "HTTP/1.0",
        _ => "other",
    }
}

/// Counts the HTTP version and connection a response arrived on.
fn note_response<T>(st: &mut Stats, resp: &Response<T>) {
    *st.versions
        .entry(version_label(resp.version()))
        .or_default() += 1;
    if let Some(info) = resp.extensions().get::<HttpInfo>() {
        st.connections.insert(info.local_addr());
    }
}

fn fail(st: &mut Stats, url: &str, kind: &str, measured: bool) {
    let what = if url.contains("_HLS_msn") {
        "blocking playlist"
    } else if url.contains(".m3u8") {
        "playlist"
    } else if url.contains("part") {
        "part"
    } else {
        "segment"
    };
    let prefix = if measured { "" } else { "outside window: " };
    *st.error_kinds
        .entry(format!("{prefix}{what}: {kind}"))
        .or_default() += 1;
}

/// Resolves a playlist URI against the playlist's own URL.
fn join(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_owned();
    }
    if let Some(path) = uri.strip_prefix('/') {
        let origin_end = base
            .find("://")
            .and_then(|i| base[i + 3..].find('/').map(|j| i + 3 + j))
            .unwrap_or(base.len());
        return format!("{}/{path}", &base[..origin_end]);
    }
    let base = base.split('?').next().unwrap_or(base);
    let dir = &base[..base.rfind('/').map_or(base.len(), |i| i + 1)];
    format!("{dir}{uri}")
}

/// What a viewer needs from a media playlist.
#[derive(Debug, Default, PartialEq)]
struct Media {
    /// msn and part index of the part the preload hint names.
    next: (u64, u64),
    hint: Option<String>,
    last_segment: Option<String>,
}

fn parse_media(text: &str) -> Media {
    let mut seq = 0u64;
    let mut segments = 0u64;
    let mut open_parts = 0u64;
    let mut m = Media::default();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = v.trim().parse().unwrap_or(0);
        } else if line.starts_with("#EXTINF:") {
            segments += 1;
            open_parts = 0;
            if let Some(uri) = lines.next()
                && !uri.starts_with('#')
                && !uri.contains("gap")
            {
                m.last_segment = Some(uri.to_owned());
            }
        } else if line.starts_with("#EXT-X-PART:") {
            open_parts += 1;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PRELOAD-HINT:") {
            m.hint = attr(rest, "URI");
        }
    }
    m.next = (seq + segments, open_parts);
    m
}

fn attr(list: &str, name: &str) -> Option<String> {
    let start = list.find(&format!("{name}=\""))? + name.len() + 2;
    let end = list[start..].find('"')? + start;
    Some(list[start..end].to_owned())
}

/// `full`: fetch parts and segments as a player does; otherwise follow the
/// playlist only (blocking reloads), for networks too slow to carry media to
/// every simulated viewer. `pace`: read every response body at no more than
/// that many bytes a second (a slow consumer).
async fn viewer<C>(
    client: &Client<C, Empty<Bytes>>,
    index_url: &str,
    deadline: Instant,
    stats: &Arc<Mutex<Stats>>,
    full: bool,
    pace: Option<u64>,
) where
    C: Connect + Clone + Send + Sync + 'static,
{
    let Some(master) = get(client, index_url, stats, pace).await else {
        return;
    };
    let Some(variant) = String::from_utf8_lossy(&master.body)
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
    else {
        return;
    };
    let media_url = join(&master.url, &variant);
    let sep = if media_url.contains('?') { '&' } else { '?' };

    let mut loops = 0u64;
    let mut media: Option<Media> = None;
    while Instant::now() < deadline {
        // Every fifth reload, and at the start: a plain playlist request and
        // the newest whole segment.
        if loops.is_multiple_of(5) || media.is_none() {
            let Some(download) = get(client, &media_url, stats, pace).await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                media = None;
                loops += 1;
                continue;
            };
            if download.measured {
                lock(stats).playlist.push(download.took);
            }
            let m = parse_media(&String::from_utf8_lossy(&download.body));
            if full
                && let Some(seg) = &m.last_segment
                && let Some(download) = get(client, &join(&media_url, seg), stats, pace).await
                && download.measured
            {
                lock(stats).segment.push(download.took);
            }
            media = Some(m);
        }
        loops += 1;
        let Some(m) = &media else { continue };
        let Some(hint) = m.hint.clone() else {
            media = None;
            continue;
        };
        // Blocking reload for the hinted part, and the part itself, at once.
        let pl_url = format!(
            "{media_url}{sep}_HLS_msn={}&_HLS_part={}",
            m.next.0, m.next.1
        );
        let part_url = join(&media_url, &hint);
        let fetch_part = async {
            if full {
                get(client, &part_url, stats, pace).await
            } else {
                None
            }
        };
        let (pl, part) = tokio::join!(get(client, &pl_url, stats, pace), fetch_part);
        if let Some(download) = &pl
            && download.measured
        {
            // When each viewer learned of the new part (all viewers).
            lock(stats)
                .playlist_updates
                .entry((m.next.0, m.next.1))
                .or_default()
                .push(download.finished);
        }
        if let Some(download) = &part
            && download.measured
        {
            // Key by path: mediamtx gives every viewer its own session query.
            let key = hint.split('?').next().unwrap_or(&hint).to_owned();
            lock(stats)
                .parts
                .entry(key)
                .or_default()
                .push(download.finished);
        }
        media = pl.map(|download| parse_media(&String::from_utf8_lossy(&download.body)));
    }
}

fn pct(v: &mut [Duration], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i].as_secs_f64() * 1000.0
}

/// The responses by HTTP version, like "HTTP/2 41230".
fn protocols(st: &Stats) -> String {
    if st.versions.is_empty() {
        return "none".into();
    }
    st.versions
        .iter()
        .map(|(v, n)| format!("{v} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn report(a: &Args, out: &Outcome) {
    let st = &out.normal;
    // The viewers the figures below cover: all of them unless some are slow.
    let normal_viewers = a.viewers - a.slow;
    let mut playlist = st.playlist.clone();
    let mut segment = st.segment.clone();
    let mut spread: Vec<Duration> = Vec::new();
    let mut reach = Vec::new();
    for times in st.parts.values() {
        if times.len() < 2 {
            continue;
        }
        let (Some(first), Some(last)) = (times.iter().min(), times.iter().max()) else {
            continue;
        };
        spread.push(last.duration_since(*first));
        reach.push(times.len());
    }
    let mean_reach = reach.iter().sum::<usize>() as f64 / reach.len().max(1) as f64;
    let mut pl_spread: Vec<Duration> = st
        .playlist_updates
        .values()
        .filter(|t| t.len() >= 2)
        .filter_map(|t| Some(t.iter().max()?.duration_since(*t.iter().min()?)))
        .collect();
    println!(
        "viewers {}  measured {} s after a {} s ramp",
        a.viewers, a.secs, a.ramp
    );
    println!("collection requests started within the fixed post-ramp window, including drain");
    println!(
        "excluded requests {}  errors {}",
        st.excluded_requests, st.excluded_errors
    );
    println!(
        "drained requests {} max {:.6} s",
        st.drained_requests,
        st.max_drain.as_secs_f64()
    );
    println!(
        "requests {}  errors {}  received {:.1} MB",
        st.requests,
        st.errors,
        st.bytes as f64 / 1e6
    );
    for (kind, n) in &st.error_kinds {
        println!("  error {kind}: {n}");
    }
    println!(
        "responses {}  connections {} ({:.2} per viewer)",
        protocols(st),
        st.connections.len(),
        st.connections.len() as f64 / normal_viewers.max(1) as f64
    );
    for (name, v) in [("playlist", &mut playlist), ("segment", &mut segment)] {
        println!(
            "{name:<9} n={:<6} p50 {:.1} ms  p95 {:.1} ms  p99 {:.1} ms  max {:.1} ms",
            v.len(),
            pct(v, 0.5),
            pct(v, 0.95),
            pct(v, 0.99),
            pct(v, 1.0)
        );
    }
    println!(
        "playlist update fan-out (first to last viewer) updates={}  p50 {:.1} ms  p95 {:.1} ms  max {:.1} ms",
        pl_spread.len(),
        pct(&mut pl_spread, 0.5),
        pct(&mut pl_spread, 0.95),
        pct(&mut pl_spread, 1.0)
    );
    println!(
        "part fan-out (first to last viewer) parts={} reach {:.0}/{}  p50 {:.1} ms  p95 {:.1} ms  max {:.1} ms",
        spread.len(),
        mean_reach,
        normal_viewers,
        pct(&mut spread, 0.5),
        pct(&mut spread, 0.95),
        pct(&mut spread, 1.0)
    );
    if a.slow > 0 {
        report_slow(a, &out.slow);
    }
}

/// The slow consumers, in lines of their own: every one starts `slow ` so none
/// can be mistaken for the figures of the other viewers above.
fn report_slow(a: &Args, st: &Stats) {
    let mut playlist = st.playlist.clone();
    let mut segment = st.segment.clone();
    let parts: usize = st.parts.values().map(Vec::len).sum();
    println!(
        "slow viewers {} of {}  rate {} B/s each; the figures above cover the other {}",
        a.slow,
        a.viewers,
        a.slow_rate.unwrap_or(0),
        a.viewers - a.slow
    );
    println!(
        "slow excluded requests {}  errors {}",
        st.excluded_requests, st.excluded_errors
    );
    println!(
        "slow requests {}  errors {}  received {:.1} MB",
        st.requests,
        st.errors,
        st.bytes as f64 / 1e6
    );
    for (kind, n) in &st.error_kinds {
        println!("  slow error {kind}: {n}");
    }
    for (name, v) in [("playlist", &mut playlist), ("segment ", &mut segment)] {
        println!(
            "slow {name} n={:<6} p50 {:.1} ms  p99 {:.1} ms",
            v.len(),
            pct(v, 0.5),
            pct(v, 0.99)
        );
    }
    println!("slow parts fetched {parts}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_excludes_ramp_and_post_deadline_starts_but_retains_slow_completions() {
        let start = Instant::now() + Duration::from_secs(10);
        let end = start + Duration::from_secs(60);
        let stats = Mutex::new(Stats {
            window: Some(CollectionWindow { start, end }),
            ..Stats::default()
        });
        let response = Response::builder()
            .version(Version::HTTP_2)
            .body(())
            .expect("response");
        for (begin, finish) in [
            (
                start - Duration::from_secs(2),
                start - Duration::from_secs(1),
            ),
            (
                start - Duration::from_millis(1),
                start + Duration::from_millis(1),
            ),
            (end, end + Duration::from_millis(1)),
        ] {
            assert!(!record_request(
                &stats,
                begin,
                finish,
                Some(&response),
                100,
                None
            ));
        }
        assert!(record_request(
            &stats,
            start,
            end,
            Some(&response),
            200,
            None
        ));
        assert!(record_request(
            &stats,
            end - Duration::from_millis(1),
            end + Duration::from_secs(2),
            Some(&response),
            100,
            None
        ));
        let st = lock(&stats);
        assert_eq!((st.requests, st.bytes, st.excluded_requests), (2, 300, 3));
        assert_eq!(
            (st.drained_requests, st.max_drain),
            (1, Duration::from_secs(2))
        );
        assert_eq!(protocols(&st), "HTTP/2 2");
    }

    #[test]
    fn an_admitted_request_failing_after_the_window_is_a_measured_failure() {
        let start = Instant::now();
        let end = start + Duration::from_secs(1);
        let stats = Mutex::new(Stats {
            window: Some(CollectionWindow { start, end }),
            ..Stats::default()
        });
        assert!(record_request(
            &stats,
            end - Duration::from_millis(1),
            end + Duration::from_secs(1),
            None,
            0,
            Some(("http://test/part.mp4", "body timeout"))
        ));
        let st = lock(&stats);
        assert_eq!(
            (
                st.requests,
                st.errors,
                st.excluded_errors,
                st.drained_requests
            ),
            (1, 1, 0, 1)
        );
        assert_eq!(st.error_kinds["part: body timeout"], 1);
    }

    #[test]
    fn failures_outside_collection_remain_visible_without_polluting_percentiles() {
        let start = Instant::now() + Duration::from_secs(1);
        let stats = Mutex::new(Stats {
            window: Some(CollectionWindow {
                start,
                end: start + Duration::from_secs(1),
            }),
            ..Stats::default()
        });
        assert!(!record_request(
            &stats,
            start - Duration::from_secs(1),
            start,
            None,
            0,
            Some(("http://test/stream.m3u8", "timeout"))
        ));
        let st = lock(&stats);
        assert_eq!(
            (
                st.requests,
                st.errors,
                st.excluded_requests,
                st.excluded_errors
            ),
            (0, 0, 1, 1)
        );
        assert_eq!(st.error_kinds["outside window: playlist: timeout"], 1);
        assert!(st.playlist.is_empty() && st.segment.is_empty() && st.parts.is_empty());
    }

    #[test]
    fn joins_relative_uris() {
        let b = "http://h:8888/live/event-a/stream.m3u8?x=1";
        assert_eq!(
            join(b, "p_part3.mp4"),
            "http://h:8888/live/event-a/p_part3.mp4"
        );
        assert_eq!(join(b, "/other/x"), "http://h:8888/other/x");
        assert_eq!(join(b, "http://o/x"), "http://o/x");
    }

    #[test]
    fn joins_against_an_https_base() {
        let b = "https://h:8943/live-hls/live/event-a/stream.m3u8?_HLS_msn=3&_HLS_part=1";
        assert_eq!(
            join(b, "p_part3.mp4"),
            "https://h:8943/live-hls/live/event-a/p_part3.mp4"
        );
        assert_eq!(join(b, "/other/x"), "https://h:8943/other/x");
        assert_eq!(join(b, "https://o/x"), "https://o/x");
    }

    fn parse(args: &str) -> Result<Args, String> {
        parse_args(args.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn plain_http_is_the_default() {
        let a = parse("--url http://127.0.0.1:8888/x.m3u8").expect("plain http parses");
        assert!(!a.http2);
        assert_eq!(a.ca, None);
        assert_eq!((a.viewers, a.secs, a.ramp, a.full), (10, 30, 5, None));
        let a = parse("--url http://h/x --viewers 250 --secs 60 --ramp 10 --full 50")
            .expect("options parse");
        assert_eq!((a.viewers, a.secs, a.ramp, a.full), (250, 60, 10, Some(50)));
    }

    #[test]
    fn https_needs_a_ca_and_takes_http2_as_a_flag() {
        let a = parse("--url https://h:8943/x.m3u8 --ca root.pem").expect("https parses");
        assert!(!a.http2, "HTTP/1.1 over TLS unless asked");
        assert_eq!(a.ca.as_deref(), Some("root.pem"));
        // The bare flag may sit anywhere, including before an option.
        let a = parse("--http2 --url https://h/x --viewers 3 --ca root.pem").expect("flag parses");
        assert!(a.http2);
        assert_eq!((a.viewers, a.ca.as_deref()), (3, Some("root.pem")));
        let a = parse("--url https://h/x --ca root.pem --http2").expect("trailing flag parses");
        assert!(a.http2);
    }

    #[test]
    fn rejects_inconsistent_options() {
        let e = parse("--url https://h/x").expect_err("no CA");
        assert!(e.contains("--ca"), "{e}");
        let e = parse("--url http://h/x --http2").expect_err("h2 needs TLS");
        assert!(e.contains("--http2") && e.contains("https://"), "{e}");
        let e = parse("--url http://h/x --ca root.pem").expect_err("CA needs TLS");
        assert!(e.contains("--ca"), "{e}");
        assert!(parse("--http2").is_err(), "no URL");
        assert!(parse("--url ftp://h/x").is_err());
        assert!(parse("--url http://h/x --viewers many").is_err());
        assert!(parse("--url http://h/x --nope 1").is_err());
        assert!(parse("--url").is_err(), "a value is missing");
    }

    #[test]
    fn names_the_http_version() {
        assert_eq!(version_label(Version::HTTP_2), "HTTP/2");
        assert_eq!(version_label(Version::HTTP_11), "HTTP/1.1");
        assert_eq!(version_label(Version::HTTP_3), "other");
    }

    #[test]
    fn tallies_responses_by_version() {
        let mut st = Stats::default();
        assert_eq!(protocols(&st), "none");
        let resp = |v| Response::builder().version(v).body(()).expect("response");
        let stats = Mutex::new(std::mem::take(&mut st));
        note_response(&mut lock(&stats), &resp(Version::HTTP_2));
        note_response(&mut lock(&stats), &resp(Version::HTTP_2));
        note_response(&mut lock(&stats), &resp(Version::HTTP_11));
        assert_eq!(protocols(&lock(&stats)), "HTTP/1.1 1, HTTP/2 2");
        // No connection information on a hand-made response.
        assert!(lock(&stats).connections.is_empty());
    }

    #[test]
    fn slow_options_default_to_off() {
        let a = parse("--url http://h/x").expect("parses");
        assert_eq!((a.slow, a.slow_rate), (0, None));
        let a = parse("--url http://h/x --viewers 20 --slow 5 --slow-rate 50000").expect("parses");
        assert_eq!((a.viewers, a.slow, a.slow_rate), (20, 5, Some(50_000)));
        // As many slow viewers as full-media viewers is allowed.
        let a =
            parse("--url http://h/x --viewers 20 --full 8 --slow 8 --slow-rate 1").expect("all");
        assert_eq!((a.full, a.slow), (Some(8), 8));
        let a = parse("--url http://h/x --viewers 4 --slow 4 --slow-rate 1").expect("all");
        assert_eq!(a.slow, 4);
        // `--full` beyond `--viewers` still means every viewer.
        parse("--url http://h/x --viewers 4 --full 99 --slow 4 --slow-rate 1").expect("capped");
        // `--slow 0` is the same as leaving it off.
        let a = parse("--url http://h/x --slow 0").expect("parses");
        assert_eq!((a.slow, a.slow_rate), (0, None));
    }

    #[test]
    fn rejects_inconsistent_slow_options() {
        let e = parse("--url http://h/x --slow 2").expect_err("no rate");
        assert!(e.contains("--slow-rate"), "{e}");
        let e = parse("--url http://h/x --slow-rate 5000").expect_err("rate without slow");
        assert!(e.contains("--slow"), "{e}");
        let e = parse("--url http://h/x --slow 0 --slow-rate 5000").expect_err("zero slow");
        assert!(e.contains("--slow"), "{e}");
        let e = parse("--url http://h/x --slow 2 --slow-rate 0").expect_err("zero rate");
        assert!(e.contains("positive"), "{e}");
        let e = parse("--url http://h/x --viewers 5 --slow 6 --slow-rate 5000").expect_err("more");
        assert!(e.contains("--slow 6") && e.contains("5 full-media"), "{e}");
        let e = parse("--url http://h/x --viewers 20 --full 3 --slow 4 --slow-rate 5000")
            .expect_err("more than the full-media viewers");
        assert!(e.contains("3 full-media"), "{e}");
        assert!(parse("--url http://h/x --slow many --slow-rate 5").is_err());
        assert!(parse("--url http://h/x --slow 1 --slow-rate fast").is_err());
        assert!(parse("--url http://h/x --slow 1 --slow-rate -5").is_err());
        assert!(
            parse("--url http://h/x --slow").is_err(),
            "a value is missing"
        );
    }

    #[test]
    fn paces_bytes_to_the_rate() {
        let ms = Duration::from_millis;
        // Nothing read yet, or nothing due: no wait.
        assert_eq!(pace_delay(0, 50_000, Duration::ZERO), Duration::ZERO);
        // 50 000 bytes at 50 000 B/s are due after a second.
        assert_eq!(pace_delay(50_000, 50_000, Duration::ZERO), ms(1000));
        assert_eq!(pace_delay(50_000, 50_000, ms(400)), ms(600));
        // On schedule or behind: no wait, never a negative one.
        assert_eq!(pace_delay(50_000, 50_000, ms(1000)), Duration::ZERO);
        assert_eq!(pace_delay(50_000, 50_000, ms(2500)), Duration::ZERO);
        // Fractions of a second are exact, and the delay is never early.
        assert_eq!(pace_delay(1, 4, Duration::ZERO), ms(250));
        assert_eq!(
            pace_delay(1, 3, Duration::ZERO),
            Duration::from_nanos(333_333_333)
        );
        // 62 500 bytes (a 200 ms part at 2.5 Mbit/s) at 50 000 B/s.
        assert_eq!(pace_delay(62_500, 50_000, Duration::ZERO), ms(1250));
        // A faster rate than the data needs waits less.
        assert_eq!(pace_delay(1000, 1_000_000, Duration::ZERO), ms(1));
    }

    #[test]
    fn pacing_does_not_overflow_or_divide_by_zero() {
        assert_eq!(
            pace_delay(u64::MAX, 1, Duration::ZERO),
            Duration::from_secs(u64::MAX)
        );
        assert_eq!(pace_delay(5, 0, Duration::ZERO), Duration::from_secs(5));
        let huge = pace_delay(u64::MAX, u64::MAX, Duration::ZERO);
        assert_eq!(huge, Duration::from_secs(1));
    }

    /// A body that yields the given frames at once.
    struct Frames(Vec<Bytes>);

    impl hyper::body::Body for Frames {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Ready(if self.0.is_empty() {
                None
            } else {
                Some(Ok(hyper::body::Frame::data(self.0.remove(0))))
            })
        }
    }

    fn frames(sizes: &[usize]) -> Frames {
        Frames(sizes.iter().map(|n| Bytes::from(vec![9u8; *n])).collect())
    }

    #[tokio::test]
    async fn an_unpaced_body_is_read_whole_and_at_once() {
        let started = Instant::now();
        let body = read_body(frames(&[1000, 1000, 500]), None).await;
        assert_eq!(body.expect("read").len(), 2500);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn a_paced_body_arrives_whole_but_no_sooner_than_its_rate_allows() {
        // 3 000 bytes at 10 000 B/s: 0.3 s, however the frames fall.
        let started = Instant::now();
        let body = read_body(frames(&[1000, 1000, 1000]), Some(10_000)).await;
        assert_eq!(body.expect("read").len(), 3000);
        assert!(started.elapsed() >= Duration::from_millis(300));
        // An empty body, or one with no data, is not waited for.
        let started = Instant::now();
        assert!(
            read_body(frames(&[]), Some(1))
                .await
                .expect("read")
                .is_empty()
        );
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn the_timeout_cuts_a_paced_body_off_in_mid_wait() {
        // 1 000 bytes at 100 B/s would take 10 s; the caller's timeout ends it.
        let paced = read_body(frames(&[1000]), Some(100));
        let outcome = tokio::time::timeout(Duration::from_millis(100), paced).await;
        assert!(
            outcome.is_err(),
            "the timeout must fire during the pacing wait"
        );
    }

    #[test]
    fn describes_an_error_with_its_causes() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connect")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::other("handshake refused"));
        assert_eq!(describe(&e), "connect: handshake refused");
    }

    #[test]
    fn finds_the_hinted_part() {
        let pl = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:3\n#EXT-X-GAP\n#EXTINF:1.0,\ngap.mp4\n\
                  #EXT-X-PART:DURATION=0.2,URI=\"p_part0.mp4\"\n#EXTINF:1.0,\np_seg4.mp4\n\
                  #EXT-X-PART:DURATION=0.2,URI=\"p_part5.mp4\"\n\
                  #EXT-X-PART:DURATION=0.2,URI=\"p_part6.mp4\"\n\
                  #EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p_part7.mp4\"\n";
        let m = parse_media(pl);
        assert_eq!(m.next, (5, 2));
        assert_eq!(m.hint.as_deref(), Some("p_part7.mp4"));
        assert_eq!(m.last_segment.as_deref(), Some("p_seg4.mp4"));
    }
}
