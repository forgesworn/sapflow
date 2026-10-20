//! SRT ingest: the encoder calls, we listen.
//!
//! The stream id is checked during the handshake, before any media is
//! accepted: wrong form, wrong path, wrong credentials or an already-live
//! path are all rejected there.
//!
//! Two implementations serve the port, chosen by `SAPFLOW_SRT_IMPL`:
//! `srt-tokio` (the default, below) and our own `sapflow-srt`
//! ([`crate::srt_own`]). The policy (who may publish, limits, what happens
//! to the media) is shared and lives here.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use sapflow_core::{PublishError, PublishRequest};
use srt_tokio::SrtListener;
use srt_tokio::access::{RejectReason, ServerRejectReason};

use crate::App;
use crate::config::SrtImpl;
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
        let served = match app.config.srt_impl {
            SrtImpl::SrtRs => serve(app.clone(), addr).await,
            SrtImpl::Own => crate::srt_own::serve(app.clone(), addr).await,
        };
        match served {
            Err(e) if first => return Err(e),
            Err(e) => tracing::error!(error = %e, "SRT listener failed; retrying"),
            Ok(()) => tracing::error!("SRT listener stopped; restarting"),
        }
        first = false;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Why a caller is turned away in the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Policy(Refusal),
    /// `SAPFLOW_MAX_PUBLISHERS` already publishing over SRT.
    PublisherLimit,
}

impl Refused {
    /// The HTTP-style status the caller is told (libsrt prints it as
    /// `PREDEFINED:<status>`): the codes `srt-tokio` sends for the same
    /// refusals.
    pub fn status(self) -> u16 {
        match self {
            Self::Policy(Refusal::Request(PublishError::BadRequest)) => 400,
            Self::Policy(Refusal::Request(PublishError::Unauthorized)) => 401,
            Self::Policy(Refusal::Request(PublishError::BadPath)) => 404,
            Self::Policy(Refusal::AlreadyLive) => 409,
            Self::Policy(Refusal::Throttled) | Self::PublisherLimit => 402,
        }
    }
}

/// Decides on a caller from its stream id, with `active` SRT publishers
/// already live: begins its publication, or says why not (and logs it,
/// rate-limited).
pub fn admit(
    app: &Arc<App>,
    stream_id: &str,
    from: IpAddr,
    active: usize,
) -> Result<Publication, Refused> {
    let admitted = match PublishRequest::parse_srt_stream_id(stream_id) {
        Err(e) => Err(Refused::Policy(Refusal::Request(e))),
        Ok(_) if active >= app.config.max_publishers => Err(Refused::PublisherLimit),
        Ok(req) => Publication::begin(app, &req, SourceKind::Ts, from).map_err(Refused::Policy),
    };
    if let Err(r) = &admitted {
        match r {
            Refused::PublisherLimit => app.log_refusal("srt", &"publisher limit"),
            Refused::Policy(p) => app.log_refusal("srt", p),
        }
    }
    admitted
}

/// Feeds one payload (MPEG-TS) to the publication. An error means the
/// publisher is dropped.
pub fn feed(publication: &mut Publication, data: &[u8]) -> Result<(), String> {
    publication
        .received(data.len())
        .map_err(|e| e.to_string())?;
    publication
        .push(|src, now| match src {
            Source::Ts(ts) => ts.push(data, now),
            Source::Flv(_) => Ok(Default::default()),
        })
        .map_err(|e| e.to_string())
}

async fn serve(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let (_listener, mut incoming) = SrtListener::builder().bind(addr).await?;
    tracing::info!(%addr, implementation = "srt-rs", "SRT listening");
    let active = Arc::new(AtomicUsize::new(0));
    while let Some(request) = incoming.incoming().next().await {
        let app = app.clone();
        let active = active.clone();
        tokio::spawn(async move {
            let stream_id = request
                .stream_id()
                .map(|s| s.to_string())
                .unwrap_or_default();
            let from = request.remote().ip();
            let mut publication =
                match admit(&app, &stream_id, from, active.load(Ordering::Relaxed)) {
                    Ok(p) => p,
                    Err(r) => {
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
                if let Err(e) = feed(&mut publication, &data) {
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

fn reject_reason(r: Refused) -> RejectReason {
    RejectReason::Server(match r {
        Refused::Policy(Refusal::Request(PublishError::BadRequest)) => {
            ServerRejectReason::BadRequest
        }
        Refused::Policy(Refusal::Request(PublishError::BadPath)) => ServerRejectReason::Notfound,
        Refused::Policy(Refusal::Request(PublishError::Unauthorized)) => {
            ServerRejectReason::Unauthorized
        }
        Refused::Policy(Refusal::AlreadyLive) => ServerRejectReason::Conflict,
        Refused::Policy(Refusal::Throttled) | Refused::PublisherLimit => {
            ServerRejectReason::Overload
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_match_what_srt_tokio_sends() {
        // Observed from srt-live-transmit against the srt-tokio listener:
        // PREDEFINED:400, 401, 402, 404 and 409.
        for (r, status) in [
            (
                Refused::Policy(Refusal::Request(PublishError::BadRequest)),
                400,
            ),
            (
                Refused::Policy(Refusal::Request(PublishError::Unauthorized)),
                401,
            ),
            (
                Refused::Policy(Refusal::Request(PublishError::BadPath)),
                404,
            ),
            (Refused::Policy(Refusal::AlreadyLive), 409),
            (Refused::Policy(Refusal::Throttled), 402),
            (Refused::PublisherLimit, 402),
        ] {
            assert_eq!(r.status(), status, "{r:?}");
        }
    }
}
