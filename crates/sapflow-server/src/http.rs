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
            // A restart (a reconnect with new settings) renames every chunk.
            if st.closed || st.prefix != prefix {
                return not_found();
            }
            let Some(m) = st.ingest.muxer() else {
                return not_found();
            };
            match chunk {
                Chunk::Init => return body(m.init().clone(), "video/mp4", CACHE),
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
