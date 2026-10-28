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

use crate::App;
use crate::publish::{Publication, Refusal};
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
            Self::Policy(Refusal::Request(PublishError::Unauthorized)) => 401,
            Self::Policy(Refusal::Request(PublishError::BadPath)) => 404,
            Self::Policy(Refusal::AlreadyLive) => 409,
            Self::Policy(
                Refusal::Throttled | Refusal::PublisherLimit | Refusal::PresentationLimit,
            )
            | Self::PublisherLimit => 402,
        }
    }
}

/// Decides on a caller from its stream id, with `active` SRT publishers
/// already live: begins its publication, or says why not (and logs it,
/// rate-limited).
pub async fn admit(
    app: &Arc<App>,
    stream_id: &str,
    from: IpAddr,
    active: usize,
) -> Result<Publication, Refused> {
    let admitted = match PublishRequest::parse_srt_stream_id(stream_id) {
        Err(e) => Err(Refused::Policy(Refusal::Request(e))),
        Ok(_) if active >= app.config.max_publishers => Err(Refused::PublisherLimit),
        Ok(req) => Publication::begin_srt(app, &req, from)
            .await
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
            (Refused::Policy(Refusal::Throttled), 402),
            (Refused::Policy(Refusal::PublisherLimit), 402),
            (Refused::Policy(Refusal::PresentationLimit), 402),
            (Refused::PublisherLimit, 402),
        ] {
            assert_eq!(r.status(), status, "{r:?}");
        }
    }
}
