#!/bin/bash
# Compile the production NWConnection/FFI adapter against the real Apple package.
# The caller owns its receiver and provides endpoint/key through the environment.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
: "${SAPFLOW_CHECK_ENDPOINT:?Set a manual RTMP(S) application URL}"
if [ "${SAPFLOW_CHECK_TRANSPORT:-rtmp}" != "srt" ]; then
  : "${SAPFLOW_CHECK_STREAM_KEY:?Set a test-only stream key}"
fi
if [ "$#" -ne 1 ]; then echo 'Usage: check-publisher.sh <H264-AAC-fixture.mp4>' >&2; exit 2; fi
sapflow_fixture="$1"
mkdir -p "$sapflow_root/target"
if [ -n "${SAPFLOW_CHECK_WORKSPACE:-}" ]; then
  sapflow_scratch="$SAPFLOW_CHECK_WORKSPACE"
  if [ -e "$sapflow_scratch" ] && [ ! -f "$sapflow_scratch/.sapflow-publisher-check" ]; then
    echo 'Refusing an unowned reusable check workspace' >&2; exit 2
  fi
  mkdir -p "$sapflow_scratch"
  touch "$sapflow_scratch/.sapflow-publisher-check"
  # The caller owns cleanup of an explicitly retained workspace.
else
  sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-publisher-XXXXXX")"
  trap 'rm -rf "$sapflow_scratch"' EXIT
fi
mkdir -p "$sapflow_scratch/Sources/PublisherCheck"
for sapflow_source in EncodedPipeline EncodedWorkBudget DestinationProfile SrtDestination NativeDeadlineTimer ReconnectController ManualRtmpPublisher ManualSrtPublisher; do
  cp "$sapflow_root/mobile/ios/Sources/$sapflow_source.swift" "$sapflow_scratch/Sources/PublisherCheck/"
done
cp "$sapflow_root/mobile/ios/Tools/PublisherFixtureCheck.swift" "$sapflow_scratch/Sources/PublisherCheck/main.swift"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
from pathlib import Path
import json,sys
root=Path(sys.argv[1]); destination=Path(sys.argv[2])
package='''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "PublisherCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE_PATH)], targets: [.executableTarget(name: "PublisherCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")])])
'''.replace('PACKAGE_PATH',json.dumps(str(root/'target/native-packages/apple')))
(destination/'Package.swift').write_text(package)
PY
swift build --package-path "$sapflow_scratch" --scratch-path "$sapflow_scratch/build" --jobs 2
if [ "${SAPFLOW_CHECK_ONLY_BUILD:-0}" = "1" ]; then exit 0; fi
"$sapflow_scratch/build/debug/PublisherCheck" "$sapflow_fixture"
