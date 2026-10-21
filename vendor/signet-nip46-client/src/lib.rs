//! # signet-nip46-client
//!
//! A thin, **generic** NIP-46 bunker signer for Rust. It wraps
//! [`nostr_connect::prelude::NostrConnect`] and exposes a small **pair / sign / restore**
//! surface plus an implementation of [`nostr::prelude::AsyncSignEvent`], so a paired session
//! is a drop-in signer anywhere rust-nostr wants one — `EventBuilder::sign`,
//! `other 0.45 clients`, or a save-sync layer like `stash-rs`.
//!
//! The "key stays on your phone" model: the user's secret key lives in a remote NIP-46
//! bunker (Signet / Heartwood / nsec.app / Amber / a self-hosted signer). This crate is
//! the **client** half — it never holds the user's key, only a throwaway per-session app
//! keypair used to talk to the bunker.
//!
//! ## Generic, with Signet-flow defaults
//!
//! Nothing here is specific to any one bunker product or to any consuming app. The only
//! "Signet" content is [`SessionOptions::default`] (a 60s approval timeout tuned for the
//! Signet/Heartwood pairing UX). Any NIP-46 bunker works.
//!
//! ## Pair → persist → restore
//!
//! ```no_run
//! use signet_nip46_client::{BunkerSession, SessionOptions};
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Pair (the user approves on their phone on first use).
//! let session = BunkerSession::pair(
//!     "bunker://<remote-pubkey>?relay=wss://relay.example.invalid&secret=...",
//!     SessionOptions::default(),
//! )?;
//! let _persona = session.user_public_key().await?;       // round-trips to the bunker
//!
//! // 2. Persist for later (store this somewhere the app owns — keystore / file).
//! let persisted = session.persist().await?;
//! let json = serde_json::to_string(&persisted)?;
//!
//! // 3. On a later launch — reconnect WITHOUT a fresh approval.
//! let persisted = serde_json::from_str(&json)?;
//! let session = BunkerSession::restore(persisted, SessionOptions::default())?;
//! # Ok(()) }
//! ```
//!
//! The **live pairing handshake needs the user's device** — it is the one part that can't
//! be exercised headlessly. Construction, persistence round-tripping, and the
//! `AsyncSignEvent` delegation are unit-tested with local keys.

mod error;

pub use error::Error;
pub use nostr_sdk::prelude::{ClientBuilder, RelayOptions};

use std::time::Duration;

use nostr::nips::{nip04::AsyncNip04, nip44::AsyncNip44};
use nostr::prelude::*;
use nostr_connect::prelude::NostrConnect;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

/// Tunables for a bunker session.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// How long to wait for the bunker to answer a request (connect, sign, …).
    pub timeout: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        // Tuned for the Signet/Heartwood approval UX: long enough for a human to tap
        // "approve" on their phone, short enough to fail fast if the bunker is offline.
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

/// Everything needed to silently reconnect a session on a later launch.
///
/// Holds **no user key**: `app_secret_key` is the throwaway NIP-46 *client* identity, and
/// `bunker_uri` carries the remote signer's pubkey + relays. Persist it somewhere the app
/// controls (an OS keystore, an app-private file). Treat `app_secret_key` as a secret —
/// it identifies this client session to the bunker.
#[derive(Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    /// Hex secret key of the per-session NIP-46 **client** keypair (not the persona).
    pub app_secret_key: String,
    /// `bunker://<remote-signer-pubkey>?relay=...` — the reconnect URI (secret stripped).
    pub bunker_uri: String,
}

impl std::fmt::Debug for PersistedSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersistedSession(redacted)")
    }
}

/// A live NIP-46 bunker session. Implements [`nostr::prelude::AsyncSignEvent`].
pub struct BunkerSession {
    inner: NostrConnect,
    /// The URI this session was paired/restored from, kept for a secret-less persist
    /// fallback when the canonical post-handshake `bunker_uri()` isn't reachable yet.
    original_uri: String,
}

impl std::fmt::Debug for BunkerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BunkerSession(redacted)")
    }
}

impl BunkerSession {
    /// Pair with a bunker from a `bunker://` or `nostrconnect://` URI. A fresh app keypair
    /// is generated for this session. Construction is offline; the first
    /// [`user_public_key`](Self::user_public_key) / sign call performs the relay handshake
    /// and triggers the bunker's approval prompt.
    pub fn pair(uri: &str, opts: SessionOptions) -> Result<Self, Error> {
        Self::pair_with_relay_options(uri, opts, RelayOptions::default())
    }

    /// Pair using caller-selected relay limits. Existing pairing semantics and
    /// signer authority are unchanged; no extra transport implementation.
    pub fn pair_with_relay_options(
        uri: &str,
        opts: SessionOptions,
        relay_options: RelayOptions,
    ) -> Result<Self, Error> {
        let parsed = NostrConnectUri::parse(uri).map_err(|e| Error::Uri(e.to_string()))?;
        let app_keys = Keys::generate();
        let inner = NostrConnect::new(parsed, app_keys, opts.timeout, Some(relay_options))
            .map_err(|e| Error::Connect(e.to_string()))?;
        Ok(Self {
            inner,
            original_uri: uri.to_owned(),
        })
    }

    /// Pair on a fresh bounded SDK client. Requires the reviewed maintained
    /// NostrConnect client-builder seam; no protocol implementation is replaced.
    #[cfg(feature = "bounded-client")]
    pub fn pair_with_client_builder(
        uri: &str,
        opts: SessionOptions,
        relay_options: RelayOptions,
        builder: ClientBuilder,
    ) -> Result<Self, Error> {
        let parsed = NostrConnectUri::parse(uri).map_err(|e| Error::Uri(e.to_string()))?;
        let app_keys = Keys::generate();
        let inner = NostrConnect::new_with_client_builder(
            parsed,
            app_keys,
            opts.timeout,
            Some(relay_options),
            builder,
        )
        .map_err(|e| Error::Connect(e.to_string()))?;
        Ok(Self {
            inner,
            original_uri: uri.to_string(),
        })
    }

    /// Reconnect a previously-paired session by reusing its stored app keypair + bunker
    /// URI. Re-uses the existing NIP-46 grant, so a well-behaved bunker does not re-prompt.
    pub fn restore(persisted: PersistedSession, opts: SessionOptions) -> Result<Self, Error> {
        Self::restore_with_relay_options(persisted, opts, RelayOptions::default())
    }

    /// Restore the same client identity with caller-selected relay limits.
    pub fn restore_with_relay_options(
        persisted: PersistedSession,
        opts: SessionOptions,
        relay_options: RelayOptions,
    ) -> Result<Self, Error> {
        let parsed =
            NostrConnectUri::parse(&persisted.bunker_uri).map_err(|e| Error::Uri(e.to_string()))?;
        let app_keys =
            Keys::parse(&persisted.app_secret_key).map_err(|e| Error::Key(e.to_string()))?;
        let inner = NostrConnect::new(parsed, app_keys, opts.timeout, Some(relay_options))
            .map_err(|e| Error::Connect(e.to_string()))?;
        Ok(Self {
            inner,
            original_uri: persisted.bunker_uri,
        })
    }

    /// Restore the same credential on a fresh caller-bounded SDK client.
    #[cfg(feature = "bounded-client")]
    pub fn restore_with_client_builder(
        persisted: PersistedSession,
        opts: SessionOptions,
        relay_options: RelayOptions,
        builder: ClientBuilder,
    ) -> Result<Self, Error> {
        let parsed =
            NostrConnectUri::parse(&persisted.bunker_uri).map_err(|e| Error::Uri(e.to_string()))?;
        let app_keys =
            Keys::parse(&persisted.app_secret_key).map_err(|e| Error::Key(e.to_string()))?;
        let inner = NostrConnect::new_with_client_builder(
            parsed,
            app_keys,
            opts.timeout,
            Some(relay_options),
            builder,
        )
        .map_err(|e| Error::Connect(e.to_string()))?;
        Ok(Self {
            inner,
            original_uri: persisted.bunker_uri,
        })
    }

    /// Produce the [`PersistedSession`] to store for a later [`restore`](Self::restore).
    ///
    /// Prefers the canonical post-handshake bunker URI (carries the discovered remote
    /// signer pubkey for the `nostrconnect://` flow and drops the single-use connect
    /// secret). Falls back to a secret-stripped form of the original `bunker://` URI when
    /// the session hasn't completed its handshake yet.
    ///
    /// **Errors** if the session was paired from a `nostrconnect://` URI and hasn't
    /// completed its handshake — that flow has no stable reconnect address until the remote
    /// signer pubkey is discovered, so call [`user_public_key`](Self::user_public_key)
    /// first (which completes the handshake), then `persist()`.
    pub async fn persist(&self) -> Result<PersistedSession, Error> {
        let bunker_uri = match self.inner.bunker_uri().await {
            Ok(uri) => uri.to_string(),
            Err(_) => self.secretless_original()?,
        };
        Ok(PersistedSession {
            app_secret_key: self.inner.local_keys().secret_key().to_secret_hex(),
            bunker_uri,
        })
    }

    /// The persona pubkey this session signs as. Round-trips to the bunker on first call.
    pub async fn user_public_key(&self) -> Result<PublicKey, Error> {
        self.inner
            .get_public_key_async()
            .await
            .map_err(|e| Error::Signer(e.to_string()))
    }

    /// Tear down relay connections held by the session.
    pub async fn shutdown(self) {
        self.inner.shutdown().await;
    }

    /// Re-emit the original `bunker://` URI without its single-use connect secret (restore
    /// must not replay it). A `nostrconnect://` original has no stable reconnect address
    /// until the handshake completes, so it errors rather than persist an unusable URI that
    /// would silently re-pair on restore.
    fn secretless_original(&self) -> Result<String, Error> {
        let parsed =
            NostrConnectUri::parse(&self.original_uri).map_err(|e| Error::Uri(e.to_string()))?;
        match parsed {
            NostrConnectUri::Bunker {
                remote_signer_public_key,
                relays,
                ..
            } => Ok(NostrConnectUri::Bunker {
                remote_signer_public_key,
                relays,
                secret: None,
            }
            .to_string()),
            NostrConnectUri::Client { .. } => Err(Error::Connect(
                "cannot persist a nostrconnect:// session before its handshake completes \
                 (no stable reconnect URI yet — call user_public_key() first)"
                    .to_string(),
            )),
        }
    }
}

// Delegate the maintained 0.45 async capabilities. Each associated error is
// the upstream error, preserving typed protocol failures for generic callers.
impl AsyncGetPublicKey for BunkerSession {
    type Error = nostr_connect::error::Error;
    fn get_public_key_async(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PublicKey, Self::Error>> + Send + '_>> {
        self.inner.get_public_key_async()
    }
}
impl AsyncSignEvent for BunkerSession {
    type Error = nostr_connect::error::Error;
    fn sign_event_async(
        &self,
        unsigned: UnsignedEvent,
    ) -> Pin<Box<dyn Future<Output = Result<Event, Self::Error>> + Send + '_>> {
        self.inner.sign_event_async(unsigned)
    }
}
impl AsyncNip04 for BunkerSession {
    type Error = nostr_connect::error::Error;
    fn nip04_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        self.inner.nip04_encrypt_async(public_key, content)
    }
    fn nip04_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        self.inner.nip04_decrypt_async(public_key, content)
    }
}
impl AsyncNip44 for BunkerSession {
    type Error = nostr_connect::error::Error;
    fn nip44_encrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        self.inner.nip44_encrypt_async(public_key, content)
    }
    fn nip44_decrypt_async<'a>(
        &'a self,
        public_key: &'a PublicKey,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, Self::Error>> + Send + 'a>> {
        self.inner.nip44_decrypt_async(public_key, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A well-formed bunker URI for construction tests (random pubkey; never connected).
    const BUNKER_URI: &str = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid&secret=abc123";

    #[test]
    fn persisted_debug_never_exposes_identity_uri_or_secret() {
        let persisted = PersistedSession {
            app_secret_key: "generated-secret".into(),
            bunker_uri: "generated-private-uri".into(),
        };
        assert_eq!(format!("{persisted:?}"), "PersistedSession(redacted)");
        let json = serde_json::to_value(&persisted).unwrap();
        assert_eq!(json["app_secret_key"], "generated-secret");
        assert_eq!(json["bunker_uri"], "generated-private-uri");
    }

    #[test]
    fn caller_relay_limits_and_default_constructors_preserve_same_restore_identity() {
        let keys = Keys::generate();
        let options = RelayOptions::new()
            .notification_channel_size(16)
            .reconnect(false);
        let persisted = PersistedSession {
            app_secret_key: keys.secret_key().to_secret_hex(),
            bunker_uri: BUNKER_URI.into(),
        };
        let session = BunkerSession::restore_with_relay_options(
            persisted,
            SessionOptions::default(),
            options,
        )
        .unwrap();
        assert_eq!(session.inner.local_keys().public_key(), keys.public_key());
        assert!(BunkerSession::pair_with_relay_options(
            BUNKER_URI,
            SessionOptions::default(),
            RelayOptions::default()
        )
        .is_ok());
    }

    #[test]
    fn pair_constructs_from_a_valid_bunker_uri() {
        let s = BunkerSession::pair(BUNKER_URI, SessionOptions::default());
        assert!(
            s.is_ok(),
            "pairing should construct (no live connection yet)"
        );
        assert_eq!(format!("{:?}", s.unwrap()), "BunkerSession(redacted)");
    }

    #[test]
    fn pair_rejects_a_malformed_uri() {
        let s = BunkerSession::pair("not-a-uri", SessionOptions::default());
        assert!(matches!(s, Err(Error::Uri(_))));
    }

    #[test]
    fn restore_round_trips_a_persisted_session() {
        // The app keypair survives a hex round-trip with a stable pubkey, and the restored
        // session keeps the same client identity — the core of silent reconnection.
        let app_keys = Keys::generate();
        let persisted = PersistedSession {
            app_secret_key: app_keys.secret_key().to_secret_hex(),
            bunker_uri: BUNKER_URI.to_string(),
        };
        let restored =
            BunkerSession::restore(persisted, SessionOptions::default()).expect("restore");
        assert_eq!(
            restored.inner.local_keys().public_key(),
            app_keys.public_key(),
            "restored session must reuse the same app keypair"
        );
    }

    #[test]
    fn restore_rejects_a_bad_secret_key() {
        let persisted = PersistedSession {
            app_secret_key: "deadbeef".to_string(), // too short to be a valid secret key
            bunker_uri: BUNKER_URI.to_string(),
        };
        assert!(matches!(
            BunkerSession::restore(persisted, SessionOptions::default()),
            Err(Error::Key(_))
        ));
    }

    #[test]
    fn persisted_session_serde_round_trips() {
        let p = PersistedSession {
            app_secret_key: "a".repeat(64),
            bunker_uri: BUNKER_URI.to_string(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.app_secret_key, p.app_secret_key);
        assert_eq!(back.bunker_uri, p.bunker_uri);
    }

    #[test]
    fn secretless_fallback_strips_the_connect_secret() {
        let s = BunkerSession::pair(BUNKER_URI, SessionOptions::default()).unwrap();
        let stripped = s.secretless_original().unwrap();
        assert!(
            !stripped.contains("secret="),
            "the single-use connect secret must not survive into a restore URI: {stripped}"
        );
        // Round-trips back to a parseable bunker URI.
        assert!(NostrConnectUri::parse(&stripped).is_ok());
    }

    // The `AsyncSignEvent` delegation pattern, exercised with a local Keys signer (the same
    // trait a BunkerSession implements) — proves the contract `stash-rs`/`other 0.45 clients`
    // rely on holds. The live bunker delegation is the boundary (needs a device).
    #[tokio::test]
    async fn nostr_signer_contract_holds_for_a_local_key_signer() {
        let keys = Keys::generate();
        let pk = keys.public_key();
        // encrypt-to-self round-trip (the exact path stash-rs uses for blobs).
        let ct = AsyncNip44::nip44_encrypt_async(&keys, &pk, "payload")
            .await
            .unwrap();
        let pt = AsyncNip44::nip44_decrypt_async(&keys, &pk, &ct)
            .await
            .unwrap();
        assert_eq!(pt, "payload");
        // sign + verify.
        let event = EventBuilder::new(Kind::TextNote, "hi")
            .finalize_async(&keys)
            .await
            .unwrap();
        assert!(event.verify_signature());
    }
}
