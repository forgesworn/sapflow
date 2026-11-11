#!/usr/bin/env bash
# A0.2 asset recipe: stereo Opus recorded by headless Chrome, muxed with a
# 1280x720, 30 fps, libx264 test video (keyframe every 2 s) into
#   av/          fMP4 HLS, 2 s segments, master playlist CODECS "avc1...,opus"
#   audio/       audio-only fMP4 HLS
#   recording/   fragmented MP4 recording
#   av-preskip312/  (a) with the dOps pre-skip patched to 312 (extra)
# Real FFmpeg HLS is not LL-HLS: the assets answer the codec question only.
# Usage (repository root): scripts/whip-w0/make-opus-assets.sh [outdir] [existing.webm]
# Default outdir is ~/whip-w0-assets. Media is never committed.
set -euo pipefail
OUT="${1:-$HOME/whip-w0-assets}"
WEBM="${2:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
RANGE="${W0_PORT_RANGE:-47000-47199}"
SECS=20
mkdir -p "$OUT"/{src,av,audio,recording}

if [ -z "$WEBM" ]; then
  node "$HERE/run.js" record-opus --range "$RANGE" --out "$OUT/src"
  WEBM="$OUT/src/opus-20s.webm"
fi
cp -n "$WEBM" "$OUT/src/opus-20s.webm" 2>/dev/null || true

# One H.264 encode shared by the three outputs, so they differ only in packaging.
VIDEO=(-f lavfi -i "testsrc2=size=1280x720:rate=30:duration=$SECS")
VENC=(-c:v libx264 -profile:v main -level 3.1 -preset veryfast -pix_fmt yuv420p -g 60 -keyint_min 60 -sc_threshold 0 -bf 0 -r 30)
HLS=(-f hls -hls_time 2 -hls_playlist_type vod -hls_segment_type fmp4 -hls_fmp4_init_filename init.mp4 -hls_segment_filename seg_%03d.m4s)

# (a) audio and video, fMP4 HLS, master playlist
(cd "$OUT/av" && ffmpeg -hide_banner -y "${VIDEO[@]}" -i "$OUT/src/opus-20s.webm" -t $SECS -map 0:v -map 1:a "${VENC[@]}" -c:a copy \
  "${HLS[@]}" -master_pl_name master.m3u8 -var_stream_map "v:0,a:0" stream_%v.m3u8 2> ../ffmpeg-av.log)
# (b) audio only, fMP4 HLS
(cd "$OUT/audio" && ffmpeg -hide_banner -y -i "$OUT/src/opus-20s.webm" -t $SECS -vn -c:a copy \
  "${HLS[@]}" -master_pl_name master.m3u8 -var_stream_map "a:0" stream_%v.m3u8 2> ../ffmpeg-audio.log)
# (c) fragmented MP4 recording
ffmpeg -hide_banner -y "${VIDEO[@]}" -i "$OUT/src/opus-20s.webm" -t $SECS -map 0:v -map 1:a "${VENC[@]}" -c:a copy \
  -movflags +frag_keyframe+empty_moov+default_base_moof "$OUT/recording/recording.mp4" 2> "$OUT/ffmpeg-recording.log"

# (d) extra, not asked for in the spec: (a) again, with the dOps pre-skip patched to 312, the value the
# design sets by rule (RTP carries no Opus header). The recorder's own value is whatever (a) shows.
mkdir -p "$OUT/av-preskip312"
cp "$OUT"/av/*.m4s "$OUT"/av/*.m3u8 "$OUT/av-preskip312/"
node -e '
const fs = require("fs"), f = process.argv[1], b = fs.readFileSync(f), i = b.indexOf("dOps");
if (i < 0) throw new Error("no dOps box"); b.writeUInt16BE(312, i + 4 + 2); fs.writeFileSync(process.argv[2], b);
' "$OUT/av/init.mp4" "$OUT/av-preskip312/init.mp4"

# What ffmpeg wrote: dOps fields (pre-skip, channels) and the codec strings.
{
  echo "== ffprobe, init segments and recording (codec, channels, initial_padding = Opus pre-skip, sample_rate)"
  for f in "$OUT/av/init.mp4" "$OUT/audio/init.mp4" "$OUT/recording/recording.mp4"; do
    echo "-- $f"; ffprobe -v error -show_entries stream=codec_name,codec_tag_string,channels,channel_layout,sample_rate,initial_padding,profile -of default=nw=1 "$f"
  done
  echo "== dOps box bytes from the init segment (version, channels, pre-skip BE16, input rate BE32, gain BE16, mapping family)"
  for f in "$OUT/av/init.mp4" "$OUT/audio/init.mp4" "$OUT/av-preskip312/init.mp4"; do
    echo "-- $f"; xxd -p "$f" | tr -d '\n' | grep -o '644f7073.\{26\}' | head -1
  done
  echo "== master playlists"; cat "$OUT/av/master.m3u8"; echo; cat "$OUT/audio/master.m3u8"
  echo "== segments"; ls "$OUT/av" | head -4; ls "$OUT/av" | wc -l
} > "$OUT/assets-info.txt" 2>&1
echo "assets in $OUT; see $OUT/assets-info.txt"
