#!/bin/bash
# Real packaged Rust session plus simulated cold facts. No camera/network/device.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-observed-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
mkdir -p "$sapflow_scratch/Sources/SapflowBroadcaster" "$sapflow_scratch/Tests/NativePublisherTests"
for sapflow_source in EncodedPipeline EncodedWorkBudget DestinationProfile SrtDestination SrtPublishingLimits NativeDeadlineTimer ReconnectController ReconnectingPublisher ManualSrtPublisher V4vContracts V4vSessionEngine V4vControlClient V4vLoginContracts OriginObservationPolicy OriginMediaHTTP OriginMediaDecoder V4vOriginObserver V4vObservationSlot OriginSDKConfirmation V4vObservedPublisher; do
  cp "$sapflow_root/mobile/ios/Sources/$sapflow_source.swift" "$sapflow_scratch/Sources/SapflowBroadcaster/"
done
sed '/^final class V4vTypedContractsTests/,$d' "$sapflow_root/mobile/ios/Tests/V4vSessionEngineTests.swift" > "$sapflow_scratch/Tests/NativePublisherTests/Fixtures.swift"
cp "$sapflow_root/mobile/ios/Tests/OriginObservationTests.swift" "$sapflow_scratch/Tests/NativePublisherTests/OriginObservationTests.swift"
cp "$sapflow_root/mobile/ios/Tests/V4vObservedPublisherTests.swift" "$sapflow_scratch/Tests/NativePublisherTests/V4vObservedPublisherTests.swift"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
import json,sys
from pathlib import Path
root,scratch=map(Path,sys.argv[1:])
package='''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "ObservedPublisherCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: PACKAGE_PATH)], targets: [.target(name: "SapflowBroadcaster", dependencies: [.product(name: "Sapflow", package: "Sapflow")]), .testTarget(name: "NativePublisherTests", dependencies: ["SapflowBroadcaster", .product(name: "Sapflow", package: "Sapflow")])])
'''.replace('PACKAGE_PATH',json.dumps(str(root/'target/native-packages/apple')))
(scratch/'Package.swift').write_text(package)
PY
swift test --package-path "$sapflow_scratch" --scratch-path "$sapflow_scratch/build" --jobs 2
