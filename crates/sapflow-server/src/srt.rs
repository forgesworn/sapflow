//! SRT ingest: the encoder calls, we listen.
//!
//! The stream id is checked during the handshake, before any media is
//! accepted: wrong form, wrong path, wrong credentials or an already-live
//! path are all rejected there.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use sapflow_core::{PublishError, PublishRequest};
use srt_tokio::SrtListener;
use srt_tokio::access::{RejectReason, ServerRejectReason};

use crate::App;
use crate::publish::{Publication, Refusal};
use crate::stream::{Source, SourceKind};

/// Drop a publisher that sends nothing for this long (SRT's own idle
/// timeout normally fires first).
pub const IDLE: Duration = Duration::from_secs(10);

/// Serves SRT, rebinding if the listener ever stops (a panic inside
/// srt-tokio's listener task ends its incoming stream).
pub async fn listen(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let mut first = true;
    loop {
        match serve(app.clone(), addr).await {
            Err(e) if first => return Err(e),
            Err(e) => tracing::error!(error = %e, "SRT listener failed; retrying"),
            Ok(()) => tracing::error!("SRT listener stopped; restarting"),
        }
        first = false;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn serve(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let (_listener, mut incoming) = SrtListener::builder().bind(addr).await?;
    tracing::info!(%addr, "SRT listening");
    let active = Arc::new(AtomicUsize::new(0));
    while let Some(request) = incoming.incoming().next().await {
        let app = app.clone();
        let active = active.clone();
        tokio::spawn(async move {
            let stream_id = request
                .stream_id()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let begun = match PublishRequest::parse_srt_stream_id(&stream_id) {
                Err(e) => Err(Refusal::Request(e)),
                Ok(_) if active.load(Ordering::Relaxed) >= app.config.max_publishers => {
                    app.log_refusal("srt", &"publisher limit");
                    let _ = request
                        .reject(RejectReason::Server(ServerRejectReason::Overload))
                        .await;
                    return;
                }
                Ok(req) => Publication::begin(&app, &req, SourceKind::Ts, request.remote().ip()),
            };
            let mut publication = match begun {
                Ok(p) => p,
                Err(r) => {
                    app.log_refusal("srt", &r);
                    let _ = request.reject(reject_reason(r)).await;
                    return;
                }
            };
            let mut socket = match request.accept(None).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "SRT accept failed");
                    publication.end();
                    return;
                }
            };
            active.fetch_add(1, Ordering::Relaxed);
            loop {
                let data = match tokio::time::timeout(IDLE, socket.next()).await {
                    Ok(Some(Ok((_, data)))) => data,
                    Ok(Some(Err(e))) => {
                        tracing::info!(error = %e, "SRT receive ended");
                        break;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        tracing::info!("SRT publisher idle, dropping");
                        break;
                    }
                };
                if let Err(e) = publication.received(data.len()) {
                    tracing::warn!(error = %e, "dropping SRT publisher");
                    break;
                }
                let pushed = publication.push(|src, now| match src {
                    Source::Ts(ts) => ts.push(&data, now),
                    Source::Flv(_) => Ok(Default::default()),
                });
                if let Err(e) = pushed {
                    tracing::warn!(error = %e, "dropping SRT publisher");
                    break;
                }
            }
            active.fetch_sub(1, Ordering::Relaxed);
            publication.end();
        });
    }
    Ok(())
}

fn reject_reason(r: Refusal) -> RejectReason {
    RejectReason::Server(match r {
        Refusal::Request(PublishError::BadRequest) => ServerRejectReason::BadRequest,
        Refusal::Request(PublishError::BadPath) => ServerRejectReason::Notfound,
        Refusal::Request(PublishError::Unauthorized) => ServerRejectReason::Unauthorized,
        Refusal::AlreadyLive => ServerRejectReason::Conflict,
        Refusal::Throttled => ServerRejectReason::Overload,
    })
}
