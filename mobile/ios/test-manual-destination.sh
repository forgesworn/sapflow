#!/bin/bash
# Generated local JSON/file fixtures only; no camera, Keychain writes or network.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-manual-destination-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
export SAPFLOW_MANUAL_LINK_VECTORS="$sapflow_root/tests/fixtures/manual-destination-link-v1.json"
export SAPFLOW_MANUAL_PROFILE_VECTORS="$sapflow_root/tests/fixtures/manual-destination-v1.json"
export CLANG_MODULE_CACHE_PATH="$sapflow_root/target/ios-module-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$CLANG_MODULE_CACHE_PATH"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/ManualDestinationImportTests.swift" > "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/ManualDestinationEnvelopeTests.swift" >> "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Manual destination import")
suite.addTest(ManualDestinationImportTests.defaultTestSuite)
suite.addTest(ManualDestinationEnvelopeTests.defaultTestSuite)
let observer = XCTestObservationCenter.shared
suite.run()
if suite.testRun?.hasSucceeded != true { exit(1) }
SWIFT
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
swiftc -swift-version 5 -warnings-as-errors -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" -o "$sapflow_scratch/check" \
  "$sapflow_root/mobile/ios/Sources/DestinationProfile.swift" \
  "$sapflow_root/mobile/ios/Sources/SrtDestination.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vContracts.swift" \
  "$sapflow_root/mobile/ios/Sources/ManualDestinationImport.swift" \
  "$sapflow_root/mobile/ios/Sources/ManualDestinationEnvelope.swift" "$sapflow_scratch/main.swift"
"$sapflow_scratch/check"
