# Native capture orientation

Automatic remains the default on both mobile apps. It follows the phone's
physical pose, including when screen rotation is locked. A live cardinal
orientation change uses the existing encoder/transport epoch replacement and
finishes the preceding private recording segment.

Fixed orientation is intended for a mounted camera. Choose it before Prepare:

| Choice | Encoded canvas |
| --- | --- |
| Automatic | Follows the physical pose |
| Portrait | Fixed upright portrait |
| Landscape (top left) | Fixed landscape, phone's top pointing left when viewed from its screen |
| Landscape (top right) | Fixed landscape, phone's top pointing right when viewed from its screen |

A fixed choice keeps the encoded dimensions and pixel transform stable. Moving
the phone away from that mounting position can therefore make the subject look
tilted in the stream. Fixed mode does not track the horizon. Preview layout
continues to fit the native display separately from the encoded canvas.

The Android natural-landscape convention uses a normal-portrait reference one
clockwise quarter-turn from the device's natural position. Relative to natural,
its Portrait, top-left and top-right poses are respectively 90, 0 and 180
degrees. On a natural-portrait device they are 0, 270 and 90 degrees. This is a
declared coordinate convention, not an inference from a tablet's camera or
earpiece location. Camera sensor metadata still controls the existing rear
camera transform.

## Command ownership

The actual capture admission must be idle before changing the selection. The
exact Preparation freezes it before asynchronous permissions, destination work
or Start. Fixed capture ignores later sensor, display and qualification pose
updates. A delayed event from another preparation cannot change the fixed show
or revive a stopped one. Automatic retains its existing restart and cleanup
rules. Microphone-only preparation ignores the video orientation setting.

Both adapters use their existing rear-camera hardware path and a single video
encoder. This setting adds no image classification, horizon detection, second
encode or per-frame control task. Front-camera mirroring remains a separate
feature.

The platform conventions are documented in Apple's
[capture orientation reference](https://developer.apple.com/documentation/avfoundation/avcapturevideoorientation)
and Android's
[orientation event reference](https://developer.android.com/reference/android/view/OrientationEventListener).
Device, display and capture enum names must not be treated as interchangeable.

## Physical qualification

Source tests and unsigned app builds do not establish physical orientation or
viewer recovery. Run each supported video profile on the primary iPhone and
Android device, with a real decoder/viewer and private local recording:

1. Check Automatic in portrait and both landscape positions, with screen
   rotation enabled and locked. Confirm upright, undistorted encoded pixels,
   matching dimensions, picture and sound, and automatic viewer recovery.
2. Prepare each fixed choice in its matching mount position. Move through all
   cardinal poses during permissions, prepared state and live capture. Confirm
   the frozen canvas, no orientation-driven codec/publisher restart and no new
   recording segment. Return to the matching position to check upright pixels.
3. Rotate the display during fixed capture. Check the preview's fit and aspect
   ratio separately from the decoded stream and recording.
4. Stop during a pending Automatic rotation, then prepare a fixed successor.
   Check that old pose and cleanup callbacks cannot rotate or restart it.
5. Start microphone-only after selecting each fixed setting. Confirm there is
   no camera, video encoder, preview or video-keyframe wait.

Retain device/OS, profile, initial mount, decoded dimensions, observed epoch and
recording continuity, failures and viewer latency in ignored local evidence.
Never commit or publish camera footage or extracted frames.
