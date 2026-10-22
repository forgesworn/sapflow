# Native camera source and controls

This contract covers the B4R camera packet. Implementation, source review,
native builds and physical acceptance are separate gates in the
[delivery status](mobile-delivery-status.md). It does not qualify a phone or
claim a performance advantage over another broadcaster.

## Source choice and live switching

Rear is the default. Front or Rear is selected before Prepare and retained by
that exact preparation through asynchronous permissions and destination checks.
Microphone-only mode creates no camera, preview, video encoder or camera-control
owner, regardless of the saved camera choice.

A live source change is a video-input transaction within the existing show.
It preserves the hardware encoder, encoded canvas, audio capture and encoder,
media clock, recording, publisher, transport connection and destination grant.
The native adapter owns raw frames; none cross the Rust FFI or a UI bridge.
Camera switching must not call the Automatic orientation restart path, finish
the recording or acquire another grant. A subsequent Automatic orientation
restart must retain the selected Front or Rear source.

A current source failure immediately fences new camera and microphone
admission on that exact run, before queued owner cleanup. Its recording is
incomplete. A delayed fault from an old run cannot affect a successor. Normal
Stop keeps its existing accepted-tail policy and permanently cancels the old
run's switch budget, so a racing request cannot rearm a retired deadline.

Only one source transaction may be pending. Preflight checks the candidate and
prepared profile before retiring the current source. An unavailable camera or
unsupported profile leaves the existing source running. Once retirement has
started, failure stops that exact show visibly and retains any uncertain native
cleanup owner. There is no silent return to an old source or start of another
show. The original six-second monotonic transaction deadline is not extended
by another gesture or native callback. It is an operation and cleanup ceiling,
not a promise of successful media continuity for a six-second camera gap.

Each native source has its own identity, delegates and callbacks. Control and
capability handles bind the preparation, capture generation, source revision
and operation. Stop invalidates those identities synchronously. A delayed open,
frame, close, control reply or error from a retired source cannot act on its
successor. Native resources remain owned until their real completion receipts;
a timeout does not manufacture a successful close.

Requested, pending and native-confirmed source are distinct. Confirmation needs
a validated frame from the new source. This says which source the local capture
uses; it does not claim that a remote viewer has displayed it. Compressed media
already accepted by the existing chronological queues may drain. Switching a
camera is not a privacy replacement or an instantaneous cut at the viewer.

## Geometry and time

The transmitted picture is unmirrored for both sources. Front preview may be
mirrored for natural self-framing. Its reflection must not leak into encoded
pixels, local recording or point-of-interest coordinate mapping. Use native
sensor, display and producer-transform metadata; never infer the horizon from
image content. Preserve aspect ratio and any producer crop.

The prepared encoded dimensions and codec epoch stay fixed during a source
change. Android retains the GL context and encoder input surface and replaces
only the cold camera producer. iOS replaces the video input/output within the
capture session while retaining the existing hardware encoder and audio path.
Source timestamps must map into the existing run's monotonic timeline. A reset,
regression or unsupported clock domain must fail visibly rather than move the
show's clock backwards or invent a new epoch.

The existing publisher media limits remain in force. A successful live source
change must keep the true video timestamp gap below two seconds. Android uses
its ordinary fixed frame durations. iOS must drain the old hardware callbacks
and seal its retained last compressed frame at nominal cadence without closing
the encoder, rather than report the entire camera gap as one oversized sample.
The first new source timestamp is still compared with the old source's actual
last timestamp. Exceeding the media budget stops the exact show visibly; it
does not relax the Rust MPEG-TS guards, rebase timestamps or replace the codec.

In fixed orientation mode, pose changes do not restart capture. In Automatic
mode, a pose change during a source transaction is coalesced; any required
ordinary orientation restart follows source completion. See the separate
[orientation contract](capture-orientation.md).

## Native controls

Expose only capabilities returned for the current native source. The baseline
is supported zoom, focus Auto/Lock, exposure Auto/Lock and compensation, and
white-balance Auto/Lock. Supported platform presets or focus/exposure points
may extend it. Do not label digital zoom as optical zoom, native focus position
as metres, or an unsupported value as manual ISO, shutter speed or Kelvin.

Reject non-finite, out-of-range, unsupported and stale requests before calling
the native setter. Slider traffic has one bounded latest-value slot, rather
than an unbounded queue of camera work. Display requested and native-confirmed
values separately. A native rejection must not become a successful UI state.
Control work is cold; it adds no per-frame UI task, polling, copy or allocation.

## Qualification

Source tests must exercise production-used policy and ownership seams:

- Delayed source A callbacks after B, Stop at each transaction stage, withheld
  cleanup, stale capability/control tickets, original deadline and bounded work.
- One encoder, audio owner, recorder, publisher and grant through successful
  switching, plus monotonically increasing media timestamps and a fixed canvas.
  Include a half-second source gap and refusal at two seconds, old compressed
  callback completion before the boundary, and withheld cold deadline callbacks.
- Front/Rear with asymmetric labelled corners, both natural device
  orientations, all cardinal sensor/pose combinations, producer crop, preview
  reflection and inverse point mapping. A matching transform implementation
  copied into a test is insufficient evidence.
- Supported and refused controls, malformed values, capability changes, fixed
  pose behaviour, coalesced Automatic rotation and microphone-only isolation.

Physical acceptance still needs both primary phones, front/rear switches before
and during capture, preview and independent decoded pixels, picture and sound,
private recording continuity, source failures, controls and sustained resource
measurement. Retain sanitised scalar receipts in ignored local evidence.
Never commit or publish camera footage, extracted frames or phone recordings.
