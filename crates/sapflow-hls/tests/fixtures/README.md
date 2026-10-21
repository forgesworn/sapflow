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
