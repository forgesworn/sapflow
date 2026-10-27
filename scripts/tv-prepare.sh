#!/usr/bin/env bash
# Normalise a folder of videos to one fixed format for a linear "TV channel".
#
#   scripts/tv-prepare.sh [--force] SRC_DIR OUT_DIR
#   scripts/tv-prepare.sh --verify DIR
#
# sapflow serves one rendition, and a publisher that carries on must send the
# same SPS/PPS and AAC config as the stream's init segment, or the stream
# restarts in place. So every file is re-encoded to the same format, with the
# same encoder settings, and the outputs are then compared byte for byte
# (docs/tv-channel.md). Sources are never touched. An output that exists is
# never overwritten: it is skipped when newer than its source, and reported as
# stale (and left alone) when older, unless --force is given. Output is
# written to a temporary name and moved into place only when ffmpeg succeeds.
#
# Format, overridable through the environment (defaults shown):
#   TV_WIDTH=1920 TV_HEIGHT=1080   scale to fit, pad with black, keep aspect
#   TV_FPS=25                      constant; 30000/1001 etc. are accepted
#   TV_ENCODER=libx264             or videotoolbox (hardware; the SPS check
#                                  still decides whether the outputs match)
#   TV_PRESET=medium               libx264 preset
#   TV_VIDEO_BITRATE=4500k         maxrate is 10/9 of it (5M), bufsize twice that
#   TV_MAXRATE / TV_BUFSIZE        override the two derived values
#   TV_AUDIO_BITRATE=160k          AAC-LC, 48 kHz, stereo
set -euo pipefail

TV_WIDTH=${TV_WIDTH:-1920}
TV_HEIGHT=${TV_HEIGHT:-1080}
TV_FPS=${TV_FPS:-25}
TV_ENCODER=${TV_ENCODER:-libx264}
TV_PRESET=${TV_PRESET:-medium}
TV_VIDEO_BITRATE=${TV_VIDEO_BITRATE:-4500k}
TV_AUDIO_BITRATE=${TV_AUDIO_BITRATE:-160k}
TV_AUDIO_RATE=48000

die() { echo "tv-prepare: $*" >&2; exit 1; }

usage() {
  sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//' >&2
  exit "${1:-2}"
}

# 4500k, 4.5M, 5000000 -> kbit/s as an integer.
to_kbit() {
  awk -v v="$1" 'BEGIN {
    if (match(v, /^[0-9]+(\.[0-9]+)?[kKmM]?$/) == 0) exit 1
    n = v + 0
    s = substr(v, length(v))
    if (s == "k" || s == "K") k = n
    else if (s == "m" || s == "M") k = n * 1000
    else k = n / 1000
    if (k < 1 || k > 1000000) exit 1
    printf "%d\n", k + 0.5
  }'
}

# 25 -> 25, 30000/1001 -> 30 (the keyframe interval is one second of frames).
fps_gop() {
  awk -v v="$1" 'BEGIN {
    if (v !~ /^[0-9]+(\.[0-9]+)?(\/[0-9]+(\.[0-9]+)?)?$/) exit 1
    n = split(v, p, "/")
    if (n == 1) f = p[1] + 0
    else if (n == 2 && p[2] + 0 > 0) f = p[1] / p[2]
    else exit 1
    if (f < 1 || f > 120) exit 1
    printf "%d\n", f + 0.5
  }'
}

# Seconds -> m:ss.s, bytes -> MiB, for the per-file line.
fmt_duration() { awk -v s="$1" 'BEGIN { printf "%d:%04.1f", s / 60, s - 60 * int(s / 60) }'; }
fmt_size() { awk -v b="$1" 'BEGIN { printf "%.1f MiB", b / 1048576 }'; }
file_size() { wc -c < "$1" | tr -d ' '; }

# Everything a concat of these files needs to agree on, one "key=value" per
# line: the stream fields ffprobe reports, the SPS and PPS NAL units of the
# first access unit (Annex B, as hex), and the AAC AudioSpecificConfig.
fingerprint() {
  local f=$1
  ffprobe -v error -protocol_whitelist file -select_streams v:0 -of default=noprint_wrappers=1 \
    -show_entries stream=codec_name,profile,level,width,height,coded_width,coded_height,pix_fmt,sample_aspect_ratio,r_frame_rate,avg_frame_rate,field_order,color_range,color_space,color_transfer,color_primaries,time_base \
    "$f" | sed 's/^/video./'
  ffprobe -v error -protocol_whitelist file -select_streams a:0 -of default=noprint_wrappers=1 \
    -show_entries stream=codec_name,profile,sample_fmt,sample_rate,channels,channel_layout,time_base \
    "$f" | sed 's/^/audio./'
  printf 'video.sps_pps='
  ffmpeg -v error -protocol_whitelist file -i "$f" -map 0:v:0 -c:v copy \
    -bsf:v 'h264_mp4toannexb,filter_units=pass_types=7|8' -frames:v 1 -f h264 - \
    | od -An -v -tx1 | tr -d ' \n'
  printf '\n'
  # The hex dump of the audio extradata, without its offsets and ASCII column.
  printf 'audio.config='
  ffprobe -v error -protocol_whitelist file -select_streams a:0 -show_streams -show_data -of default=noprint_wrappers=1 "$f" \
    | awk '/^extradata=/ { on = 1; next } /^extradata_size=/ { on = 0 } on && NF { sub(/^[0-9a-f]+: /, ""); print }' \
    | awk '{ for (i = 1; i <= NF && $i ~ /^[0-9a-f]+$/ ; i++) printf "%s", $i } END { printf "\n" }'
}

# Compare the fingerprint of every file against the first. Exits 1 on a
# mismatch, showing which fields differ.
verify_files() {
  local ref_file="" ref="" cur f status=0
  [ "$#" -gt 0 ] || die "nothing to verify"
  for f in "$@"; do
    cur=$(fingerprint "$f")
    case "$cur" in
      *'video.sps_pps='[0-9a-f]*) ;;
      *) die "$f: no H.264 SPS/PPS found" ;;
    esac
    case "$cur" in
      *'audio.codec_name=aac'*) ;;
      *) die "$f: no AAC audio stream" ;;
    esac
    if [ -z "$ref_file" ]; then
      ref_file=$f
      ref=$cur
    elif [ "$cur" != "$ref" ]; then
      echo "MISMATCH: $f differs from $ref_file" >&2
      diff <(printf '%s\n' "$ref") <(printf '%s\n' "$cur") | sed 's/^/  /' >&2 || true
      status=1
    fi
  done
  if [ "$status" -ne 0 ]; then
    echo "tv-prepare: outputs do not share one format; a publisher cannot carry on between them without restarting the stream" >&2
    return 1
  fi
  echo "verified: $# file(s) share one format and identical SPS/PPS and AAC config"
  printf '%s\n' "$ref" | grep -v '\(sps_pps\|audio.config\)=' | sed 's/^/  /'
  printf '%s\n' "$ref" | grep '\(sps_pps\|audio.config\)=' | sed 's/^/  /'
}

list_videos() { # dir -> NUL-separated names, sorted by name, hidden files skipped
  find "$1" -maxdepth 1 -type f ! -name '.*' \
    \( -iname '*.mp4' -o -iname '*.mov' -o -iname '*.mkv' \) -print0 | LC_ALL=C sort -z
}

has_audio() { [ -n "$(ffprobe -v error -select_streams a:0 -show_entries stream=index -of csv=p=0 "$1")" ]; }
has_video() { [ -n "$(ffprobe -v error -select_streams v:0 -show_entries stream=index -of csv=p=0 "$1")" ]; }

encode_one() { # src tmp_out
  local src=$1 out=$2 gop vb maxrate bufsize vf
  gop=$(fps_gop "$TV_FPS") || die "TV_FPS=$TV_FPS: expected a number or N/D"
  vb=$(to_kbit "$TV_VIDEO_BITRATE") || die "TV_VIDEO_BITRATE=$TV_VIDEO_BITRATE: expected like 4500k or 4.5M"
  maxrate=$(to_kbit "${TV_MAXRATE:-$(( vb * 10 / 9 ))k}") || die "TV_MAXRATE: expected like 5M"
  bufsize=$(to_kbit "${TV_BUFSIZE:-$(( maxrate * 2 ))k}") || die "TV_BUFSIZE: expected like 10M"

  # Anamorphic sources are widened to square pixels first, then scaled to fit
  # and padded to the channel's frame. Colour is converted to BT.709 limited
  # range and tagged, so the VUI is the same in every file.
  vf="scale=w=trunc(iw*sar/2)*2:h=ih,setsar=1"
  vf="$vf,scale=${TV_WIDTH}:${TV_HEIGHT}:force_original_aspect_ratio=decrease:force_divisible_by=2:flags=bicubic:out_color_matrix=bt709:out_range=tv"
  vf="$vf,pad=${TV_WIDTH}:${TV_HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1"
  vf="$vf,setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709"
  vf="$vf,fps=${TV_FPS}:start_time=0,format=yuv420p"

  local -a venc=() ain=() amap=()
  case "$TV_ENCODER" in
    libx264)
      venc=(-c:v libx264 -preset "$TV_PRESET" -profile:v high -level:v 4.1
        -b:v "${vb}k" -maxrate "${maxrate}k" -bufsize "${bufsize}k"
        -g "$gop" -keyint_min "$gop" -sc_threshold 0
        -x264-params "ref=3:bframes=2:b-pyramid=none:open-gop=0:cabac=1:8x8dct=1:weightp=1:nal-hrd=none:aud=0:repeat-headers=0")
      ;;
    videotoolbox)
      venc=(-c:v h264_videotoolbox -profile:v high -level:v 4.1
        -b:v "${vb}k" -maxrate "${maxrate}k" -g "$gop" -allow_sw 0)
      ;;
    *) die "TV_ENCODER=$TV_ENCODER: libx264 or videotoolbox" ;;
  esac

  # A source without sound gets silence, so every output has the same streams.
  if has_audio "$src"; then
    amap=(-map 0:a:0)
  else
    ain=(-f lavfi -i "anullsrc=r=${TV_AUDIO_RATE}:cl=stereo")
    amap=(-map 1:a:0 -shortest)
  fi

  ffmpeg -hide_banner -nostdin -loglevel error -stats -y \
    -i "$src" ${ain[@]+"${ain[@]}"} \
    -map 0:v:0 "${amap[@]}" -map_metadata -1 -map_chapters -1 -sn -dn \
    -vf "$vf" -fps_mode cfr -r "$TV_FPS" "${venc[@]}" \
    -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
    -af "aresample=${TV_AUDIO_RATE}:async=1:first_pts=0" \
    -c:a aac -profile:a aac_low -b:a "$TV_AUDIO_BITRATE" -ar "$TV_AUDIO_RATE" -ac 2 \
    -movflags +faststart -f mp4 "$out"
}

main() {
  local force=0 verify_only=0
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --force) force=1; shift ;;
      --verify) verify_only=1; shift ;;
      -h|--help) usage 0 ;;
      --) shift; break ;;
      -*) echo "tv-prepare: unknown option $1" >&2; usage ;;
      *) break ;;
    esac
  done

  for tool in ffmpeg ffprobe awk od find sort; do
    command -v "$tool" > /dev/null || die "missing $tool"
  done
  for dimension in "$TV_WIDTH" "$TV_HEIGHT"; do
    [[ "$dimension" =~ ^[1-9][0-9]{0,3}$ ]] || die "width and height must be positive even integers"
    (( dimension >= 2 && dimension <= 8192 && dimension % 2 == 0 )) || die "width and height must be even and between 2 and 8192"
  done
  fps_gop "$TV_FPS" > /dev/null || die "TV_FPS: expected a frame rate between 1 and 120"

  if [ "$verify_only" = 1 ]; then
    [ "$#" -eq 1 ] || usage
    [ -d "$1" ] || die "$1 is not a directory"
    local -a files=()
    local f
    while IFS= read -r -d '' f; do files+=("$f"); done < <(list_videos "$1")
    [ "${#files[@]}" -gt 0 ] || die "no mp4/mov/mkv files in $1"
    verify_files "${files[@]}"
    return
  fi

  [ "$#" -eq 2 ] || usage
  local src_dir=$1 out_dir=$2
  [ -d "$src_dir" ] || die "$src_dir is not a directory"
  mkdir -p "$out_dir"
  # Refuse an output folder that is the source folder: nothing may be touched.
  [ "$(cd "$src_dir" && pwd -P)" != "$(cd "$out_dir" && pwd -P)" ] || die "SRC_DIR and OUT_DIR must differ"

  local -a sources=() outputs=()
  local src name out seen=$'\n'
  while IFS= read -r -d '' src; do
    name=$(basename "$src")
    out="$out_dir/${name%.*}.mp4"
    case "$seen" in
      *$'\n'"$out"$'\n'*) die "two sources would write $out; rename one" ;;
    esac
    seen="$seen$out"$'\n'
    sources+=("$src")
    outputs+=("$out")
  done < <(list_videos "$src_dir")
  [ "${#sources[@]}" -gt 0 ] || die "no mp4/mov/mkv files in $src_dir"

  local i tmp done_n=0 skipped=0 stale=0 secs
  for i in "${!sources[@]}"; do
    src=${sources[$i]}
    out=${outputs[$i]}
    if [ -L "$out" ] || { [ -e "$out" ] && [ ! -f "$out" ]; }; then
      die "$out is not a regular output file"
    fi
    if [ -e "$out" ] && [ "$force" = 0 ]; then
      # macOS Bash 3 compares -nt at whole-second precision; find retains the
      # filesystem's subsecond timestamps for short clips prepared together.
      if [ -n "$(find "$out" -newer "$src" -print)" ]; then
        echo "skip   $(basename "$out") (present, newer than its source)"
        skipped=$((skipped + 1))
      else
        echo "STALE  $(basename "$out") is older than its source; left alone (use --force to redo)" >&2
        stale=$((stale + 1))
      fi
      continue
    fi
    has_video "$src" || die "$src: no video stream"
    echo "encode $(basename "$src")"
    tmp="$out_dir/.$(basename "$out").tmp.$$.mp4"
    trap 'rm -f "$tmp"' EXIT
    if ! encode_one "$src" "$tmp"; then
      rm -f "$tmp"
      die "ffmpeg failed on $src"
    fi
    if [ "$force" = 1 ]; then
      mv -f "$tmp" "$out"
    else
      mv -n "$tmp" "$out"
      [ ! -e "$tmp" ] || die "$out appeared during encoding; left alone"
    fi
    trap - EXIT
    secs=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$out")
    echo "done   $(basename "$out"): $(fmt_duration "$secs"), $(fmt_size "$(file_size "$out")")"
    done_n=$((done_n + 1))
  done
  echo "prepared $done_n, skipped $skipped, stale $stale"

  # The channel is the set of outputs that belong to the current sources.
  local -a present=()
  for out in "${outputs[@]}"; do [ -e "$out" ] && present+=("$out"); done
  [ "${#present[@]}" -gt 0 ] || die "no outputs to verify"
  verify_files "${present[@]}"
}

# Sourced by tests, run as a script otherwise.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
