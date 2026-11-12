#!/usr/bin/env bash
# One scripts/ingest-capacity.py scenario against sapflow built for Linux:
#
#   scripts/ingest-capacity-linux.sh SCRATCH LABEL [ingest-capacity.py options] -- [publisher options]
#
# e.g. ... /tmp/cap 1080p --mem 1g -- --profile hd1080 --srt 16 --rtmp 16 --step 5 --hold 180 --reconnect
#
# SCRATCH (outside the repository) holds the Linux build (SCRATCH/target,
# its own, since a Linux build cannot share the host's target directory),
# cargo's registry cache, the pinned FFmpeg, the generated clips and the
# results (SCRATCH/out/LABEL). The image and FFmpeg are scripts/whip-cost-linux.sh's.
# Needs Docker on an arm64 or x86_64 host and network access for the image,
# crates and FFmpeg. Set RMEM_MAX (bytes) to raise net.core.rmem_max in
# Docker's VM for the run, as on a host tuned for many SRT publishers; it is
# put back afterwards.
set -euo pipefail
cd "$(dirname "$0")/.."
scratch=${1:?usage: scripts/ingest-capacity-linux.sh SCRATCH LABEL [options] -- [publisher options]}
label=${2:?usage: scripts/ingest-capacity-linux.sh SCRATCH LABEL [options] -- [publisher options]}
shift 2
mkdir -p "$scratch"/target "$scratch"/cargo "$scratch"/clips "$scratch"/out
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

docker build -q -t sapflow-cost-linux:local - <<'DOCKER'
FROM rust:1.94.1-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends srt-tools procps python3 \
 && rm -rf /var/lib/apt/lists/*
DOCKER
docker run --rm -v "$PWD":/src:ro -v "$scratch":/work \
  -v "$scratch/cargo":/usr/local/cargo/registry -e CARGO_TARGET_DIR=/work/target \
  -w /src sapflow-cost-linux:local cargo build --locked --release -p sapflow

sysctl=(docker run --rm --privileged --net=host sapflow-cost-linux:local sysctl)
if [ -n "${RMEM_MAX:-}" ]; then
  was=$("${sysctl[@]}" -n net.core.rmem_max)
  trap '"${sysctl[@]}" -w net.core.rmem_max="$was" >/dev/null' EXIT
  "${sysctl[@]}" -w net.core.rmem_max="$RMEM_MAX" >/dev/null
fi
python3 -B scripts/ingest-capacity.py --scratch "$scratch" --label "$label" \
  --bin "$scratch/target/release/sapflow" --image sapflow-cost-linux:local "$@"
