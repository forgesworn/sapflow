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
//!
//! Every byte that arrives is hostile until it authenticates, and the
//! parsers stay careful after that: nothing indexes without a bound or
//! trusts a length.

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

pub mod rtcp;
pub mod rtp;

pub use rtcp::{Compound, Packet, RtcpError};
pub use rtp::{RtpError, RtpPacket};
