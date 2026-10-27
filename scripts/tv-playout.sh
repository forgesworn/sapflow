#!/usr/bin/env bash
# Loop a folder of prepared videos into sapflow as one continuous publish.
#
#   scripts/tv-playout.sh [--shuffle] [--once] [--audio encode|copy] [--no-verify]
#                         [--strip-metadata]
#                         PREPARED_DIR [PUBLISH_URL]
#
# PREPARED_DIR holds the output of scripts/tv-prepare.sh. PUBLISH_URL is
# srt://... or rtmp://...; when omitted, TV_PUBLISH_URL is used, which keeps
# credentials out of a service file's command line. The files are joined with
# ffmpeg's concat demuxer, played in real time (-re) and looped without end
# (-stream_loop -1). When supported, a temporary 2x input rate replenishes
# the receiver after a blocked upload instead of FFmpeg's slow default catch-up. Video is copied untouched. Audio is re-encoded by default
# (AAC-LC, 48 kHz, stereo) because the AAC priming and padding frames at each
# file boundary overlap in time when copied (ffmpeg then reports non-monotonic
# DTS); --audio copy keeps the prepared audio bytes at the cost of that.
#
#   --shuffle     play in a random order (chosen at each start of ffmpeg)
#   --once        play the list once and exit with ffmpeg's status; no loop,
#                 no restart (for testing)
#   --audio MODE  encode (default) or copy
#   --no-verify   skip the check that the files share one SPS/PPS and AAC config
#   --strip-metadata  remove source tags, chapters and H.264 SEI (including
#                 captions); encode audio without FFmpeg's version marker.
#                 This does not anonymise the network or visible/audible content.
#
# ffmpeg is restarted with a growing delay (TV_BACKOFF_MIN to TV_BACKOFF_MAX
# seconds, 2 and 60 by default) whenever it exits, and why is logged. A
# restart is a new publisher on the same path: sapflow carries the stream on
# with one discontinuity when it comes back within SAPFLOW_RECONNECT_GRACE.
#
# SRT goes out through ffmpeg when it was built with libsrt, otherwise through
# srt-live-transmit. The URL's credentials (userinfo, streamid, passphrase,
# every query value but a few transport settings, an RTMP stream key) are
# replaced by *** in every line this script logs. They remain in the process
# list, as for any ffmpeg publisher.
set -euo pipefail

TV_BACKOFF_MIN=${TV_BACKOFF_MIN:-2}
TV_BACKOFF_MAX=${TV_BACKOFF_MAX:-60}
TV_AUDIO_BITRATE=${TV_AUDIO_BITRATE:-160k}
# An ffmpeg that ran this long counts as healthy: the next restart starts
# again from the minimum delay.
TV_STABLE_SECONDS=${TV_STABLE_SECONDS:-60}

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)

log() { printf '%s tv-playout: %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }
die() { log "$*"; exit 1; }

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2
  exit "${1:-2}"
}

# --- pure helpers (tests source this file and call them) ---------------------

# One concat-demuxer "file" line for an absolute path. Inside single quotes
# everything is literal; a single quote is written as '\''.
tv_concat_quote() {
  local p=$1 q="'" bs=$'\\' rep
  rep="$q$bs$q$q"
  printf "file '%s'\n" "${p//$q/$rep}"
}

# The concat list for the given paths, one "file" line each.
tv_concat_list() {
  local p
  for p in "$@"; do tv_concat_quote "$p"; done
}

# Query settings whose values are not secret, compared in lower case.
tv_safe_param() {
  case $1 in
    latency|rcvlatency|peerlatency|mode|pbkeylen|pkt_size|payload_size|connect_timeout|\
    send_buffer_size|recv_buffer_size|maxbw|oheadbw|tlpktdrop|nakreport|transtype|\
    tsbpddelay|rtmp_live|rtmp_buffer|timeout|listen_timeout) return 0 ;;
  esac
  return 1
}

tv_lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# %XX -> byte, so a secret can be found whether the log shows it encoded or not.
tv_urldecode() {
  local v=$1 bs=$'\\'
  printf '%b' "${v//%/${bs}x}"
}

# The URL with its credentials replaced by ***.
tv_redact_url() {
  local url=$1 scheme rest authority path query='' out pair key
  scheme=${url%%://*}
  rest=${url#*://}
  case $rest in *\?*) query=${rest#*\?}; rest=${rest%%\?*} ;; esac
  authority=${rest%%/*}
  path=${rest#"$authority"}
  case $authority in *@*) authority="***@${authority##*@}" ;; esac
  case $scheme in
    rtmp|rtmps|rtmpe|rtmpt|rtmpte|rtmpts)
      # app/key: the key is the last segment, and only worth hiding when
      # there is an app in front of it.
      case ${path#/} in */*) path="${path%/*}/***" ;; esac ;;
  esac
  out="$scheme://$authority$path"
  if [ -n "$query" ]; then
    local -a pairs=()
    local joined='' sep='?'
    IFS='&' read -r -a pairs <<< "$query"
    for pair in "${pairs[@]}"; do
      key=$(tv_lower "${pair%%=*}")
      case $pair in
        *=*) if tv_safe_param "$key"; then :; else pair="${pair%%=*}=***"; fi ;;
      esac
      joined="$joined$sep$pair"
      sep='&'
    done
    out="$out$joined"
  fi
  printf '%s\n' "$out"
}

# Every secret string in the URL, one per line, raw and decoded: the strings
# the log filter must replace wherever ffmpeg or srt-live-transmit echo them.
tv_secrets() {
  local url=$1 rest authority query='' userinfo pair key val path seg
  rest=${url#*://}
  case $rest in *\?*) query=${rest#*\?}; rest=${rest%%\?*} ;; esac
  authority=${rest%%/*}
  path=${rest#"$authority"}
  case $authority in
    *@*)
      userinfo=${authority%@*}
      printf '%s\n' "$userinfo" "${userinfo%%:*}" "$(tv_urldecode "${userinfo%%:*}")" "${userinfo#*:}" "$(tv_urldecode "${userinfo#*:}")" ;;
  esac
  case ${url%%://*} in
    rtmp|rtmps|rtmpe|rtmpt|rtmpte|rtmpts)
      case ${path#/} in */*) seg=${path##*/}; printf '%s\n' "$seg" "$(tv_urldecode "$seg")" ;; esac ;;
  esac
  if [ -n "$query" ]; then
    local -a pairs=()
    IFS='&' read -r -a pairs <<< "$query"
    for pair in "${pairs[@]}"; do
      case $pair in *=*) ;; *) continue ;; esac
      key=$(tv_lower "${pair%%=*}")
      val=${pair#*=}
      tv_safe_param "$key" && continue
      [ -n "$val" ] || continue
      printf '%s\n' "$val" "$(tv_urldecode "$val")"
      if [ "$key" = streamid ]; then
        # Encoders may print individual username/password fields without the
        # full stream ID. Hide those too, including short credentials.
        local decoded fields
        decoded=$(tv_urldecode "$val")
        fields=${decoded#publish:}
        case $fields in
          *:*:*) fields=${fields#*:}; printf '%s\n' "${fields%%:*}" "${fields#*:}" ;;
        esac
      fi
    done
  fi
  return 0
}

# Filter stdin to stdout, replacing each secret (arguments) with ***. The
# longest secrets go first so a secret holding another is replaced whole.
tv_redact_stream() {
  local line s
  local -a secrets=()
  while IFS= read -r s; do [ -n "$s" ] && secrets+=("$s"); done < <(printf '%s\n' "$@" | awk '{ print length($0) "\t" $0 }' | sort -rn | cut -f2-)
  while IFS= read -r line || [ -n "$line" ]; do
    if [ "${#secrets[@]}" -gt 0 ]; then
      for s in "${secrets[@]}"; do line=${line//"$s"/***}; done
    fi
    printf '%s\n' "$line"
  done
}

# --- the player --------------------------------------------------------------

kill_tree() { # pid
  local child
  for child in $(pgrep -P "$1" 2> /dev/null || true); do kill_tree "$child"; done
  kill -TERM "$1" 2> /dev/null || true
}

prepared_files() { # dir -> absolute paths, one per line, sorted by name
  local dir=$1 f
  while IFS= read -r -d '' f; do
    case $f in *$'\n'*) die "a file name with a newline cannot go in a concat list: $f" ;; esac
    printf '%s\n' "$f"
  done < <(find "$dir" -maxdepth 1 -type f ! -name '.*' -iname '*.mp4' -print0 | LC_ALL=C sort -z)
}

shuffle_lines() {
  awk -v seed="$RANDOM$RANDOM" 'BEGIN { srand(seed) } { printf "%.9f\t%s\n", rand(), $0 }' | sort -n | cut -f2-
}

# srt-live-transmit does not notice the end of its input for about 12 s.
# Stop it 3 s after ffmpeg is done (longer than a 2 s SRT latency, so the
# last packets are out) so a restart is not held up.
sink_srt() { # url; the MPEG-TS arrives on stdin
  srt-live-transmit -q -a:no -c:1316 file://con "$1" <&0 &
  local pid=$! waited=0
  while kill -0 "$pid" 2> /dev/null; do
    if [ -e "$work/ffmpeg.done" ]; then
      waited=$((waited + 1))
      if [ "$waited" -ge 6 ]; then kill -TERM "$pid" 2> /dev/null; break; fi
    fi
    sleep 0.5
  done
  wait "$pid"
}

tv_catchup_options() { # ffmpeg full help text -> supported input options
  case $1 in *-readrate_catchup*) printf '%s\n' -readrate_catchup 2 ;; esac
}

ffmpeg_has_srt() { ffmpeg -hide_banner -protocols 2> /dev/null | grep -qw srt; }

main() {
  local shuffle=0 once=0 audio=encode verify=1 strip_metadata=0
  while [ "$#" -gt 0 ]; do
    case $1 in
      --shuffle) shuffle=1; shift ;;
      --once) once=1; shift ;;
      --audio) [ "$#" -ge 2 ] || usage; audio=$2; shift 2 ;;
      --no-verify) verify=0; shift ;;
      --strip-metadata) strip_metadata=1; shift ;;
      -h|--help) usage 0 ;;
      --) shift; break ;;
      -*) echo "tv-playout: unknown option $1" >&2; usage ;;
      *) break ;;
    esac
  done
  case $audio in encode|copy) ;; *) die "--audio: encode or copy" ;; esac
  [ "$strip_metadata" = 0 ] || [ "$audio" = encode ] || die "--strip-metadata requires encoded audio"
  local setting
  for setting in "$TV_BACKOFF_MIN" "$TV_BACKOFF_MAX" "$TV_STABLE_SECONDS"; do
    [[ "$setting" =~ ^[1-9][0-9]{0,4}$ ]] || die "backoff and stable times must be positive integers up to 99999 seconds"
  done
  [ "$TV_BACKOFF_MIN" -le "$TV_BACKOFF_MAX" ] || die "TV_BACKOFF_MIN exceeds TV_BACKOFF_MAX"
  [ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
  local dir url
  dir=$(cd "$1" 2> /dev/null && pwd -P) || die "$1 is not a directory"
  url=${2:-${TV_PUBLISH_URL:-}}
  [ -n "$url" ] || die "no PUBLISH_URL argument and TV_PUBLISH_URL is not set"
  case $url in
    srt://*|rtmp://*|rtmps://*) ;;
    *) die "PUBLISH_URL must start with srt://, rtmp:// or rtmps://" ;;
  esac
  command -v ffmpeg > /dev/null || die "missing ffmpeg"

  local redacted sink_mode
  redacted=$(tv_redact_url "$url")
  local -a secrets=()
  local s
  while IFS= read -r s; do [ -n "$s" ] && secrets+=("$s"); done < <(tv_secrets "$url")

  case $url in
    srt://*)
      if ffmpeg_has_srt; then
        sink_mode=ffmpeg-srt
      elif command -v srt-live-transmit > /dev/null; then
        sink_mode=srt-live-transmit
      else
        die "this ffmpeg has no SRT support and srt-live-transmit is not installed"
      fi ;;
    *) sink_mode=ffmpeg-rtmp ;;
  esac

  if [ "$verify" = 1 ]; then
    local report
    report=$("$HERE/tv-prepare.sh" --verify "$dir" 2>&1) \
      || { printf '%s\n' "$report" | tv_redact_stream ${secrets[@]+"${secrets[@]}"} >&2; die "files in $dir do not share one format; run scripts/tv-prepare.sh, or pass --no-verify"; }
    log "$(printf '%s\n' "$report" | head -1)"
  fi

  local work
  work=$(mktemp -d "${TMPDIR:-/tmp}/tv-playout.XXXXXX")
  local child=0 stop=0
  on_signal() { stop=1; [ "$child" -gt 0 ] && kill_tree "$child"; return 0; }
  cleanup() { rm -rf "$work"; }
  trap on_signal INT TERM
  trap cleanup EXIT

  local catchup
  catchup=$(tv_catchup_options "$(ffmpeg -hide_banner -h full 2> /dev/null || true)")
  local backoff=$TV_BACKOFF_MIN started elapsed rc reason attempt=0
  log "publishing to $redacted via $sink_mode (audio $audio$([ "$once" = 1 ] && echo ', once'))"
  while :; do
    attempt=$((attempt + 1))
    local -a files=()
    while IFS= read -r s; do files+=("$s"); done < <(prepared_files "$dir")
    [ "${#files[@]}" -gt 0 ] || die "no .mp4 files in $dir"
    if [ "$shuffle" = 1 ]; then
      local -a shuffled=()
      while IFS= read -r s; do shuffled+=("$s"); done < <(printf '%s\n' "${files[@]}" | shuffle_lines)
      files=("${shuffled[@]}")
    fi
    tv_concat_list "${files[@]}" > "$work/list.txt"
    log "attempt $attempt: ${#files[@]} file(s)"

    local -a in=(-re)
    [ -z "$catchup" ] || in+=(-readrate_catchup 2)
    [ "$once" = 1 ] || in+=(-stream_loop -1)
    # Only local prepared files may be opened in the private publishing mode.
    [ "$strip_metadata" = 0 ] || in+=(-protocol_whitelist file,concat)
    in+=(-f concat -safe 0 -i "$work/list.txt")
    local -a codec=(-map 0:v:0 -map 0:a:0 -c:v copy)
    if [ "$audio" = encode ]; then
      codec+=(-af "aresample=48000:async=1:first_pts=0"
        -c:a aac -profile:a aac_low -b:a "$TV_AUDIO_BITRATE" -ar 48000 -ac 2)
    else
      codec+=(-c:a copy)
    fi
    if [ "$strip_metadata" = 1 ]; then
      codec+=(-map_metadata -1 -map_metadata:s -1 -map_chapters -1
        -fflags +bitexact -flags:a +bitexact -metadata encoder= -metadata:s encoder=
        -bsf:v filter_units=remove_types=6)
    fi
    local -a base=(ffmpeg -hide_banner -nostdin -nostats -loglevel warning "${in[@]}" "${codec[@]}")
    local -a rtmp_options=()
    # FFmpeg otherwise advertises its library version in the RTMP connect
    # command, independently of the muxer and encoded-media markers.
    [ "$strip_metadata" = 0 ] || rtmp_options=(-rtmp_flashver FMLE/3.0)

    : > "$work/rc"
    : > "$work/err"
    rm -f "$work/ffmpeg.done"
    started=$(date +%s)
    {
     (
      set +e
      case $sink_mode in
        ffmpeg-srt)
          "${base[@]}" -flush_packets 1 -f mpegts "$url"
          echo "ffmpeg=$?" > "$work/rc" ;;
        srt-live-transmit)
          { "${base[@]}" -flush_packets 1 -f mpegts pipe:1; echo "ffmpeg=$?" > "$work/rc"; : > "$work/ffmpeg.done"; } | sink_srt "$url"
          echo "srt-live-transmit=${PIPESTATUS[1]}" >> "$work/rc" ;;
        ffmpeg-rtmp)
          "${base[@]}" ${rtmp_options[@]+"${rtmp_options[@]}"} -flvflags no_duration_filesize -f flv "$url"
          echo "ffmpeg=$?" > "$work/rc" ;;
      esac
     ) 2>&1 | tv_redact_stream ${secrets[@]+"${secrets[@]}"} | tee -a "$work/err" | sed 's/^/ffmpeg: /' >&3
    } 3>&2 2> /dev/null &
    child=$!
    { wait "$child" || true; } 2> /dev/null
    child=0
    elapsed=$(( $(date +%s) - started ))
    reason=$(tail -n 3 "$work/err" 2> /dev/null | tr '\n' '|' | sed 's/|$//')
    rc=$(tr '\n' ' ' < "$work/rc" 2> /dev/null | sed 's/ $//' || true)
    [ "$stop" = 0 ] || { log "stopping on signal"; exit 0; }
    if [ "$once" = 1 ]; then
      log "finished after ${elapsed}s (${rc:-no status}); last output: ${reason:-none}"
      case $rc in "ffmpeg=0"|"ffmpeg=0 srt-live-transmit=0"|"ffmpeg=0 srt-live-transmit=143") exit 0 ;; *) exit 1 ;; esac
    fi
    if [ "$elapsed" -ge "$TV_STABLE_SECONDS" ]; then backoff=$TV_BACKOFF_MIN; fi
    log "ffmpeg ended after ${elapsed}s (${rc:-no status}); last output: ${reason:-none}; restarting in ${backoff}s"
    sleep "$backoff" &
    child=$!
    { wait "$child" || true; } 2> /dev/null
    child=0
    [ "$stop" = 0 ] || { log "stopping on signal"; exit 0; }
    backoff=$(( backoff * 2 ))
    [ "$backoff" -le "$TV_BACKOFF_MAX" ] || backoff=$TV_BACKOFF_MAX
  done
}

# Sourced by tests, run as a script otherwise.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
