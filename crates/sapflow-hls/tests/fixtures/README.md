# Generated transport-loss fixture

`transport-loss-6s.ts` is six seconds of a generated test pattern and sine
wave. It contains no camera, microphone or uploaded media. Regenerate it with:

```sh
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i testsrc2=size=160x90:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 6 \
  -c:v libx264 -preset ultrafast -tune zerolatency \
  -g 30 -keyint_min 30 -sc_threshold 0 -bf 0 -pix_fmt yuv420p \
  -c:a aac -b:a 64k -f mpegts transport-loss-6s.ts
```

The ingest regressions remove or mark transport packets after playback has
begun, then compare the emitted video samples with intact input and check
recovery through the final complete part. Longer input is needed to include
the next clean keyframe after a damaged PES start.

## Generated audio-only fixtures

`audio-48k-mono.ts` and `audio-44k1-stereo.ts` each contain six seconds of
generated 440 Hz sine audio and no video, camera, microphone or uploaded media.
They exercise AAC-LC transport declarations, audio-only parts and segments,
recording, and the non-integer 90 kHz timestamp conversion at 44.1 kHz.
Regenerate them with:

```sh
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 6 \
  -map 0:a:0 -vn -ac 1 -c:a aac -profile:a aac_low -b:a 96k \
  -map_metadata -1 -fflags +bitexact -flags:a +bitexact \
  -f mpegts audio-48k-mono.ts
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i sine=frequency=440:sample_rate=44100 -t 6 \
  -map 0:a:0 -vn -ac 2 -c:a aac -profile:a aac_low -b:a 128k \
  -map_metadata -1 -fflags +bitexact -flags:a +bitexact \
  -f mpegts audio-44k1-stereo.ts
```

Both were regenerated twice with FFmpeg 8.0.1 and compared byte for byte, then
independently decoded to signed 16-bit PCM. Each has exactly one AAC-LC audio
track and zero video tracks. Encoder delay and tail padding mean decoded PCM
contains slightly more than six seconds. Different FFmpeg versions may produce
different encoded bytes.

| Fixture | Bytes | SHA-256 | Decoded PCM frames per channel |
| --- | ---: | --- | ---: |
| `audio-48k-mono.ts` | 89300 | `6e69970d5bcc68a80b6c78b9606a5bf27ecc2daf5d1e8e1117421b9c2dc21670` | 289792 |
| `audio-44k1-stereo.ts` | 119944 | `01a6851216d482fc822752f0fd70baf0a81d80fd1439ee373ea7768219392b69` | 266240 |

These generated fixtures establish parser and media test inputs. They do not
establish native phone capture, hardware decoding, background operation or
whole-show recording acceptance.
