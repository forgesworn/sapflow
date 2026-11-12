//! A3.10's RTCP stimuli for a WHIP publisher's rate control, in a build with
//! the `test-feedback` feature only. Sapflow never claims loss it did not
//! see and never sends REMB; to see whether a publisher changes its sending
//! rate on either, a test build armed at run time can:
//!
//! - `SAPFLOW_TEST_WHIP_FEEDBACK=loss:<n>:<s>`: the answer leaves out
//!   `transport-cc`, so the receiver reports are the sender's only signal,
//!   and from `s` seconds after SRTP is up every report block claims
//!   `n`/256 lost, its cumulative loss growing to match (`loss:0:0` is the
//!   same answer with nothing claimed);
//! - `SAPFLOW_TEST_WHIP_FEEDBACK=remb:<bits a second>:<s>`: the answer
//!   echoes `goog-remb` for video where the offer lists it and leaves out
//!   `transport-cc`, and from `s` seconds every report also carries a REMB
//!   of that bitrate for every stream.
//!
//! Unset, nothing changes. Set, the server warns at the first `POST` that
//! the hooks are armed.

use std::sync::OnceLock;
use std::time::Duration;

use sapflow_rtp::FeedbackHooks;
use sapflow_whip::Session;
use sapflow_whip::sdp::{self, AnswerConfig, AnswerHooks, Negotiated, Offer, ProfileError};

/// The variable that arms the hooks.
pub(crate) const VAR: &str = "SAPFLOW_TEST_WHIP_FEEDBACK";

/// Reads a value of [`VAR`].
pub(crate) fn parse(value: &str) -> Result<FeedbackHooks, String> {
    let bad = || {
        format!("{VAR}: loss:<0-255>:<seconds> or remb:<bits a second>:<seconds>, not {value:?}")
    };
    let mut parts = value.split(':');
    let (Some(kind), Some(amount), Some(after), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    let after = after
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|_| bad())?;
    match kind {
        "loss" => Ok(FeedbackHooks {
            loss: Some(amount.parse::<u8>().map_err(|_| bad())?),
            after,
            ..FeedbackHooks::default()
        }),
        "remb" => Ok(FeedbackHooks {
            remb: Some(amount.parse::<u64>().map_err(|_| bad())?),
            after,
            ..FeedbackHooks::default()
        }),
        _ => Err(bad()),
    }
}

/// The armed hooks, read once.
fn armed() -> Option<FeedbackHooks> {
    static ARMED: OnceLock<Option<FeedbackHooks>> = OnceLock::new();
    *ARMED.get_or_init(|| {
        let value = std::env::var(VAR).ok()?;
        match parse(&value) {
            Ok(h) => {
                tracing::warn!(hooks = ?h, "WHIP test feedback hooks armed: not for production use");
                Some(h)
            }
            Err(e) => {
                tracing::error!("{e}; test feedback hooks not armed");
                None
            }
        }
    })
}

/// The answer: armed, without `transport-cc`, and with `goog-remb` for
/// REMB.
pub(crate) fn answer(
    offer: &Offer,
    config: &AnswerConfig<'_>,
) -> Result<(String, Negotiated), ProfileError> {
    let Some(h) = armed() else {
        return sdp::answer(offer, config);
    };
    sdp::answer_with(
        offer,
        config,
        AnswerHooks {
            remb: h.remb.is_some(),
            no_transport_cc: true,
        },
    )
}

/// Arms a new session's receiver, if the hooks are.
pub(crate) fn arm(session: &mut Session) {
    if let Some(h) = armed() {
        session.set_feedback_hooks(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_two_stimuli_and_refuses_the_rest() {
        assert_eq!(
            parse("loss:64:20"),
            Ok(FeedbackHooks {
                loss: Some(64),
                remb: None,
                after: Duration::from_secs(20),
            })
        );
        assert_eq!(
            parse("remb:500000:15"),
            Ok(FeedbackHooks {
                loss: None,
                remb: Some(500_000),
                after: Duration::from_secs(15),
            })
        );
        for bad in [
            "",
            "loss",
            "loss:256:1",
            "loss:64",
            "remb:x:1",
            "remb:1:1:1",
            "rr:1:1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
