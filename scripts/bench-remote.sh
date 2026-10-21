#!/usr/bin/env bash
# The M4 comparison across two machines: the server runs on HOST, the encoder
# and the simulated viewers run here, over the real network.
#
#   HOST=<ssh host> scripts/bench-remote.sh sapflow|mediamtx
#
# HOST needs `sapflow` and `mediamtx` in ~/sapflow-bench. VIEWERS (default
# 250) simulated viewers follow the LL-HLS playlist with blocking reloads;
# only the first FULL (default 10) also fetch parts and segments, so a slow
# link carries the test (each full viewer pulls about twice the stream
# bitrate). Prints server CPU and memory on HOST and the viewers' results.
#
# Through a TLS front on HOST, as production runs (Caddy, HTTP/2): BENCH_URL
# is the front's URL for the stream and BENCH_ARGS goes to the viewers, e.g.
#   BENCH_URL=https://front.example/live-hls/live/event-bench/index.m3u8 \
#   BENCH_ARGS='--http2 --ca caddy-root.crt' HOST=... scripts/bench-remote.sh sapflow
# The CPU and memory reported are sapflow's alone, not the front's.
set -euo pipefail
cd "$(dirname "$0")/.."

SERVER=${1:?usage: HOST=... scripts/bench-remote.sh sapflow|mediamtx}
HOST=${HOST:?set HOST to the ssh host running the server}
VIEWERS=${VIEWERS:-250}
FULL=${FULL:-10}
SECS=${SECS:-60}
EVENT=event-bench
REMOTE_IP=$(ssh -o BatchMode=yes "$HOST" 'ipconfig getifaddr en0 || ipconfig getifaddr en1 || hostname -I | cut -d" " -f1')
pids=()
cleanup() {
  { for p in "${pids[@]}"; do kill "$p"; done; wait; } 2>/dev/null || true
  ssh -o BatchMode=yes "$HOST" 'pkill -f "\./(sapflow|mediamtx)"' 2>/dev/null || true
}
# A server left over from an earlier run would answer in place of this one
# (sapflow binds IPv4, mediamtx IPv6, so both can hold the port).
ssh -o BatchMode=yes "$HOST" 'pkill -f "\./(sapflow|mediamtx)"; sleep 1' 2>/dev/null || true
trap cleanup EXIT

cargo build --release -q -p sapflow-bench
BENCH=./target/release/sapflow-bench

case "$SERVER" in
  sapflow)
    ssh -o BatchMode=yes "$HOST" 'cd ~/sapflow-bench && SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=0.0.0.0:8888 \
      SAPFLOW_RTMP_ADDR=0.0.0.0:1935 SAPFLOW_SRT_ADDR=127.0.0.1:8890 RUST_LOG=warn \
      nohup ./sapflow > server.log 2>&1 < /dev/null &'
    ;;
  mediamtx)
    ssh -o BatchMode=yes "$HOST" 'cd ~/sapflow-bench && cat > mediamtx.yml <<EOF
logLevel: warn
rtmp: yes
rtmpAddress: :1935
hls: yes
hlsAddress: :8888
hlsAlwaysRemux: yes
hlsVariant: lowLatency
hlsSegmentDuration: 1s
hlsPartDuration: 200ms
hlsSegmentCount: 24
hlsAllowOrigins: ["*"]
rtsp: no
webrtc: no
srt: no
api: no
authInternalUsers:
  - user: any
    permissions:
      - action: publish
      - action: read
paths:
  all_others:
EOF
nohup ./mediamtx mediamtx.yml > server.log 2>&1 < /dev/null &'
    ;;
  *) echo "unknown server $SERVER" >&2; exit 2 ;;
esac
sleep 1
SPID=$(ssh -o BatchMode=yes "$HOST" "pgrep -f '\./$SERVER' | head -1")
[ -n "$SPID" ] || { ssh "$HOST" 'tail ~/sapflow-bench/server.log'; exit 1; }
# The process measured must be the one holding the HLS port.
LISTENERS=$(ssh -o BatchMode=yes "$HOST" "lsof -nP -t -iTCP:8888 -sTCP:LISTEN | sort -u | tr '\n' ' '")
[ "$(echo $LISTENERS)" = "$SPID" ] || { echo "port 8888 on $HOST is held by '$LISTENERS', not $SERVER ($SPID)" >&2; exit 1; }

remote_cpu() { ssh -o BatchMode=yes "$HOST" "ps -o time= -p $SPID" | awk '{ n=split($1,a,/[-:]/); s=0; for(i=1;i<=n;i++) s=s*60+a[i]; print s }'; }
remote_rss() { ssh -o BatchMode=yes "$HOST" "ps -o rss= -p $SPID" | awk '{ printf "%.1f", $1/1024 }'; }
now() { python3 -c 'import time; print(time.time())'; }
measure() { # secs -> "cpu% peakMB"
  local c0 t0 c1 t1 peak=0 r
  c0=$(remote_cpu); t0=$(now)
  for _ in $(seq 1 $(( $1 / 5 ))); do r=$(remote_rss); peak=$(awk -v a="$peak" -v b="$r" 'BEGIN{print (b>a)?b:a}'); sleep 5; done
  c1=$(remote_cpu); t1=$(now)
  awk -v c0="$c0" -v c1="$c1" -v t0="$t0" -v t1="$t1" -v p="$peak" 'BEGIN { printf "%.1f %.1f\n", 100*(c1-c0)/(t1-t0), p }'
}

idle_mb=$(remote_rss)
ffmpeg -hide_banner -loglevel error -re -stream_loop -1 -f lavfi -i testsrc2=size=1280x720:rate=30 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -maxrate 2500k -bufsize 2500k \
  -c:a aac -b:a 128k -f flv "rtmp://$REMOTE_IP:1935/live/$EVENT" > /dev/null 2>&1 &
pids+=($!)
URL="http://$REMOTE_IP:8888/live/$EVENT/index.m3u8"
for _ in $(seq 1 60); do curl -sf -o /dev/null "$URL" && break; sleep 0.5; done
curl -sf -o /dev/null "$URL" || { echo "stream never became ready on $HOST" >&2; ssh "$HOST" 'tail ~/sapflow-bench/server.log' >&2; exit 1; }
sleep 25
read -r cpu_one one_mb < <(measure 20)
echo "$SERVER on $HOST: idle RSS ${idle_mb} MB; one stream ${one_mb} MB, CPU ${cpu_one}%"

# shellcheck disable=SC2086  # BENCH_ARGS is a list of options
"$BENCH" --url "${BENCH_URL:-$URL}" --viewers "$VIEWERS" --full "$FULL" --secs "$SECS" --ramp 10 ${BENCH_ARGS:-} > /tmp/bench-remote-$SERVER.txt &
bp=$!
sleep 10
read -r cpu_n peak_n < <(measure "$SECS")
wait "$bp" || true
echo "$SERVER, $VIEWERS viewers ($FULL full) across the network: server CPU ${cpu_n}%, peak RSS ${peak_n} MB"
cat /tmp/bench-remote-$SERVER.txt
