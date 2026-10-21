# iOS creator authentication source

The cold `sapflow-signet-ffi` library and `SignetAuth` Swift module reuse
ForgeSworn's maintained Signet NIP-46 wrapper. They are separate from Sapflow's
media library and runtime. The application UI and existing signed phone app do
not yet enable this connector.

A caller explicitly supplies a bunker URI or chooses restore from the encrypted
client-credential journal. Only WSS relays are admitted. Restore retains the
same client key and expected persona; a missing, corrupt or refused credential
never triggers a fresh pairing. This client key is a secret capability for a
remote signer, not the creator's Nostr private key. The remote signer's consent
and revocation policy remains authoritative. Local forgetting does not revoke a
remote grant, an issued site bearer, or a prepared publishing session.

One native worker owns one current-thread Tokio runtime. One pending request
and one completion slot, increasing request IDs, a caller's original deadline
of at most 120 seconds and out-of-band cancellation bound the operation. No
foreign callback executes on the native worker. Blocking native waits belong
only on the dedicated cold Swift queue. The process permit is released after
actual runtime destruction, including outstanding blocking work. A close timeout
is an error with retained ownership, never proof of cleanup or permission to
create a replacement owner.

The FFI exposes pairing/restore, persona discovery and fixed kind-21236 login
signing only. It verifies every requested event field, canonical ID and signature
before returning a proof. This limits the local API; it does not assert that the
remote signer's underlying remembered grant is restricted to one event kind.
Owned Rust credential buffers are zeroising, but upstream/foreign/serialisation
copies exist and no complete memory-erasure claim is made.

The Swift owner reuses the strict bounded JSON parser and the platform-trusted,
redirect-refusing `V4vControlClient`. It calls the actual site's challenge and
login routes without a bearer, signs the exact challenge/origin/app template,
and accepts only a same-persona login result. Remote signature validity alone
is not site or event-management admission. No password, imported bearer or
creator private-key input is provided. A lost login response is uncertain and
requires an explicit fresh challenge, not an automatic replay of a consumed
proof. There is no automatic capture or publishing start.

The original login deadline includes consent, HTTP and cleanup. Bearer lifetime
is conservatively anchored before login POST, subtracting request/cleanup/delivery
elapsed time and the backend's integer-second mint-time margin before adoption.
Late replies after cancellation cannot install a session. Adoption executes on
the caller's actual serial engine-owner queue. Both native destruction and
URLSession invalidation must be observed before success; merely requesting cancel
is insufficient. A bounded retained owner survives a cleanup-failure result so
later actual HTTP invalidation can release capacity without adopting a late token.

The signer credential uses a **separate** `V4vEncryptedJournal` directory and
Keychain service from the publisher's durable pending-operation/End journal.
The application integration must preserve that separation. No test accesses an
actual Keychain or account. Sign-out cannot erase unresolved publishing End.

Run `mobile/ios/test-v4v.sh` for the strict contracts and owner races, and
`node scripts/build-signet-native.mjs check` for actual generated host bindings
linked with the existing Sapflow package. The combined consumer also compiles
the attempt-scoped origin-confirmation bridge. Source/package gates, actual
remote consent, HTTPS login/authority, Keychain behaviour, app integration and
physical iPhone acceptance are separate evidence. iOS auth slices and their
combined static link remain a subsequent packaging gate.

The native transport caps and exact source provenance are described in
[the snapshot inventory](../vendor/README.md). Loopback tests cover normal and
exact-limit messages, early oversized-frame rejection, fragmented total-message
limits, write-buffer refusal and normal rejection of a generated self-signed TLS
peer. They do not qualify real NIP-46 consent or site API interoperability.
