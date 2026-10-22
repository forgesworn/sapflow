//! SRT for sapflow: live receiver/listener and bounded paced caller/sender.
//!
//! Written from the SRT specification (draft-sharabayko-srt-01) and from
//! watching real sessions on the wire (libsrt 1.5.4's `srt-live-transmit`
//! and the encoders in `docs/interop.md`); where the two differ, the wire
//! wins and `docs/reference.md` says so.
//!
//! Sans-IO, no async runtime: [`Listener`] takes datagrams with their
//! source address and the time, and gives back datagrams to send,
//! [`Event`]s (a caller asking to connect, payloads in order, a connection
//! closing) and when it next needs the time. The server owns the socket.
//!
//! Layers:
//!
//! - [`seq`]: 31-bit sequence numbers and 32-bit timestamps, both wrapping.
//! - [`packet`]: every packet type, parsed from and encoded to bytes.
//! - [`loss`]: the receiver's loss list and when to ask again.
//! - [`rtt`]: round-trip time from ACK/ACKACK pairs.
//! - [`receiver`]: one connection: buffer, timestamp-based delivery with
//!   too-late drop, ACKs, NAKs, keepalives, idle timeout.
//! - [`listener`]: handshakes (version 5, SYN cookies keyed per process),
//!   stream ids, limits, and the connections.
//!
//! [`Caller`] adds bounded, paced live sending. Live mode only: no file
//! mode, rendezvous, groups or packet filters. Optional AES-128/256-CTR uses
//! strict key negotiation and refresh. CTR does not authenticate media;
//! control/stream IDs remain clear. See [`crypto`] for the supported profile.
//!
//! Every byte that arrives is hostile until the application accepts the
//! caller: nothing here indexes without a bound or trusts a length, and
//! it is fuzzed (`own_srt_packet`, `own_srt_listener`).

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

pub mod caller;
pub mod crypto;
pub mod listener;
pub mod loss;
pub mod packet;
pub mod receiver;
pub mod rtt;
pub mod sender;
pub mod seq;

pub use caller::{Caller, CallerCloseReason, CallerConfig, CallerEvent, CallerStats};
pub use sender::{SendConfig, SendError, SendStats};

pub use listener::{Config, ConnId, Event, Listener, ListenerStats, RejectCode};
pub use receiver::{CloseReason, Stats};
