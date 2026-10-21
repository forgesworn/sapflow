//! Creator authentication is separate from capture and the runtime-free media SDK.
mod contract;
mod owner;
pub use owner::NativeSignet;
use std::fmt;
use std::sync::Arc;
use zeroize::Zeroizing;
uniffi::setup_scaffolding!();

/// Fixed redacted failures. Never export upstream URI, signer or relay messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Error)]
pub enum SignetError {
    InvalidInput,
    InvalidCredential,
    InvalidProof,
    Busy,
    Deadline,
    Cancelled,
    Refused,
    Unavailable,
    Closed,
}
impl fmt::Display for SignetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signet authentication failed ({self:?})")
    }
}
impl std::error::Error for SignetError {}

/// Only the client credential, never the creator key. Encrypt before persistence.
#[derive(uniffi::Object)]
pub struct SignetCredential {
    bytes: Zeroizing<Vec<u8>>,
}
impl fmt::Debug for SignetCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SignetCredential(redacted)")
    }
}
#[uniffi::export]
impl SignetCredential {
    /// Validate a previously encrypted-and-restored client credential.
    #[uniffi::constructor]
    pub fn restore(bytes: Vec<u8>) -> Result<Arc<Self>, SignetError> {
        let bytes = Zeroizing::new(bytes);
        contract::decode_credential(&bytes)?;
        Ok(Arc::new(Self { bytes }))
    }
    /// Foreign copies remain in Swift custody. Never log or export this value.
    pub fn encoded(&self) -> Vec<u8> {
        self.bytes.to_vec()
    }
}

/// One bounded completion slot, tagged by the caller's monotonically rising ID.
#[derive(uniffi::Enum)]
pub enum SignetEvent {
    Ready {
        request_id: u64,
        persona: String,
        credential: Arc<SignetCredential>,
    },
    Proof {
        request_id: u64,
        json: Vec<u8>,
    },
    Failed {
        request_id: u64,
        error: SignetError,
    },
}

/// Runtime-free validation for fuzzing and non-FFI callers. This creates no
/// signer, runtime, key or network connection and exposes no general signing.
#[doc(hidden)]
pub fn validate_login_contract(
    persona: &str,
    challenge: &str,
    origin: &str,
    app: &str,
    created_at: u64,
) -> Result<(), SignetError> {
    use nostr::prelude::PublicKey;
    contract::hex(persona, 64)?;
    let persona = PublicKey::from_hex(persona).map_err(|_| SignetError::InvalidInput)?;
    contract::LoginTemplate::new(persona, challenge, origin, app, created_at).map(|_| ())
}
