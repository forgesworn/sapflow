#!/usr/bin/env bash
# Publishes a test signal with a phone encoder's irregular timing to a
# running sapflow, to reproduce the Larix part-target fault (docs/interop.md).
#
#   scripts/irregular-publish.sh srt  HOST:PORT EVENT [SECONDS] [USER PASS]
#   scripts/irregular-publish.sh rtmp HOST:PORT EVENT [SECONDS] [USER PASS]
#
# 30 fps whose frame times jitter by up to 0.05 ms either way (as Larix's
# 2998 to 3004 ticks), and in every 10 s: one dropped frame, two dropped
# frames together (a 9000-tick step), and ten dropped frames together (a
# 0.367 s frame, as a Wi-Fi stall gave). Audio runs on unbroken. Keyframes
# every 2 s, as Larix's defaults. Without credentials the server needs
# SAPFLOW_OPEN_PUBLISH=1.
#
# SRT keeps the 90 kHz jitter (needs srt-live-transmit, as smoke.sh); RTMP
# rounds every timestamp to the millisecond.
set -euo pipefail
[ $# -ge 3 ] || { sed -n '2,17p' "$0" >&2; exit 2; }
proto=$1 addr=$2 event=$3 secs=${4:-120} user=${5:-pub} pass=${6:-pass}

# select: drop frames 150, 200-201 and 290-299 of every 300. setpts: jitter
# each kept frame, on a 90 kHz time base (settb, -enc_time_base) or it is
# rounded away. -fps_mode passthrough stops ffmpeg duplicating frames into
# the gaps, which would hide them.
video_filter="select='not(eq(mod(n\,300)\,150)+between(mod(n\,300)\,200\,201)+between(mod(n\,300)\,290\,299))',settb=1/90000,setpts='PTS+(random(0)-0.5)*0.0001/TB'"
encode=(ffmpeg -hide_banner -loglevel error -re
  -f lavfi -i "testsrc=size=1280x720:rate=30"
  -f lavfi -i "sine=frequency=440:sample_rate=48000"
  -t "$secs" -vf "$video_filter" -fps_mode passthrough -enc_time_base:v 1/90000 -pix_fmt yuv420p
  -c:v libx264 -preset veryfast -tune zerolatency -force_key_frames 'expr:gte(t,n_forced*2)'
  -b:v 2500k -c:a aac -b:a 128k)

case $proto in
  srt)
    "${encode[@]}" -f mpegts - |
      srt-live-transmit -q -a:no file://con "srt://$addr?streamid=publish:live/$event:$user:$pass&latency=2000"
    ;;
  rtmp)
    "${encode[@]}" -f flv "rtmp://$addr/live/$event?user=$user&pass=$pass"
    ;;
  *) echo "protocol is srt or rtmp" >&2; exit 2 ;;
esac
