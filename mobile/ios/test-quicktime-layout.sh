#!/bin/bash
# Pure sparse-file layout tests; no media or real file-descriptor runtime.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
developer="$(xcode-select -p)"
frameworks="$developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
testlibs="$developer/Platforms/MacOSX.platform/Developer/usr/lib"
scratch="$(mktemp -d "$root/target/sapflow-quicktime-layout-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d' "$root/mobile/ios/Tests/QuickTimeMovieLayoutTests.swift" > "$scratch/main.swift"
cat >> "$scratch/main.swift" <<'SWIFT'
let suite = QuickTimeMovieLayoutTests.defaultTestSuite
suite.run()
guard let result = suite.testRun, result.executionCount == 14, result.hasSucceeded else { exit(1) }
if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--write-fuzz-seeds" {
    typealias Fixture = QuickTimeMovieLayoutTests
    let directory = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
    let ordinary = Data([2]) + Fixture.header("mdat", bytes: 16) + Data(repeating: 0, count: 8) + Fixture.header("moov", bytes: 8)
    try ordinary.write(to: directory.appendingPathComponent("layout-contiguous"))
    let wide: UInt64 = 1 << 42
    for (name, position, total) in [("sparse", wide, wide + 32), ("sparse-end", UInt64.max - 16, UInt64.max)] {
        let movieSize = total - position
        let record0 = Fixture.be(0, 8) + Fixture.header("mdat", bytes: 1)
        let record1 = Fixture.be(8, 8) + Fixture.be(position, 8)
        let record2 = Fixture.be(position, 8) + Fixture.header("moov", bytes: movieSize)
        try (Data([3]) + Fixture.be(total, 8) + record0 + record1 + record2)
            .write(to: directory.appendingPathComponent("layout-" + name))
    }
}
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$root/target/ios-module-cache" \
  -F "$frameworks" -I "$testlibs" -L "$testlibs" \
  -Xlinker -rpath -Xlinker "$frameworks" -Xlinker -rpath -Xlinker "$testlibs" \
  "$root/mobile/ios/Sources/QuickTimeMovieLayout.swift" "$scratch/main.swift" -o "$scratch/tests"
"$scratch/tests" "$@"
