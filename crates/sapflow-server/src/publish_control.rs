//! A separate loopback-only control listener. A private TLS proxy owns remote
//! access; neither the HLS listener nor CORS exposes these credentials.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Json, Router};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use std::sync::Arc;
use std::time::Duration;

use crate::App;
use crate::publish_session_wire::{BODY_LIMIT, PutSession, RevokeSession};
use crate::publish_sessions::Error;

fn failure(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({"ok":false,"error":code})),
    )
        .into_response()
}
fn error(value: Error) -> Response {
    let (status, code) = match value {
        Error::Invalid => (StatusCode::BAD_REQUEST, "invalid-request"),
        Error::Conflict => (StatusCode::CONFLICT, "session-conflict"),
        Error::Denied => (StatusCode::FORBIDDEN, "session-denied"),
        Error::Unknown => (StatusCode::NOT_FOUND, "unknown-session"),
        Error::Busy => (StatusCode::TOO_MANY_REQUESTS, "capacity"),
        Error::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "authority-unavailable"),
    };
    failure(status, code)
}
async fn authenticate(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let Some(config) = &app.config.control else {
        return error(Error::Denied);
    };
    let allowed = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|v| config.token.matches(v));
    if !allowed {
        return failure(StatusCode::UNAUTHORIZED, "unauthorised");
    }
    match tokio::time::timeout(Duration::from_secs(10), next.run(request)).await {
        Ok(mut response) => {
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(_) => failure(StatusCode::GATEWAY_TIMEOUT, "request-timeout"),
    }
}
async fn provision(State(app): State<Arc<App>>, Path(id): Path<String>, body: Bytes) -> Response {
    let Some(sessions) = &app.publish_sessions else {
        return error(Error::Unavailable);
    };
    let Ok(wire) = PutSession::parse(&body) else {
        return error(Error::Invalid);
    };
    match sessions.put(&id, wire, app.config.max_publish_bps).await {
        Ok(receipt) => Json(receipt).into_response(),
        Err(e) => error(e),
    }
}
async fn revoke(State(app): State<Arc<App>>, Path(id): Path<String>, body: Bytes) -> Response {
    let Some(sessions) = &app.publish_sessions else {
        return error(Error::Unavailable);
    };
    let Ok(wire) = RevokeSession::parse(&body) else {
        return error(Error::Invalid);
    };
    match sessions.revoke(&id, wire).await {
        Ok(receipt) => Json(receipt).into_response(),
        Err(e) => error(e),
    }
}
async fn status(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    if sapflow_core::publish_session::SessionId::parse(&id).is_err() {
        return error(Error::Invalid);
    }
    let Some(sessions) = &app.publish_sessions else {
        return error(Error::Unavailable);
    };
    match sessions.status(&id).await {
        Ok(receipt) => Json(receipt).into_response(),
        Err(e) => error(e),
    }
}
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/publish-sessions/{id}", put(provision).get(status))
        .route("/v1/publish-sessions/{id}/revoke", post(revoke))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .with_state(app)
}
pub async fn serve(listener: tokio::net::TcpListener, app: Arc<App>) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::other("control listener must be loopback"));
    }
    let router = router(app);
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        let (socket, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let service = TowerToHyperService::new(router.clone());
        tokio::spawn(async move {
            let _slot = slot;
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(5))
                .max_headers(32)
                .max_buf_size(16 * 1024)
                .keep_alive(false)
                .serve_connection(TokioIo::new(socket), service)
                .await;
        });
    }
}

#[cfg(test)]
#[path = "publish_control_tests.rs"]
mod tests;
