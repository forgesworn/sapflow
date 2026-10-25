#!/usr/bin/env bash
# SRT under packet loss: sapflow's receiver against libsrt's, same sender, same
# loss, several seeds.
#
#   scripts/srt-loss.sh                          # the full matrix, 5 seeds
#   SEEDS=3 LOSSES=random:10 LATENCIES=200 scripts/srt-loss.sh
#   RECEIVERS=sapflow SAPFLOW_BIN=/path/to/sapflow scripts/srt-loss.sh
#   RECEIVERS="old=/path/sapflow-old new=/path/sapflow-new libsrt" scripts/srt-loss.sh
#   PASSPHRASE=loss-test-key KMREFRESH=2000 RECEIVERS="own libsrt" scripts/srt-loss.sh
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
#   - proxy admission loss: originals with no surviving copy admitted to the
#     delay queue within the SRT latency of the first send. This is a proxy
#     diagnostic, not the receiver's actual delivery or playout deadline.
#   - video and audio packets delivered, against the same receiver's clean run
#   - decode messages: lines `ffmpeg -v error` prints decoding the output
# plus sapflow minus libsrt with a Welch t statistic. With 5 seeds |t| below
# about 2.3 is not a difference worth claiming.
#
# A trial passes only with a completed source encoder, positive video/audio
# counts, successful probes/decoding and no decoder error messages. Every
# planned trial is retained, including crashes and trials not attempted after
# an earlier failure. A failed or incomplete collection exits nonzero.
#
# Needs python3, ffmpeg, ffprobe and srt-live-transmit on PATH. Nothing is written
# outside OUT (default scratch/srt-loss/<time>, which is gitignored) and a
# temporary directory that is removed on exit (KEEP=1 keeps it).
#
# Environment:
#   SEEDS=5             seeds per condition
#   LOSSES="random:3 random:10 random:20 burst:8,8"
#                       srt-loss-proxy --loss values; burst:P,L is bursty loss
#                       (Gilbert-Elliott), P percent on average, L packets a burst
#   LATENCIES="2000 200"   SRT latency in ms, set on sender and receiver
#   RECEIVERS="sapflow libsrt"   `NAME=binary` runs another sapflow build as NAME
#                       (each with its own default SRT: builds before
#                       2026-10-21 default to srt-rs, since then our own)
#   CLIP_SECS=30  DELAY_MS=30 (one way)  JOBS=3 (trials at once)
#   PASSPHRASE=         encrypt the stream, off when unset. 10 to 79 of
#                       A-Z a-z 0-9 . _ - (libsrt 1.5.3 does not decode %20).
#                       The sender and libsrt receiver get it as URL options,
#                       sapflow as SAPFLOW_SRT_PASSPHRASE, so every sapflow
#                       receiver must be our own SRT (a name starting `own`).
#   PBKEYLEN=16         AES key length with PASSPHRASE: 16 (AES-128) or 32
#   KMREFRESH=          sender key refresh every this many packets (libsrt
#                       kmrefreshrate; its default of 2^24 never fires in a
#                       clip). At 2.5 Mbit/s a 30 s clip is about 7,300 packets.
#   KMPREANNOUNCE=      packets before the switch that the new key is sent
#                       (default KMREFRESH / 4; at most (KMREFRESH - 1) / 2)
#   SENDER_PASSPHRASE=  testing only: a different passphrase on the sender, to
#                       show that a mismatch delivers nothing
#   Encryption and qualification fields follow the original 15 result columns.
#   Failed and encrypted trials retain diagnostics in $OUT/logs.
#   SAPFLOW_BIN, PROXY_BIN   default: release builds of this checkout
# See docs/srt-loss-method.md for qualification rules and evidence limits.
set -euo pipefail

SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
ROOT=$(cd "$(dirname "$0")/.." && pwd)
RESULTS=$ROOT/scripts/srt-loss-results.py
UDP_FIXTURE=$ROOT/scripts/srt-loss-udp.py

summarise() { # results.tsv
  awk -F'\t' '
    function mean(s, c) { return c ? s / c : 0 }
    function sd(s, ss, c) { return c > 1 ? sqrt((ss - s * s / c) / (c - 1)) : 0 }
    NR == 1 { for (i = 1; i <= NF; i++) if ($i == "encryption") enc_col = i; next }
    {
      trial_rows++
      r = $1; loss = $2; lat = $3; v = $5; a = $6; dec = $7; orig = $8; retx = $9
      if ($13 + 0 > 0) reordered++
      if (enc_col && !($(enc_col) in encseen)) { encseen[$(enc_col)] = 1; encs[++nenc] = $(enc_col) }
      if ($12 != "ok" || v !~ /^[0-9]+$/ || a !~ /^[0-9]+$/ || v + 0 <= 0 || a + 0 <= 0 || dec !~ /^0$/) {
        failed[r SUBSEP loss SUBSEP lat]++; total_failed++
        printf "Unqualified trial: %s %s %s ms seed %s (%s)\n", r, loss, lat, $4, $12
        next
      }
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
      if (!trial_rows) { print "Unqualified collection: no trial rows"; total_failed++ }
      for (i = 1; i <= nenc; i++) printf "Encryption: %s\n", encs[i]
      if (nenc) print ""
      if (reordered) printf "WARNING: the proxy reordered packets in %d trials; those numbers are suspect\n\n", reordered
      if (total_failed) printf "\nUnqualified or incomplete trials: %d (retained in results.tsv)\n\n", total_failed
      print "| Loss | Latency | Receiver | n | Retransmission overhead | Proxy admission loss | Video delivered | Audio delivered | Decode messages |"
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
      print "| Loss | Latency | Receiver | Overhead, points | t | Proxy admission loss, points | t | Video delivered, points | t | Decode messages | t |"
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
      if (total_failed) exit 1
    }
    function welch(m1, s1, n1, m2, s2, n2,   se) {
      se = sqrt(s1 * s1 / n1 + s2 * s2 / n2)
      return se > 0 ? sprintf("%.1f", (m1 - m2) / se) : "-"
    }
  ' "$1"
}

if [ "${1:-}" = "--summarise" ]; then summarise "${2:?usage: --summarise results.tsv}"; exit $?; fi

# One trial: --trial INDEX RECEIVER LOSS LATENCY SEED. Saves a raw receipt and
# its actual exit status. The parent joins these to the durable trial plan.
if [ "${1:-}" = "--trial" ]; then
  shift; idx=$1; spec=$2; loss=$3; lat=$4; seed=$5
  # A receiver is `libsrt` or `NAME[=sapflow binary]`; any name starting with
  # libsrt means libsrt's srt-live-transmit.
  recv=${spec%%=*}; bin=${spec#*=}; [ "$bin" != "$spec" ] || bin=$SAPFLOW_BIN
  if [ "${recv#libsrt}" != "$recv" ]; then kind=libsrt; else kind=sapflow; fi
  d=$WORK/t$idx; pids=(); trial_dir_ready=0
  printf 'started\n' > "$OUT/trials/$idx.started"
  cleanup_trial() {
    code=$?
    trap - EXIT
    set +e
    # Stop only this trial's children. Killing the producer children first
    # lets its shell save PIPESTATUS, including the actual encoder exit.
    for p in "${pids[@]:-}"; do
      [ -n "$p" ] || continue
      pkill -P "$p" 2>/dev/null || true
      kill "$p" 2>/dev/null || true
    done
    for _ in 1 2 3 4 5; do
      alive=0
      for p in "${pids[@]:-}"; do [ -z "$p" ] || ! kill -0 "$p" 2>/dev/null || alive=1; done
      [ "$alive" = 1 ] || break
      sleep 0.1
    done
    for p in "${pids[@]:-}"; do [ -z "$p" ] || kill -KILL "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    printf '%s\n' "$code" > "$OUT/trials/$idx.exit"
    if [ "$trial_dir_ready" = 1 ] && { [ "$code" != 0 ] || [ -n "${PASSPHRASE:-}" ]; }; then
      mkdir -p "$OUT/logs/$idx-$recv"
      for n in rcv.log snd.log proxy.log proxy.txt ff.log pipeline.status outcomes.txt video.packets audio.packets video-probe.log audio-probe.log decode.log; do
        [ ! -f "$d/$n" ] || cp "$d/$n" "$OUT/logs/$idx-$recv/$n"
      done
    fi
    [ "$trial_dir_ready" != 1 ] || [ -n "${KEEP:-}" ] || rm -rf "$d"
    exit "$code"
  }
  trap cleanup_trial EXIT
  mkdir -p "$d"; trial_dir_ready=1
  : > "$d/outcomes.txt"
  # Five unique ports per planned trial, with no wrap into an active trial.
  base=$((PORT_BASE + (idx - 1) * 5)); sp=$base; pp=$((base + 1)); lp=$((base + 2)); hp=$((base + 3)); up=$((base + 4))
  python3 "$UDP_FIXTURE" free --udp "$sp" "$pp" "$lp" "$up" --tcp "$hp" >> "$d/outcomes.txt" 2> "$d/snd.log"
  # Encryption is opt-in. libsrt takes it as URL options; sapflow as
  # environment (exported here, so it reaches only this trial's children).
  sndq=""; rcvq=""
  if [ -n "${PASSPHRASE:-}" ]; then
    export SAPFLOW_SRT_PASSPHRASE=$PASSPHRASE SAPFLOW_SRT_PBKEYLEN=$PBKEYLEN
    rcvq="&passphrase=$PASSPHRASE&pbkeylen=$PBKEYLEN"
    sndq="&passphrase=${SENDER_PASSPHRASE:-$PASSPHRASE}&pbkeylen=$PBKEYLEN"
    [ -z "${KMREFRESH:-}" ] || sndq="$sndq&kmrefreshrate=$KMREFRESH&kmpreannounce=$KMPREANNOUNCE"
  fi
  retire_child() {
    local finished=$1 slot
    for slot in "${!pids[@]}"; do
      if [ "${pids[$slot]}" = "$finished" ]; then unset 'pids[slot]'; fi
    done
  }
  stop_child() { # PID, name, grace seconds, optional signal.
    pid=$1; name=$2; grace=$3; stopped=0; requested_signal=${4:-INT}
    if kill -0 "$pid" 2>/dev/null; then stopped=1; kill -"$requested_signal" "$pid" 2>/dev/null || true; fi
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$grace" ]; do sleep 1; waited=$((waited + 1)); done
    if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
    child_code=0; wait "$pid" || child_code=$?
    retire_child "$pid"
    printf '%s_stop=%s\n%s_exit=%s\n' "$name" "$stopped" "$name" "$child_code" >> "$d/outcomes.txt"
  }

  if [ "$kind" = sapflow ]; then
    SAPFLOW_OPEN_PUBLISH=1 SAPFLOW_HTTP_ADDR=127.0.0.1:$hp SAPFLOW_SRT_ADDR=127.0.0.1:$sp \
      SAPFLOW_RTMP_ADDR=off SAPFLOW_RECORDINGS="$d/rec" RUST_LOG=warn \
      "$bin" > "$d/rcv.log" 2>&1 &
    rp=$!; target=$sp; sid="&streamid=publish:live/event-loss"
  else
    srt-live-transmit "srt://127.0.0.1:$lp?mode=listener&latency=$lat$rcvq" file://con \
      > "$d/out.ts" 2> "$d/rcv.log" &
    rp=$!; target=$lp; sid=""
  fi
  pids+=("$rp"); sleep 1
  "$PROXY_BIN" --listen 127.0.0.1:$pp --target 127.0.0.1:$target --loss "$loss" \
    --delay-ms "$DELAY_MS" --latency-ms "$lat" --seed "$seed" --stats "$d/proxy.txt" 2> "$d/proxy.log" &
  prp=$!; pids+=("$prp"); sleep 0.5

  # UDP supplies explicit TS message boundaries instead of relying on the
  # CLI's file-input framing. Verify the actual child owns its
  # UDP input before the real-time feeder sends its first datagram.
  srt-live-transmit -a:no -c:1316 "udp://127.0.0.1:$up" "srt://127.0.0.1:$pp?latency=$lat$sndq$sid" > "$d/snd.log" 2>&1 &
  srp=$!; pids+=("$srp")
  python3 "$UDP_FIXTURE" wait --pid "$srp" --port "$up" >> "$d/outcomes.txt" 2>> "$d/snd.log"
  # Preserve the actual FFmpeg PIPESTATUS even when it exits before discovery.
  # Its supervising shell and the independent sender are reaped separately.
  ( set +e
    ffmpeg -hide_banner -loglevel error -re -i "$WORK/src.ts" -c copy -f mpegts "udp://127.0.0.1:$up?pkt_size=1316" 2> "$d/ff.log"
    codes=("${PIPESTATUS[@]}")
    printf 'encoder_exit=%s\n' "${codes[0]}" > "$d/pipeline.status.tmp"
    mv "$d/pipeline.status.tmp" "$d/pipeline.status"
  ) &
  sp_=$!; pids+=("$sp_"); sleep 2
  fp=$(pgrep -P "$sp_" -x ffmpeg | head -1 || true)
  waited=0
  while [ -n "$fp" ] && kill -0 "$fp" 2>/dev/null && [ "$waited" -lt $((CLIP_SECS + 40)) ]; do sleep 1; waited=$((waited + 1)); done
  encoder_timeout=0
  [ -z "$fp" ] || ! kill -0 "$fp" 2>/dev/null || encoder_timeout=1
  printf 'encoder_timeout=%s\n' "$encoder_timeout" >> "$d/outcomes.txt"
  if [ -s "$d/pipeline.status" ]; then
    wait "$sp_" || true
    retire_child "$sp_"; sp_=""
  fi
  sleep $((lat / 1000 + 4))
  [ -z "$sp_" ] || pkill -P "$sp_" 2>/dev/null || true
  stop_child "$srp" sender 5 TERM
  sleep 1
  for _ in 1 2 3 4 5; do [ ! -s "$d/pipeline.status" ] || break; sleep 1; done
  if [ -s "$d/pipeline.status" ]; then
    if [ -n "$sp_" ]; then wait "$sp_" || true; retire_child "$sp_"; fi
    printf 'sender_exit=%s\n' "$child_code" >> "$d/pipeline.status"
  fi
  stop_child "$prp" proxy 5
  stop_child "$rp" receiver 6

  if [ "$kind" = sapflow ]; then f=$(ls "$d"/rec/live/event-loss/*.mp4 2>/dev/null | head -1 || true); else f=$d/out.ts; fi
  if [ -n "${f:-}" ] && [ -s "$f" ]; then
    # Keep tool exits separately from their output. A decoder can print
    # errors and still exit zero, and a probe can return zero packets.
    v_exit=0; a_exit=0; e_exit=0
    ffprobe -v error -count_packets -select_streams v -show_entries stream=nb_read_packets -of csv=p=0 "$f" > "$d/video.packets" 2> "$d/video-probe.log" || v_exit=$?
    ffprobe -v error -count_packets -select_streams a -show_entries stream=nb_read_packets -of csv=p=0 "$f" > "$d/audio.packets" 2> "$d/audio-probe.log" || a_exit=$?
    ffmpeg -v error -i "$f" -f null - > "$d/decode.log" 2>&1 || e_exit=$?
    printf 'video_probe_exit=%s\naudio_probe_exit=%s\ndecoder_exit=%s\n' "$v_exit" "$a_exit" "$e_exit" >> "$d/outcomes.txt"
  fi
  python3 "$RESULTS" record --output "$OUT" --index "$idx" --work "$d"
  exit $?
fi

SEEDS=${SEEDS:-5}
LOSSES=${LOSSES:-"random:3 random:10 random:20 burst:8,8"}
LATENCIES=${LATENCIES:-"2000 200"}
RECEIVERS=${RECEIVERS:-"sapflow libsrt"}
CLIP_SECS=${CLIP_SECS:-30}
DELAY_MS=${DELAY_MS:-30}
JOBS=${JOBS:-3}
PASSPHRASE=${PASSPHRASE:-}
PBKEYLEN=${PBKEYLEN:-16}
KMREFRESH=${KMREFRESH:-}
KMPREANNOUNCE=${KMPREANNOUNCE:-}
SENDER_PASSPHRASE=${SENDER_PASSPHRASE:-}

die() { echo "$*" >&2; exit 1; }
ENC_DESC=""
if [ -n "$PASSPHRASE" ]; then
  for pw in "$PASSPHRASE" "$SENDER_PASSPHRASE"; do
    [ -z "$pw" ] || [[ $pw =~ ^[A-Za-z0-9._-]{10,79}$ ]] || die "a passphrase is 10 to 79 characters of A-Z a-z 0-9 . _ -"
  done
  case $PBKEYLEN in 16|32) ;; *) die "PBKEYLEN is 16 or 32" ;; esac
  ENC_DESC="aes$((PBKEYLEN * 8))"
  if [ -n "$KMREFRESH" ]; then
    [[ $KMREFRESH =~ ^[0-9]+$ ]] && [ "$KMREFRESH" -ge 3 ] || die "KMREFRESH is a packet count, at least 3"
    KMPREANNOUNCE=${KMPREANNOUNCE:-$((KMREFRESH / 4))}
    [[ $KMPREANNOUNCE =~ ^[0-9]+$ ]] && [ "$KMPREANNOUNCE" -le $(((KMREFRESH - 1) / 2)) ] ||
      die "KMPREANNOUNCE is at most (KMREFRESH - 1) / 2 = $(((KMREFRESH - 1) / 2))"
    ENC_DESC="$ENC_DESC, key refresh every $KMREFRESH packets (new key sent $KMPREANNOUNCE before)"
  else
    [ -z "$KMPREANNOUNCE" ] || die "KMPREANNOUNCE needs KMREFRESH"
    ENC_DESC="$ENC_DESC, no key refresh"
  fi
  [ -z "$SENDER_PASSPHRASE" ] || ENC_DESC="$ENC_DESC, sender passphrase differs from the receiver's"
elif [ -n "$KMREFRESH$KMPREANNOUNCE$SENDER_PASSPHRASE" ]; then
  die "KMREFRESH, KMPREANNOUNCE and SENDER_PASSPHRASE need PASSPHRASE"
fi

for bin in python3 ffmpeg ffprobe srt-live-transmit; do
  command -v "$bin" > /dev/null || { echo "missing $bin" >&2; exit 1; }
done
if [ -z "${SAPFLOW_BIN:-}" ] || [ -z "${PROXY_BIN:-}" ]; then
  (cd "$ROOT" && cargo build --release -q -p sapflow -p sapflow-bench)
fi
SAPFLOW_BIN=${SAPFLOW_BIN:-$ROOT/target/release/sapflow}
PROXY_BIN=${PROXY_BIN:-$ROOT/target/release/srt-loss-proxy}
OUT=${OUT:-$ROOT/scratch/srt-loss/$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT/trials"
[ ! -e "$OUT/plan.tsv" ] && [ ! -e "$OUT/results.tsv" ] && [ -z "$(ls -A "$OUT/trials")" ] || die "OUT already contains trial evidence: $OUT"
WORK=$(mktemp -d)
PORT_BASE=${PORT_BASE:-19000}
finish_run() {
  code=$?
  trap - EXIT
  set +e
  printf '%s\n' "$code" > "$OUT/run-body.exit"
  if [ -s "$OUT/plan.tsv" ]; then
    python3 "$RESULTS" collate --output "$OUT" --work "$WORK" || code=1
    summarise "$OUT/results.tsv" > "$OUT/summary.md" || code=1
    cat "$OUT/summary.md"
    echo
    echo "results: $OUT/results.tsv"
  fi
  [ -n "${KEEP:-}" ] || rm -rf "$WORK"
  printf '%s\n' "$code" > "$OUT/run.exit"
  exit "$code"
}
trap finish_run EXIT
export WORK OUT SAPFLOW_BIN PROXY_BIN DELAY_MS CLIP_SECS PORT_BASE KEEP
[ -z "$PASSPHRASE" ] || export PASSPHRASE PBKEYLEN KMREFRESH KMPREANNOUNCE SENDER_PASSPHRASE ENC_DESC

echo "clip: ${CLIP_SECS} s, seeds: $SEEDS, receivers: $RECEIVERS, losses: $LOSSES, latencies: $LATENCIES, jobs: $JOBS"
[ -z "$PASSPHRASE" ] || echo "encryption: $ENC_DESC"
# Job list: a clean run for each receiver and latency, then the loss trials.
jobs=$WORK/jobs; : > "$jobs"; : > "$WORK/clean"; i=0
printf 'trial_index\treceiver\treceiver_spec\tloss\tlatency_ms\tseed\tencryption\n' > "$OUT/plan.tsv"
for lat in $LATENCIES; do for r in $RECEIVERS; do
  i=$((i + 1)); echo "$i $r none $lat 0" >> "$WORK/clean"
  printf '%s\t%s\t%s\tnone\t%s\t0\t%s\n' "$i" "${r%%=*}" "$r" "$lat" "$ENC_DESC" >> "$OUT/plan.tsv"
done; done
# Receivers alternate for each seed, so the two run side by side under the same
# conditions on a busy machine.
for lat in $LATENCIES; do for loss in $LOSSES; do
  s=1; while [ "$s" -le "$SEEDS" ]; do
    for r in $RECEIVERS; do
      i=$((i + 1)); echo "$i $r $loss $lat $s" >> "$jobs"
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$i" "${r%%=*}" "$r" "$loss" "$lat" "$s" "$ENC_DESC" >> "$OUT/plan.tsv"
    done
    s=$((s + 1))
  done
done; done
echo "$(wc -l < "$WORK/clean" | tr -d ' ') clean runs, then $(wc -l < "$jobs" | tr -d ' ') loss trials"
python3 "$UDP_FIXTURE" range --base "$PORT_BASE" --trials "$i"
# Freeze the plan before generating media, so setup failure leaves every
# planned trial explicitly not attempted rather than an empty successful run.
ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=1280x720:rate=30" \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -pix_fmt yuv420p \
  -c:v libx264 -preset veryfast -tune zerolatency -g 30 -b:v 2500k -maxrate 2500k -bufsize 2500k \
  -c:a aac -b:a 128k -t "$CLIP_SECS" -f mpegts "$WORK/src.ts"
python3 "$UDP_FIXTURE" source "$WORK/src.ts" > "$OUT/generated-source.txt"
xargs -P "$JOBS" -L 1 "$SELF" --trial < "$WORK/clean" || echo "warning: a clean trial failed; retained in results.tsv" >&2
xargs -P "$JOBS" -L 1 "$SELF" --trial < "$jobs" || echo "warning: a loss trial failed; retained in results.tsv" >&2
