#!/bin/bash
# Native callback policy/slate memory only. No VT codec, capture or media files.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
developer="$(xcode-select -p)"
frameworks="$developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
testlibs="$developer/Platforms/MacOSX.platform/Developer/usr/lib"
scratch="$(mktemp -d "$root/target/sapflow-video-encoding-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
cat > "$scratch/Failure.swift" <<'SWIFT'
import Foundation
struct CaptureFailure: Error { let message: String }
SWIFT
sed '/@testable import SapflowBroadcaster/d' "$root/mobile/ios/Tests/VideoEncodingPolicyTests.swift" > "$scratch/main.swift"
cat >> "$scratch/main.swift" <<'SWIFT'
let suite = VideoEncodingPolicyTests.defaultTestSuite
suite.run()
guard let result = suite.testRun, result.executionCount == 20, result.hasSucceeded else { exit(1) }
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$root/target/ios-module-cache" \
  -F "$frameworks" -I "$testlibs" -L "$testlibs" \
  -Xlinker -rpath -Xlinker "$frameworks" -Xlinker -rpath -Xlinker "$testlibs" \
  "$root/mobile/ios/Sources/VideoEncodingPolicy.swift" "$root/mobile/ios/Sources/PreparedVideoSlate.swift" \
  "$scratch/Failure.swift" "$scratch/main.swift" -o "$scratch/tests"
"$scratch/tests"
