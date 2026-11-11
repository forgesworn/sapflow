//! Receive-only WHIP ingest for sapflow, sans-IO: STUN and ICE-lite, SDP
//! offer parsing and answer building, RFC 4571 framing, the WHIP resource
//! rules, and a session that composes ICE, `sapflow-dtls`, SRTP and the
//! `sapflow-rtp` receiver.
//!
//! Written from the RFCs only: RFC 8489 and RFC 5769 (STUN and its test
//! vectors), RFC 8445 and RFC 7675 (ICE-lite and consent), RFC 7983 and
//! RFC 9443 (demultiplexing), RFC 8866, RFC 8829, RFC 8839, RFC 8843 and
//! RFC 8842 (SDP, JSEP, ICE and BUNDLE in SDP, DTLS roles), RFC 8122
//! (fingerprints), RFC 4571 and RFC 6544 (TCP framing and passive ICE-TCP),
//! RFC 6184 and RFC 7587 (the H.264 and Opus SDP parameters), RFC 4585 and
//! RFC 4588 (feedback and RTX) and RFC 9725 (WHIP). No other WebRTC, ICE,
//! STUN or SDP implementation's source was read; browsers were run only as
//! black boxes.
//!
//! - [`stun`]: the message codec, MESSAGE-INTEGRITY and FINGERPRINT.
//! - [`ice`]: the lite agent, credentials, tuples and the first-byte
//!   demultiplexer.
//! - [`sdp`]: [`sdp::Offer::parse`] and [`sdp::answer`].
//! - [`framing`]: RFC 4571 frames.
//! - [`resource`]: tokens, session IDs, refusals and `Location`.
//! - [`session`]: the [`Session`].
//! - [`router`]: every session on one socket, for the UDP owner: checks
//!   by USERNAME, everything else by the tuple's owner.
//!
//! Sans-IO: nothing here owns a socket, a clock or a thread. Every byte that
//! arrives is hostile: nothing indexes without a bound or trusts a length,
//! every buffer has a stated cap, and each parser has a fuzz target
//! (`stun_message`, `sdp_offer`, `rfc4571_frames`, `whip_session`). No
//! `Debug` output shows an address, a token or a password.

#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::unreachable,
        clippy::todo
    )
)]

mod entropy;
pub mod framing;
pub mod ice;
pub mod resource;
pub mod router;
pub mod sdp;
pub mod session;
pub mod stun;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod fuzzing;

pub use entropy::EntropyError;
pub use ice::{Credentials, IceConfig, Transport, Tuple};
pub use session::{
    EndReason, Event, Session, SessionConfig, SessionError, SessionStats, State, Transmit,
};
