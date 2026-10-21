# Transport recovery and decoded readiness

The native session policy distinguishes network retry permission from decoded
playback. Manual SRT and RTMP destinations do not need a Sapflow/V4V control API.
A socket handshake, Publish.Start, local preview, socket write or peer ACK is
never decoded-media confirmation.

## Policy

`NativeSession.snapshot()` exposes the logical generation, `attemptEpoch`,
`mediaReadiness` (`unknown` or `confirmed`), `retryBudget` (`available`,
`windowOpen` or `exhausted`) and optional `transportRecoverySource`.
Each emitted Connect advances the attempt epoch. Native callbacks must also
retain their destination identity and fence their socket ownership.

After complete startup media enters bounded transport ownership, call
`sending(generation, attemptEpoch, nowNs)`. This changes the state to `sending`,
with media readiness unknown, and clears no outage deadline. Only a validated
peer acknowledgement crossing that attempt's startup watermark may call
`transportRecovered(generation, attemptEpoch, nowNs, source)`.
A timely acknowledgement clears the network outage window and backoff. A later
actual failure then starts a new window using the destination's actual grace,
including zero. Repeated failures or intentional codec/socket replacement do
not extend an existing window. Clock rollback and stale attempts are refused.

`mediaConfirmed` remains a decoded-observation input. Only it sets `live` and
confirmed readiness. Transport recovery leaves decoded readiness unknown.
Actual decoded observation after the retry window has exhausted may describe
playback, but cannot restore retry permission. Admission expiry, revoke and
explicit End remain independently terminal; network recovery grants no authority.

When the original outage deadline expires during an accepted `sending` attempt,
`retryBudgetExhausted` leaves that bounded attempt running. This accommodates
RTMP peers with optional or sparse ACKs without pretending that socket writes
prove recovery. Automatic retry and transport replacement are then forbidden;
a later failure ends publishing until an explicit new session. A disconnected
attempt, handshake or attempt with no complete startup media still expires.
A late ACK cannot revive exhausted retry permission.

`outageDeadlineNs` retains the original historical deadline. Drive
`nextDeadlineNs` instead: it is empty after exhaustion, avoiding a timer loop.
The legacy `reconnectDeadlineNs` also becomes empty after exhaustion.
An honest status is: **Sending; peer media progress unverified; automatic
retries exhausted**. Existing byte, age, packet, native output and socket-write
watchdogs remain mandatory. Local recording stays independent.

## Bounded startup watermark

There is one scalar candidate per fresh transport, with no retained sample,
media payload or per-show list. Video requires a complete validated IDR. When
AAC is declared, a complete AAC sample must subsequently enter transport
ownership. Audio-only and video-only configurations require their declared
track only. This is submission order; A/V timestamps are preserved.

SRT marks the candidate end only when the final TS chunk enters Caller.
The target is the cumulative accepted payload-byte position. Our sender assigns
packet sequence numbers only when emitting packets and advances acknowledged
bytes only while retiring the contiguous actual flight prefix after a validated
current-peer cumulative ACK. Pending unsent chunks cannot therefore satisfy the
target. All local abandonment currently closes Caller; closing cancels the
watermark before any progress event can escape. Any future nonterminal dropping
must explicitly invalidate affected candidates rather than reuse this invariant.

SRT's cumulative ACK can include packets the receiver dropped as too late.
Consequently `srtPeerAcknowledgedWatermark` means transport progress, not
lossless/application delivery, decoding or playback. This follows the primary
[protocol draft, section 3.2.4](https://datatracker.ietf.org/doc/html/draft-sharabayko-srt-01#section-3.2.4),
which is an expired individual Internet-Draft, not an IETF standard.

RTMP marks the candidate end in the absolute post-handshake FIFO wire-byte
coordinate after a successful complete typed send. Configuration and chunk
headers count in that position. Partial output transfers leave the target
unchanged. Proof requires a validated cumulative peer ACK at least **3073 bytes beyond**
that position, with checked arithmetic. The margin covers the two conventional
counter domains: post-handshake bytes, or bytes including C0+C1+C2 (3073 bytes).
A handshake-inclusive ACK cannot therefore falsely cross a small media target;
a post-handshake peer must acknowledge another 3073 bytes after the candidate.
No baseline is inferred from the first ACK. Overflow leaves proof unknown.
Handshake/control-only ACKs before the candidate, local output transfer and
socket writes cannot satisfy it. This proof margin does not change the existing
post-handshake peer-window accounting or establish compatibility with arbitrary
counter conventions or handshake-inclusive peers. Backpressure does not commit sample timing or its candidate marker.
No ordered Ping barrier or new peer-response obligation is introduced.

## Native API

SRT already declares its tracks through its immutable mux configuration.
`NativeSrtEvent.sending` and `.peerMediaProgress` occur at most once each per
publisher. The platform adapter drains these with its existing event loop.

RTMP adds cold `declareTracks(hasVideo, hasAudio)` before the first sample.
Neither track is invalid; conflicting declarations and a new declaration after
samples are refused. Identical declarations are idempotent. Legacy callers
without a declaration keep publishing but receive no recovery proof.
`NativeRtmpEvent.sending` and `.peerMediaProgress` are drained through
`pollEvent` after existing socket reads. The fused send record also exposes
sticky `startupSubmitted` and `peerMediaProgress` scalar flags, avoiding new
per-sample FFI polling. Every proof belongs only to that publisher instance;
the native coordinator supplies its generation and attempt epoch to the policy.

These are source-level invariants. New bindings, packaged artefacts and physical
network recovery require their own qualification; they are not implied by unit
tests or this document.
