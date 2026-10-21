#!/usr/bin/env bash
# sapflow behind a real Caddy (TLS, HTTP/2): is LL-HLS intact through it, and
# how far behind does it run? Docker runs Caddy with scripts/behind-caddy.Caddyfile;
# sapflow, the publisher and the measuring clients run on the host.
#
#   scripts/behind-caddy.sh [STAGE...]    STAGE: lag blocking check player (default: all four)
#
# Each stage starts what it needs (sapflow, Caddy, an ffmpeg RTMP publisher)
# and stops it again. Settings, all optional:
#   SAPFLOW_BIN      sapflow to run (default target/release/sapflow); SAPFLOW_RUNNING=1
#                    uses one already listening on HTTP_PORT, with RTMP_PORT open to publish
#   HTTP_PORT RTMP_PORT CADDY_PORT   (8888 1935 8943; sapflow listens on loopback,
#                    or on every interface for a bridge-network Caddy on Linux)
#   KEYINT           keyframe interval in seconds (default 1; v4v advises 1)
#   SECS             length of the lag stage (default 90)  SAMPLES  blocking reloads (150)
#   CONTROL=0        skip the blocking stage's run against a second plain connection
#   PLAYER_SECS PLAYER_RUNS   each player run's length (default 70 s) and how many (6);
#                    NO_H3=1 stops the browser trying HTTP/3
#   CADDY_PROTOCOLS  h1 h2 h3 (default "h1 h2 h3": Caddy's own)
#   CADDY_SITE_EXTRA, CADDY_PROXY_EXTRA   extra Caddyfile lines to try
#   CADDY_NETWORK    bridge (default; reaches the host as host.docker.internal) or
#                    host (Linux: Caddy shares the host's network, upstream 127.0.0.1)
#   CADDY_URL        test a Caddy already running instead of starting one, e.g.
#                    https://media.example.test/live-hls (no stream path); then
#                    CADDY_CA=<PEM> if its certificate is not publicly trusted,
#                    and sapflow must be the one it proxies to
#   PLAYWRIGHT_DIR, HLS_JS   for the player stage: an existing playwright
#                    package directory and an hls.js dist/hls.min.js (nothing is downloaded);
#                    CHROME_CHANNEL (default chrome) names the installed browser
#   WORK             where logs and the Caddy CA go (default: a new temp directory)
#
# Needs docker (unless CADDY_URL), ffmpeg, curl, python3, node 20+.
set -euo pipefail
cd "$(dirname "$0")/.."

STAGES=("$@")
[ ${#STAGES[@]} -gt 0 ] || STAGES=(lag blocking check player)
SAPFLOW_BIN=${SAPFLOW_BIN:-target/release/sapflow}
HTTP_PORT=${HTTP_PORT:-8888}
RTMP_PORT=${RTMP_PORT:-1935}
CADDY_PORT=${CADDY_PORT:-8943}
KEYINT=${KEYINT:-1}
SECS=${SECS:-90}
SAMPLES=${SAMPLES:-150}
PLAYER_SECS=${PLAYER_SECS:-70}
PLAYER_RUNS=${PLAYER_RUNS:-6}
CADDY_PROTOCOLS=${CADDY_PROTOCOLS:-h1 h2 h3}
CADDY_NETWORK=${CADDY_NETWORK:-bridge}
CADDY_IMAGE=${CADDY_IMAGE:-caddy:2}
CHROME_CHANNEL=${CHROME_CHANNEL:-chrome}
CONTAINER=sapflow-behind-caddy
WORK=${WORK:-$(mktemp -d)}
mkdir -p "$WORK"
pids=()
streams=0
started_caddy=0

for bin in node ffmpeg curl python3; do
  command -v "$bin" >/dev/null || { echo "missing $bin" >&2; exit 1; }
done
[ -n "${CADDY_URL:-}" ] || command -v docker >/dev/null || { echo "missing docker (or set CADDY_URL)" >&2; exit 1; }

stop_publisher() {
  [ -z "${PUB_PID:-}" ] || { kill "$PUB_PID" 2>/dev/null || true; wait "$PUB_PID" 2>/dev/null || true; PUB_PID=; }
}
cleanup() {
  stop_publisher
  for p in "${pids[@]:-}"; do [ -z "$p" ] || { kill "$p" 2>/dev/null || true; wait "$p" 2>/dev/null || true; }; done
  if [ "$started_caddy" = 1 ]; then docker logs "$CONTAINER" > "$WORK/caddy.log" 2>&1 || true; docker rm -f "$CONTAINER" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

start_sapflow() {
  if [ "${SAPFLOW_RUNNING:-}" = 1 ]; then return; fi
  [ -x "$SAPFLOW_BIN" ] || fail "no sapflow at $SAPFLOW_BIN (set SAPFLOW_BIN)"
  # Caddy in a bridge network reaches the host through its gateway address,
  # which is not 127.0.0.1: Linux needs the wider bind, Docker Desktop does not.
  local bind=127.0.0.1
  [ "$CADDY_NETWORK" = bridge ] && [ "$(uname)" = Linux ] && bind=0.0.0.0
  SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=$bind:$HTTP_PORT SAPFLOW_RTMP_ADDR=127.0.0.1:$RTMP_PORT \
    SAPFLOW_SRT_ADDR=127.0.0.1:$((HTTP_PORT + 2)) "$SAPFLOW_BIN" > "$WORK/sapflow.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 40); do curl -s -o /dev/null "http://127.0.0.1:$HTTP_PORT/live/none/index.m3u8" && return; sleep 0.25; done
  fail "sapflow did not start (see $WORK/sapflow.log)"
}

start_caddy() {
  if [ -n "${CADDY_URL:-}" ]; then
    FRONT_BASE=${CADDY_URL%/}
    CA=${CADDY_CA:-}
    return
  fi
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  local net=(-p "127.0.0.1:$CADDY_PORT:$CADDY_PORT" -p "127.0.0.1:$CADDY_PORT:$CADDY_PORT/udp" --add-host host.docker.internal:host-gateway)
  local upstream=host.docker.internal:$HTTP_PORT
  if [ "$CADDY_NETWORK" = host ]; then net=(--network host); upstream=127.0.0.1:$HTTP_PORT; fi
  docker run -d --name "$CONTAINER" "${net[@]}" \
    -e CADDY_HTTPS_PORT="$CADDY_PORT" -e CADDY_SITE=localhost -e CADDY_PROTOCOLS="$CADDY_PROTOCOLS" \
    -e SAPFLOW_UPSTREAM="$upstream" -e CADDY_SITE_EXTRA="${CADDY_SITE_EXTRA:-}" -e CADDY_PROXY_EXTRA="${CADDY_PROXY_EXTRA:-}" \
    -v "$PWD/scripts/behind-caddy.Caddyfile:/etc/caddy/Caddyfile:ro" "$CADDY_IMAGE" >/dev/null
  started_caddy=1
  CA=$WORK/caddy-root.crt
  for _ in $(seq 40); do
    docker cp "$CONTAINER:/data/caddy/pki/authorities/local/root.crt" "$CA" 2>/dev/null && break
    sleep 0.5
  done
  [ -s "$CA" ] || fail "Caddy did not start (docker logs $CONTAINER)"
  FRONT_BASE=https://localhost:$CADDY_PORT/live-hls
  docker exec "$CONTAINER" caddy version | sed 's/^/Caddy /'
}

# Every stage gets a stream of its own: a publisher reconnecting to a held
# stream resumes it after a discontinuity, which would colour what follows.
start_publisher() {
  local gop=$((KEYINT * 30))
  streams=$((streams + 1))
  EVENT=event-caddy-$streams
  DIRECT=http://127.0.0.1:$HTTP_PORT/live/$EVENT
  FRONT=$FRONT_BASE/live/$EVENT
  T0=$(node -p 'Date.now()')
  ffmpeg -hide_banner -loglevel error -re \
    -f lavfi -i testsrc=size=1280x720:rate=30 -f lavfi -i sine \
    -c:v libx264 -preset veryfast -tune zerolatency -g $gop -keyint_min $gop -sc_threshold 0 \
    -b:v 2500k -maxrate 2500k -bufsize 2500k -c:a aac -b:a 128k -t "${PUB_SECONDS:-3600}" \
    -f flv "rtmp://127.0.0.1:$RTMP_PORT/live/$EVENT" > "$WORK/ffmpeg.log" 2>&1 &
  PUB_PID=$!
  for _ in $(seq 80); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' "$DIRECT/index.m3u8")" = 200 ] && return
    sleep 0.25
  done
  fail "no stream after 20 s (see $WORK/ffmpeg.log)"
}

ca_args() { [ -z "${CA:-}" ] || echo --ca "$CA"; }

start_sapflow
start_caddy
echo "work directory $WORK; keyframes every $KEYINT s${CADDY_URL:+; front: $CADDY_URL}${CADDY_URL:-; Caddy protocols: $CADDY_PROTOCOLS}"
for stage in "${STAGES[@]}"; do
  echo
  echo "== $stage"
  case "$stage" in
    lag)
      start_publisher
      # shellcheck disable=SC2046
      node scripts/behind-caddy.mjs lag --direct "$DIRECT" --caddy "$FRONT" $(ca_args) --seconds "$SECS" --t0 "$T0"
      stop_publisher ;;
    blocking)
      start_publisher
      sleep 10
      if [ "${CONTROL:-1}" != 0 ]; then
        echo "-- control: two plain connections"
        node scripts/behind-caddy.mjs blocking --direct "$DIRECT" --control --samples "$SAMPLES"
      fi
      echo "-- through the front"
      # shellcheck disable=SC2046
      node scripts/behind-caddy.mjs blocking --direct "$DIRECT" --caddy "$FRONT" $(ca_args) --samples "$SAMPLES"
      stop_publisher ;;
    check)
      start_publisher
      sleep 10
      echo "-- curl --http2 -v"
      curl -sv --http2 ${CA:+--cacert "$CA"} -o /dev/null -w 'http_version %{http_version}\n' "$FRONT/index.m3u8" 2>&1 \
        | grep -E 'ALPN|using HTTP|^< (HTTP|content-type|cache-control|access-control|alt-svc|via|server|content-encoding)|http_version'
      echo "-- scripts/check-playlist.py through the front (30 s)"
      env ${CA:+SSL_CERT_FILE="$CA"} python3 -I scripts/check-playlist.py "$FRONT/stream.m3u8" 30 || true
      echo "-- scripts/check-playlist.py direct (30 s)"
      python3 -I scripts/check-playlist.py "$DIRECT/stream.m3u8" 30 || true
      echo "-- protocol checks"
      # shellcheck disable=SC2046
      node scripts/behind-caddy.mjs check --direct "$DIRECT" --caddy "$FRONT" $(ca_args) --pause-pid "$PUB_PID"
      stop_publisher ;;
    player)
      [ -n "${PLAYWRIGHT_DIR:-}" ] && [ -n "${HLS_JS:-}" ] || { echo "skipped: set PLAYWRIGHT_DIR and HLS_JS"; continue; }
      start_publisher
      sleep 10
      echo "-- hls.js lowLatencyMode, direct and through the front playing together, $PLAYER_RUNS runs of $PLAYER_SECS s"
      PLAYWRIGHT_DIR=$PLAYWRIGHT_DIR HLS_JS=$HLS_JS CHROME_CHANNEL=$CHROME_CHANNEL \
        node scripts/behind-caddy-player.mjs --src "direct=$DIRECT/index.m3u8" --src "front=$FRONT/index.m3u8" \
        --seconds "$PLAYER_SECS" --runs "$PLAYER_RUNS" ${NO_H3:+--no-h3}
      stop_publisher ;;
    *) fail "unknown stage $stage" ;;
  esac
done
