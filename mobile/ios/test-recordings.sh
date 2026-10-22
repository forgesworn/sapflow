#!/bin/bash
# Generated-byte filesystem invariants only; no simulator, media or Keychain.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
mkdir -p "$sapflow_root/target"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-recordings-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
mkdir "$sapflow_scratch/tmp"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/RecordingCatalogueTests.swift" > "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/RecordingStorageTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/AudioRecordingTimingTests.swift" >> "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Private recording filesystem lifecycle")
suite.addTest(RecordingCatalogueTests.defaultTestSuite)
suite.addTest(RecordingStorageTests.defaultTestSuite)
suite.addTest(AudioRecordingTimingTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 48, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$sapflow_scratch/modules" \
  -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
  "$sapflow_root/mobile/ios/Sources/CaptureOrientation.swift" \
  "$sapflow_root/mobile/ios/Sources/CaptureProfile.swift" \
  "$sapflow_root/mobile/ios/Sources/AudioRecordingTiming.swift" \
  "$sapflow_root/mobile/ios/Sources/RecordingStorage.swift" \
  "$sapflow_root/mobile/ios/Sources/RecordingCatalogue.swift" "$sapflow_scratch/main.swift" -o "$sapflow_scratch/recording-tests"
TMPDIR="$sapflow_scratch/tmp" "$sapflow_scratch/recording-tests"
