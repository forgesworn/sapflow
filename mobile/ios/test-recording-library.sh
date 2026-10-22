#!/bin/bash
# Generated-file library ownership and destination checks, without a phone.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
mkdir -p "$sapflow_root/target"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-library-check-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
for sapflow_test in RecordingLibraryAdmissionTests RecordingExportDestinationTests RecordingLibraryOwnerTests; do
    sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/$sapflow_test.swift" >> "$sapflow_scratch/main.swift"
done
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Private recording library invariants")
suite.addTest(RecordingLibraryAdmissionTests.defaultTestSuite)
suite.addTest(RecordingExportDestinationTests.defaultTestSuite)
suite.addTest(RecordingLibraryOwnerTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 33, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
    -Xlinker -rpath -Xlinker "$sapflow_frameworks" -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
    "$sapflow_root/mobile/ios/Sources/CaptureOrientation.swift" "$sapflow_root/mobile/ios/Sources/CaptureProfile.swift" \
    "$sapflow_root/mobile/ios/Sources/RecordingStorage.swift" "$sapflow_root/mobile/ios/Sources/RecordingCatalogue.swift" \
    "$sapflow_root/mobile/ios/Sources/RecordingLibraryAdmission.swift" "$sapflow_root/mobile/ios/Sources/RecordingExportDestination.swift" \
    "$sapflow_root/mobile/ios/Sources/RecordingLibraryOwner.swift" "$sapflow_scratch/main.swift" -o "$sapflow_scratch/library-tests"
"$sapflow_scratch/library-tests"
