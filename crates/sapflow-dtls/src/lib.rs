//! A DTLS 1.2 server for sapflow's receive-only WHIP ingest, limited to the
//! profile WebRTC browsers use, with DTLS-SRTP key export.
//!
//! Written from the RFCs only: RFC 6347 (DTLS 1.2) over RFC 5246 (TLS
//! 1.2), RFC 5289 and RFC 5288 (the AES-GCM suite and its nonce), RFC 8422
//! (ECDHE and ECDSA), RFC 7627 (extended master secret), RFC 5746
//! (renegotiation indication), RFC 5764 and RFC 5705 (DTLS-SRTP and the
//! exporter), RFC 8446 section 4.1.3 (the downgrade sentinel, so it is
//! never sent), RFC 8122, RFC 8827 and RFC 8842 (fingerprints and roles in
//! WebRTC), and RFC 9147 only to recognise a DTLS 1.3 offer. No other TLS
//! or DTLS implementation's source was read; OpenSSL was run only as a
//! black box.
//!
//! The profile, and nothing outside it:
//!
//! - DTLS 1.2 as the server, answering 1.2 to a hello that also offers 1.3;
//! - `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`, P-256,
//!   `ecdsa_secp256r1_sha256`;
//! - the extended master secret required; `renegotiation_info` or the SCSV
//!   required; renegotiation refused;
//! - `use_srtp` with `SRTP_AEAD_AES_128_GCM` if offered, else
//!   `SRTP_AES128_CM_HMAC_SHA1_80`, and no MKI;
//! - mutual certificates: ours is self-signed ECDSA P-256 ([`Identity`]);
//!   the client's is accepted only if its SHA-256 matches the fingerprint
//!   from the offer ([`Fingerprint`]), with no way to skip that check.
//!
//! Sans-IO: [`Server`] owns no socket, clock or thread. Every byte that
//! arrives is hostile: nothing indexes without a bound or trusts a length,
//! every buffer has a stated cap ([`Config`]), and each parser has a fuzz
//! target (`dtls_record`, `dtls_server`, `dtls_handshake_messages`). Key
//! material and handshake secrets are zeroed on drop and never appear in
//! `Debug` output.

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

mod codec;
mod crypto;
mod error;
mod flight;
mod identity;
pub mod message;
mod reassembly;
pub mod record;
mod server;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use error::{ConfigError, Error, Limit, alert};
pub use identity::{Fingerprint, Identity};
pub use message::SrtpProfile;
pub use server::{
    Config, Connected, Event, KeyingMaterial, MAX_CHAIN, MAX_QUEUED, Server, State, Stats,
};
