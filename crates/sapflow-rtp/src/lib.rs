//! RTP, RTCP, SRTP and SRTCP for sapflow's receive-only WHIP ingest.
//!
//! Written from the RFCs only: RFC 3550 and RFC 3551 (RTP and RTCP),
//! RFC 4585 and RFC 5104 (feedback), RFC 5506 (reduced-size RTCP),
//! RFC 8285 (header extensions), RFC 3711 (SRTP), RFC 7714 (AES-GCM) and
//! RFC 5764 section 4.2 (DTLS-SRTP keys). No other RTP or SRTP
//! implementation's source was read.
//!
//! Sans-IO: nothing here owns a socket, a clock or a thread. Callers hand in
//! datagrams and get back borrowed views or in-place results.
//!
//! - [`rtp`]: a zero-copy view of an RTP packet, with RFC 8285 extensions.
//! - [`rtcp`]: compound packet parsing, and [`rtcp::build`] for the receiver
//!   reports and feedback sapflow sends.
//! - [`srtp`]: SRTP and SRTCP in `SRTP_AES128_CM_HMAC_SHA1_80` and
//!   `SRTP_AEAD_AES_128_GCM`, DTLS-SRTP key splitting, replay windows.
//!
//! Every byte that arrives is hostile until it authenticates, and the
//! parsers stay careful after that: nothing indexes without a bound or
//! trusts a length, and each parser has a fuzz target (`rtp_packet`,
//! `rtcp_compound`, `srtp_unprotect`, `srtcp_unprotect`).

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

pub mod h264;
pub mod opus;
pub mod rtcp;
pub mod rtp;
pub mod rtx;
pub mod srtp;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod fuzzing;

pub use rtcp::{Compound, Packet, RtcpError};
pub use rtp::{RtpError, RtpPacket};
pub use srtp::{
    Config, DtlsSrtpKeys, Inbound, MasterKey, Outbound, Profile, Role, SrtpError,
    split_keying_material,
};
