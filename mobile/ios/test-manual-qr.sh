#!/bin/bash
# Pure generated settings/ownership checks. No camera, Keychain writes or network.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-manual-qr-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
export SAPFLOW_MANUAL_LINK_VECTORS="$sapflow_root/tests/fixtures/manual-destination-link-v1.json"
export SAPFLOW_MANUAL_PROFILE_VECTORS="$sapflow_root/tests/fixtures/manual-destination-v1.json"
export CLANG_MODULE_CACHE_PATH="$sapflow_root/target/ios-module-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$CLANG_MODULE_CACHE_PATH"
for sapflow_test in ManualDestinationImportTests ManualDestinationEnvelopeTests ManualQrPolicyTests RecordingLibraryAdmissionTests; do
    sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/$sapflow_test.swift" >> "$sapflow_scratch/main.swift"
done
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Manual QR settings and exact camera ownership")
suite.addTest(ManualDestinationImportTests.defaultTestSuite)
suite.addTest(ManualDestinationEnvelopeTests.defaultTestSuite)
suite.addTest(ManualQrPolicyTests.defaultTestSuite)
suite.addTest(RecordingLibraryAdmissionTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 66, result.hasSucceeded else { exit(1) }
SWIFT
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
swiftc -swift-version 5 -warnings-as-errors -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" -o "$sapflow_scratch/check" \
  "$sapflow_root/mobile/ios/Sources/DestinationProfile.swift" \
  "$sapflow_root/mobile/ios/Sources/SrtDestination.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vContracts.swift" \
  "$sapflow_root/mobile/ios/Sources/RecordingLibraryAdmission.swift" \
  "$sapflow_root/mobile/ios/Sources/ManualDestinationImport.swift" \
  "$sapflow_root/mobile/ios/Sources/ManualDestinationEnvelope.swift" \
  "$sapflow_root/mobile/ios/Sources/ManualQrPolicy.swift" "$sapflow_scratch/main.swift"
"$sapflow_scratch/check"
