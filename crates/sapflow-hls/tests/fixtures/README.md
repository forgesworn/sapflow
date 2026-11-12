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

## Generated resolution-change fixture

`resolution-change.ts` is two seconds of a generated test pattern and sine
wave at 160x90, then two at 128x72, joined by stream copy, so the SPS changes
mid-stream at the second part's first IDR (3.44 s) on one timeline. It
contains no camera, microphone or uploaded media. The ingest regressions
check that each front end starts one new codec epoch there. Regenerate it
with:

```sh
for s in 160x90 128x72; do
  ffmpeg -nostdin -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=${s}:rate=30" \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t 2 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 -sc_threshold 0 -bf 0 -pix_fmt yuv420p \
    -c:a aac -profile:a aac_low -b:a 64k \
    -map_metadata -1 -fflags +bitexact -flags +bitexact -f mpegts "part-${s}.ts"
done
printf "file part-160x90.ts\nfile part-128x72.ts\n" > list.txt
ffmpeg -nostdin -hide_banner -loglevel error -y -f concat -safe 0 -i list.txt \
  -c copy -map_metadata -1 -fflags +bitexact -f mpegts resolution-change.ts
```

`resolution-blip.ts` is the same test pattern at 160x90 for two seconds,
128x72 for three frames (0.1 s), then 160x90 for one second, joined the same
way. Its middle epoch is shorter than one part, after part timing is known,
so the change out of it seals its one open part: it is served, and the
server's regression checks that it is recorded. Regenerate it with the
commands above, encoding `160x90` for `-t 2`, `128x72` for `-t 0.1` and
`160x90` for `-t 1`, and joining the three in that order into
`resolution-blip.ts`.

Both were generated with FFmpeg 8.1.2. Different FFmpeg versions may produce
different encoded bytes.

| Fixture | Bytes | SHA-256 | Video frames |
| --- | ---: | --- | ---: |
| `resolution-change.ts` | 231804 | `c9aba77df0c453aaaa7fd9a33ead9533f4ad28c06475fbaa1bf46eddbba7e34b` | 60 at 160x90, 60 at 128x72 |
| `resolution-blip.ts` | 192700 | `e5bc8b165dd46647fd1f0bcbf08f81f048096dfeead241bbce63a7df98e53337` | 60 at 160x90, 3 at 128x72, 30 at 160x90 |

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

## Generated Opus fixtures

`opus-48k-stereo-20ms.ogg`, `opus-48k-mono-60ms.ogg` and
`opus-48k-stereo-120ms.ogg` are generated sine tones (440 Hz, with 880 Hz on
the right in the stereo ones), encoded with FFmpeg's libopus encoder. They
contain no camera, microphone or uploaded media. They give the Opus tests real encoded packets: 20 ms
packets, 60 ms SILK packets and 120 ms packets of several frames. The Ogg
container is read only by test code; sapflow takes Opus packets, not Ogg.
Regenerate them with:

```sh
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex "[0:a][1:a]amerge=inputs=2[a]" -map "[a]" -t 2 \
  -c:a libopus -b:a 64k -frame_duration 20 \
  -map_metadata -1 -fflags +bitexact -flags:a +bitexact opus-48k-stereo-20ms.ogg
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 1 -ac 1 \
  -c:a libopus -b:a 32k -frame_duration 60 \
  -map_metadata -1 -fflags +bitexact -flags:a +bitexact opus-48k-mono-60ms.ogg
ffmpeg -nostdin -hide_banner -loglevel error \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -filter_complex "[0:a][1:a]amerge=inputs=2[a]" -map "[a]" -t 1 \
  -c:a libopus -b:a 64k -frame_duration 120 \
  -map_metadata -1 -fflags +bitexact -flags:a +bitexact opus-48k-stereo-120ms.ogg
```

They were made with FFmpeg 8.1.2. Each header gives a pre-skip of 312; the
last packet of each is trimmed by its Ogg granule position, which sapflow
does not read.

| Fixture | Bytes | SHA-256 | Packets | TOC duration |
| --- | ---: | --- | ---: | --- |
| `opus-48k-stereo-20ms.ogg` | 21426 | `6b4ea92d9b700d07252c2a28abe9ec7f2113a98c9d2e13f23c6d7e4d71d95a30` | 101 | 960 samples each |
| `opus-48k-mono-60ms.ogg` | 4228 | `09fe117f190eda3a869291abeab11b5322d420ea1539b71402cf7ca3cf216fe3` | 17 | 2880 samples each |
| `opus-48k-stereo-120ms.ogg` | 8788 | `fe31350cc590d72bd436e25dbb907bd1cd83eb651a8fcfb26fe1c955408f452b` | 9 | 5760 samples each |
