#!/usr/bin/env bash
# Local check of M1-M3: SRT and RTMP in, LL-HLS out, callbacks, refusals,
# recording.
#
# Needs ffmpeg and srt-live-transmit (Homebrew's ffmpeg has no SRT, so
# ffmpeg encodes to stdout and srt-live-transmit, which is libsrt, sends it).
# Builds sapflow, publishes a test signal, checks the playlists, decodes the
# segments and parts, checks the publish and unpublish callbacks.
set -euo pipefail
cd "$(dirname "$0")/.."

for bin in ffmpeg ffprobe srt-live-transmit python3 curl; do
  command -v "$bin" >/dev/null || { echo "missing $bin" >&2; exit 1; }
done

HTTP=127.0.0.1:18888
SRT_PORT=18890
RTMP_PORT=11935
CB_PORT=18899
EVENT=event-smoke
WORK=$(mktemp -d)
pids=()
cleanup() { { for p in "${pids[@]}"; do kill "$p"; done; wait; } 2>/dev/null || true; rm -rf "$WORK"; }
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; echo "--- sapflow log" >&2; tail -20 "$WORK/sapflow.log" >&2; exit 1; }

cargo build -q
cat > "$WORK/cb.py" <<'EOF'
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('content-length', 0)))
        print(self.path, body.decode(), flush=True)
        self.send_response(200); self.end_headers()
    def log_message(self, *a): pass
http.server.HTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
EOF
python3 -I "$WORK/cb.py" "$CB_PORT" > "$WORK/cb.log" 2>&1 & pids+=($!)

SAPFLOW_PUBLISH_USER=pub SAPFLOW_PUBLISH_PASS=secret \
SAPFLOW_CALLBACK_URL="http://127.0.0.1:$CB_PORT/api/public/live-events/{eventId}/stream-health" \
SAPFLOW_CALLBACK_SECRET=tok SAPFLOW_HTTP_ADDR=$HTTP SAPFLOW_SRT_ADDR=127.0.0.1:$SRT_PORT \
SAPFLOW_RTMP_ADDR=127.0.0.1:$RTMP_PORT SAPFLOW_RECORDINGS="$WORK/rec" SAPFLOW_RECORD_SKIP='live/event-norec' \
  ./target/debug/sapflow > "$WORK/sapflow.log" 2>&1 & pids+=($!)
sleep 1

publish() { # streamid seconds
  ffmpeg -hide_banner -loglevel error -re -f lavfi -i testsrc=size=1280x720:rate=30 \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t "$2" -pix_fmt yuv420p \
    -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -c:a aac -b:a 128k \
    -f mpegts - | srt-live-transmit -q -a:no file://con "srt://127.0.0.1:$SRT_PORT?streamid=$1&latency=2000"
}

publish "publish:live/$EVENT:pub:secret" 25 > /dev/null 2>&1 & pids+=($!)
BASE="http://$HTTP/live/$EVENT"
for _ in $(seq 1 40); do
  curl -sf "$BASE/index.m3u8" -o "$WORK/index.m3u8" && break; sleep 0.25
done
[ -s "$WORK/index.m3u8" ] || fail "multivariant playlist never appeared"
grep -q 'CODECS="avc1\.[0-9a-f]*,mp4a\.40\.2",RESOLUTION=1280x720' "$WORK/index.m3u8" || fail "multivariant playlist"
echo "ok   multivariant playlist"

curl -s "http://$HTTP/live/event-nope/index.m3u8" -o /dev/null -w '%{http_code}' | grep -q 404 || fail "unknown path not 404"
grep -q '"event":"publish","path":"live/event-smoke"' "$WORK/cb.log" || fail "no publish callback"
grep -q 'token=tok' "$WORK/cb.log" || fail "callback without token"
echo "ok   publish callback"

out=$(publish "publish:live/$EVENT:pub:secret" 2 2>&1 || true)
echo "$out" | grep -q 409 || fail "second publisher not refused: $out"
out=$(publish "publish:live/event-other:pub:wrong" 2 2>&1 || true)
echo "$out" | grep -q 401 || fail "wrong password not refused: $out"
out=$(publish "read:live/$EVENT" 2 2>&1 || true)
echo "$out" | grep -q 400 || fail "read request not refused: $out"
echo "ok   refusals (409 live path, 401 credentials, 400 read)"

sleep 4
curl -s "$BASE/stream.m3u8" -o "$WORK/stream.m3u8"
for tag in '#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=' '#EXT-X-PART-INF:PART-TARGET=0.2' \
           '#EXT-X-TARGETDURATION:1' '#EXT-X-PRELOAD-HINT:TYPE=PART' '#EXT-X-PART:DURATION='; do
  grep -q "$tag" "$WORK/stream.m3u8" || fail "media playlist lacks $tag"
done
[ "$(grep -c '^#EXT-X-GAP' "$WORK/stream.m3u8")" = 7 ] || fail "expected 7 leading gaps"
echo "ok   media playlist (LL-HLS tags, 7 gaps)"

init=$(grep -o '[0-9a-f]*_init.mp4' "$WORK/stream.m3u8")
curl -s "$BASE/$init" > "$WORK/segs.mp4"; cp "$WORK/segs.mp4" "$WORK/parts.mp4"
for s in $(grep '_seg' "$WORK/stream.m3u8"); do curl -s "$BASE/$s" >> "$WORK/segs.mp4"; done
for p in $(grep -o '[0-9a-f]*_part[0-9]*\.mp4' "$WORK/stream.m3u8"); do curl -s "$BASE/$p" >> "$WORK/parts.mp4"; done
ffmpeg -hide_banner -v error -xerror -i "$WORK/segs.mp4" -f null - || fail "segments do not decode"
ffmpeg -hide_banner -v error -xerror -i "$WORK/parts.mp4" -f null - || fail "parts do not decode"
echo "ok   segments and parts decode"

curl -s "$BASE/stream.m3u8" -o "$WORK/now.m3u8"
msn=$(( $(grep MEDIA-SEQUENCE "$WORK/now.m3u8" | cut -d: -f2) + $(grep -c EXTINF "$WORK/now.m3u8") + 1 ))
t=$(curl -s -o /dev/null -w '%{time_total}' "$BASE/stream.m3u8?_HLS_msn=$msn")
awk "BEGIN { exit !($t > 0.05) }" || fail "blocking reload returned at once ($t s)"
curl -s -o /dev/null -w '%{http_code}' "$BASE/stream.m3u8?_HLS_msn=$((msn + 5))" | grep -q 400 || fail "far msn not 400"
echo "ok   blocking reload (waited ${t}s; far ahead is 400)"

for _ in $(seq 1 60); do grep -q '"event":"unpublish"' "$WORK/cb.log" && break; sleep 0.5; done
grep -q '"event":"unpublish","path":"live/event-smoke"' "$WORK/cb.log" || fail "no unpublish callback"
echo "ok   unpublish callback"

for _ in $(seq 1 20); do grep -q recordSegmentComplete "$WORK/cb.log" && break; sleep 0.5; done
rec=$(grep -o '"segmentPath":"[^"]*"' "$WORK/cb.log" | head -1 | cut -d'"' -f4)
case "$rec" in "$WORK/rec/live/$EVENT/"[0-9]*.mp4) ;; *) fail "recordSegmentComplete path: $rec" ;; esac
grep -q '"segmentDuration":"[0-9.]*"' "$WORK/cb.log" || fail "no segmentDuration"
ffmpeg -hide_banner -v error -xerror -i "$rec" -f null - || fail "recording does not decode"
echo "ok   recording ($(basename "$rec"), recordSegmentComplete)"

rtmp() { # key seconds
  ffmpeg -hide_banner -loglevel error -re -f lavfi -i testsrc=size=1280x720:rate=30 \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t "$2" -pix_fmt yuv420p \
    -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -c:a aac -b:a 128k \
    -f flv "rtmp://127.0.0.1:$RTMP_PORT/live/$1"
}
rtmp "event-norec?user=pub&pass=secret" 8 > /dev/null 2>&1 & pids+=($!)
for _ in $(seq 1 40); do
  curl -sf "http://$HTTP/live/event-norec/index.m3u8" -o /dev/null && break; sleep 0.25
done
curl -sf "http://$HTTP/live/event-norec/index.m3u8" -o /dev/null || fail "RTMP stream never appeared"
rtmp "event-other?user=pub&pass=wrong" 2 > /dev/null 2>&1 && fail "RTMP wrong password accepted"
rtmp "event-norec?user=pub&pass=secret" 2 > /dev/null 2>&1 && fail "second RTMP publisher accepted"
for _ in $(seq 1 40); do grep -q '"unpublish","path":"live/event-norec"' "$WORK/cb.log" && break; sleep 0.5; done
grep -q '"unpublish","path":"live/event-norec"' "$WORK/cb.log" || fail "no RTMP unpublish"
[ ! -e "$WORK/rec/live/event-norec" ] || fail "skipped path was recorded"
echo "ok   RTMP publish, refusals, callbacks; skipped path not recorded"
echo "PASS"
