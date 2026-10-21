# Android encoded orientation implementation plan

This is a source audit, implementation contract and admission plan dated
21 October 2026. It does not establish portrait support on a physical Android
device. The initial audit was read-only. After the batch APK/source hashes were
frozen, new production helpers/renderer and JVM tests were authorised; integration
into existing camera/UI/lifecycle files is a separate source-owner packet.

The required result is an upright, non-mirrored encoded image: 720×1280 when
the phone is portrait, 1280×720 when landscape, including the two reverse poses.
The same hardware H.264 encode must feed publishing and recording. A live turn
may create a bounded interruption, new publishing epoch and new recording
segment. The UI must disclose that interruption; seamless aspect changes are
outside this first implementation.

## Current path and evidence boundary

| Source | Current behaviour | Change needed |
| --- | --- | --- |
| `mobile/android/app/src/main/AndroidManifest.xml` | Activity fixed to landscape. | Permit portrait/reverse poses and handle window relayout without an accidental publishing restart. |
| `MainActivity.kt` | Landscape row, 16:9 `SurfaceView`, `holder.setFixedSize(1280,720)`. Surface loss and `onStop` stop capture. Prepare constructs single-use publisher/recorder sinks. | Adaptive layout, selected canvas aspect, explicit surface generation and orientation-restart coordination. Preserve real foreground-loss cancellation. |
| `CaptureController.kt:prepareOnHandler` | Rear camera must advertise MediaCodec output 1280×720, exact 30 FPS and REALTIME sensor timestamps. Hardware AVC surface encoder must support 1280×720 at 30 FPS. | Probe both encoded canvases and the actual camera target type; choose a verified direct path or GPU path. Probe requested CBR support too. |
| `CaptureController.kt:setRepeating` | Camera directly targets preview and encoder surfaces. No orientation transform is requested. | Replace direct encoder target with an owned GPU input surface when rotation is needed. |
| `CaptureController.kt:codecCallback` | Format change must report 1280×720; SPS/PPS are frozen in one Rust codec handle. | Validate the selected dimensions and start a new codec/transport epoch on a turn. |
| `CaptureClock.kt` | Encoded video uses absolute MONOTONIC time, rebased at Start. AAC input is already relative to that epoch. | Preserve this contract when introducing SurfaceTexture timestamps. |
| `QueueDrainer.kt`, `PrivateRecordingSink.kt` | One immutable sample owner per consumer; file work on recorder worker. Recorder is single-use, UUID filename, create-new file. Stop drains accepted recording samples with bounded worker finalisation. | Reuse this ownership path. New segment needs fresh sinks and fanout, not a second encode or a second payload queue. |

The retained `target/mobile-android/drain-throughput-receipt.json` identifies a
real generated-binding APK with 37 passing JVM tests and no lint errors. That
receipt explicitly says physical testing of its APK is pending. Prior landscape
picture/sound observations and clock measurements do not prove upright portrait
pixels, portrait hardware capability, local recording integrity or orientation
restart recovery. There is no retained orientation capability report to admit
the current device for this feature yet.

Android documents automatic camera-preview rotation for SurfaceView, with aspect
handling left to the app. That display behaviour is not evidence that another
camera target, such as the encoder surface, contains rotated pixels. A 180°
turn also need not cause a configuration callback. Device pose and window
geometry therefore need separate inputs. [Camera preview and relative rotation](https://developer.android.com/media/camera/camera2/camera-preview).

## Admission and first implementation choice

Implement the GPU surface path first for consistent cardinal-pose behaviour.
Keep a verified direct camera-to-encoder path as an optional optimisation, not
an assumed shortcut. No new third-party camera, broadcaster or rendering
dependency is needed: Camera2, SurfaceTexture, OpenGL ES, EGL and MediaCodec
are platform APIs.

Changing only encoder width/height, a SurfaceView matrix, JPEG orientation,
recording rotation metadata or an RTMP metadata field cannot satisfy the decoded
pixel gate. Camera size support does not certify pixel orientation. The public
OutputConfiguration API provides no general output-rotation setter; do not use
hidden constructors or reflection. [Public output configuration API](https://developer.android.com/reference/android/hardware/camera2/params/OutputConfiguration).

`SCALER_ROTATE_AND_CROP` can physically rotate processed output, but it also
crops/scales into the configured output dimensions. Its documented 1280×720
example still delivers 1280×720, and supported modes vary by camera. It does not
turn the existing 1280×720 encoder into a 720×1280 encoder. If a device advertises
the desired portrait camera size and all required rotation modes, a direct path
can be admitted only after verifying session creation, returned capture-result
mode, decoded dimensions, chart orientation, field of view and timestamps in all
four poses. An unsupported/missing mode selects GPU or visibly rejects the
profile; it must not quietly publish sideways landscape.
[Rotate-and-crop semantics](https://developer.android.com/reference/android/hardware/camera2/CaptureRequest#SCALER_ROTATE_AND_CROP).

Collect one bounded, secret-free capability record during a future authorised
device session. Query once for the selected rear camera and regular AVC hardware
encoder candidates, not every camera/frame:

| Capability | Minimum query / admission |
| --- | --- |
| Pose | `OrientationEventListener.canDetectOrientation()`, natural device orientation, `SENSOR_ORIENTATION`, `LENS_FACING`; current display rotation and window size recorded separately. Unknown/near-flat readings retain the last stable pose. |
| Camera GPU input | `SCALER_STREAM_CONFIGURATION_MAP.getOutputSizes(SurfaceTexture::class.java)`, chosen size's minimum frame duration, exact 30-FPS AE range, supported continuous-video AF or explicit supported fallback. Start with sensor-native 1280×720 when advertised; choose another supported input only with an explicit aspect/crop plan. |
| Camera direct candidate | MediaCodec output sizes for both 1280×720 and 720×1280; API 31+ available rotate-and-crop modes and request/result keys. Size alone never admits a pose. |
| Session | Actual output combination: one SurfaceTexture camera target for GPU, or preview plus encoder for direct. Use `isSessionConfigurationSupported` where available, handle unsupported query, and require successful real session creation. A query is not sustained-FPS proof. |
| Encoder | `isHardwareAccelerated`, AVC Baseline/no B frames, `COLOR_FormatSurface`, both canvas sizes at 30 FPS, width/height alignments, 2.5 Mbit/s range and CBR mode. Validate actual configured/output dimensions and CSD. If portrait is rejected, do not advertise the complete orientation profile. |
| GPU | Hardware EGL renderer, external-OES texture support, recordable window config (`EGL_RECORDABLE_ANDROID`), presentation-time extension, texture/viewport limits for chosen input and both canvases. Reject known software renderers and failed shader/EGL operations; record actual renderer/config. |
| Clock | API level, sensor timestamp source and selected output timestamp base. For API 33+ request MONOTONIC explicitly; older devices need the validated mapping below. |

The existing minimum API is 29. Probe logic must gate newer keys/methods and
must not raise the minimum silently. Candidate existence is preparation evidence;
actual hardware identity, delivered cadence, memory and power still need physical
measurement. [Encoder video capabilities](https://developer.android.com/reference/android/media/MediaCodecInfo.VideoCapabilities),
[encoder bitrate modes](https://developer.android.com/reference/android/media/MediaCodecInfo.EncoderCapabilities).

## GPU surface ownership and timestamps

The proposed raw-video path is:

```text
Camera2 -> Surface(SurfaceTexture external OES texture)
                 -> one owned EGL/GL worker
                    -> rotated upright canvas -> MediaCodec input Surface
                    -> same image/transform -> preview window Surface
MediaCodec compressed output -> existing single-copy Rust ingress
                             -> independent publisher and recording handles
```

The renderer imports native camera buffers as a texture. It must not use
ImageReader YUV byte arrays, Bitmap, `glReadPixels` or application CPU colour
conversion. Rotation/scaling are GPU rendering costs and must be measured;
this is not a claim of zero copies through the camera/GPU/encoder drivers.
Use the SurfaceTexture transform matrix from each latched frame together with
the selected pose transform; producer crop/texture transforms cannot be ignored.
[SurfaceTexture image and transform contract](https://developer.android.com/reference/android/graphics/SurfaceTexture).

One dedicated GL HandlerThread owns its context, input texture, SurfaceTexture,
EGL surfaces and destruction. A frame callback sets one pending flag and posts
at most one render continuation. Latch the latest frame, draw once to the encoder
and once to preview if available; do not enqueue raw frames or post one runnable
per accumulated callback. Reuse matrices and geometry. Preview uses the same
unmirrored image, aspect-fit viewport and explicit mapping into current window
coordinates. Apply no second camera-preview auto-rotation to an already rotated
GL image. Rear-camera output stays unmirrored; future front-camera mirror policy
must be explicit and tested separately.

Use the latched camera image timestamp as the source presentation time, after
mapping it into the capture MONOTONIC domain. Set it with
`eglPresentationTimeANDROID` before encoder-surface swap. Never timestamp frames
with callback/GL completion time, repeat frames to conceal drops, or advance a
synthetic counter while ignoring capture gaps. The current CaptureClock can
then preserve its Start epoch, non-regressing video slots and separate exact
AAC cadence. [EGL presentation time API](https://developer.android.com/reference/android/opengl/EGLExt#eglPresentationTimeANDROID(android.opengl.EGLDisplay,android.opengl.EGLSurface,long)).

Important clock change: a camera SurfaceTexture defaults to SENSOR timestamps;
the current direct MediaCodec camera target defaults to MONOTONIC. For API 33+,
create its OutputConfiguration with `TIMESTAMP_BASE_MONOTONIC` explicitly and
validate actual returned image timing. On API 29–32 with the currently required
REALTIME sensor, map BOOTTIME camera timestamps using a bounded bracketed sample
of `elapsedRealtimeNanos` against `System.nanoTime` taken before capture. Retain
sampling uncertainty and recheck the offset; a changed offset, unknown timebase,
regression or implausible age fails the epoch. Background/suspend already stops
capture, so the next epoch recalibrates. Do not estimate this offset from callback
latency or directly compare sensor timestamps with the current video epoch.
[Output timestamp bases](https://developer.android.com/reference/android/hardware/camera2/params/OutputConfiguration#TIMESTAMP_BASE_DEFAULT).

Do not block the codec/camera/UI queues on EGL swaps or fences. Measure frame
age and GL submission duration; propose a 100 ms stalled-submission watchdog on
the control owner, with coalesced diagnostics. A blocked driver call cannot be
forcibly interrupted safely: close admission/cancel restart on timeout, report
unfinished cleanup and never build another GL epoch over still-owned resources.
Preview swap must not create an unbounded wait ahead of encoder submission.
Physical stall testing must determine whether the chosen window-surface swap
configuration meets this bound. Reuse existing compressed queue bounds; GPU
driver buffers are separate retention and must appear in profiling.

## Pose changes, lifecycle and restart

Use actual physical pose rather than Activity aspect alone. Quantise the sensor
angle to four cardinal poses with hysteresis (initial proposal: change only
within 30° of the next cardinal pose, leaving a 30° transition band). Retain
the last stable pose for unknown/flat readings; use display orientation only
before the first valid pose. Natural-landscape devices need an explicit natural
orientation mapping. Use one documented coordinate convention for sensor angle,
clockwise pixel rotation, texture UVs and window rotation; convert between them
once. Test all sensor-orientation/pose combinations, including 180° turns.
[OrientationEventListener semantics](https://developer.android.com/reference/android/view/OrientationEventListener).

For the initial rear-camera profile, define `deviceClockwiseDegrees` as the
listener's quantised angle relative to the device's natural pose. The proposed
clockwise pixel correction is `(sensorOrientation - deviceClockwiseDegrees +
360) % 360`. This is the rear-camera preview formula after converting the
display-rotation convention to the physical-listener convention. For a
natural-portrait phone with sensor orientation 90°, physical angles
0/90/180/270 require clockwise pixel corrections 90/0/270/180 respectively.
Do not pass a `Surface.ROTATION_*` integer or a named landscape enum into that
formula without conversion. Front-camera signs/mirroring and natural-landscape
canvas mapping require their own fixtures before admission. The texture shader
uses the inverse coordinate mapping to sample the corrected image, so its UV
matrix is not the same as a forward corner-coordinate matrix.

The implementation can keep camera ownership within the current Activity while
handling expected orientation/window configuration changes explicitly. Remove
the landscape restriction and handle the relevant orientation/screen-size
configuration changes with adaptive Compose layout. Do not simply remove the
restriction: the current Activity recreation, surfaceDestroyed and onStop paths
otherwise stop capture and discard destination state. Genuine foreground loss,
permission loss or an unexpected Activity recreation still cancels auto restart.
Any expected replacement preview surface needs a generation-matched reattachment
within the same restart budget; unrelated surface loss stops capture.

For the first implementation, restart on any stable cardinal-pose change while
capturing, including 180° changes. This is simpler to qualify than changing the
transform in flight while retaining an old epoch. During Prepared, reconfigure
only the camera/encoder/preview; publishing remains dormant. While stopping,
coalesce to one latest desired pose and do not extend the original deadline.

1. Freeze the selected destination/recording choices in a session factory owned
   by the controller. Current sink instances are single-use: reusing a closed
   PrivateRecordingSink, publisher or configured Rust codec is invalid.
2. Close old camera/GL admission and invalidate its producer token. Stop old
   producers, then signal video input EOS and microphone/AAC EOS. Allow at most
   500 ms total for old codec tail callbacks, driven asynchronously on the codec
   handler. Keep a separate old-codec drain token until EOS; incrementing today's
   single generation first would discard every tail callback. Admit only that
   old epoch's tail into its old fanout. On EOS invalidate the codec token, close
   fanout admission and drain/finalise the accepted recording prefix on its
   existing worker. Tail timeout cancels auto restart and reports a prefix.
   Keep network and recorder teardown independent and overlap their waits where
   safe; neither may run file/network IO on the codec handler.
3. Add an asynchronous stop result that distinguishes complete cleanup from
   failed/timed-out cleanup. The current `stopOnHandler` has individual worker
   waits and synchronous platform calls; their sum is not a single bounded
   orientation transaction. Start one six-second control watchdog for the whole
   old-epoch finalisation. On expiry, fence restart; cleanup can finish later.
4. Resume only after old encoders, sockets, sample owners, GL resources and file
   writer have actually released, recording finalisation succeeded, the app is
   still foreground, permissions/storage remain valid and the same restart token
   is current. Revalidate the latest pose's admitted canvas. Any failure is
   visible and leaves accepted files/prefixes available.
5. Build fresh encoder/CSD handles, fanout, UUID recorder and publisher from the
   frozen factory, then explicitly Start. Establish a new shared A/V epoch and
   request/observe a real first IDR. No old sample or callback can enter this
   epoch. New connection startup uses its existing separate deadlines.

This is a user-rotation restart, not an alternative automatic network retry or
V4V grace policy. Uplink failure alone must continue independent recording and
must not cause orientation retries. Stop/background/lock/interruptions cancel
the pending token before any cleanup wait; a late success cannot resume capture.
Expose a truthful “rotating, saving segment” state with a working Stop action,
then report “sending, playback unconfirmed” on normal publisher acceptance.

The origin/player may see a new session, timeline and HLS initialisation after
the restart. Measure and qualify recovery separately; a working app restart
does not establish continuous viewer playback. Each file has one immutable
orientation/configuration. Preserve prior segment names/status for recovery;
do not overwrite the old file or label an incomplete prefix as saved.

## Separable helper and test packet

The table records the initial pure-logic test responsibilities. The implemented
production APIs are listed in the source packet below; capability admission can
remain in the controller rather than adding an unused standalone helper:

| Helper | Contract and meaningful tests |
| --- | --- |
| `CardinalPoseSelector` | Natural-orientation mapping, integer angle normalisation, hysteresis, unknown retention. Test boundaries, jitter, reverse poses, 180° events and a natural-landscape device. |
| `OrientationPlan` | Rear camera sensor orientation plus physical pose -> clockwise pixel transform, output canvas and unmirrored policy. Test a labelled asymmetric corner chart through each transform; width/height alone is insufficient. |
| `OrientationAdmission` | Immutable camera/encoder/GPU capability snapshots -> direct/GPU/unsupported with explicit reason. Test portrait absent, wrong alignment/rate, missing CBR, missing GL extension, unsupported rotation mode and all-valid path. No hardware support inferred from a test fixture. |
| `OrientationRestartPlan` | Latest desired pose, one generation token and original deadline. Test coalescing, no deadline extension, Stop/timeout/background cancellation, late callback, failed finalisation and a subsequent fresh manual session. |
| `CameraTimestampMapper` | Explicit source timebase and bracketed offset/uncertainty -> monotonic ns with checked arithmetic. Test a large accumulated-suspend offset, regression, changed offset, source unknown and overflow. Keep queue/protocol clocks outside this helper. |
| `RenderAdmission` | At most one scheduled continuation and one latched frame; generation-fenced release. Test a callback burst, frame arrival during draw, cancellation, stalled draw and old-generation delivery without storing raw payloads. |

Integration tests then use actual platform surfaces and real generated bindings:
verify selected encoded dimensions/CSD, tail flush/finalisation, fresh sinks and
no concurrent epochs. Do not add polling or raw-frame staging just to make the
test path easier.

## Physical acceptance gate

For each admitted camera/encoder/profile, retain a secret-free capability snapshot,
APK/native-package hashes, actual hardware encoder and EGL renderer identity,
pose/rotation/canvas/timebase diagnostics and independent decoded samples.

- Cold-start in all four poses with a non-symmetric `TOP / R` chart. Preview,
  local MP4 and decoded RTMP/RTMPS and encrypted-SRT output must agree on upright,
  unmirrored image, intended field of view and 720×1280/1280×720 dimensions.
  Decode with autorotation disabled and inspect the H.264 dimensions; a player
  screenshot or recording rotation tag alone cannot pass this case.
- Turn portrait → landscape → portrait, reverse portrait and both landscape
  directions. Probe both tracks' timestamps, A/V sync, each segment's seekability
  and final duration. Measure interruption length and independent origin/player
  recovery. No timestamp jump within a segment should be hidden with duplicates.
- Hold the phone flat, jitter near a boundary, rotate rapidly during finalisation
  and turn with display rotation locked. Confirm one latest-pose restart, one
  active encode/publisher, finite callback work and correct preview geometry.
- Stop, background, lock, revoke permission or interrupt audio during rotation.
  Confirm no late restart and prompt cancellation. Fail a recorder/uplink or
  simulate low storage and slow EGL/file cleanup: retain the accepted prefix,
  keep failure status truthful and never start over still-owned resources.
- Measure capture-to-encode age, frame drops, compressed queue occupancy, GPU
  stall counts, memory, CPU/GPU load, battery and thermals against the current
  direct-surface profile with preview/publishing/recording combinations. The
  orientation renderer must meet the native performance contract, including
  no application CPU raw-frame copy and no second encode.

Implementation proceeds under Android source ownership coordination. Portrait is
admitted per verified profile only after this gate; until then the product must
report physical qualification as pending.

## Production source packet and integration contract

New files under the Android broadcaster package implement the following APIs;
they do not establish that an installed APK uses them or passes the physical gate:

- `OrientationPlan.kt`: `CardinalPose` is a physical clockwise turn from the
  natural position (`NATURAL`, `CLOCKWISE_90`, `REVERSE`, `CLOCKWISE_270`).
  `CardinalPoseSelector.accept(angleDegrees,nowNs)` admits a candidate within
  30° of its centre. Its one-shot control timer calls `settle(nowNs)` at
  `deadlineNs` after 250ms, even if Android suppresses unchanged integer-angle
  events. Unknown/boundary readings and `cancelPending()` discard the candidate
  while retaining the stable pose. `OrientationPlan.forRear(sensorOrientationDegrees,
  pose, naturalPortrait)` freezes separate sensor, natural-texture, encoder and
  preview bases. `cameraWidth`/`cameraHeight` remain in raw SENSOR space, derived
  from `sensorClockwiseDegrees` and natural dimensions, independently of pose.
  `width`/`height` describe encoded pixels; `encoderClockwiseDegrees` is only the
  residual rotation of an already natural-oriented sampled texture. Renderer
  construction refuses a raw-to-natural aspect mismatch.
- `CameraTextureGeometry.kt`: validates the complete producer matrix's rear
  sensor rotation, affine/crop bounds and aspect. Positive uniform crop and a
  bounded filtering inset of two source pixels per axis are accepted; a different
  basis or wider anisotropic crop fails explicitly. The matrix is never discarded
  or replaced with a synthetic flip.
- `CameraTimestampMapper.kt`: `.monotonic()` is for an explicitly MONOTONIC
  camera output. `.realtime()` takes three bracketed observations, retains the
  narrowest offset (maximum 250 μs uncertainty) and checks it once per second of
  incoming frames. The epoch's original mapping is frozen; a change over 1 ms
  fails the epoch. `map(timestampNs)` returns absolute MONOTONIC ns and rejects
  repetition/regression, over-one-second age or over-five-millisecond future
  time. Duplicate latest-texture deliveries are skipped before mapping. These
  conservative age/uncertainty limits still require physical validation.
- `RenderAdmission.kt`: a synchronised scalar pending flag admits one scheduled
  render task and coalesces arrivals during drawing into one continuation.
  Closing permanently fences new admission. It stores no camera pixels.
- `OrientationRestartPlan.kt`: `.request(pose,running,nowNs)` returns a ticket
  containing `token` and the original `deadlineNs`. Pending turns only change
  `desiredPose`. `.complete(token,nowNs)` accepts a real cleanup result strictly
  before deadline; `.expired(nowNs)` and `.cancel()` inhibit new automatic
  tickets. Call `.beginSession()` only after a real fresh camera/encoder epoch
  is successfully prepared, allowing a later independent rotation to obtain
  its own budget. This also permits repeated Prepared-state pose changes; it
  never starts publishing or bypasses the explicit Start action.
- `CameraGlRenderer.kt`: construction starts one GL owner asynchronously. Pass
  input dimensions, immutable plan/mapper, `captureEpoch: Long`, a control `callbacks: Handler`,
  `onReady(Surface,String)` and `onFailure(String)`. Ready returns the renderer's
  owned camera input surface plus bounded GPU-renderer identity. Open the camera
  only after Ready. All callbacks are posted to the supplied control Handler,
  which must outlive cleanup and must not block awaiting GL.

The renderer owns its input Surface/SurfaceTexture, OES texture, recordable EGL
context and EGL window surfaces. `setPreview(surface,width,height,
displayRotationDegrees)` borrows a preview Surface and uses its actual holder
dimensions. Convert display `Surface.ROTATION_*` constants to degrees first.
`startEncoding(surface,token,onStarted)` attaches a borrowed MediaCodec input
Surface only on explicit Start. `stopEncoding(token,onStopped)` closes encoder
admission immediately; the callback runs after its last accepted swap returns
and that EGL window is destroyed. The controller may then signal codec EOS.
`close(onClosed)` stops camera-image admission and cleans owned native resources;
it never releases borrowed preview/encoder Surface objects.

Initialisation has a three-second watchdog; frame/surface/cleanup operations
have independent 100 ms watchdogs driven on the control Handler only while work
is active. There is no idle polling. Failure closes admission and reports once.
Actual stop/close completion callbacks remain completion-only: a blocked driver
cannot be reported detached/released merely because a watchdog fired. The
controller's six-second restart watchdog must cancel resumption and retain its
codec/Surface owners until real GL completion; callbacks may arrive later.
Known software GPU renderer identities, missing required EGL/OES extensions and
unsupported texture/viewport sizes fail preparation. Driver-internal buffer and
rendering costs remain physical profiling work.

The renderer reuses vertex/UV buffers, frame transform matrices and viewport
geometry. Its only direct ByteBuffers contain sixteen geometry floats, not raw
camera media. It never reads camera pixels into application memory. Shader
sampling respects the SurfaceTexture producer transform; preview follows the
same rotated image with letterboxing. The callback/watchdog objects and native
driver allocations still need allocation measurement on a physical device.

A SurfaceView replacement can abandon its borrowed native window before a
queued detach runs. The renderer treats an invalid preview or preview-only
`EGL_BAD_SURFACE` / `EGL_BAD_NATIVE_WINDOW` as a detached preview and awaits the
next surface update. It preserves encoder admission. Other GPU/context errors
still fail closed; this recovery does not make an EGL driver call interruptible
or alter the controller's foreground/restart deadlines.

## Source validation and physical handoff

The integrated arm64 development APK passed offline `testDebugUnitTest`,
`lintDebug` and `assembleDebug` against the actual generated AAR on 21 October
2026. All 68 JVM tests passed, including nineteen tests for these new helpers;
lint reported zero errors and six warnings. The helper tests cover cardinal
pixel transforms, rotation-lock preview compensation, hysteresis/unknown pose,
viewport aspect, preview-loss error classification, coalesced concurrent render
admission, clock offset/uncertainty/regression and restart deadline/cancellation
races. The EGL renderer is compiled into the APK; JVM tests do not execute GLES.

The frozen candidate is `target/mobile-android/orientation-audio-debug.apk`,
SHA-256 `f90e729a390bdc9b991a250d08f909638d425415a32b91c6e6ae22ff8bf81d57`.
Its exact source manifest/archive and build results are named in
`target/mobile-android/orientation-audio-receipt.json`. The helper subset receipt
is `target/mobile-android/orientation-helper-receipt.json`; all nine source/test
hashes match the frozen APK source manifest. Earlier standalone compiler/JUnit
logs describe the preceding seventeen-test helper snapshot; final Gradle XML
and the joint build log establish the nineteen-test/current-source result.

Controller/UI integration uses fresh sink factories and async EOS/cleanup,
explicit MONOTONIC camera output on API 33+, the older validated clock mapping,
new files/transport epochs and cancellation gates. Read-only integration review
also caught and corrected expired-pose events hiding a restart timeout,
recording-finalisation failure permitting resumption, initial unknown-pose
fallback and premature stop completion during failed GL cleanup.

No device, simulator or live origin was used by this helper implementation task.
All owned isolated compiler/JVM scratch was removed. The Android owner retains
the APK and named build evidence for physical handoff. Upright decoded pixels,
preview alignment, real GPU identity/cost, recordings/A-V sync, rotation recovery
and driver/storage failure handling remain physical acceptance gates above.

## Producer-transform correction after physical failure

The later physical status-cache candidate failed orientation acceptance: the
owner saw sideways/stretched preview, and an independently decoded origin
1280×720 frame with autorotation disabled also showed the ground at the right.
The earlier arithmetic-only vertex tests did not compose the camera producer
matrix and therefore missed this defect. Those build results above remain
historical evidence, not orientation acceptance.

Camera2 SurfaceTexture output already includes sensor rotation in its producer
matrix, bringing sampling to device-natural orientation. The old shader applied
`sensor - physicalPose` again to vertices, doubling the sensor rotation. The
corrected encoder vertices apply only `+physicalPose`; preview vertices apply
`-displayRotation`. Raw camera stream sizing remains in sensor space. The full
producer crop/rotation/GL flip is preserved. This behaviour is documented in
[AndroidX Preview's SurfaceTexture contract](https://android.googlesource.com/platform/frameworks/support/+/edd8bafb328feda384a1048242a66d77f12b6699/camera/camera-core/src/main/java/androidx/camera/core/Preview.java)
and [AOSP's GLConsumer matrix composition](https://android.googlesource.com/platform/frameworks/native/+/refs/heads/main/libs/gui/GLConsumerUtils.cpp).

The new tests independently compose output raster coordinates through inverse
vertex rotation and fixed producer matrices into labelled raw-image coordinates.
They cover sensor 90°/270°, natural portrait/landscape, four physical poses,
display rotation lock, asymmetric crop translation and metric pixel lengths on
non-square canvases. They reject unexpected crop aspect beyond the filtering
allowance. These are software shader-oracle tests, not a GLES/device result.

MainActivity observes display changes including 180° turns, derives the natural
orientation from physical display-mode dimensions, and cancels pending pose
timers on background/Stop. The renderer logs one bounded `SapflowGeometry` line
per epoch with raw/natural/encoded/preview dimensions, sensor/display/pose and
the actual 16-float producer matrix. It performs no per-frame string formatting.
The next exact-source M4 build and stationary/four-pose visual acceptance are
pending; native, protocol, payload-copy and media-age bounds remain unchanged.

The first producer-aware candidate retained the wrong residual sign and failed
physical landscape acceptance: the ground appeared above the sky. The actual
Pixel producer matrix matched the sensor-90 fixture, isolating the remaining
error to physical viewpoint rather than producer layout. The correction follows
the public [CaptureRequest.JPEG_ORIENTATION rear-camera example](https://developer.android.com/reference/android/hardware/camera2/CaptureRequest#JPEG_ORIENTATION):
raw image rotation is sensor plus the clockwise OrientationEventListener angle.
Since the producer already supplies sensor rotation, the encoder applies the
positive physical angle. Display rotation uses the opposite convention.
The world-image regression now hardcodes raw A/B/C/D corner labels and separate
ground/sky positions for each physical pose, rather than assuming a sign in a
rotation-only expected-value table. Sensor 90° at physical 270° has an already
upright raw world image, independently anchoring both landscape directions.
