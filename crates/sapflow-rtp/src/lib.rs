//! RTP, RTCP, SRTP and SRTCP for sapflow's receive-only WHIP ingest, and
//! the media handling behind them.
//!
//! Written from the RFCs only: RFC 3550 and RFC 3551 (RTP and RTCP),
//! RFC 4585 and RFC 5104 (feedback), RFC 5506 (reduced-size RTCP),
//! RFC 8285 (header extensions), RFC 3711 (SRTP), RFC 7714 (AES-GCM),
//! RFC 5764 section 4.2 (DTLS-SRTP keys), RFC 6184 (H.264 payload) with
//! ITU-T H.264 for the NAL unit types and the start of the slice header,
//! RFC 7587 and RFC 6716 sections 3.1 to 3.4 and 4.3 (Opus payload,
//! framing and the CELT silence flag), RFC 4588 (RTX) and
//! draft-holmer-rmcat-transport-wide-cc-extensions-01 (transport-wide
//! congestion control). No other RTP, SRTP, depacketiser, jitter buffer,
//! congestion control or WebRTC implementation's source was read.
//!
//! Sans-IO: nothing here owns a socket, a clock or a thread. Callers hand in
//! datagrams and get back borrowed views or in-place results.
//!
//! - [`rtp`]: a zero-copy view of an RTP packet, with RFC 8285 extensions.
//! - [`rtcp`]: compound packet parsing, and [`rtcp::build`] for the receiver
//!   reports and feedback sapflow sends.
//! - [`twcc`]: transport-wide sequence numbers recorded and encoded as
//!   transport-cc feedback.
//! - [`timeline`]: one 90 kHz timeline for a session's streams, from sender
//!   reports or arrival time, with the start hold.
//! - [`srtp`]: SRTP and SRTCP in `SRTP_AES128_CM_HMAC_SHA1_80` and
//!   `SRTP_AEAD_AES_128_GCM`, DTLS-SRTP key splitting, replay windows.
//! - [`receive`]: per-stream reordering, loss recovery with NACK and RTX,
//!   keyframe requests, and depacketising into frames: [`h264`] access
//!   units and [`opus`] packets, with silence filled for DTX and loss;
//!   receiver report statistics (RFC 3550 Appendix A.3 and A.8), and the
//!   RTCP datagrams to send.
//!
//! Every byte that arrives is hostile until it authenticates, and the
//! parsers stay careful after that: nothing indexes without a bound or
//! trusts a length, and each parser has a fuzz target (`rtp_packet`,
//! `rtcp_compound`, `srtp_unprotect`, `srtcp_unprotect`, `h264_depacketise`,
//! `opus_depacketise` and `reorder_nack`), as do the report statistics,
//! transport-cc and the timeline (`rr_stats`, `twcc_feedback` and
//! `sr_timeline`).

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
pub mod receive;
mod reorder;
pub mod rtcp;
pub mod rtp;
pub mod rtx;
pub mod srtp;
mod stats;
pub mod timeline;
pub mod twcc;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod fuzzing;

pub use receive::{
    Event, Feedback, Receiver, ReceiverConfig, ReportConfig, StreamConfig, StreamId,
};
pub use rtcp::{Compound, Packet, RtcpError};
pub use rtp::{RtpError, RtpPacket};
pub use srtp::{
    Config, DtlsSrtpKeys, Inbound, MasterKey, Outbound, Profile, Role, SrtpError,
    split_keying_material,
};
pub use timeline::{Alignment, TimelineConfig};
pub use twcc::TransportCcConfig;
