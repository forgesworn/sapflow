# iOS audio-only Standby ownership

This is the implementation and qualification contract for continuing an
explicitly authorised microphone-only show while Standby sends generated
silence. It extends the [privacy contract](privacy-standby.md), without changing
the first release's exclusion of background camera capture. Source, generated
checks, installed builds and physical locked-screen acceptance are separate
gates in the [delivery status](mobile-delivery-status.md).

## Platform basis and limits

Apple documents continuing audio recording in the background, including screen
lock, with an input-capable audio session, microphone permission and the
`audio` background mode. See the [recording category](https://developer.apple.com/documentation/avfaudio/avaudiosession/category-swift.struct/record)
and [audio session activation guidance](https://developer.apple.com/library/archive/documentation/Audio/Conceptual/AudioSessionProgrammingGuide/ConfiguringanAudioSession/ConfiguringanAudioSession.html).
Those settings describe the supported use; they do not prove that a timer
continues after capture input stops.

The candidate retains the existing authorised audio capture session, input,
output and pinned route. A native discard delegate returns without inspecting
PCM, copying samples, metering or feeding an encoder. The fresh AAC generation
receives independently paced generated silence. Keeping that capture graph
active is the proposed execution anchor, subject to actual phone qualification.
It adds no playback stream, audio engine, permission or background mode.

Apple permits a nil [audio sample delegate](https://developer.apple.com/documentation/avfoundation/avcaptureaudiodataoutput/setsamplebufferdelegate(_:queue:)).
That API contract does not establish that a nil delegate preserves active
recording or background execution. The candidate uses an explicit discard
owner so its lifecycle and absence of PCM access can be checked.

Standby mutes the broadcast and recording, rather than electrically disabling
the microphone. The system microphone indicator may remain visible. Explain
that the microphone remains active for the opted-in background show before
Start and while Standby is active. Do not label it microphone-off.

## Authority and lifecycle

| Phase | Required ownership and behaviour |
| --- | --- |
| Prepare in initial Standby | Permission, route inventory and graph configuration only; no execution anchor starts and no media is published or recorded |
| Explicit Start | Only the current Start reservation may activate the input graph; raw admission stays closed until the exact initial mode commits. A rejected Start joins actual cleanup |
| Live to Standby | Fence raw admission synchronously, join the accepted old codec prefix, retire the old AAC generation and install the discard delegate. Keep the exact authorised audio-only input and route |
| Standby | Run one independent bounded synthetic driver. No input callback supplies its cadence. Repeated gestures retain the same owner and original deadline |
| Explicit Resume | Revalidate permission, pinned route, actual PCM format and source clock; install a fresh immutable callback identity and use the existing measured cutover. A foreground or retry callback cannot resume input |
| End publishing | Close publishing admission; independently authorised local recording and Standby may continue |
| Stop or native fault | Fence that run, join native input and callback retirement, consume the actual route-fault receipt and account for audio-session deactivation. File and network cleanup proceed independently |

Only an audio-only preparation with the existing explicit background option
can retain the anchor. An A/V show, preparation-only state, stale run, revoked
permission or unselected background option cannot acquire it. An orientation
epoch, reconnect or repeated Standby cannot create another owner or reset an
operation deadline.

Previously queued raw delegates retain their original token and must reject
new input before sample inspection. The discard delegate owns no samples.
The existing codec-prefix policy still applies: already admitted output stays
old output and cannot confirm the synthetic boundary.

A route or interruption fault must latch against the exact owner before its
queued diagnostic can be delayed or superseded. Intentional route closure and
an already latched fault have different receipts. A stale callback cannot mark
a successor incomplete. Resume still requires the creator's explicit action.

Keep native-input, callback, route, audio-session, recorder and publisher
closure facts separate. A failed or ambiguous operation cannot become a
successful cleanup receipt because another owner closed. Preserve uncertain
ownership and refuse a conflicting Start; do not wait for network drain before
finishing an independently closed recording.

## Qualification

Production-shared tests must cover initial Standby, explicit Start cancellation,
ordinary foreground operation, A/V and missing-opt-in refusal, repeated
Standby, exact-run Resume, End with recording continuation, held callbacks,
route fault versus intentional close, and ambiguous Stop cleanup.

Actual iOS API checks and all app configurations must compile with warnings as
errors. Generated codec tests must retain their original payload, timing,
independent presentation and publishing-admission oracles.

On each supported physical phone, verify the active input implementation,
locked-screen synthetic output and recording over a show-length run; then
exercise external route loss, permission changes, interruptions, Stop and
foreground return. Measure CPU, memory, wake-ups, battery and heat. A retained
capture graph or passing host test alone does not close those gates or establish
App Store acceptance.
