# Reviewed manual destination links

This contract extends [manual JSON profiles](manual-destination-profiles.md)
with a bounded link envelope implemented in both native apps. Source and host
checks are separate from installed OS routing and physical acceptance. The same
envelope is intended for QR input; camera scanning retains its own implementation
and physical gates. Neither feature grants V4V authority.

## Version 1 envelope

The exact format is:

```text
sapflow-manual://import/v1#<unpadded-base64url-of-profile-json>
```

Require the literal, lower-case prefix, ASCII input of at most 24,576 bytes,
and a non-empty payload using only `A-Z`, `a-z`, `0-9`, `-` and `_`. Decode at
most 16,384 bytes and require that encoding those bytes again gives the exact
original unpadded payload. Reject padding, whitespace, percent escapes,
non-zero unused bits, alternate authorities, ports, paths, queries, extra
fragments and unsupported versions. Do not normalise an alternate spelling
into an accepted envelope. Pass the decoded bytes to the existing strict
version 1 manual profile parser, including its duplicate-field, UTF-8,
endpoint, secret and transport rules.

An envelope is data, not a resource to fetch. Import performs no DNS, HTTP,
socket connection, signer operation or entitlement lookup. Raw SRT/RTMP URLs,
bunker links, creator grants and V4V configuration are not this format.

Base64url does not encrypt credentials. Deliver these links privately; do not
log, show, persist in activity restoration or include the full link in errors.
The OS or another application may handle a custom scheme. A received link
therefore remains untrusted input, even when it opens Sapflow successfully.

## Creator workflow and ownership

Opening a link offers one transient, redacted invitation. It does not change
destination mode or fields, save a profile, prepare capture, publish, resume
raw input or consent to plaintext. A second invitation cannot replace one
already under review or attach its reply to a successor action. Both apps refuse
arrival while V4V mode is selected. To import a manual destination, deliberately
leave V4V mode while idle before opening a new link.

The creator explicitly chooses Review. That action may select Manual mode
from an otherwise eligible idle mode,
then uses the ordinary profile review and Apply flow. Show only validated
transport, host, port and security. Do not show the supplied name, application
path, key, stream ID or passphrase. Plaintext requires fresh independent
consent for that exact review. Apply only fills fields; Save, Prepare and Start
remain separate actions.

Bind an invitation to its original app or activity owner and edit revision.
Use actual capture, library and import admission at receipt and Review, rather
than a delayed UI snapshot. Refuse arrival during an active preparation,
capture or conflicting private operation. Do not queue that link for a later
show. A cold launch may retain one bounded invitation until its first
foreground admission; it cannot review or apply automatically.

Background departure, edits, another import, capture reservation and app or
activity replacement invalidate stale invitations and reviews. Initial
foreground admission is distinct from leaving and returning to the background.
A genuine new delivery to the same idle Android activity may be admitted before
its next foreground callback; a restored launch Intent is not a new delivery.
Consume native link delivery once and clear the Android Intent data after the
bounded read. Do not restore credentials or approval from saved instance state.
Delayed callbacks, repeated native delivery and cancelled consent cannot rearm
an old invitation or change a newer destination.

## Camera QR import and verification

The Android camera scanner is implemented using native Camera2 and pinned
ZXing core 3.5.4, with its Apache notices packaged in every app variant. iOS
camera scanning remains under development. Installed camera behaviour and
physical acceptance are separate from source tests and app builds.

The scanner accepts only the same envelope with a smaller 2,048-byte ASCII
limit. Canonical base64url cannot produce a complete envelope of exactly
2,048 bytes: the adjacent valid sizes are 2,047 and 2,049. Larger profiles use
the existing file-import route. A successful scan offers the ordinary transient
invitation; Review, independent plaintext consent, Apply, Prepare and Start
remain separate.

Scanning holds an exclusive native camera owner that excludes capture before
permissions or device opening. Android Prepare reserves that same admission
before its permissions, destination grant or recording-storage checks. A scan
cannot borrow a prepared, live, audio-only, Standby or incompletely closed
capture owner. Retain the exact outstanding OS permission reply independently
of cancellation or activity replacement. Native camera closure, decoder return
and actual scanner-dialog dismissal must all be confirmed before releasing
scanner capacity or offering its result.

The original scan has a fixed 60-second logical deadline. Expiry cancels result
authority; it cannot prove cleanup or release an uncertain native owner. Recheck
the original deadline at final delivery, including delayed cleanup or dismissal.
A late permission grant cannot reopen a cancelled scan or supply a successor.
No background scanning, external scanner Intent, network model or Google runtime
service is used. Android copies only checked, bounded Y-plane pixels, closes the
Image before decoding, and keeps one decoder owner. Physical scanning, native
closure behaviour and coverage-guided JVM fuzzing retain separate gates.

The shared synthetic corpus is
[`manual-destination-link-v1.json`](../tests/fixtures/manual-destination-link-v1.json).
It includes all existing manual profile vectors inside the envelope, syntax
and canonical base64 controls, invalid UTF-8 and exact decoded-size boundaries.
Platform tests must also exercise real invitation/review helpers, stale
activity and foreground transitions, busy capture, repeated delivery and
independent plaintext consent. Installed OS link routing and the complete
creator workflow retain physical acceptance in
[mobile delivery status](mobile-delivery-status.md).

Run `bash mobile/ios/test-manual-destination.sh` for the shared corpus and
production invitation/review helpers. The separate
`bash mobile/ios/fuzz-manual-destination.sh` exercises the actual envelope and
strict profile parser with ASan and coverage-guided libFuzzer mutations, using
an already installed or built runtime. Its default ten-second host run is a
finite parser check, not native link routing or phone acceptance. Kotlin's
shared-corpus and deterministic mutation tests remain separate from a
coverage-guided JVM fuzz gate.
