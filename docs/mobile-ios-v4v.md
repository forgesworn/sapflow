# iOS V4V creator-control foundation

This packet adds source and host tests only. The existing manual destination UI,
capture, independent recording and logical reconnect owner are unchanged. It
does not enable a V4V destination, sign a creator in, start capture, confirm
decoded origin media, or replace the signed physical test app. B4 phone
qualification remains open.

The wire follows V4V `15353b1857d6a77067adb5bed1434abb18f8c654`, specifically
`docs/native-broadcaster-sessions.md`, `src/live/native-sessions.js` and
`src/live/sapflow-native-provider.js`. The fixtures are generated contract data;
no account, origin recording or private phone data is included.

## Boundary and use

`V4vConnectorProfile` requires explicit reviewed HTTPS API/playback origins and
a playback prefix. There is no enabled default, imported URL or guessed public
service profile. Parsing a grant does not itself establish a trusted V4V/free
destination: only the eventual authenticated owner using that approved profile
may apply the connector classification.

`V4vJSON` checks UTF-8, 64KiB, sixteen container levels and decoded duplicate
keys before object normalisation. Numeric fields require canonical unsigned
integer lexemes within their declared bounds. Booleans, strings, floats,
exponents, negative zero and unsafe JavaScript integers do not coerce. The
platform String decoder validates JSON escapes and Unicode surrogate pairs.
Typed session, origin, grant and operation parsers reject unknown fields.
Matching-revision origin and session expiry must agree, apart from the
validated zero-expiry revoke-before-provision tombstone. Grant admission also
requires the exact open origin identity in prepared/connected/live/held state.
Capture rechecks that latest receipt. Contradictory authenticated status drops
the usable grant and fences the owner without clearing an uncertain operation.
An explicit null grant acknowledges a valid operation without publishing
authority, matching the backend's expired/no-grant response.

`V4vSessionEngine` has one serial caller. The future app owner runs file and
HTTP work outside capture/UI callbacks, and returns the exact captured
`V4vOperation` to `applyResponse`. It must not concurrently invoke the engine.
The engine does not schedule retries or perform network I/O itself:

1. Construct the encrypted journal and engine. Restore never starts capture;
   a restored usable grant requires a fresh authoritative Refresh response.
2. A future creator-auth adapter supplies an actual verified login result to
   `installAuthenticatedSession`. Fetch the current generation using the
   authenticated collection GET before explicitly staging Prepare.
3. `stagePrepare`, `stageRefresh` or `stageAcknowledgement` durably saves the
   exact nonce/body before `pendingRequest` can return a network request.
   `V4vHTTPCall` builds only typed collection/status/mutation routes.
4. Send via `V4vControlClient`. On timeout, disconnection, HTTP error or malformed
   response, retain the exact saved operation. Retry that same operation;
   status cannot acknowledge it or reconstruct missing grant credentials.
5. `requestEnd` invokes the local publishing fence first and saves terminal
   intent, even when authentication or a prior request is uncertain. Resolve
   that prior operation exactly, then `continueEnd` stages End using its
   acknowledged revision. A late Refresh may resolve the operation but cannot
   restore a usable grant after End or expiry.
6. Poll exact authenticated status for the separate admission/protocol/
   recording/callback receipt. `endComplete` requires the exact session's
   denied admission, stopped/no protocol, clean recording finalisation or no
   recording, and delivered/no callbacks. A failed/incomplete prefix is never
   complete replay. Acknowledgement of such a failure is explicit, returns no
   grant, and requires a separate replacement Prepare.

The owner supplies wall and monotonic clocks. The engine anchors elapsed time
to the greater of current wall time and its durable clock floor; wall rollback
cannot extend a grant. A monotonic rollback poisons/fences the owner. Successful
refresh preserves immutable transport/crypto/limit/grace policy, pinned by a
SHA-256 digest retained after active secrets are dropped. Reconnect grace
accepts zero through 3,600,000ms unchanged. The current fixed capture profile
requires at least 4Mbps; a smaller configured cap is rejected for capture, not
silently clamped or met by changing the encoder.

The future capture owner must independently arm a monotonic expiry alarm
before handing a grant to media code. HTTP/journal work can block its serial
owner. `captureGrant`/`tick` are cold admission checks, not a substitute for
that separate fast expiry fence. A late response must not revive an expired
capture owner. This packet does not yet implement that app integration.

## Platform adapters

`V4vControlClient` uses an ephemeral URLSession with normal platform TLS and
hostname validation. It follows no redirects, sends no cookies, uses no cache,
admits one request before dispatch, buffers at most 64KiB and applies one
original eight-second deadline across the request. Failures are redacted;
server bodies, credentials and endpoints are not logged. Close synchronously
fences admission, cancels the current request and invalidates the session on
its serial owner. The future app owner must call close. There is no background
transfer or media fetch. Runtime TLS/HTTP interoperability has not been tested
by this source-only packet.

`V4vEncryptedJournal` uses AES-GCM with the API origin in authenticated data.
Its injectable production key source creates an app journal key in Keychain
with `WhenUnlockedThisDeviceOnly`; it is never a Nostr identity key. A private
0700 directory, held exclusive advisory lock, pinned directory inode, no-follow
0600 single-link file, bounded read, create-new temporary file, fsync, atomic
rename, directory fsync and exact encrypted readback precede acknowledgement.
Any uncertain save poisons the live store/engine. A missing key with existing
file, missing file with existing key, locked Keychain, corruption, wrong
profile or replaced directory fails closed. There is no automatic reset,
repair, bearer import or secret export. Explicit account/journal recovery UX
and iOS Keychain/data-protection behaviour remain later gates. The adapter
does not claim protection against a compromised same-process app or provide
zeroisation guarantees for Swift/Foundation values.

## Creator identity and decoded observation remain separate

The backend requires an actual signed kind-21236 Nostr login event with its
challenge/origin/app tags and empty content. Existing browser `signet-login`
is not a native Swift signer. The workspace's Signet pairing/signing Rust
implementation needs a reviewed Swift FFI/dependency, account selection and
user consent; it is not replaced by an ephemeral RelaySwarm key or invented
key custody. No auth/import UI is included here. Signer interaction will be a
cold explicit login action, never a capture callback or automatic prompt.

Origin `connectionAttemptId` and `freshMedia` are strictly parsed availability
metadata. Missing/null values remain missing/null and do not preserve a cached
old descriptor. Paths are canonical same-event `/live/...` paths, not arbitrary
URLs. They are not fetched here and never resolved against the private control
origin. A future finite hardware decoder probe needs an approved public media
mapping, per-attempt nonce and captured local SDK attempt, actual H264 output
and declared AAC PCM, and a fresh exact post-check. Normal part/revision
advancement under the same immutable presentation must not starve that probe.
Availability, a manifest, network ACK, player time or decoder configuration
alone is not decoded confirmation. This foundation never calls the SDK's
decoded-media transition.

## Verification

Run `mobile/ios/test-v4v.sh`. It compiles the actual new sources and existing
numeric SRT destination validator against macOS Foundation, Network,
CryptoKit and Security, then runs XCTest. No app/native-package build, device,
camera, actual Keychain access or network request is used. Tests use injected
keys/stores and remove only their own generated temporary files.

The source gate also passed an arm64 iOS 16 SDK `swiftc -typecheck` with
warnings treated as errors for these four sources and the existing SRT address
validator. This checks target API availability, not an app build/signing or
execution of Keychain/TLS on an iPhone. The host suite currently has 35 passing
tests; real URLSession requests and the production Keychain key source remain
runtime qualification gaps.

Coverage includes escaped duplicate keys, numeric/Unicode/depth/body bounds,
strict scoped descriptors, End completeness, exact uncertain operations,
restart, End during Refresh, expiry versus late response, rollback, immutable
policy, failed durable commit, explicit ACK/replacement, encrypted round-trip,
missing halves/corruption/lock/profile mismatch, file/directory replacement,
bounded response accumulation, and actual HTTP-client invalid-request/close
admission. These are host source gates, not signed app, real creator login,
trusted endpoint, decoded observation or physical iPhone acceptance.
