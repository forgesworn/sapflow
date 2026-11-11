#!/usr/bin/env bash
# Build two exact revisions sequentially in this checkout's existing target.
set -euo pipefail
cd "$(dirname "$0")/.."
baseline=${1:?usage: build-http-comparison.sh BASELINE_COMMIT CANDIDATE_COMMIT}
candidate=${2:?candidate commit required}
[[ "$baseline" =~ ^[0-9a-f]{40}$ && "$candidate" =~ ^[0-9a-f]{40}$ ]] || exit 2
test "$(git rev-parse HEAD)" = "$candidate"
git diff --exit-code
git diff --cached --exit-code
git merge-base --is-ancestor "$baseline" "$candidate"
mkdir -p target/performance-qualification
restore_candidate() { git switch --detach "$candidate"; }
trap restore_candidate EXIT
git switch --detach "$baseline"
cargo build --locked --release -p sapflow -j 2
cp target/release/sapflow target/performance-qualification/baseline-sapflow
git switch --detach "$candidate"
trap - EXIT
cargo build --locked --release -p sapflow -j 2
# Keep viewer features from being unified into the measured server build.
cargo build --locked --release -p sapflow-bench -j 2
git diff --exit-code
