#!/usr/bin/env bash
# Build only the pure realtime scheduler, reusing this checkout's target/.
set -euo pipefail
cd "$(dirname "$0")/.."

expected="wasm-bindgen 0.2.129"
if [[ "$(wasm-bindgen --version)" != "$expected" ]]; then
  echo "Install the exact wasm-bindgen-cli 0.2.129 before building." >&2
  exit 1
fi
cargo build --locked --release -p sapflow-realtime-wasm --target wasm32-unknown-unknown
mkdir -p target/realtime-browser
wasm-bindgen target/wasm32-unknown-unknown/release/sapflow_realtime.wasm \
  --target web --out-dir target/realtime-browser --out-name sapflow_realtime
cp crates/sapflow-realtime-wasm/browser.mjs target/realtime-browser/browser.mjs
cp LICENSE target/realtime-browser/LICENSE
python3 scripts/third-party-licences.py --package sapflow-realtime-wasm \
  --output target/realtime-browser/THIRD_PARTY_LICENSES.txt
echo "Prepared target/realtime-browser (local build, not published)."
