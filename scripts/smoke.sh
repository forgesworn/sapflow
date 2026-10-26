#!/usr/bin/env bash
# Local check of M1-M3: SRT and RTMP in, LL-HLS out, callbacks, refusals,
# recording.
#
# Needs ffmpeg and srt-live-transmit (Homebrew's ffmpeg has no SRT, so
# ffmpeg emits whole TS datagrams to srt-live-transmit's verified UDP input).
# Builds sapflow unless SAPFLOW_BIN is supplied, checks playlists, decodes the
# segments and parts, checks the publish and unpublish callbacks.
set -euo pipefail
cd "$(dirname "$0")/.."

for bin in ffmpeg ffprobe srt-live-transmit python3 curl; do
  command -v "$bin" >/dev/null || { echo "missing $bin" >&2; exit 1; }
done

# Viewers run on the own event loop. Export once so the primary, encrypted and
# opt-in server launches agree. A stale SAPFLOW_HTTP_IMPL=hyper is passed on
# so the server refuses it, rather than being overridden here.
export SAPFLOW_HTTP_IMPL=${SAPFLOW_HTTP_IMPL:-own}
export SAPFLOW_HTTP_THREADS=${SAPFLOW_HTTP_THREADS:-$(python3 -c 'import os; print(min(256, os.cpu_count() or 4))')}

HTTP=127.0.0.1:18888
SRT_PORT=18890
RTMP_PORT=11935
CB_PORT=18899
EVENT=event-smoke
WORK=$(mktemp -d)
pids=()
retire_child() {
  local finished=$1 slot
  for slot in "${!pids[@]}"; do [ "${pids[$slot]}" != "$finished" ] || unset 'pids[slot]'; done
}
wait_child() {
  local finished=$1 code=0
  wait "$finished" || code=$?
  retire_child "$finished"
  return "$code"
}
child_owned() { [ -n "$1" ] && [ "$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ')" = "$$" ]; }
cleanup() {
  code=$?; trap - EXIT; set +e
  owned=("${pids[@]:-}")
  for ((slot=${#owned[@]}-1; slot>=0; slot--)); do
    p=${owned[$slot]}; child_owned "$p" && kill -TERM "$p" 2>/dev/null
  done
  for _ in $(seq 1 50); do
    alive=0
    for p in "${owned[@]}"; do child_owned "$p" && alive=1; done
    [ "$alive" = 1 ] || break
    sleep 0.1
  done
  for p in "${owned[@]}"; do
    [ -n "$p" ] || continue
    child_owned "$p" && kill -KILL "$p" 2>/dev/null
    wait_child "$p" 2>/dev/null
  done
  [ -n "${SMOKE_KEEP:-}" ] || rm -rf "$WORK"
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fail() { echo "FAIL: $*" >&2; echo "--- sapflow log" >&2; [ ! -f "$WORK/sapflow.log" ] || tail -20 "$WORK/sapflow.log" >&2; exit 1; }
echo "smoke work: $WORK"
python3 scripts/srt-loss-udp.py free --udp 18890 18891 18892 $(seq 18900 18912)
python3 -B scripts/smoke-media.py ports --tcp 18888 18887 18886 11935 18899

if [ -z "${SAPFLOW_BIN:-}" ]; then cargo build -q; SAPFLOW_BIN=./target/debug/sapflow; fi
[ -x "$SAPFLOW_BIN" ] || fail "SAPFLOW_BIN is not executable"
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
SAPFLOW_RECONNECT_GRACE=4 SAPFLOW_HEALTH_INTERVAL=2 \
"$SAPFLOW_BIN" > "$WORK/sapflow.log" 2>&1 & pids+=($!)
sleep 1
grep 'SRT listening' "$WORK/sapflow.log" | grep -q 'implementation="own"' || fail "SRT is not served by our own receiver"
echo "ok   SRT served by our own receiver"

start_publish() { # streamid seconds UDP-input-port [SRT-port] [extra-options]
  python3 -B scripts/smoke-media.py publish --seconds "$2" --port "$3" \
    --uri "srt://127.0.0.1:${4:-$SRT_PORT}?streamid=$1&latency=2000${5:-}" &
  publisher=$!; pids+=("$publisher")
}
publish() { start_publish "$@"; wait_child "$publisher"; }

start_publish "publish:live/$EVENT:pub:secret" 25 18900 > /dev/null 2>&1
BASE="http://$HTTP/live/$EVENT"
for _ in $(seq 1 40); do
  curl -sf "$BASE/index.m3u8" -o "$WORK/index.m3u8" && break; sleep 0.25
done
[ -s "$WORK/index.m3u8" ] || fail "multivariant playlist never appeared"
python3 - "$WORK/index.m3u8" <<'PY' || fail "multivariant playlist"
import pathlib, re, sys
lines = pathlib.Path(sys.argv[1]).read_text().splitlines()
assert lines[0] == "#EXTM3U"
variants = [i for i, line in enumerate(lines) if line.startswith("#EXT-X-STREAM-INF:")]
assert len(variants) == 1
i = variants[0]
assert re.search(r'(?:[:,])CODECS="avc1\.[0-9a-f]{6},mp4a\.40\.2"(?:,|$)', lines[i])
assert lines[i + 1] == "stream.m3u8"
# Codec epochs can change dimensions without replacing the master source.
# Check a resolution attribute when present, and verify actual media below.
resolution = re.search(r'(?:[:,])RESOLUTION=([^,]+)', lines[i])
assert resolution is None or resolution.group(1) == "1280x720"
PY
echo "ok   multivariant playlist"

curl -s "http://$HTTP/live/event-nope/index.m3u8" -o /dev/null -w '%{http_code}' | grep -q 404 || fail "unknown path not 404"
grep -q '"event":"publish","path":"live/event-smoke"' "$WORK/cb.log" || fail "no publish callback"
grep -q 'token=tok' "$WORK/cb.log" || fail "callback without token"
echo "ok   publish callback"

publish "publish:live/event-other:pub:wrong" 2 18901 > "$WORK/refusal.out" 2>&1 || true
out=$(cat "$WORK/refusal.out")
echo "$out" | grep -q 401 || fail "wrong password not refused: $out"
publish "read:live/$EVENT" 2 18902 > "$WORK/refusal.out" 2>&1 || true
out=$(cat "$WORK/refusal.out")
echo "$out" | grep -q 400 || fail "read request not refused: $out"
echo "ok   refusals (401 credentials, 400 read)"

sleep 4
curl -s "$BASE/stream.m3u8" -o "$WORK/stream.m3u8"
for tag in '#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=' '#EXT-X-PART-INF:PART-TARGET=0.2' \
           '#EXT-X-TARGETDURATION:2' '#EXT-X-PRELOAD-HINT:TYPE=PART' '#EXT-X-PART:DURATION='; do
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
for media in "$WORK/segs.mp4" "$WORK/parts.mp4"; do
  python3 -B scripts/smoke-media.py cadence "$media" || fail "decoded video cadence"
  ffprobe -v error -show_streams -of json "$media" | python3 -c '
import json, sys
streams = json.load(sys.stdin)["streams"]
video = [s for s in streams if s["codec_type"] == "video"]
audio = [s for s in streams if s["codec_type"] == "audio"]
assert len(video) == len(audio) == 1
assert (video[0]["codec_name"], video[0]["width"], video[0]["height"]) == ("h264", 1280, 720)
assert (audio[0]["codec_name"], int(audio[0]["sample_rate"]), audio[0]["channels"]) == ("aac", 48000, 1)
' || fail "decoded track profile"
done
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

grep -q '"event":"health","path":"live/event-smoke","health":{"bitrateKbps":[1-9]' "$WORK/cb.log" || fail "no health callback"
grep -q '"keyframeIntervalMs":1000,"audio":true' "$WORK/cb.log" || fail "health: keyframe interval or audio"
echo "ok   health callback (bitrate, keyframe interval, audio)"

# The recording finishes when the reconnect grace (4 s here) is over.
for _ in $(seq 1 30); do grep -q recordSegmentComplete "$WORK/cb.log" && break; sleep 0.5; done
rec=$(grep -o '"segmentPath":"[^"]*"' "$WORK/cb.log" | head -1 | cut -d'"' -f4)
case "$rec" in "$WORK/rec/live/$EVENT/"[0-9]*.mp4) ;; *) fail "recordSegmentComplete path: $rec" ;; esac
grep -q '"segmentDuration":"[0-9.]*"' "$WORK/cb.log" || fail "no segmentDuration"
ffmpeg -hide_banner -v error -xerror -i "$rec" -f null - || fail "recording does not decode"
echo "ok   recording ($(basename "$rec"), recordSegmentComplete)"

start_rtmp() { # key seconds
  ffmpeg -hide_banner -loglevel error -re -f lavfi -i testsrc=size=1280x720:rate=30 \
    -f lavfi -i sine=frequency=440:sample_rate=48000 -t "$2" -pix_fmt yuv420p \
    -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -c:a aac -b:a 128k \
    -f flv "rtmp://127.0.0.1:$RTMP_PORT/live/$1" &
  publisher=$!; pids+=("$publisher")
}
rtmp() { start_rtmp "$@"; wait_child "$publisher"; }
start_rtmp "event-norec?user=pub&pass=secret" 8 > /dev/null 2>&1
for _ in $(seq 1 40); do
  curl -sf "http://$HTTP/live/event-norec/index.m3u8" -o /dev/null && break; sleep 0.25
done
curl -sf "http://$HTTP/live/event-norec/index.m3u8" -o /dev/null || fail "RTMP stream never appeared"
rtmp "event-other?user=pub&pass=wrong" 2 > /dev/null 2>&1 && fail "RTMP wrong password accepted"
# The same publisher again takes the stream over (see the SRT check below).
rtmp "event-norec?user=pub&pass=secret" 2 > /dev/null 2>&1 || fail "RTMP takeover refused"
grep -q 'publisher replaced by a new connection path=live/event-norec' "$WORK/sapflow.log" \
  || fail "second RTMP connection did not take the stream over"
for _ in $(seq 1 40); do grep -q '"unpublish","path":"live/event-norec"' "$WORK/cb.log" && break; sleep 0.5; done
grep -q '"unpublish","path":"live/event-norec"' "$WORK/cb.log" || fail "no RTMP unpublish"
[ ! -e "$WORK/rec/live/event-norec" ] || fail "skipped path was recorded"
echo "ok   RTMP publish, refusal, takeover, callbacks; skipped path not recorded"

# An encoder that drops and comes back within the grace period carries on
# the same playlist (same chunk names, after a discontinuity) and the same
# recording; v4v still hears unpublish and publish.
R=event-resume
publish "publish:live/$R:pub:secret" 8 18903 > /dev/null 2>&1
for _ in $(seq 1 20); do grep -q "\"unpublish\",\"path\":\"live/$R\"" "$WORK/cb.log" && break; sleep 0.25; done
grep -q "\"unpublish\",\"path\":\"live/$R\"" "$WORK/cb.log" || fail "no unpublish at the drop"
curl -sf "http://$HTTP/live/$R/stream.m3u8" -o "$WORK/held.m3u8" || fail "held stream not served"
init=$(grep -o '[0-9a-f]*_init.mp4' "$WORK/held.m3u8")
python3 -I scripts/check-playlist.py "http://$HTTP/live/$R/stream.m3u8" 12 > "$WORK/resume-check.log" 2>&1 & check=$!
pids+=("$check")
publish "publish:live/$R:pub:secret" 8 18904 > /dev/null 2>&1
curl -sf "http://$HTTP/live/$R/stream.m3u8" -o "$WORK/resumed.m3u8" || fail "resumed stream not served"
grep -q "$init" "$WORK/resumed.m3u8" || fail "reconnect did not keep the chunk names"
grep -q '^#EXT-X-DISCONTINUITY$' "$WORK/resumed.m3u8" || fail "no discontinuity at the reconnect"
wait_child "$check" || { cat "$WORK/resume-check.log" >&2; fail "playlist rules broken across the reconnect"; }
[ "$(grep -c "\"publish\",\"path\":\"live/$R\"" "$WORK/cb.log")" = 2 ] || fail "expected two publish callbacks"
for _ in $(seq 1 30); do grep -q "recordSegmentComplete\",\"path\":\"live/$R\"" "$WORK/cb.log" && break; sleep 0.5; done
[ "$(grep -c "recordSegmentComplete\",\"path\":\"live/$R\"" "$WORK/cb.log")" = 1 ] || fail "expected one recording across the reconnect"
rec=$(grep "live/$R" "$WORK/cb.log" | grep -o '"segmentPath":"[^"]*"' | cut -d'"' -f4)
secs=$(grep "live/$R" "$WORK/cb.log" | grep -o '"segmentDuration":"[0-9.]*"' | cut -d'"' -f4)
awk "BEGIN { exit !($secs > 12) }" || fail "recording too short across the reconnect ($secs s)"
ffmpeg -hide_banner -v error -xerror -i "$rec" -f null - || fail "resumed recording does not decode"
echo "ok   reconnect within grace: same playlist, discontinuity, one recording (${secs}s)"

# The same publisher connecting again takes its stream over, as mediamtx's
# overridePublisher does: an encoder that changed network must not wait for
# its old connection to time out.
start_publish "publish:live/event-take:pub:secret" 10 18905 > /dev/null 2>&1
for _ in $(seq 1 40); do
  curl -sf "http://$HTTP/live/event-take/index.m3u8" -o /dev/null && break; sleep 0.25
done
publish "publish:live/event-take:pub:secret" 3 18906 > /dev/null 2>&1 || true
grep -q 'path=live/event-take.*publisher replaced by a new connection\|publisher replaced by a new connection path=live/event-take' "$WORK/sapflow.log" \
  || fail "second connection did not take the stream over"
grep -q 'replaced publisher closed path=live/event-take' "$WORK/sapflow.log" \
  || fail "the replaced connection was not closed"
echo "ok   the same publisher reconnecting takes its stream over"

# SRT passphrase encryption: a second server that requires it. The right
# passphrase plays; none, or a wrong one, is refused.
ESRT=18891 EHTTP=127.0.0.1:18887 PASS_E=smoke-passphrase-1
SAPFLOW_PUBLISH_USER=pub SAPFLOW_PUBLISH_PASS=secret \
SAPFLOW_SRT_PASSPHRASE="$PASS_E" SAPFLOW_HTTP_ADDR=$EHTTP SAPFLOW_SRT_ADDR=127.0.0.1:$ESRT \
SAPFLOW_RTMP_ADDR=off "$SAPFLOW_BIN" > "$WORK/sapflow-enc.log" 2>&1 & pids+=($!)
sleep 1
grep -q 'encryption="required"' "$WORK/sapflow-enc.log" || fail "encrypted server did not start"
epublish() { publish "$1" "$2" "$4" "$ESRT" "$3"; }
start_epublish() { start_publish "$1" "$2" "$4" "$ESRT" "$3"; }
start_epublish "publish:live/event-enc:pub:secret" 10 "&passphrase=$PASS_E&pbkeylen=16" 18907 > /dev/null 2>&1
for _ in $(seq 1 60); do
  curl -sf "http://$EHTTP/live/event-enc/stream.m3u8" -o "$WORK/enc.m3u8" && grep -q _seg "$WORK/enc.m3u8" && break; sleep 0.25
done
grep -qs _seg "$WORK/enc.m3u8" || { tail -20 "$WORK/sapflow-enc.log" >&2; fail "encrypted publish never became a stream"; }
einit=$(grep -o '[0-9a-f]*_init.mp4' "$WORK/enc.m3u8")
curl -s "http://$EHTTP/live/event-enc/$einit" > "$WORK/enc.mp4"
for s in $(grep '_seg' "$WORK/enc.m3u8"); do curl -s "http://$EHTTP/live/event-enc/$s" >> "$WORK/enc.mp4"; done
ffmpeg -hide_banner -v error -xerror -i "$WORK/enc.mp4" -f null - || fail "encrypted stream does not decode"
epublish "publish:live/event-plain:pub:secret" 3 "" 18908 > /dev/null 2>&1 || true
epublish "publish:live/event-wrong:pub:secret" 3 "&passphrase=not-the-passphrase&pbkeylen=16" 18909 > /dev/null 2>&1 || true
for e in event-plain event-wrong; do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "http://$EHTTP/live/$e/index.m3u8")" = 404 ] || fail "$e was accepted"
done
grep -Eq 'wrong passphrase or key length|passphrase, key length or admission handshake refused' "$WORK/sapflow-enc.log" || fail "wrong passphrase not logged as refused"
grep -q 'not-the-passphrase' "$WORK/sapflow-enc.log" && fail "a passphrase reached the log"
echo "ok   SRT passphrase: encrypted publish plays; none or a wrong one refused"

# Optional encryption: a third server with the same passphrase that also
# admits callers without one. Both play; a wrong passphrase is still refused.
ESRT=18892 EHTTP=127.0.0.1:18886
SAPFLOW_PUBLISH_USER=pub SAPFLOW_PUBLISH_PASS=secret SAPFLOW_SRT_ENCRYPTION=optional \
SAPFLOW_SRT_PASSPHRASE="$PASS_E" SAPFLOW_HTTP_ADDR=$EHTTP SAPFLOW_SRT_ADDR=127.0.0.1:$ESRT \
SAPFLOW_RTMP_ADDR=off "$SAPFLOW_BIN" > "$WORK/sapflow-opt.log" 2>&1 & opt=$!; pids+=($opt)
sleep 1
grep -q 'encryption="optional"' "$WORK/sapflow-opt.log" || fail "optional-encryption server did not start"
start_epublish "publish:live/event-optplain:pub:secret" 8 "" 18910 > /dev/null 2>&1
start_epublish "publish:live/event-optenc:pub:secret" 8 "&passphrase=$PASS_E" 18911 > /dev/null 2>&1
for e in event-optplain event-optenc; do
  for _ in $(seq 1 60); do
    curl -sf "http://$EHTTP/live/$e/stream.m3u8" -o "$WORK/$e.m3u8" && grep -q _seg "$WORK/$e.m3u8" && break; sleep 0.25
  done
  grep -qs _seg "$WORK/$e.m3u8" || { tail -20 "$WORK/sapflow-opt.log" >&2; fail "$e never became a stream"; }
done
epublish "publish:live/event-optwrong:pub:secret" 3 "&passphrase=not-the-passphrase&pbkeylen=16" 18912 > /dev/null 2>&1 || true
[ "$(curl -s -o /dev/null -w '%{http_code}' "http://$EHTTP/live/event-optwrong/index.m3u8")" = 404 ] || fail "wrong passphrase accepted with optional encryption"
echo "ok   SRT optional encryption: with and without the passphrase play; a wrong one refused"

# SIGTERM (docker stop, systemd) is handled: a clean exit, not the signal's
# default kill (status 143), which a container's first process would not get.
kill -TERM "$opt"
for _ in $(seq 1 30); do child_owned "$opt" || break; sleep 0.1; done
child_owned "$opt" && fail "SIGTERM did not stop the server within 3 s"
rc=0; wait_child "$opt" || rc=$?
[ "$rc" = 0 ] || fail "SIGTERM was not handled (exit status $rc)"
echo "ok   SIGTERM stops the server cleanly"
echo "PASS"
