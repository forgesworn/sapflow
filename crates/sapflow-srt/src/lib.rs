//! SRT for sapflow: the receiving side of live-mode SRT, as a listener.
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
//! Live mode only: no file mode, rendezvous, caller mode, groups, packet
//! filters or sending. Encrypted callers are refused for now.
//!
//! Every byte that arrives is hostile until the application accepts the
//! caller: nothing here indexes without a bound or trusts a length, and
//! it is fuzzed (`own_srt_packet`, `own_srt_listener`).

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

pub mod listener;
pub mod loss;
pub mod packet;
pub mod receiver;
pub mod rtt;
pub mod seq;

pub use listener::{Config, ConnId, Event, Listener, ListenerStats, RejectCode};
pub use receiver::{CloseReason, Stats};
