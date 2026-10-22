# Native privacy standby

This is the B4R implementation and qualification contract for pre-start standby
and live privacy pause. An Android runtime candidate is under review; the iOS
runtime control remains to be implemented and neither app is qualified.
Source policy tests, native codec experiments, app builds and physical acceptance
are separate gates. See the [delivery status](mobile-delivery-status.md).

## Behaviour

The creator may select Standby before Prepare. Prepare and profile import still
cannot publish or record: Start remains explicit. Starting in Standby sends an
intentional original replacement picture and generated silence. Microphone-only
standby sends silence and creates no camera, preview or video encoder.

During a show, Standby synchronously closes admission of new camera frames and
microphone PCM for that exact show, before queued native work. An opaque local
preview cover appears immediately. The original replacement picture and silence
then continue on the show's existing media clock, independently of camera and
microphone callbacks. Recording receives the same sanitised compressed media as
publishing. Standby does not end the event, acquire another grant, reconnect the
publisher or reset its timeline. Resume requires an explicit creator action.
It revalidates the selected source, audio route, permissions and current
destination authority before admitting real input. Standby grants no additional
background or lock-screen execution permission.

The UI distinguishes entering standby, locally confirmed standby and failure.
Local confirmation requires actual sanitised codec output admitted by every
attached sink; a requested state, generated input, socket acknowledgement or
origin health callback is insufficient. It does not claim remote presentation.

Admission here means validated acceptance into each bounded local compressed
branch, such as the SDK fanout's actual publishing/recording queued results.
A void sink-consume call that may drop its input is not an acceptance receipt.
Transient network recovery does not detach the publishing branch or make
standby wait for a remote handshake. An exact run and sink receipt may remove
a branch only when its admission has irreversibly terminated, such as explicit
End or a failed publishing worker; its old owner still drains or closes the
accepted prefix. Recording-only continuation must retain usable standby. A
terminal branch cannot reattach within that logical show, including orientation
child epochs, and an old detach receipt
cannot remove a successor's sink.

Previously admitted native inputs, compressed queues, recorder batches and
transport output form an explicitly old chronological prefix. They may drain
before replacement; they must never be relabelled sanitised. Media already sent
to an origin, recording, DVR or viewer cannot be retracted by this control.
Do not discard partial RTMP messages or SRT retransmission state to simulate an
instantaneous remote privacy cut.

Replacement failure keeps raw admission closed and stops that exact show with
an incomplete recording. Failure, permission changes, a native timeout or a
stale callback must never fall back to camera frames or microphone audio.

## Ownership and scheduling

Privacy intent belongs to the preparation and logical show. An Automatic
orientation child epoch, transport retry, authority callback or activity/view
recreation cannot restore Live from an old initial profile. Each transaction
also binds the actual codec run and a monotonically increasing privacy revision.
Old codec output, deferred Resume, camera callbacks and cleanup receipts cannot
act on a successor. Stop permanently cancels its run's privacy admission and
deadlines, while preserving the ordinary accepted-tail policy.

Only one privacy transaction may be pending. Standby always closes raw admission,
including when Resume or a camera-source change is pending. A busy native owner
cannot turn a privacy request into continued Live capture. Native replacement
and camera-source mutation exclude each other at reservation time; retirement
must join actual cleanup or fail closed before replacement proceeds. Repeated
Standby coalesces, and stale Resume cannot reopen admission. The original
monotonic deadline cannot be extended by another gesture, new
frames, a retry or a delayed callback. The operation and cleanup ceiling is six
seconds; successful media continuity still requires the existing strict
sub-two-second timestamp gap. Native input and output paths check that
deadline themselves; withholding the cold timeout callback cannot leave raw
admission open. Exhausted counters, missing native ownership receipts and
ambiguous media attribution fail closed.

Use one bounded native media driver with a rational audio/video cadence on the
existing clock. Keep replacement pixels and zero PCM in preallocated native
storage. Android draws the slate on the retained encoder EGL surface; iOS feeds
the native hardware encoder from a native pixel buffer. A missing camera or
microphone callback must not stop standby. Catch-up is bounded; a stalled driver
must fail rather than allocate or enqueue an unbounded burst.

Raw pixels and PCM never cross Rust FFI or a UI bridge. Add no per-frame UI work,
native capability query, control polling or second video encode. The prepared
video canvas, active hardware encoder, compressed sinks and destination grant
remain unchanged. Successful transitions retain the existing publisher timing
and queue limits; they do not weaken MPEG-TS continuity guards.

## Codec boundary

Closing raw ingress alone does not remove history from a native codec. In
particular, partial AAC input, resampling and encoder lookahead can retain old
microphone PCM. A fixed number of discarded frames observed on one phone is
not a portable privacy proof. Android documents encoder flush behaviour as
[undefined](https://developer.android.com/reference/android/media/MediaCodec#states);
do not use decoder flush semantics to promise encoder sanitisation.

The chosen design retires the old native AAC generation and constructs a fresh
one supplied only generated zero PCM until replacement confirmation. Retain one
active platform AAC encoder at a time and join actual output release,
quiescence and close receipts before replacement. Old-generation output remains
old prefix material. A failure to retire the old owner blocks the replacement
and keeps raw admission closed. Resume may feed verified fresh microphone PCM
to the warmed replacement encoder; it must not reuse queued old microphone
blocks.

Use actual platform ownership facts: Android joins callback and handoff
quiescence followed by native stop/release return; iOS joins synchronous
conversion and output completion, then disposes of the old converter. Do not
invent an iOS converter-close callback.

The fresh encoder must prove the same AAC-LC 48 kHz mono configuration. Android
must retain the existing Rust codec descriptor: byte-identical configuration in
a newly allocated descriptor is not sufficient for SDK continuity. Preserve a
show-level 48 kHz cursor and exact 1024-sample access-unit cadence independently
of each native encoder's priming and input clock. Attribute any omitted warm-up,
partial input and padding explicitly. Do not conceal a gap or overlap by
resetting the show epoch or arbitrarily rebasing captured timestamps.

For video, complete and attribute the old prefix, then require a matching
generated picture and actual independent H.264 output at the new boundary.
Require a coded IDR, not just an I-picture, native keyframe flag or keyframe
request. Native input submission alone is insufficient. A delayed
old video callback cannot confirm or reopen the new privacy revision.

## Recording prerequisite

iOS audio-only recording currently expects one converter's initial priming and
final extent. Cold AAC replacement therefore requires explicit per-generation
accounting before it can be integrated. Do not replay initial-file trim on an
interior transition, discard valid samples without accounting, count priming as
original capture or report a false complete recording. Apple describes
[sample-buffer trim metadata](https://developer.apple.com/documentation/coremedia/kcmsamplebufferattachmentkey_trimdurationatstart);
its actual interaction with the chosen writer and interior transitions needs a
generated-media experiment and independent decode.

The candidate accounting keeps all coded units on one show-level cursor and
applies only the first file-leading and final file-trailing trim. Interior
priming and padding consume explicitly accounted transition slots in the
standby interval; reduce later generated zero input by the measured overhead,
rather than extending the recording or dropping original captured input.
This accounting does not relabel old-generation EOF output as sanitised: it
remains the accepted old prefix until the fresh-generation coded boundary.
An interval too short for the measured overhead must be refused. Generated
host experiments support this candidate, including repeated joins, but its
native production integration, continuous real-time pacing and resumed
microphone timestamp mapping remain prerequisites.

The experiment must exercise non-1024-aligned input lengths, old signal followed
by fresh zero-only encoding, multiple Standby/Resume cycles and final partial
input. Record actual converter priming, retained access units, valid input
frames, discarded warm-up and decoded extents. Use a recognisable generated
marker buffered before the fence and a distinct raw marker attempted after it.
Predetermine input and coded-output cutover watermarks; do not move them after
decoding to hide a failure. The post-fence marker must not be admitted. New
encoded output must have provably synthetic provenance. Test both live output
and the recording independently, including the decoder's settling interval:
old decoder overlap and queued old prefix are distinct from newly admitted
private content. Local coded-output confirmation does not promise instantaneous
decoded silence in every remote player.
Decode the concatenated old prefix and fresh AAC with one continuous decoder,
as a receiver would. Decoding the fresh generation with a reset decoder is a
useful diagnostic but cannot establish transition behaviour.

## Acceptance

- Production-used admission tests cover pre-start standby, delayed raw input,
  stale codec output, racing Resume and Stop, exhausted revisions, withheld
  deadlines, source-operation exclusion and microphone-only isolation.
- Native generated-media tests prove bounded replacement output, marker
  exclusion, continuous timestamps and truthful recording extents across repeat
  transitions. A host codec result does not qualify an Android or iOS phone.
- Full app and package checks cover both ordinary and qualification builds.
  Physical tests then verify preview concealment, independently decoded picture
  and silence, Resume, front/rear and orientation changes, audio routing,
  interruption, background policy and sustained CPU, memory, battery and heat.

Never commit or publish camera footage, extracted camera frames, microphone
recordings or archives containing them. Retain private physical evidence in
ignored lab storage; source-controlled fixtures must have verified synthetic
provenance.
