//! Error type for the signer wrapper.
//!
//! Kept deliberately small and string-backed: this is a thin wrapper, so we map the
//! various rust-nostr error types into three meaningful buckets rather than re-export
//! their (version-coupled) error enums across the API boundary.

use std::fmt;

/// Errors from pairing, persisting, restoring, or signing through a NIP-46 bunker.
#[derive(Debug)]
pub enum Error {
    /// A malformed `bunker://` / `nostrconnect://` URI.
    Uri(String),
    /// An invalid persisted app secret key (keystore corruption / bad serialization) —
    /// distinct from a bad URI so a caller can pick the right recovery (re-pair vs re-key).
    Key(String),
    /// NIP-46 client construction or relay-transport failure.
    Connect(String),
    /// The remote signer rejected or failed a request (approval declined, timeout, …).
    Signer(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Uri(m) => write!(f, "invalid NIP-46 URI: {m}"),
            Error::Key(m) => write!(f, "invalid persisted app secret key: {m}"),
            Error::Connect(m) => write!(f, "NIP-46 connect/transport error: {m}"),
            Error::Signer(m) => write!(f, "remote signer error: {m}"),
        }
    }
}

impl std::error::Error for Error {}
