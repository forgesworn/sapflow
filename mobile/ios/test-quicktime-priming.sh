#!/bin/bash
# Pure bounded header tests; optional generated header paths contain no media.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
developer="$(xcode-select -p)"
frameworks="$developer/Platforms/MacOSX.platform/Developer/Library/Frameworks"
testlibs="$developer/Platforms/MacOSX.platform/Developer/usr/lib"
scratch="$(mktemp -d "$root/target/sapflow-quicktime-priming-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
sed '/@testable import SapflowBroadcaster/d' "$root/mobile/ios/Tests/QuickTimeAudioPrimingTests.swift" > "$scratch/main.swift"
cat >> "$scratch/main.swift" <<'SWIFT'
let suite = QuickTimeAudioPrimingTests.defaultTestSuite
suite.run()
guard let result = suite.testRun, result.executionCount == 20, result.hasSucceeded else { exit(1) }
if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--write-fuzz-seeds" {
    let directory = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
    let samples = [QuickTimeAudioPrimingTests.header(), QuickTimeAudioPrimingTests.header(version: 1, wide: true),
                   QuickTimeAudioPrimingTests.header(video: false), QuickTimeAudioPrimingTests.header(asc: 0x1190),
                   QuickTimeAudioPrimingTests.header(mediaTime: 2112),
                   QuickTimeAudioPrimingTests.header(empty: 2048), QuickTimeAudioPrimingTests.header(version: 1, empty: 2048)]
    for (index, sample) in samples.enumerated() {
        try (Data([0]) + sample).write(to: directory.appendingPathComponent("generated-\(index)"))
    }
    try Data([0]).write(to: directory.appendingPathComponent("empty"))
    exit(0)
}
// These optional inputs must be payload-free headers from generated fixtures.
// They are a separate source-bound integration gate, never a silent test skip.
func generatedMoov(_ filename: String) throws -> Data {
    let file = URL(fileURLWithPath: filename)
    let attributes = try FileManager.default.attributesOfItem(atPath: file.path)
    guard let size = attributes[.size] as? NSNumber, size.uint64Value <= UInt64(QuickTimeAudioPriming.maximumHeaderBytes) else { exit(1) }
    let bytes = try Data(contentsOf: file, options: .mappedIfSafe)
    guard bytes.count <= QuickTimeAudioPriming.maximumHeaderBytes else { exit(1) }
    var at = 0, found: Data?
    while at < bytes.count {
        guard bytes.count - at >= 8 else { exit(1) }
        let size = bytes[at..<(at + 4)].reduce(0) { ($0 << 8) | Int($1) }
        guard size >= 8, size <= bytes.count - at else { exit(1) }
        let kind = bytes.subdata(in: (at + 4)..<(at + 8))
        guard kind == Data("moov".utf8) || kind == Data("ftyp".utf8) else { exit(1) }
        if kind == Data("moov".utf8) { guard found == nil else { exit(1) }; found = bytes.subdata(in: at..<(at + size)) }
        at += size
    }
    guard let moov = found else { exit(1) }
    return moov
}
for filename in CommandLine.arguments.dropFirst() {
    guard filename.hasSuffix("-before-header.bin") else { exit(1) }
    let moov = try generatedMoov(filename)
    let valid: UInt64 = filename.contains("native-failure") ? 97216 : 96356
    let prepared = try QuickTimeAudioPriming.prepare(moov: moov, facts: .init(audioTrackID: 2, accessUnits: 97, leadingFrames: 2112, validFrames: valid))
    guard prepared.moov.count == moov.count + 54 else { exit(1) }
    let expected = try generatedMoov(filename.replacingOccurrences(of: "-before-header.bin", with: "-after-header.bin"))
    guard prepared.moov == expected else { exit(1) }
    print("Generated native header byte-exact to decoder-proven experiment; valid=\(valid), bytes=\(prepared.moov.count)")
}
SWIFT
swiftc -swift-version 5 -warnings-as-errors -module-cache-path "$root/target/ios-module-cache" \
  -F "$frameworks" -I "$testlibs" -L "$testlibs" \
  -Xlinker -rpath -Xlinker "$frameworks" -Xlinker -rpath -Xlinker "$testlibs" \
  "$root/mobile/ios/Sources/QuickTimeAudioPriming.swift" "$scratch/main.swift" -o "$scratch/tests"
"$scratch/tests" "$@"
