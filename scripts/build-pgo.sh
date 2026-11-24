#!/usr/bin/env bash
# A profile-guided release build of the sapflow server:
#
#   scripts/build-pgo.sh OUT
#
# Builds an instrumented server, trains it on generated media (SRT ingest
# with no viewers, then RTMP ingest with viewers, the two existing paired
# harnesses run with one binary), merges the profiles and builds again with
# them. Writes the optimised binary to OUT. A default `cargo build --release`
# is untouched: the instrumented and optimised builds use their own
# directories under target/.
#
# Needs ffmpeg, srt-live-transmit and the toolchain's llvm-tools component
# (rustup component add llvm-tools). Training runs on loopback only.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:?usage: scripts/build-pgo.sh OUT}
seconds=${PGO_TRAIN_SECONDS:-20}
for bin in ffmpeg srt-live-transmit python3; do
  command -v "$bin" >/dev/null || { echo "missing $bin" >&2; exit 1; }
done
profdata=$(find "$(rustc --print sysroot)" -name llvm-profdata -type f | head -1)
[ -x "$profdata" ] || { echo "missing llvm-profdata: rustup component add llvm-tools" >&2; exit 1; }

work=$PWD/target/pgo
rm -rf "$work"
mkdir -p "$work/raw"
# The viewers for the training run; built first so its features are not
# unified into the server builds.
cargo build --locked --release -p sapflow-bench -j 2

RUSTFLAGS="-Cprofile-generate=$work/raw" \
  cargo build --locked --release -p sapflow -j 2 --target-dir "$work/instrumented"
instrumented=$(find "$work/instrumented" -path '*release/sapflow' -type f | head -1)
[ -x "$instrumented" ] || { echo "no instrumented binary" >&2; exit 1; }

export LLVM_PROFILE_FILE="$work/raw/sapflow-%p-%m.profraw"
revision=$(git rev-parse HEAD)
python3 -B scripts/compare-srt-ingest.py --baseline "$instrumented" --candidate "$instrumented" \
  --publishers 4 --bitrate-kbit 4000 --seconds "$seconds" --pairs 1 --output "$work/train-srt.json"
python3 -B scripts/compare-http-efficiency.py --baseline "$instrumented" --candidate "$instrumented" \
  --bench target/release/sapflow-bench --baseline-source "$revision" --candidate-source "$revision" \
  --pairs 1 --seconds "$seconds" --threads 4 --output "$work/train-http.json"
unset LLVM_PROFILE_FILE
ls "$work"/raw/*.profraw >/dev/null 2>&1 || { echo "training wrote no profile" >&2; exit 1; }
"$profdata" merge -o "$work/merged.profdata" "$work"/raw/*.profraw

RUSTFLAGS="-Cprofile-use=$work/merged.profdata" \
  cargo build --locked --release -p sapflow -j 2 --target-dir "$work/optimised"
optimised=$(find "$work/optimised" -path '*release/sapflow' -type f | head -1)
[ -x "$optimised" ] || { echo "no optimised binary" >&2; exit 1; }
mkdir -p "$(dirname "$out")"
cp "$optimised" "$out"
# The two extra build directories are large; only the result is kept.
rm -rf "$work/instrumented" "$work/optimised" "$work/raw"
echo "profile-guided build: $out"
