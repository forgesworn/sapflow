#!/bin/bash
# Real packaged media/auth APIs; generated cold control fixtures only.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-creator-owner-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
mkdir -p "$sapflow_scratch/Sources/SapflowBroadcaster" "$sapflow_scratch/Tests/CreatorOwnerTests"
for sapflow_source in CaptureAdmission EncodedPipeline EncodedWorkBudget DestinationProfile SrtDestination SrtPublishingLimits NativeDeadlineTimer ReconnectController ReconnectingPublisher ManualSrtPublisher V4vContracts V4vSessionEngine V4vJournal V4vControlClient V4vLoginContracts V4vSignetLoginOwner V4vNativeSignet V4vCreatorContracts OriginObservationPolicy OriginMediaHTTP OriginMediaDecoder V4vOriginObserver V4vObservationSlot OriginSDKConfirmation V4vObservedPublisher V4vCreatorOwner; do
  cp "$sapflow_root/mobile/ios/Sources/$sapflow_source.swift" "$sapflow_scratch/Sources/SapflowBroadcaster/"
done
sed '/^final class V4vTypedContractsTests/,$d' "$sapflow_root/mobile/ios/Tests/V4vSessionEngineTests.swift" > "$sapflow_scratch/Tests/CreatorOwnerTests/Fixtures.swift"
cp "$sapflow_root/mobile/ios/Tests/V4vCreatorOwnerTests.swift" "$sapflow_scratch/Tests/CreatorOwnerTests/"
python3 - "$sapflow_root" "$sapflow_scratch" <<'PY'
import hashlib,json,sys
from pathlib import Path
root,scratch=map(Path,sys.argv[1:])
package='''// swift-tools-version: 5.9
import PackageDescription
let package = Package(name: "CreatorOwnerCheck", platforms: [.macOS(.v12)], dependencies: [.package(name: "Sapflow", path: MEDIA), .package(name: "SignetAuth", path: AUTH)], targets: [.target(name: "SapflowBroadcaster", dependencies: [.product(name: "Sapflow", package: "Sapflow"), .product(name: "SignetAuth", package: "SignetAuth")], swiftSettings: [.unsafeFlags(["-warnings-as-errors"])]), .testTarget(name: "CreatorOwnerTests", dependencies: ["SapflowBroadcaster", .product(name: "Sapflow", package: "Sapflow")])])
'''.replace('MEDIA',json.dumps(str(root/'target/native-packages/apple'))).replace('AUTH',json.dumps(str(root/'target/native-packages/signet-apple')))
(scratch/'Package.swift').write_text(package)
inputs={str(p.relative_to(scratch)):hashlib.sha256(p.read_bytes()).hexdigest() for directory in ['Sources','Tests'] for p in sorted((scratch/directory).rglob('*.swift'))}
packages={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for directory in ['apple','signet-apple'] for p in sorted((root/'target/native-packages'/directory).rglob('*')) if p.is_file()}
print('CREATOR_OWNER_BUILD_INPUTS '+json.dumps({'copiedSwift':inputs,'packages':packages},sort_keys=True),flush=True)
PY
swift test --package-path "$sapflow_scratch" --scratch-path "$sapflow_scratch/build" --jobs 2
