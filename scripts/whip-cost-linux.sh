#!/usr/bin/env bash
# A4.2 in a Linux container: scripts/whip-cost.py (SRT, RTMP and WHIP
# publishers into one sapflow, benchmarks.md section B) with sapflow built
# for Linux, so the measurement is free of macOS's process accounting and of
# the host's other work outside the container's own load gate.
#
#   scripts/whip-cost-linux.sh SCRATCH [whip-cost.py options]
#
# SCRATCH (outside the repository) holds everything this makes: the Linux
# build (SCRATCH/target, its own, since a Linux build cannot share the
# host's target directory), cargo's registry cache, the pinned FFmpeg and
# the results (SCRATCH/out). Needs Docker on an arm64 or x86_64 host and
# network access for the image, crates and FFmpeg.
#
# The image (sapflow-cost-linux:local) is Rust 1.94.1 (rust-toolchain.toml)
# on Debian bookworm, with srt-tools, procps and python3 from Debian. FFmpeg
# is the pinned FFmpeg-Builds n8.1.3 static build for the host's
# architecture (the one CI uses for x86_64), checked by SHA-256, since
# Debian's has no WHIP muxer. The container runs with the repository read
# only and is removed when it ends; the image is left for the caller to
# remove (docker image rm sapflow-cost-linux:local).
set -euo pipefail
cd "$(dirname "$0")/.."
scratch=${1:?usage: scripts/whip-cost-linux.sh SCRATCH [whip-cost.py options]}
shift
mkdir -p "$scratch"/target "$scratch"/cargo "$scratch"/out
scratch=$(cd "$scratch" && pwd)

release=autobuild-2026-10-31-12-59
case "$(uname -m)" in
  arm64|aarch64) asset=ffmpeg-n8.1.3-27-g8dca1586f4-linuxarm64-gpl-8.1.tar.xz
    sha=a08c0ee6e1c4e02139344b303002dcc7c37cf125d639f1fbd15dabee40fcb525 ;;
  x86_64) asset=ffmpeg-n8.1.3-27-g8dca1586f4-linux64-gpl-8.1.tar.xz
    sha=41e9c565594c390622cf968d82fa4135d9e9a11d06c1515644288cbc844e3380 ;;
  *) echo "unsupported host $(uname -m)" >&2; exit 1 ;;
esac
if [ ! -x "$scratch/ffmpeg/bin/ffmpeg" ]; then
  curl -fsSL --proto '=https' --retry 3 -o "$scratch/ffmpeg.tar.xz" \
    "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/$release/$asset"
  echo "$sha  $scratch/ffmpeg.tar.xz" | shasum -a 256 -c -
  mkdir -p "$scratch/ffmpeg"
  tar -xJf "$scratch/ffmpeg.tar.xz" -C "$scratch/ffmpeg" --strip-components=1
fi

docker build -q -t sapflow-cost-linux:local - <<'EOF'
FROM rust:1.94.1-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends srt-tools procps python3 \
 && rm -rf /var/lib/apt/lists/*
EOF

run() {
  docker run --rm --name "sapflow-cost-linux-$$" \
    -v "$PWD":/src:ro -v "$scratch":/work \
    -v "$scratch/cargo":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/work/target -e PATH="/work/ffmpeg/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -w /src sapflow-cost-linux:local "$@"
}
run cargo build --locked --release -p sapflow
run sh -c 'ffmpeg -hide_banner -version | sed -n 1p; srt-live-transmit -version 2>&1 | sed -n 1p; nproc; uname -srm'
run python3 -B scripts/whip-cost.py --bin /work/target/release/sapflow --out /work/out "$@"
