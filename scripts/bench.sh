#!/usr/bin/env bash
# M4 measurements for one server, the same way for sapflow and mediamtx.
#
#   scripts/bench.sh sapflow
#   SAPFLOW_HTTP_THREADS=1 scripts/bench.sh sapflow
#   MEDIAMTX=/path/to/mediamtx scripts/bench.sh mediamtx    # 1.19.1 or 1.21.x
#   SLOW=5 SLOW_RATE=50000 scripts/bench.sh sapflow          # 5 slow consumers
#
# SLOW=N and SLOW_RATE=BYTES_PER_SEC make N of the simulated viewers read at
# no more than that rate; they are reported in a section of their own.
# Runs on macOS and on Linux (a native container, see
# docs/performance-exercise.md); scripts/performance.py drives either.
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
if [ "${SLOW:-0}" = 0 ] && [ -n "${SLOW_RATE:-}" ]; then
  echo "SLOW_RATE needs SLOW (the number of slow viewers)" >&2; exit 2
fi
# The loader's slow-consumer options, only when asked for. The expansion below
# is safe for an empty array under set -u on macOS's Bash 3.2.
slow_args=()
if [ "${SLOW:-0}" != 0 ]; then
  slow_args=(--slow "$SLOW" --slow-rate "${SLOW_RATE:?set SLOW_RATE (bytes per second) with SLOW}")
fi
# The caller provides a new directory under ignored/private scratch. Retain
# diagnostics on success and failure; never overwrite an earlier trial's logs.
if [ -n "${BENCH_ARTIFACTS:-}" ]; then mkdir -m 700 "$BENCH_ARTIFACTS"; fi
VIEWERS=${VIEWERS:-"250 750"}
SECS=${SECS:-60}
HTTP=127.0.0.1:8888
EVENT=event-bench
WORK=$(mktemp -d)
pids=()
cleanup() {
  local result=$? log
  trap - EXIT
  { if [ "${#pids[@]}" -gt 0 ]; then for p in "${pids[@]}"; do kill "$p"; done; fi; wait; } 2>/dev/null || true
  if [ -n "${BENCH_ARTIFACTS:-}" ]; then
    for log in "$WORK"/*.log "$WORK"/bench-*.txt; do
      if [ -f "$log" ] && ! cp "$log" "$BENCH_ARTIFACTS/"; then
        echo "could not retain benchmark diagnostic $log" >&2
        if [ "$result" -eq 0 ]; then result=1; fi
      fi
    done
    if ! printf '%s\n' "$result" > "$BENCH_ARTIFACTS/exit-status.txt"; then
      if [ "$result" -eq 0 ]; then result=1; fi
    fi
  fi
  rm -rf "$WORK"
  exit "$result"
}
trap cleanup EXIT

# Two builds, not one: cargo unifies features across the packages of one build,
# and the viewers' HTTP/2 would then be compiled into the server being measured.
cargo build --release -q -p sapflow
cargo build --release -q -p sapflow-bench
# CARGO_BUILD_TARGET (a cross build, as on a Mac whose toolchain is x86_64)
# moves the binaries.
REL=./target/${CARGO_BUILD_TARGET:+$CARGO_BUILD_TARGET/}release
BENCH=$REL/sapflow-bench

# The server's own default thread count: a core each, up to 256, among the
# cores this process may use. A Linux affinity mask or cgroup CPU quota (a
# container's --cpus) lowers it, as it lowers the server's default. macOS
# has neither, so this is os.cpu_count() there.
effective_cpus() {
  python3 -c '
import math, os
n = len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else os.cpu_count() or 4
try:
    quota, period = open("/sys/fs/cgroup/cpu.max").read().split()
    n = min(n, max(1, math.ceil(int(quota) / int(period))))
except (OSError, ValueError):
    pass
print(min(256, n))'
}

case "$SERVER" in
  sapflow)
    BIN=$REL/sapflow
    # Pin the viewer event loop and its worker count, independently of the
    # binary's defaults. A stale SAPFLOW_HTTP_IMPL=hyper is passed on so the
    # server refuses it.
    export SAPFLOW_HTTP_IMPL=${SAPFLOW_HTTP_IMPL:-own}
    export SAPFLOW_HTTP_THREADS=${SAPFLOW_HTTP_THREADS:-$(effective_cpus)}
    echo "viewer HTTP: $SAPFLOW_HTTP_IMPL, threads $SAPFLOW_HTTP_THREADS"
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

# Linux reads /proc, so a minimal container needs no ps; elsewhere ps, as ever.
rss_mb() {
  if [ -r "/proc/$1/status" ]; then
    awk '/^VmRSS:/ { printf "%.1f", $2/1024 }' "/proc/$1/status"
  else
    ps -o rss= -p "$1" | awk '{ printf "%.1f", $1/1024 }'
  fi
}
# CPU % of one core over the next N seconds, and peak RSS meanwhile.
measure() { python3 scripts/measure-process.py "$1" "$2"; }

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
read -r cpu_one loaded_mb sample_one < <(measure "$SPID" 20)

echo "| $SERVER | binary MB | idle RSS MB | one stream RSS MB | one stream CPU % |"
echo "| $SERVER | $size_mb | $idle_mb | $loaded_mb | $cpu_one |"
for n in $VIEWERS; do
  "$BENCH" --url "$URL" --viewers "$n" --secs "$SECS" --ramp 10 \
    ${slow_args[@]+"${slow_args[@]}"} > "$WORK/bench-$n.txt" 2>&1 &
  bp=$!
  pids+=("$bp")
  sleep 10
  read -r cpu_n peak_n sample_n < <(measure "$SPID" "$SECS")
  wait "$bp"
  unset 'pids[${#pids[@]}-1]'
  echo "--- $SERVER, $n viewers: server CPU ${cpu_n}%  peak RSS ${peak_n} MB"
  echo "server sampling ${sample_n} s target ${SECS} s"
  cat "$WORK/bench-$n.txt"
done
