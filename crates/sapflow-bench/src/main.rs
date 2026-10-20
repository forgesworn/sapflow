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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, StatusCode};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

type HttpClient = Client<HttpConnector, Empty<Bytes>>;

#[derive(Default)]
struct Stats {
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
}

struct Args {
    url: String,
    viewers: usize,
    secs: u64,
    ramp: u64,
    /// Viewers that fetch media; the rest follow the playlist only.
    full: Option<usize>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        url: String::new(),
        viewers: 10,
        secs: 30,
        ramp: 5,
        full: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--url" => a.url = v,
            "--viewers" => a.viewers = v.parse().map_err(|_| "--viewers: a number")?,
            "--secs" => a.secs = v.parse().map_err(|_| "--secs: a number")?,
            "--ramp" => a.ramp = v.parse().map_err(|_| "--ramp: a number")?,
            "--full" => a.full = Some(v.parse().map_err(|_| "--full: a number")?),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if !a.url.starts_with("http://") {
        return Err("--url http://.../index.m3u8 is required".into());
    }
    Ok(a)
}

#[tokio::main]
async fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sapflow-bench: {e}");
            std::process::exit(2);
        }
    };
    let stats = Arc::new(Mutex::new(Stats::default()));
    let deadline = Instant::now() + Duration::from_secs(a.ramp + a.secs);
    let mut tasks = Vec::new();
    for i in 0..a.viewers {
        let stats = stats.clone();
        let url = a.url.clone();
        let full = a.full.is_none_or(|n| i < n);
        let delay = Duration::from_millis(a.ramp * 1000 * i as u64 / a.viewers.max(1) as u64);
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            viewer(&url, deadline, &stats, full).await;
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    let st = stats.lock().unwrap_or_else(|e| e.into_inner());
    report(&a, &st);
}

fn lock(stats: &Mutex<Stats>) -> std::sync::MutexGuard<'_, Stats> {
    stats.lock().unwrap_or_else(|e| e.into_inner())
}

async fn get(
    client: &HttpClient,
    url: &str,
    stats: &Mutex<Stats>,
) -> Option<(Bytes, Duration, String)> {
    let mut url = url.to_owned();
    // Follow at most one redirect (mediamtx's session check).
    for _ in 0..2 {
        let start = Instant::now();
        let req = Request::get(&url).body(Empty::new()).ok()?;
        let resp = tokio::time::timeout(Duration::from_secs(15), client.request(req)).await;
        lock(stats).requests += 1;
        let resp = match resp {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                fail(stats, &url, &format!("client error: {e}"));
                return None;
            }
            Err(_) => {
                fail(stats, &url, "timeout");
                return None;
            }
        };
        if resp.status().is_redirection() {
            let loc = resp.headers().get("location")?.to_str().ok()?.to_owned();
            url = join(&url, &loc);
            continue;
        }
        if resp.status() != StatusCode::OK {
            fail(stats, &url, &format!("HTTP {}", resp.status().as_u16()));
            return None;
        }
        let body = resp.into_body().collect().await.ok()?.to_bytes();
        let took = start.elapsed();
        lock(stats).bytes += body.len() as u64;
        return Some((body, took, url));
    }
    None
}

fn fail(stats: &Mutex<Stats>, url: &str, kind: &str) {
    let what = if url.contains("_HLS_msn") {
        "blocking playlist"
    } else if url.contains(".m3u8") {
        "playlist"
    } else if url.contains("part") {
        "part"
    } else {
        "segment"
    };
    let mut st = lock(stats);
    st.errors += 1;
    *st.error_kinds.entry(format!("{what}: {kind}")).or_default() += 1;
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
/// every simulated viewer.
async fn viewer(index_url: &str, deadline: Instant, stats: &Arc<Mutex<Stats>>, full: bool) {
    let client: HttpClient = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(4)
        .build_http();
    let Some((master, _, index_url)) = get(&client, index_url, stats).await else {
        return;
    };
    let Some(variant) = String::from_utf8_lossy(&master)
        .lines()
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
    else {
        return;
    };
    let media_url = join(&index_url, &variant);
    let sep = if media_url.contains('?') { '&' } else { '?' };

    let mut loops = 0u64;
    let mut media: Option<Media> = None;
    while Instant::now() < deadline {
        // Every fifth reload, and at the start: a plain playlist request and
        // the newest whole segment.
        if loops.is_multiple_of(5) || media.is_none() {
            let Some((body, took, _)) = get(&client, &media_url, stats).await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                media = None;
                loops += 1;
                continue;
            };
            lock(stats).playlist.push(took);
            let m = parse_media(&String::from_utf8_lossy(&body));
            if full
                && let Some(seg) = &m.last_segment
                && let Some((_, took, _)) = get(&client, &join(&media_url, seg), stats).await
            {
                lock(stats).segment.push(took);
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
                get(&client, &part_url, stats).await
            } else {
                None
            }
        };
        let (pl, part) = tokio::join!(get(&client, &pl_url, stats), fetch_part);
        if pl.is_some() {
            // When each viewer learned of the new part (all viewers).
            lock(stats)
                .playlist_updates
                .entry((m.next.0, m.next.1))
                .or_default()
                .push(Instant::now());
        }
        if part.is_some() {
            // Key by path: mediamtx gives every viewer its own session query.
            let key = hint.split('?').next().unwrap_or(&hint).to_owned();
            lock(stats)
                .parts
                .entry(key)
                .or_default()
                .push(Instant::now());
        }
        media = pl.map(|(body, _, _)| parse_media(&String::from_utf8_lossy(&body)));
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

fn report(a: &Args, st: &Stats) {
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
    println!(
        "requests {}  errors {}  received {:.1} MB",
        st.requests,
        st.errors,
        st.bytes as f64 / 1e6
    );
    for (kind, n) in &st.error_kinds {
        println!("  error {kind}: {n}");
    }
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
        a.viewers,
        pct(&mut spread, 0.5),
        pct(&mut spread, 0.95),
        pct(&mut spread, 1.0)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
