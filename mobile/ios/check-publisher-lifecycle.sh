#!/bin/bash
# Bounded no-media loopback check of the actual production UDP cancellation owner.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
mkdir -p "$sapflow_root/target"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-lifecycle-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
mkdir -p "$sapflow_scratch/Sources/PublisherLifecycleCheck"
for sapflow_source in EncodedPipeline EncodedWorkBudget DestinationProfile SrtDestination NativeDeadlineTimer ReconnectController ManualSrtPublisher; do
  cp "$sapflow_root/mobile/ios/Sources/$sapflow_source.swift" "$sapflow_scratch/Sources/PublisherLifecycleCheck/"
done
cp "$sapflow_root/mobile/ios/Tools/PublisherLifecycleCheck.swift" "$sapflow_scratch/Sources/PublisherLifecycleCheck/main.swift"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
from pathlib import Path
import json, sys
root, scratch = map(Path, sys.argv[1:])
package = '''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "PublisherLifecycleCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE_PATH)], targets: [.executableTarget(name: "PublisherLifecycleCheck", dependencies: [.product(name: "Sapflow", package: "Sapflow")])])
'''.replace('PACKAGE_PATH', json.dumps(str(root / 'target/native-packages/apple')))
(scratch / 'Package.swift').write_text(package)
PY
swift build --package-path "$sapflow_scratch" --scratch-path "$sapflow_scratch/build" --jobs 2
"$sapflow_scratch/build/debug/PublisherLifecycleCheck"
