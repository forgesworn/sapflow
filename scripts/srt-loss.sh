#!/usr/bin/env bash
# SRT under packet loss: sapflow's receiver against libsrt's, same sender, same
# loss, several seeds.
#
#   scripts/srt-loss.sh                          # the full matrix, 5 seeds
#   SEEDS=3 LOSSES=random:10 LATENCIES=200 scripts/srt-loss.sh
#   RECEIVERS=sapflow SAPFLOW_BIN=/path/to/sapflow scripts/srt-loss.sh
#   RECEIVERS="old=/path/sapflow-old new=/path/sapflow-new libsrt" scripts/srt-loss.sh
#   RECEIVERS="srt-rs own libsrt" scripts/srt-loss.sh   # both SRT implementations
#   scripts/srt-loss.sh --summarise scratch/srt-loss/<run>/results.tsv
#
# A 30 s test clip (ffmpeg lavfi: 720p30 and a tone, 2.5 Mbit/s H.264 with 1 s
# keyframes, AAC) is played in real time by `srt-live-transmit` (libsrt, the
# sender in every trial) through `srt-loss-proxy`, which drops and delays UDP
# packets in both directions and counts retransmissions from the SRT headers.
# The receiver is either sapflow (its recording of the show is counted) or
# `srt-live-transmit` as a listener (its output file is counted). Handshakes are
# never dropped. Round trip 60 ms. Each trial uses its own ports.
#
# Reported per receiver and condition over the seeds, as mean and sample
# standard deviation:
#   - retransmission overhead: retransmitted / original data packets
#   - not recovered in time: original data packets for which no copy reached the
#     receiver within the SRT latency of the first send, counted by the proxy
#     (the same definition for both receivers, whatever they do afterwards)
#   - video and audio packets delivered, against the same receiver's clean run
#   - decode messages: lines `ffmpeg -v error` prints decoding the output
# plus sapflow minus libsrt with a Welch t statistic. With 5 seeds |t| below
# about 2.3 is not a difference worth claiming.
#
# Needs ffmpeg, ffprobe and srt-live-transmit on PATH. Nothing is written
# outside OUT (default scratch/srt-loss/<time>, which is gitignored) and a
# temporary directory that is removed on exit (KEEP=1 keeps it).
#
# Environment:
#   SEEDS=5             seeds per condition
#   LOSSES="random:3 random:10 random:20 burst:8,8"
#                       srt-loss-proxy --loss values; burst:P,L is bursty loss
#                       (Gilbert-Elliott), P percent on average, L packets a burst
#   LATENCIES="2000 200"   SRT latency in ms, set on sender and receiver
#   RECEIVERS="sapflow libsrt"   `NAME=binary` runs another sapflow build as NAME;
#                       a NAME starting with `own` runs sapflow with
#                       SAPFLOW_SRT_IMPL=own (our SRT), any other with srt-rs
#   CLIP_SECS=30  DELAY_MS=30 (one way)  JOBS=3 (trials at once)
#   SAPFLOW_BIN, PROXY_BIN   default: release builds of this checkout
set -euo pipefail

SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
ROOT=$(cd "$(dirname "$0")/.." && pwd)

summarise() { # results.tsv
  awk -F'\t' '
    function mean(s, c) { return c ? s / c : 0 }
    function sd(s, ss, c) { return c > 1 ? sqrt((ss - s * s / c) / (c - 1)) : 0 }
    NR == 1 { next }
    {
      r = $1; loss = $2; lat = $3; v = $5; a = $6; dec = $7; orig = $8; retx = $9
      if ($13 + 0 > 0) reordered++
      if ($12 != "ok") { failed[r SUBSEP loss SUBSEP lat]++; next }
      if (loss == "none") { bv[r, lat] = v; ba[r, lat] = a; bd[r, lat] = dec; bo[r, lat] = orig > 0 ? 100 * retx / orig : 0; next }
      k = r SUBSEP loss SUBSEP lat
      if (!(k in seen)) { seen[k] = 1; keys[++nk] = k }
      if (!(r in rseen)) { rseen[r] = 1; recs[++nr] = r }
      n[k]++
      o = orig > 0 ? 100 * retx / orig : 0
      so[k] += o; sso[k] += o * o
      rl = $14 > 0 ? 100 * (1 - $15 / $14) : 0
      sr[k] += rl; ssr[k] += rl * rl
      sv[k] += v; ssv[k] += v * v; sa[k] += a; ssa[k] += a * a
      sd_[k] += dec; ssd[k] += dec * dec
    }
    END {
      if (reordered) printf "WARNING: the proxy reordered packets in %d trials; those numbers are suspect\n\n", reordered
      print "| Loss | Latency | Receiver | n | Retransmission overhead | Not recovered in time | Video delivered | Audio delivered | Decode messages |"
      print "|------|--------:|----------|--:|------------------------:|----------------------:|----------------:|----------------:|----------------:|"
      for (i = 1; i <= nk; i++) {
        k = keys[i]; split(k, p, SUBSEP); r = p[1]; loss = p[2]; lat = p[3]
        bvv = bv[r, lat]; baa = ba[r, lat]
        m_o[k] = mean(so[k], n[k]); s_o[k] = sd(so[k], sso[k], n[k])
        m_r[k] = mean(sr[k], n[k]); s_r[k] = sd(sr[k], ssr[k], n[k])
        m_v[k] = bvv ? 100 * mean(sv[k], n[k]) / bvv : 0
        s_v[k] = bvv ? 100 * sd(sv[k], ssv[k], n[k]) / bvv : 0
        m_a[k] = baa ? 100 * mean(sa[k], n[k]) / baa : 0
        s_a[k] = baa ? 100 * sd(sa[k], ssa[k], n[k]) / baa : 0
        m_d[k] = mean(sd_[k], n[k]); s_d[k] = sd(sd_[k], ssd[k], n[k])
        f = failed[k] ? sprintf(" (%d failed)", failed[k]) : ""
        printf "| %s | %s ms | %s | %d%s | %.1f%% ± %.1f | %.2f%% ± %.2f | %.2f%% ± %.2f | %.2f%% ± %.2f | %.1f ± %.1f |\n", \
          loss, lat, r, n[k], f, m_o[k], s_o[k], m_r[k], s_r[k], m_v[k], s_v[k], m_a[k], s_a[k], m_d[k], s_d[k]
        nn[k] = n[k]
        if (!(loss SUBSEP lat in cond)) { cond[loss SUBSEP lat] = 1; conds[++nc] = loss SUBSEP lat }
      }
      print ""
      print "Each sapflow build minus libsrt (Welch t over the seeds; |t| below about 2.3 is noise):"
      print ""
      print "| Loss | Latency | Receiver | Overhead, points | t | Not recovered, points | t | Video delivered, points | t | Decode messages | t |"
      print "|------|--------:|----------|-----------------:|--:|----------------------:|--:|------------------------:|--:|----------------:|--:|"
      for (i = 1; i <= nc; i++) {
        split(conds[i], c, SUBSEP); kl = "libsrt" SUBSEP c[1] SUBSEP c[2]
        if (!(kl in n)) continue
        for (j = 1; j <= nr; j++) {
          if (recs[j] ~ /^libsrt/) continue
          ks = recs[j] SUBSEP c[1] SUBSEP c[2]
          if (!(ks in n)) continue
          printf "| %s | %s ms | %s | %+.1f | %s | %+.2f | %s | %+.2f | %s | %+.1f | %s |\n", c[1], c[2], recs[j], \
            m_o[ks] - m_o[kl], welch(m_o[ks], s_o[ks], n[ks], m_o[kl], s_o[kl], n[kl]), \
            m_r[ks] - m_r[kl], welch(m_r[ks], s_r[ks], n[ks], m_r[kl], s_r[kl], n[kl]), \
            m_v[ks] - m_v[kl], welch(m_v[ks], s_v[ks], n[ks], m_v[kl], s_v[kl], n[kl]), \
            m_d[ks] - m_d[kl], welch(m_d[ks], s_d[ks], n[ks], m_d[kl], s_d[kl], n[kl])
        }
      }
      print ""
      print "clean runs (the baseline each receiver is compared with):"
      for (x in bv) { split(x, q, SUBSEP); printf "  %s at %s ms: %d video, %d audio packets, %d decode messages, retransmission overhead %.1f%%\n", q[1], q[2], bv[x], ba[x], bd[x], bo[x] }
    }
    function welch(m1, s1, n1, m2, s2, n2,   se) {
      se = sqrt(s1 * s1 / n1 + s2 * s2 / n2)
      return se > 0 ? sprintf("%.1f", (m1 - m2) / se) : "-"
    }
  ' "$1"
}

if [ "${1:-}" = "--summarise" ]; then summarise "${2:?usage: --summarise results.tsv}"; exit 0; fi

# One trial: --trial INDEX RECEIVER LOSS LATENCY SEED. Appends one line to
# $OUT/trials. Run by xargs, below.
if [ "${1:-}" = "--trial" ]; then
  shift; idx=$1; spec=$2; loss=$3; lat=$4; seed=$5
  # A receiver is `libsrt` or `NAME[=sapflow binary]`; any name starting with
  # libsrt means libsrt's srt-live-transmit.
  recv=${spec%%=*}; bin=${spec#*=}; [ "$bin" != "$spec" ] || bin=$SAPFLOW_BIN
  if [ "${recv#libsrt}" != "$recv" ]; then kind=libsrt; else kind=sapflow; fi
  d=$WORK/t$idx; mkdir -p "$d"
  base=$((PORT_BASE + (idx % 50) * 4)); sp=$base; pp=$((base + 1)); lp=$((base + 2)); hp=$((base + 3))
  pids=()
  cleanup() { for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; }
  trap cleanup EXIT

  if [ "$kind" = sapflow ]; then
    impl=srt-rs; [ "${recv#own}" = "$recv" ] || impl=own
    SAPFLOW_SRT_IMPL=$impl SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=127.0.0.1:$hp SAPFLOW_SRT_ADDR=127.0.0.1:$sp \
      SAPFLOW_RTMP_ADDR=off SAPFLOW_RECORDINGS="$d/rec" RUST_LOG=warn \
      "$bin" > "$d/rcv.log" 2>&1 &
    rp=$!; target=$sp; sid="&streamid=publish:live/event-loss"
  else
    srt-live-transmit "srt://127.0.0.1:$lp?mode=listener&latency=$lat" file://con \
      > "$d/out.ts" 2> "$d/rcv.log" &
    rp=$!; target=$lp; sid=""
  fi
  pids+=("$rp"); sleep 1
  "$PROXY_BIN" --listen 127.0.0.1:$pp --target 127.0.0.1:$target --loss "$loss" \
    --delay-ms "$DELAY_MS" --latency-ms "$lat" --seed "$seed" --stats "$d/proxy.txt" 2> "$d/proxy.log" &
  prp=$!; pids+=("$prp"); sleep 0.5

  # srt-live-transmit does not exit when its input ends, so find ffmpeg in the
  # pipeline, wait for it, let the receiver drain, then stop the sender.
  ( ffmpeg -hide_banner -loglevel error -re -i "$WORK/src.ts" -c copy -f mpegts - 2> "$d/ff.log" |
    srt-live-transmit -a:no file://con "srt://127.0.0.1:$pp?latency=$lat$sid" > "$d/snd.log" 2>&1 ) &
  sp_=$!; pids+=("$sp_"); sleep 2
  fp=$(pgrep -P "$sp_" -x ffmpeg | head -1 || true)
  waited=0
  while [ -n "$fp" ] && kill -0 "$fp" 2>/dev/null && [ "$waited" -lt $((CLIP_SECS + 40)) ]; do sleep 1; waited=$((waited + 1)); done
  sleep $((lat / 1000 + 4))
  pkill -P "$sp_" 2>/dev/null || true
  sleep 1
  kill -INT "$prp" 2>/dev/null || true
  for _ in 1 2 3 4 5; do [ -s "$d/proxy.txt" ] && break; sleep 1; done
  kill -INT "$rp" 2>/dev/null || true
  for _ in 1 2 3 4 5 6; do kill -0 "$rp" 2>/dev/null || break; sleep 1; done

  if [ "$kind" = sapflow ]; then f=$(ls "$d"/rec/live/event-loss/*.mp4 2>/dev/null | head -1 || true); else f=$d/out.ts; fi
  v=0; a=0; e=NA; st=FAIL
  if [ -n "${f:-}" ] && [ -s "$f" ]; then
    v=$(ffprobe -v error -count_packets -select_streams v -show_entries stream=nb_read_packets -of csv=p=0 "$f" | head -1)
    a=$(ffprobe -v error -count_packets -select_streams a -show_entries stream=nb_read_packets -of csv=p=0 "$f" | head -1)
    e=$(ffmpeg -v error -i "$f" -f null - 2>&1 | wc -l | tr -d ' ')
    st=ok
  fi
  pstat() { awk -F= -v k="$1" '$1 == k { print $2 }' "$d/proxy.txt" 2>/dev/null || true; }
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$recv" "$loss" "$lat" "$seed" "${v:-0}" "${a:-0}" "$e" "$(pstat data_orig)" "$(pstat data_retx)" \
    "$(pstat nak_packets)" "$(pstat hs_flags)" "$st" "$(pstat reordered_orig)" "$(pstat orig_distinct)" "$(pstat in_time)" > "$OUT/trials/$idx.tsv"
  [ -n "${KEEP:-}" ] || rm -rf "$d"
  exit 0
fi

SEEDS=${SEEDS:-5}
LOSSES=${LOSSES:-"random:3 random:10 random:20 burst:8,8"}
LATENCIES=${LATENCIES:-"2000 200"}
RECEIVERS=${RECEIVERS:-"sapflow libsrt"}
CLIP_SECS=${CLIP_SECS:-30}
DELAY_MS=${DELAY_MS:-30}
JOBS=${JOBS:-3}

for bin in ffmpeg ffprobe srt-live-transmit; do
  command -v "$bin" > /dev/null || { echo "missing $bin" >&2; exit 1; }
done
if [ -z "${SAPFLOW_BIN:-}" ] || [ -z "${PROXY_BIN:-}" ]; then
  (cd "$ROOT" && cargo build --release -q -p sapflow-server -p sapflow-bench)
fi
SAPFLOW_BIN=${SAPFLOW_BIN:-$ROOT/target/release/sapflow}
PROXY_BIN=${PROXY_BIN:-$ROOT/target/release/srt-loss-proxy}
OUT=${OUT:-$ROOT/scratch/srt-loss/$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT/trials"
WORK=$(mktemp -d)
PORT_BASE=${PORT_BASE:-19000}
trap '[ -n "${KEEP:-}" ] || rm -rf "$WORK"' EXIT
export WORK OUT SAPFLOW_BIN PROXY_BIN DELAY_MS CLIP_SECS PORT_BASE KEEP

echo "clip: ${CLIP_SECS} s, seeds: $SEEDS, receivers: $RECEIVERS, losses: $LOSSES, latencies: $LATENCIES, jobs: $JOBS"
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1280x720:rate=30" \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -maxrate 2500k -bufsize 2500k \
  -c:a aac -b:a 128k -t "$CLIP_SECS" -f mpegts "$WORK/src.ts"

# Job list: a clean run for each receiver and latency, then the loss trials.
jobs=$WORK/jobs; : > "$jobs"; : > "$WORK/clean"; i=0
for lat in $LATENCIES; do for r in $RECEIVERS; do
  i=$((i + 1)); echo "$i $r none $lat 0" >> "$WORK/clean"
done; done
# Receivers alternate for each seed, so the two run side by side under the same
# conditions on a busy machine.
for lat in $LATENCIES; do for loss in $LOSSES; do
  s=1; while [ "$s" -le "$SEEDS" ]; do
    for r in $RECEIVERS; do i=$((i + 1)); echo "$i $r $loss $lat $s" >> "$jobs"; done
    s=$((s + 1))
  done
done; done
echo "$(wc -l < "$WORK/clean" | tr -d ' ') clean runs, then $(wc -l < "$jobs" | tr -d ' ') loss trials"
xargs -P "$JOBS" -L 1 "$SELF" --trial < "$WORK/clean"
xargs -P "$JOBS" -L 1 "$SELF" --trial < "$jobs"

printf 'receiver\tloss\tlatency_ms\tseed\tvideo\taudio\tdecode_msgs\tdata_orig\tdata_retx\tnaks\thandshake_flags\tstatus\treordered\torig_distinct\tin_time\n' > "$OUT/results.tsv"
# In the order the trials were listed, not file name order.
for f in $(ls "$OUT/trials" | sort -n); do cat "$OUT/trials/$f"; done >> "$OUT/results.tsv"
summarise "$OUT/results.tsv" | tee "$OUT/summary.md"
echo
echo "results: $OUT/results.tsv"
