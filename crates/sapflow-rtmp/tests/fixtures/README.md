# Recorded fixture

`ffmpeg-rtmp-publish.bin` is the client side of a real publish, recorded
through a TCP relay: three seconds of ffmpeg's generated test pattern and a
sine tone, no camera or microphone input.

```sh
ffmpeg -f lavfi -i testsrc=size=160x90:rate=15 -f lavfi -i sine -t 3 \
  -c:v libx264 -g 15 -b:v 100k -c:a aac -b:a 32k -f flv \
  'rtmp://127.0.0.1:<port>/live/event-x?user=u&pass=p'   # ffmpeg 8.0.1
```
