//! HLS over HTTP: playlists, init, segments and parts.
//!
//! Every viewer gets the same bytes: no sessions, no cookies, no query
//! strings in URIs. Viewer addresses are not logged.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use sapflow_hls::playlist::{self, Chunk};
use sapflow_hls::{PartLookup, Readiness};

use crate::App;
use crate::stream::LiveStream;

/// Apple: hold a blocking request at most three target durations.
fn block_limit(stream: &LiveStream) -> Duration {
    let target = stream
        .lock()
        .ingest
        .muxer()
        .map_or(1, |m| m.target_duration());
    Duration::from_secs(3 * target)
}
const PLAYLIST_TYPE: &str = "application/vnd.apple.mpegurl";

/// Concurrent HTTP connections. Past this, new connections wait in the
/// kernel's accept queue.
const MAX_CONNECTIONS: usize = 20_000;
/// Time allowed for a request's headers to arrive (slow-loris). hyper also
/// applies it to an idle keep-alive connection waiting for its next request,
/// so it must outlast clients' idle connection reuse: at 10 s, viewers that
/// reused a connection just as it closed saw failed requests.
const HEADER_TIMEOUT: Duration = Duration::from_secs(60);

/// Serves HTTP/1.1 with a connection cap and a header-read timeout, which
/// `axum::serve` does not set.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::service::TowerToHyperService;

    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            p = slots.clone().acquire_owned() => p.map_err(std::io::Error::other)?,
            () = &mut shutdown => return Ok(()),
        };
        let (sock, _) = tokio::select! {
            r = listener.accept() => match r {
                Ok(a) => a,
                // Out of file descriptors and the like: back off, carry on.
                Err(e) => {
                    tracing::warn!(error = %e, "HTTP accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => return Ok(()),
        };
        let _ = sock.set_nodelay(true);
        let service = TowerToHyperService::new(router.clone());
        tokio::spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .max_buf_size(64 * 1024)
                .serve_connection(TokioIo::new(sock), service);
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "HTTP connection ended with an error");
            }
            drop(permit);
        });
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route(
            "/live/{event}/index.m3u8",
            get(multivariant).options(preflight),
        )
        .route(
            &format!("/live/{{event}}/{}", playlist::MEDIA_PLAYLIST),
            get(media).options(preflight),
        )
        .route("/live/{event}/{file}", get(chunk).options(preflight))
        .fallback(|| async { not_found() })
        .layer(axum::middleware::map_response(cors))
        .with_state(app)
}

async fn cors(mut resp: Response) -> Response {
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    resp
}

async fn preflight() -> Response {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
            (header::ACCESS_CONTROL_MAX_AGE, "86400"),
        ],
    )
        .into_response()
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

fn body(bytes: Bytes, content_type: &'static str, cache: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        Body::from(bytes),
    )
        .into_response()
}

fn stream_for(app: &App, event: &str) -> Option<Arc<LiveStream>> {
    // Paths are validated at publish time; anything not live is a 404.
    app.streams.get(event)
}

async fn multivariant(State(app): State<Arc<App>>, Path(event): Path<String>) -> Response {
    let Some(stream) = stream_for(&app, &event) else {
        return not_found();
    };
    let mv = stream.lock().multivariant.clone();
    match mv {
        Some(b) => body(b, PLAYLIST_TYPE, "no-cache"),
        None => not_found(),
    }
}

async fn media(
    State(app): State<Arc<App>>,
    Path(event): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let Ok((msn, part)) = playlist::parse_blocking_query(query.as_deref()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(stream) = stream_for(&app, &event) else {
        return not_found();
    };
    let mut rx = stream.subscribe();
    let deadline = tokio::time::Instant::now() + block_limit(&stream);
    loop {
        {
            let st = stream.lock();
            if st.closed {
                return not_found();
            }
            let readiness = st
                .ingest
                .muxer()
                .map_or(Readiness::Wait, |m| m.readiness(msn, part));
            match readiness {
                Readiness::BadRequest => return StatusCode::BAD_REQUEST.into_response(),
                Readiness::Ready => {
                    if let Some(b) = st.media.clone() {
                        return body(b, PLAYLIST_TYPE, "no-cache");
                    }
                }
                Readiness::Wait => {}
            }
        }
        match tokio::time::timeout_at(deadline, rx.changed()).await {
            Ok(Ok(())) => {}
            // Sender gone or time up.
            Ok(Err(_)) => return not_found(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
}

async fn chunk(
    State(app): State<Arc<App>>,
    Path((event, file)): Path<(String, String)>,
) -> Response {
    let Some(stream) = stream_for(&app, &event) else {
        return not_found();
    };
    let prefix = stream.lock().prefix.clone();
    let Some(chunk) = playlist::parse_chunk(&prefix, &file) else {
        return not_found();
    };
    const CACHE: &str = "public, max-age=3600";
    let mut rx = stream.subscribe();
    let deadline = tokio::time::Instant::now() + block_limit(&stream);
    loop {
        {
            let st = stream.lock();
            // Reconnect codec epochs keep this prefix; closed presentations do not.
            if st.closed || st.prefix != prefix {
                return not_found();
            }
            let Some(m) = st.ingest.muxer() else {
                return not_found();
            };
            match chunk {
                Chunk::Init | Chunk::InitEpoch(_) => {
                    let epoch = match chunk {
                        Chunk::InitEpoch(id) => id,
                        _ => 0,
                    };
                    return match m.initialization(epoch) {
                        Some(init) => body(init.clone(), "video/mp4", CACHE),
                        None => not_found(),
                    };
                }
                Chunk::Segment(id) => {
                    return match m.segment(id) {
                        Some(s) => body(s.data.clone(), "video/mp4", CACHE),
                        None => not_found(),
                    };
                }
                Chunk::Part(id) => match m.part(id) {
                    PartLookup::Found(p) => return body(p.data.clone(), "video/mp4", CACHE),
                    PartLookup::Missing => return not_found(),
                    // The preload hint: hold the request until the part exists.
                    PartLookup::Next => {}
                },
            }
        }
        match tokio::time::timeout_at(deadline, rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return not_found(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
}

#[cfg(test)]
mod epoch_tests {
    use super::*;
    use crate::{
        callback::Callbacks,
        config::Config,
        publish::Publication,
        stream::{Registry, Source, SourceKind},
    };
    use http_body_util::BodyExt;
    use sapflow_core::{PublishRequest, StreamPath};
    use std::path::PathBuf;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };

    const PORTRAIT: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
    const LANDSCAPE: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-landscape.ts");
    static TEMP: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn app(record: bool) -> (Arc<App>, Scratch) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "origin-epoch-test-{}-{}",
                std::process::id(),
                TEMP.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir(&root).expect("new task-owned scratch");
        let mut config = Config::load(
            |key| match key {
                "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
                "SAPFLOW_RECORDINGS" => Some(
                    root.to_str()
                        .expect("valid epoch regression fixture")
                        .into(),
                ),
                "SAPFLOW_RECORD_MIN_FREE_GB" => Some("0".into()),
                _ => None,
            },
            Vec::new(),
        )
        .expect("valid epoch regression fixture");
        if !record {
            config.recordings = None;
        }
        (
            Arc::new(App {
                config,
                streams: Registry::default(),
                callbacks: Callbacks::start(None, None),
                auth_throttle: Mutex::new(sapflow_core::limits::AuthThrottle::new(
                    10,
                    Duration::from_secs(60),
                )),
                refusal_log: Mutex::new(sapflow_core::limits::LogLimiter::new(
                    20,
                    Duration::from_secs(60),
                )),
            }),
            Scratch(root),
        )
    }
    fn begin(app: &Arc<App>) -> Publication {
        let req = PublishRequest::parse_srt_stream_id("publish:live/event-epochs:fixture:fixture")
            .expect("valid epoch regression fixture");
        Publication::begin(
            app,
            &req,
            SourceKind::Ts,
            "127.0.0.1".parse().expect("valid epoch regression fixture"),
        )
        .expect("valid epoch regression fixture")
    }
    async fn exercise(app: &Arc<App>) {
        let mut check = playlist::RuleCheck::default();
        let mut pending = None;
        let mut preloaded = None;
        let mut original = None;
        let mut prefix = String::new();
        for (epoch, data) in [PORTRAIT, LANDSCAPE, PORTRAIT].into_iter().enumerate() {
            let mut publisher = begin(app);
            publisher
                .push(|source, now| match source {
                    Source::Ts(ts) => ts.push(data, now),
                    _ => unreachable!(),
                })
                .expect("valid epoch regression fixture");
            let stream = publisher.stream.clone();
            {
                let st = stream.lock();
                let m = st.ingest.muxer().expect("valid epoch regression fixture");
                assert_eq!(
                    (m.video().width, m.video().height),
                    if epoch == 1 { (1280, 720) } else { (720, 1280) }
                );
                assert_eq!(
                    m.audio()
                        .expect("valid epoch regression fixture")
                        .sample_rate,
                    48_000
                );
                let text =
                    std::str::from_utf8(st.media.as_ref().expect("valid epoch regression fixture"))
                        .expect("valid epoch regression fixture");
                check.check(text).expect("valid epoch regression fixture");
                assert_eq!(m.target_duration(), 2);
                assert_eq!(m.current_initialization_id(), epoch as u64);
                assert_eq!(text.matches("#EXT-X-MAP:").count(), epoch + 1);
                if epoch == 0 {
                    prefix = st.prefix.clone();
                    original = Some((
                        m.init().clone(),
                        m.segment(7)
                            .expect("valid epoch regression fixture")
                            .data
                            .clone(),
                    ));
                    let msn = m.media_sequence() + m.entries().len() as u64 + 1;
                    assert_eq!(m.readiness(Some(msn), Some(0)), Readiness::Wait);
                    pending = Some(tokio::spawn(media(
                        State(app.clone()),
                        Path("event-epochs".into()),
                        RawQuery(Some(format!("_HLS_msn={msn}&_HLS_part=0"))),
                    )));
                } else {
                    assert_eq!(st.prefix, prefix);
                    let (init, segment) =
                        original.as_ref().expect("valid epoch regression fixture");
                    assert_eq!(m.initialization(0), Some(init));
                    assert_eq!(
                        &m.segment(7).expect("valid epoch regression fixture").data,
                        segment
                    );
                    let new_map = format!(
                        "#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"{}\"",
                        playlist::epoch_init_name(&prefix, epoch as u64)
                    );
                    assert!(
                        text.contains(&new_map),
                        "new MAP must precede all new-epoch parts"
                    );
                }
                // Do not hold a state lock while polling handlers or publication cleanup.
            }
            let (_, parts) = stream
                .parts_after(None)
                .expect("valid epoch regression fixture");
            assert!(!parts.is_empty());
            assert!(
                parts
                    .iter()
                    .all(|part| part.initialization.id == epoch as u64)
            );
            if epoch == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert!(
                    !pending
                        .as_ref()
                        .expect("valid epoch regression fixture")
                        .is_finished()
                );
            }
            publisher.end();
            if epoch == 0 {
                let before = stream.lock().media.clone();
                // A short intermediate phone pose may connect and leave before
                // any frame. It must not advertise an empty epoch or new MAP.
                begin(app).end();
                assert_eq!(stream.lock().media, before);
                let hint = stream
                    .lock()
                    .ingest
                    .muxer()
                    .expect("valid epoch regression fixture")
                    .next_part_id();
                let request = tokio::spawn(chunk(
                    State(app.clone()),
                    Path(("event-epochs".into(), playlist::part_name(&prefix, hint))),
                ));
                preloaded = Some((hint, request));
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(
                    !preloaded
                        .as_ref()
                        .expect("valid epoch regression fixture")
                        .1
                        .is_finished()
                );
            }
            if epoch == 1 {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    pending.take().expect("valid epoch regression fixture"),
                )
                .await
                .expect("valid epoch regression fixture")
                .expect("valid epoch regression fixture");
                assert_eq!(response.status(), StatusCode::OK);
                let body = response
                    .into_body()
                    .collect()
                    .await
                    .expect("valid epoch regression fixture")
                    .to_bytes();
                assert!(
                    std::str::from_utf8(&body)
                        .expect("valid epoch regression fixture")
                        .contains(&prefix)
                );
                let (hint, request) = preloaded.take().expect("valid epoch regression fixture");
                let response = tokio::time::timeout(Duration::from_secs(1), request)
                    .await
                    .expect("valid epoch regression fixture")
                    .expect("valid epoch regression fixture");
                assert_eq!(response.status(), StatusCode::OK);
                let PartLookup::Found(part) = stream
                    .lock()
                    .ingest
                    .muxer()
                    .expect("valid epoch regression fixture")
                    .part(hint)
                else {
                    panic!("hint must resolve")
                };
                assert_eq!(part.initialization.id, 1);
                assert_eq!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .expect("valid epoch regression fixture")
                        .to_bytes(),
                    part.data
                );
            }
        }
        // Old cached initialization URI remains servable across the window.
        let response = chunk(
            State(app.clone()),
            Path(("event-epochs".into(), playlist::init_name(&prefix))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .expect("valid epoch regression fixture")
                .to_bytes(),
            original.expect("valid epoch regression fixture").0
        );
        let path = StreamPath::parse("live/event-epochs").expect("valid epoch regression fixture");
        let (stream, generation, _) = app
            .streams
            .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Ts)
            .expect("valid epoch regression fixture");
        app.streams.detach(&stream, generation, false);
    }

    #[tokio::test]
    async fn portrait_landscape_portrait_preserves_http_identity_and_pending_reload() {
        let (app, _scratch) = app(false);
        exercise(&app).await;
    }

    #[tokio::test]
    #[ignore = "requires installed ffmpeg and ffprobe; run explicitly for origin acceptance"]
    async fn each_resolution_epoch_recording_decodes_independently_with_audio() {
        let (app, scratch) = app(true);
        exercise(&app).await;
        let directory = scratch.0.join("live/event-epochs");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let files = loop {
            let mut files: Vec<_> = std::fs::read_dir(&directory)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .collect();
            files.sort();
            if files.len() == 3
                && files.iter().all(|file| {
                    let output = std::process::Command::new("ffprobe")
                        .args([
                            "-v",
                            "error",
                            "-show_entries",
                            "format=duration",
                            "-of",
                            "csv=p=0",
                        ])
                        .arg(file)
                        .output()
                        .expect("valid epoch regression fixture");
                    String::from_utf8_lossy(&output.stdout)
                        .trim()
                        .parse::<f64>()
                        .is_ok_and(|seconds| seconds > 1.5 && seconds < 3.0)
                })
            {
                break files;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "three finalised epoch files required"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        for (index, file) in files.iter().enumerate() {
            let output = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "stream=codec_name,width,height,start_time",
                    "-of",
                    "json",
                ])
                .arg(file)
                .output()
                .expect("valid epoch regression fixture");
            let text = String::from_utf8(output.stdout).expect("valid epoch regression fixture");
            assert!(output.status.success());
            assert!(
                text.contains("h264") && text.contains("aac"),
                "both tracks required: {text}"
            );
            assert!(
                text.contains(if index == 1 {
                    "\"width\": 1280"
                } else {
                    "\"width\": 720"
                }),
                "{text}"
            );
            assert!(
                text.contains("0.000000") || text.contains("0.000011"),
                "file origin near zero: {text}"
            );
            let status = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
                .arg(file)
                .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
                .status()
                .expect("valid epoch regression fixture");
            assert!(
                status.success(),
                "independent audio+video decode failed: {}",
                file.display()
            );
        }
    }
}
