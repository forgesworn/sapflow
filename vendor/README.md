# Cold-auth source snapshots

Only creator authentication uses these libraries. Media encoding, queues,
protocol engines and their runtime-free FFI do not depend on the auth runtime.

The exact original registry archives and Git bases, all copied file hashes, and
the narrow source patches are recorded in `signet-auth-sources.json` and
`patches/`. `scripts/verify-signet-sources.py` refuses source or patch drift before
host packaging. Each upstream library is MIT; original licences remain in each
snapshot and the auth package has its own generated third-party notice.

The ForgeSworn wrapper is migrated from its public 0.1 source to 0.2 with the
maintained Nostr 0.45 async traits. This is a Rust public API break. Persisted
client identity JSON remains compatible; restore never silently creates a new
identity. Its `bounded-client` feature is private source-patch integration until
the maintained option APIs are published. Default methods retain public upstream
constructors. Both session and persisted-credential debug output are redacted.

The three maintained transport changes expose options through existing code:

- NostrConnect accepts a fresh SDK client builder. Existing construction still
  uses its default builder; bootstrap, persona discovery and signing are shared.
- The SDK's configured transport forwards native WebSocket settings through its
  existing default implementation. The old unit-struct transport remains valid.
- Async WebSocket forwards those settings to the same maintained Tungstenite
  connection calls. DNS, proxy, headers, certificate and hostname validation are
  unchanged. No separate transport or cryptographic implementation is added.

The native auth profile opts into 16 KiB assembled messages and frames, 4 KiB
read/write buffers, a 32 KiB maximum write buffer, sixteen pool notifications,
sixteen per-relay notifications, and at most four caller-selected WSS relays.
These are component bounds, not an RSS or zero-copy claim. The maintained SDK
also retains bounded identifier-only verification/tracking caches (128,000 and
35,000 IDs respectively), and an outgoing channel of 1,024 entries per relay.
The auth adapter admits one RPC and one completion at a time, never a media
stream. TLS buffers, parsing, temporary copies and runtime overhead remain.

Publication remains open: submit the three generic option seams to their
maintained upstream projects, publish the wrapper's 0.2 migration and bounded
constructor API after those releases, then replace these path patches with exact
published versions and rerun the same gates. No unpublished remote Git revision
is required for a build, and this snapshot is not a claim that changes have been
accepted or published upstream. The task's Signet worktree is retained pending
that review/publication, with no separate build target.
