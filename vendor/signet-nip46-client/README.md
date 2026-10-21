# signet-nip46-client

> A thin NIP-46 bunker signer for Rust — **pair / sign / restore** over `nostr-connect`.

The "key stays on your phone" model for native apps. The user's secret key lives in a
remote [NIP-46](https://github.com/nostr-protocol/nips/blob/master/46.md) bunker
([Signet](https://mysignet.app) / [Heartwood](https://github.com/forgesworn/heartwood) /
nsec.app / Amber / any compliant signer); this crate is the **client** half. It never
holds the user's key — only a throwaway per-session app keypair used to talk to the bunker.

A paired [`BunkerSession`] implements [`nostr::prelude::AsyncSignEvent`], so it is a **drop-in
signer** anywhere rust-nostr wants one: `EventBuilder::sign`, `nostr-blossom` uploads, or a
save-sync layer like [`stash-rs`](https://github.com/forgesworn/stash-rs).

Part of the [ForgeSworn](https://github.com/forgesworn) toolkit.

## Generic, with Signet-flow defaults

Nothing here is specific to any one bunker product or any consuming app. The only "Signet"
content is `SessionOptions::default()` — a 60s approval timeout tuned for the
Signet/Heartwood pairing UX. Any NIP-46 bunker works. It is the reusable Rust counterpart
of the browser-side NIP-46 signing the web apps already use.

## Usage

```rust
use signet_nip46_client::{BunkerSession, SessionOptions};

// 1. Pair — the user approves on their phone on first use.
let session = BunkerSession::pair(
    "bunker://<remote-pubkey>?relay=wss://relay.example.invalid&secret=...",
    SessionOptions::default(),
)?;
let persona = session.user_public_key().await?;     // round-trips to the bunker

// 2. Persist for a later launch (store where the app controls — keystore / file).
let persisted = session.persist().await?;           // PersistedSession (no user key)

// 3. Reconnect WITHOUT a fresh approval.
let session = BunkerSession::restore(persisted, SessionOptions::default())?;

// 4. Use it as a signer anywhere.
let event = EventBuilder::new(Kind::TextNote, "gm").finalize_async(&session).await?;
```

`PersistedSession` holds only the throwaway NIP-46 *client* secret + the bunker URI (with
the single-use connect secret stripped) — never the persona key.

## Status (2026-06-07)

- **Solo-verified:** construction from a valid URI, malformed-URI rejection, the
  persist/restore app-key round-trip, secret-stripping on restore, `PersistedSession`
  serde, and the `0.45 async signer` contract (against a local-key signer). `cargo test` green.
- **Boundary (needs a device):** the live relay handshake with a real bunker — pairing,
  approval, and signing through the phone. Exercise it with the AxeNStax spike harness
  (`tools/native-build-spike/src/bin/bunker_pair.rs`) or any NIP-46 bunker.

## Pinned versions

`nostr = 0.45.5`, `nostr-connect = 0.45.2`, `nostr-sdk = 0.45.4` (rust-nostr family, lockstep). Native-only — in
a dual-target consumer, gate behind `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`.

## Licence

MIT © ForgeSworn

## 0.2 migration

Version 0.2 deliberately breaks Rust source compatibility with the 0.44 Nostr
trait family. It implements `AsyncGetPublicKey`, `AsyncSignEvent`, `AsyncNip04`
and `AsyncNip44`; use `finalize_async` for event builders. The persisted JSON
fields and stable client key remain unchanged, and restore never silently
creates a replacement client identity. The required Rust version is 1.85.

`pair_with_relay_options` and `restore_with_relay_options` pass the maintained
SDK's `RelayOptions` to NostrConnect. The ordinary constructors retain default
relay settings. Callers with bounded runtimes should choose their own limits;
this wrapper does not fork or replace the maintained relay transport.

`PersistedSession` and `BunkerSession` debug output is redacted. Serialisation
still contains the client credential and must go into encrypted private storage.

The optional `bounded-client` API is enabled by Sapflow's exact reviewed source
patch group: NostrConnect accepts a fresh SDK client builder, and the maintained
SDK/WebSocket path forwards native buffer/frame/message settings. Default
Signet methods keep their public 0.45 constructors and do not require those
new seams. Do not publish the bounded feature as standalone registry support
until the maintained upstream option APIs have been released and repinned.
