#!/bin/bash
# Pure production AAC admission/accounting tests, no codec/media/device runtime.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
developer="$(xcode-select -p)"
frameworks="$developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
testlibs="$developer/Platforms/MacOSX.platform/Developer/usr/lib"
scratch="$(mktemp -d "$root/target/sapflow-audio-generations-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d' "$root/mobile/ios/Tests/AudioGenerationTimingTests.swift" > "$scratch/main.swift"
cat >> "$scratch/main.swift" <<'SWIFT'
let suite = AudioGenerationTimingTests.defaultTestSuite
suite.run()
guard let result = suite.testRun, result.executionCount == 27, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$root/target/ios-module-cache" \
  -F "$frameworks" -I "$testlibs" -L "$testlibs" \
  -Xlinker -rpath -Xlinker "$frameworks" -Xlinker -rpath -Xlinker "$testlibs" \
  "$root/mobile/ios/Sources/AudioRecordingTiming.swift" "$scratch/main.swift" -o "$scratch/tests"
"$scratch/tests"
