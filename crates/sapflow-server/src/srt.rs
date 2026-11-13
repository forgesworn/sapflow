//! SRT ingest: the encoder calls, we listen.
//!
//! The stream id is checked during the handshake, before any media is
//! accepted: wrong form, wrong path, wrong credentials or an already-live
//! path are all rejected there.
//!
//! Our own `sapflow-srt` serves the port ([`crate::srt_own`]); the policy
//! (who may publish, limits, what happens to the media) lives here.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use sapflow_core::{PublishError, PublishRequest};
use sapflow_srt::crypto::CryptoConfig;

use crate::App;
use crate::publish::{Admitted, Publication, Refusal};
use crate::stream::Source;

/// Drop a publisher that sends nothing for this long (the SRT
/// connection's own 5 s timeout normally fires first).
pub const IDLE: Duration = Duration::from_secs(10);

/// Serves SRT, rebinding if the listener ever stops.
pub async fn listen(app: Arc<App>, addr: SocketAddr) -> std::io::Result<()> {
    let mut first = true;
    loop {
        match crate::srt_own::serve(app.clone(), addr).await {
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
    /// `PREDEFINED:<status>`): the codes srt-tokio, which served SRT until
    /// 2026-10-21, sent for the same refusals.
    pub fn status(self) -> u16 {
        match self {
            Self::Policy(Refusal::Request(PublishError::BadRequest)) => 400,
            // SRT answers the app's 403 as it always has: 401.
            Self::Policy(Refusal::Request(PublishError::Unauthorized) | Refusal::Forbidden) => 401,
            Self::Policy(Refusal::Request(PublishError::BadPath)) => 404,
            Self::Policy(Refusal::AlreadyLive) => 409,
            // The app that authorises publishers did not answer usably, or
            // sapflow is stopping.
            Self::Policy(Refusal::AuthUnavailable | Refusal::Stopping) => 503,
            Self::Policy(
                Refusal::Throttled
                | Refusal::PublisherLimit
                | Refusal::PresentationLimit
                | Refusal::MemoryLimit,
            )
            | Self::PublisherLimit => 402,
        }
    }
}

/// What the asynchronous part of an SRT admission decided.
pub enum Admission {
    /// Admitted and its path claimed, as always without app keys.
    Claimed(Publication),
    /// With `SAPFLOW_SRT_APP_KEYS`: the app admitted the caller, with or
    /// without a passphrase for this stream. Nothing is claimed yet: the
    /// listener first makes sure it will accept the caller.
    Pending(Pending),
}

/// An admission waiting for its key check.
pub struct Pending {
    pub req: PublishRequest,
    pub admitted: Admitted,
    /// The stream's passphrase from the app; `None` when it sent none, and
    /// the listener's own settings decide.
    pub key: Option<CryptoConfig>,
}

/// Names the path only: the request carries the publish password and the
/// key is the stream's passphrase.
impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claimed(publication) => f
                .debug_tuple("Claimed")
                .field(&publication.stream.path.as_path())
                .finish(),
            Self::Pending(pending) => f
                .debug_struct("Pending")
                .field("path", &pending.req.path.as_path())
                .field("app_key", &pending.key.is_some())
                .finish(),
        }
    }
}

/// Decides on a caller from its stream id, with `active` SRT publishers
/// already live: begins its publication, hands back an admission whose key
/// is still to be checked, or says why not (and logs it, rate-limited).
pub async fn admit(
    app: &Arc<App>,
    stream_id: &str,
    from: IpAddr,
    active: usize,
) -> Result<Admission, Refused> {
    let admitted = match PublishRequest::parse_srt_stream_id(stream_id) {
        Err(e) => Err(Refused::Policy(Refusal::Request(e))),
        Ok(_) if active >= app.config.max_publishers => Err(Refused::PublisherLimit),
        Ok(req) if app.config.srt_app_keys => admit_offering_key(app, req, from)
            .await
            .map_err(Refused::Policy),
        Ok(req) => Publication::begin_srt(app, &req, from)
            .await
            .map(Admission::Claimed)
            .map_err(Refused::Policy),
    };
    if let Err(r) = &admitted {
        match r {
            Refused::PublisherLimit => app.log_refusal("srt", &"publisher limit"),
            Refused::Policy(p) => app.log_refusal("srt", p),
        }
    }
    admitted
}

/// Asks the app, offering to check its passphrase for the stream. Claims
/// nothing, with or without one in the answer: an SRT claim takes over a live
/// path, so it waits until the caller is known to be accepted.
async fn admit_offering_key(
    app: &Arc<App>,
    req: PublishRequest,
    from: IpAddr,
) -> Result<Admission, Refusal> {
    let mut admitted = Publication::admit_srt_key(app, &req, from).await?;
    let key = admitted.take_srt_key();
    Ok(Admission::Pending(Pending { req, admitted, key }))
}

/// Feeds one payload (MPEG-TS) to the publication. An error means the
/// publisher is dropped.
#[cfg(test)]
pub fn feed(publication: &mut Publication, data: &[u8]) -> Result<(), String> {
    feed_payload(publication, data, false)
}

/// [`feed`], first resetting TS assembly when the transport gave up on data
/// just before this payload (`after_loss`): continuity counters miss gaps of
/// a multiple of 16 packets on a PID.
pub fn feed_payload(
    publication: &mut Publication,
    data: &[u8],
    after_loss: bool,
) -> Result<(), String> {
    publication
        .received(data.len())
        .map_err(|e| e.to_string())?;
    publication
        .push(|src, now| match src {
            Source::Ts(ts) => {
                if after_loss {
                    ts.transport_gap(now);
                }
                ts.push(data, now)
            }
            Source::Flv(_) | Source::Es(_) => Ok(Default::default()),
        })
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_match_what_srt_tokio_sent() {
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
            (Refused::Policy(Refusal::AuthUnavailable), 503),
            (Refused::Policy(Refusal::Stopping), 503),
            (Refused::Policy(Refusal::Throttled), 402),
            (Refused::Policy(Refusal::PublisherLimit), 402),
            (Refused::Policy(Refusal::PresentationLimit), 402),
            (Refused::Policy(Refusal::MemoryLimit), 402),
            (Refused::PublisherLimit, 402),
        ] {
            assert_eq!(r.status(), status, "{r:?}");
        }
    }
}
