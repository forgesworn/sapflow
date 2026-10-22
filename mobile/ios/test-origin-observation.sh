#!/bin/bash
# Actual host policy/pipeline tests; generated native-decode integration is separate.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-origin-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d; /^final class V4vTypedContractsTests/,$d' "$sapflow_root/mobile/ios/Tests/V4vSessionEngineTests.swift" > "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/OriginObservationTests.swift" >> "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Native origin observation")
suite.addTest(OriginObservationPolicyTests.defaultTestSuite)
suite.addTest(OriginObservationPipelineTests.defaultTestSuite)
suite.addTest(OriginObservationSlotTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 39, result.hasSucceeded else { exit(1) }
SWIFT
sapflow_sources=(SrtDestination SrtPublishingLimits V4vContracts V4vSessionEngine V4vControlClient V4vLoginContracts OriginObservationPolicy OriginMediaHTTP OriginMediaDecoder V4vOriginObserver V4vObservationSlot)
sapflow_files=()
for sapflow_source in "${sapflow_sources[@]}"; do sapflow_files+=("$sapflow_root/mobile/ios/Sources/$sapflow_source.swift"); done
swiftc -swift-version 5 -warnings-as-errors -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
  "${sapflow_files[@]}" "$sapflow_scratch/main.swift" -o "$sapflow_scratch/origin-tests"
"$sapflow_scratch/origin-tests"
