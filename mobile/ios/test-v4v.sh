#!/bin/bash
# Actual host tests of the connector foundation. No app build/device/network.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-v4v-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
for sapflow_test in V4vContractsTests V4vSessionEngineTests V4vAdaptersTests V4vLoginTests V4vCreatorContractsTests; do
  sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/$sapflow_test.swift" >> "$sapflow_scratch/main.swift"
done
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "V4V foundation")
suite.addTest(V4vContractsTests.defaultTestSuite)
suite.addTest(V4vTypedContractsTests.defaultTestSuite)
suite.addTest(V4vSessionEngineTests.defaultTestSuite)
suite.addTest(V4vAdaptersTests.defaultTestSuite)
suite.addTest(V4vLoginTests.defaultTestSuite)
suite.addTest(V4vCreatorContractsTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 66, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$sapflow_scratch/module-cache" -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
  "$sapflow_root/mobile/ios/Sources/V4vContracts.swift" "$sapflow_root/mobile/ios/Sources/SrtDestination.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vSessionEngine.swift" "$sapflow_root/mobile/ios/Sources/V4vJournal.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vControlClient.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vLoginContracts.swift" "$sapflow_root/mobile/ios/Sources/V4vSignetLoginOwner.swift" "$sapflow_root/mobile/ios/Sources/V4vNativeSignet.swift" \
  "$sapflow_root/mobile/ios/Sources/V4vCreatorContracts.swift" \
  "$sapflow_scratch/main.swift" -o "$sapflow_scratch/v4v-tests"
"$sapflow_scratch/v4v-tests"
