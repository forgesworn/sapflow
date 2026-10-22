#!/usr/bin/env bash
# M4 measurements for one server, the same way for sapflow and mediamtx.
#
#   scripts/bench.sh sapflow
#   MEDIAMTX=/path/to/mediamtx scripts/bench.sh mediamtx    # 1.19.1 or 1.21.x
#
# Feed: ffmpeg test signal over RTMP, 720p30, 2.5 Mbit/s, 1 s keyframes, AAC
# (v4v's recommended encoder settings). Measures binary size, idle and loaded
# memory, CPU for one stream with no viewers, then playlist and segment
# response times and part fan-out at 250 and 750 simulated LL-HLS viewers.
# Prints a Markdown table row set for docs/benchmarks.md.
#
# Not measured here: glass-to-glass delay (needs a camera pointed at a clock
# and a phone) and container image size (needs Docker).
set -euo pipefail
cd "$(dirname "$0")/.."

SERVER=${1:?usage: scripts/bench.sh sapflow|mediamtx}
VIEWERS=${VIEWERS:-"250 750"}
SECS=${SECS:-60}
HTTP=127.0.0.1:8888
EVENT=event-bench
WORK=$(mktemp -d)
pids=()
cleanup() { { for p in "${pids[@]}"; do kill "$p"; done; wait; } 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT

# Two builds, not one: cargo unifies features across the packages of one build,
# and the viewers' HTTP/2 would then be compiled into the server being measured.
cargo build --release -q -p sapflow-server
cargo build --release -q -p sapflow-bench
BENCH=./target/release/sapflow-bench

case "$SERVER" in
  sapflow)
    BIN=./target/release/sapflow
    SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=$HTTP SAPFLOW_SRT_ADDR=127.0.0.1:8890 \
    SAPFLOW_RTMP_ADDR=127.0.0.1:1935 RUST_LOG=warn "$BIN" > "$WORK/server.log" 2>&1 &
    ;;
  mediamtx)
    BIN=${MEDIAMTX:?set MEDIAMTX to the mediamtx binary (1.19.1 or 1.21.x)}
    # mediamtx 1.21 starts a Media-over-QUIC server unless told not to: it listens
    # on *:8892 (TCP and UDP) and UDP 8893 and writes auto.key and auto.crt into the
    # working directory. `moq: no` turns it off, but a release that predates the key
    # rejects it (`json: unknown field`), so the line is added only if this binary
    # accepts it. Written with 1.21.2 in hand; 1.19.1 was not available to run, so
    # the fallback for an older binary was tested against a made-up key instead.
    mtx_accepts() { # one config line -> 0 if the binary starts with it, 1 if it names an unknown field
      local p
      printf 'logLevel: warn\nrtmp: no\nhls: no\nrtsp: no\nwebrtc: no\nsrt: no\napi: no\n%s\n' "$1" > "$WORK/probe.yml"
      "$BIN" "$WORK/probe.yml" > "$WORK/probe.log" 2>&1 &
      p=$!
      sleep 1
      if kill -0 "$p" 2>/dev/null; then kill "$p"; wait "$p" 2>/dev/null || true; return 0; fi
      wait "$p" 2>/dev/null || true
      grep -q 'unknown field' "$WORK/probe.log" && return 1
      echo "mediamtx exited on its probe config:" >&2; cat "$WORK/probe.log" >&2; exit 1
    }
    MOQ=""
    if mtx_accepts "moq: no"; then MOQ="moq: no"; fi
    # v4v's reference settings (docs/examples/mediamtx.yml), local ports.
    cat > "$WORK/mediamtx.yml" <<EOF
logLevel: warn
rtmp: yes
rtmpAddress: 127.0.0.1:1935
hls: yes
hlsAddress: $HTTP
hlsAlwaysRemux: yes
hlsVariant: lowLatency
hlsSegmentDuration: 1s
hlsPartDuration: 200ms
hlsSegmentCount: 24
hlsAllowOrigins: ['*']
rtsp: no
webrtc: no
srt: no
$MOQ
api: no
authInternalUsers:
  - user: any
    permissions:
      - action: publish
      - action: read
paths:
  all_others:
EOF
    "$BIN" "$WORK/mediamtx.yml" > "$WORK/server.log" 2>&1 &
    ;;
  *) echo "unknown server $SERVER" >&2; exit 2 ;;
esac
SPID=$!; pids+=("$SPID")
sleep 1
kill -0 "$SPID" || { cat "$WORK/server.log"; exit 1; }

# Cumulative CPU seconds of a process (ps time is [[dd-]hh:]mm:ss.cc).
cpu_secs() {
  ps -o time= -p "$1" | awk '{ n=split($1,a,/[-:]/); s=0; for(i=1;i<=n;i++) s=s*60+a[i]; print s }'
}
rss_mb() { ps -o rss= -p "$1" | awk '{ printf "%.1f", $1/1024 }'; }
now() { python3 -c 'import time; print(time.time())'; }
# CPU % of one core over the next N seconds, and peak RSS meanwhile.
measure() { # pid secs -> "cpu% peakMB"
  local c0 t0 c1 t1 peak=0 r
  c0=$(cpu_secs "$1"); t0=$(now)
  for _ in $(seq 1 "$2"); do r=$(rss_mb "$1"); peak=$(awk -v a="$peak" -v b="$r" 'BEGIN{print (b>a)?b:a}'); sleep 1; done
  c1=$(cpu_secs "$1"); t1=$(now)
  awk -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" -v p="$peak" 'BEGIN { printf "%.1f %.1f\n", 100*(c1-c0)/(t1-t0), p }'
}

size_mb=$(ls -l "$BIN" | awk '{ printf "%.1f", $5/1048576 }')
sleep 2
idle_mb=$(rss_mb "$SPID")

ffmpeg -hide_banner -loglevel error -re -stream_loop -1 -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -maxrate 2500k -bufsize 2500k \
  -c:a aac -b:a 128k -f flv "rtmp://127.0.0.1:1935/live/$EVENT" > "$WORK/ffmpeg.log" 2>&1 &
pids+=($!)
URL="http://$HTTP/live/$EVENT/index.m3u8"
for _ in $(seq 1 60); do curl -sf -o /dev/null "$URL" && break; sleep 0.5; done
curl -sf -o /dev/null "$URL" || { echo "stream never became ready" >&2; tail "$WORK/server.log" >&2; exit 1; }
# Let the playlist window fill (24 s) before measuring.
sleep 25
read -r cpu_one loaded_mb < <(measure "$SPID" 20)

echo "| $SERVER | binary MB | idle RSS MB | one stream RSS MB | one stream CPU % |"
echo "| $SERVER | $size_mb | $idle_mb | $loaded_mb | $cpu_one |"
for n in $VIEWERS; do
  "$BENCH" --url "$URL" --viewers "$n" --secs "$SECS" --ramp 10 > "$WORK/bench-$n.txt" &
  bp=$!
  sleep 10
  read -r cpu_n peak_n < <(measure "$SPID" "$SECS")
  wait "$bp" || true
  echo "--- $SERVER, $n viewers: server CPU ${cpu_n}%  peak RSS ${peak_n} MB"
  cat "$WORK/bench-$n.txt"
done
