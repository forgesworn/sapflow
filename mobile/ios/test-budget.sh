#!/bin/bash
# Run the actual pure Swift budget/profile XCTest cases without a simulator.
set -euo pipefail
sapflow_root="$(cd "$(dirname "$0")/../.." && pwd)"
sapflow_developer="$(xcode-select -p)"
sapflow_frameworks="$sapflow_developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
sapflow_testlibs="$sapflow_developer/Platforms/MacOSX.platform/Developer/usr/lib"
mkdir -p "$sapflow_root/target"
sapflow_scratch="$(mktemp -d "$sapflow_root/target/sapflow-ios-budget-XXXXXX")"
trap 'rm -rf "$sapflow_scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/EncodedWorkBudgetTests.swift" > "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/DestinationProfileTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/NativeDeadlineTimerTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/SrtDestinationTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/CaptureOrientationTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/CaptureProfileTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/ReconnectControllerTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/CaptureAdmissionTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/PublishingTelemetryTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/BroadcasterDestinationSelectionTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/RecordingCaptureAdmissionTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/AudioInputPolicyTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/AudioInputRoutingTests.swift" >> "$sapflow_scratch/main.swift"
sed '/@testable import SapflowBroadcaster/d' "$sapflow_root/mobile/ios/Tests/AudioShowLifecycleTests.swift" >> "$sapflow_scratch/main.swift"
cat >> "$sapflow_scratch/main.swift" <<'SWIFT'
let suite = XCTestSuite(name: "Native broadcaster invariants")
suite.addTest(EncodedWorkBudgetTests.defaultTestSuite)
suite.addTest(DestinationProfileTests.defaultTestSuite)
suite.addTest(NativeDeadlineTimerTests.defaultTestSuite)
suite.addTest(SrtDestinationTests.defaultTestSuite)
suite.addTest(CaptureOrientationTests.defaultTestSuite)
suite.addTest(CaptureProfileTests.defaultTestSuite)
suite.addTest(ReconnectControllerTests.defaultTestSuite)
suite.addTest(CaptureAdmissionTests.defaultTestSuite)
suite.addTest(RecordingCaptureAdmissionTests.defaultTestSuite)
suite.addTest(PublishingTelemetryTests.defaultTestSuite)
suite.addTest(BroadcasterDestinationSelectionTests.defaultTestSuite)
suite.addTest(AudioInputPolicyTests.defaultTestSuite)
suite.addTest(AudioInputRoutingTests.defaultTestSuite)
suite.addTest(AudioShowLifecycleTests.defaultTestSuite)
suite.run()
guard let result = suite.testRun, result.executionCount == 166, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$sapflow_scratch/modules" -F "$sapflow_frameworks" -I "$sapflow_testlibs" -L "$sapflow_testlibs" \
  -Xlinker -rpath -Xlinker "$sapflow_frameworks" \
  -Xlinker -rpath -Xlinker "$sapflow_testlibs" \
  "$sapflow_root/mobile/ios/Sources/AudioShowLifecycle.swift" "$sapflow_root/mobile/ios/Sources/AudioShowControls.swift" "$sapflow_root/mobile/ios/Sources/PublishingTelemetry.swift" "$sapflow_root/mobile/ios/Sources/EncodedWorkBudget.swift" "$sapflow_root/mobile/ios/Sources/DestinationProfile.swift" "$sapflow_root/mobile/ios/Sources/SrtDestination.swift" "$sapflow_root/mobile/ios/Sources/NativeDeadlineTimer.swift" "$sapflow_root/mobile/ios/Sources/CaptureOrientation.swift" "$sapflow_root/mobile/ios/Sources/CaptureProfile.swift" "$sapflow_root/mobile/ios/Sources/ReconnectController.swift" "$sapflow_root/mobile/ios/Sources/AudioInputPolicy.swift" "$sapflow_root/mobile/ios/Sources/AudioInputRouting.swift" "$sapflow_root/mobile/ios/Sources/CaptureAdmission.swift" "$sapflow_root/mobile/ios/Sources/RecordingCaptureAdmission.swift" "$sapflow_root/mobile/ios/Sources/RecordingStorage.swift" "$sapflow_root/mobile/ios/Sources/BroadcasterDestinationSelection.swift" "$sapflow_scratch/main.swift" \
  -o "$sapflow_scratch/budget-tests"
"$sapflow_scratch/budget-tests"
