// Generated-media regression driver for the ACTUAL production decoder.
// It never accepts a phone/container/recording API or reads decoded bytes.
import Foundation
import CoreMedia
import Darwin

private func pcmControls() throws {
    func accept(_ progress: inout OriginPCMProgress, _ frames: Int, _ start: Int64 = 0, bytes: Int? = nil, last: Int64? = nil, duration: Int64 = 1) throws {
        try progress.accept(frames: frames, bytes: bytes ?? frames * 2, bytesPerFrame: 2, rate: 48000,
                            first: CMTime(value: start, timescale: 48000), last: CMTime(value: last ?? start + Int64(frames) - 1, timescale: 48000),
                            firstDuration: CMTime(value: duration, timescale: 48000), lastDuration: CMTime(value: duration, timescale: 48000))
    }
    var progress = OriginPCMProgress(); try accept(&progress, 1024); precondition(progress.frames == 1024)
    try accept(&progress, 1024, 1024); precondition(progress.frames == 2048 && progress.outputs == 2)
    var refused = false; do { try accept(&progress, 1024, 1024) } catch { refused = true }; precondition(refused)
    var coalesced = OriginPCMProgress(); try accept(&coalesced, 7104); precondition(coalesced.frames == 7104 && coalesced.outputs == 1)
    for variant in 0..<4 {
        var test = OriginPCMProgress(); refused = false
        do { try accept(&test, variant == 0 ? 0 : 2048, bytes: variant == 1 ? 2 : nil, last: variant == 2 ? 4095 : nil, duration: variant == 3 ? 2 : 1) } catch { refused = true }
        precondition(refused)
    }
}

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "--check-pcm-timing" {
    try pcmControls(); print("Production PCM boundary controls passed"); exit(0)
}
guard CommandLine.arguments.count == 3, CommandLine.arguments[1] == "--synthetic-mp4" else { exit(2) }
let input = URL(fileURLWithPath: CommandLine.arguments[2])
let properties = try input.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
guard properties.isRegularFile == true, properties.isSymbolicLink != true, let bytes = properties.fileSize, (1...OriginMediaBody.totalLimit).contains(bytes) else { exit(2) }
let source = try Data(contentsOf: input)
let decoder = OriginMediaDecoder()
let started = DispatchTime.now().uptimeNanoseconds
decoder.decode(source, deadlineNs: started + 3_000_000_000) { result in
    var summary: [String: Any] = ["cleanupComplete": decoder.cleanupComplete, "elapsedMs": Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000]
    switch result {
    case let .success(proof): summary.merge(["ok": true, "status": "pass", "videoOutputs": proof.videoOutputs, "validatedPCMFrames": proof.pcmFrames, "audioPCMOutputs": proof.pcmOutputs, "hardwareVideo": proof.hardwareVideo]) { _, new in new }
    case let .failure(error): summary.merge(["ok": false, "status": error.rawValue, "videoOutputs": 0, "validatedPCMFrames": 0]) { _, new in new }
    }
    let data = try! JSONSerialization.data(withJSONObject: summary, options: [.sortedKeys])
    FileHandle.standardOutput.write(data); FileHandle.standardOutput.write(Data([10]))
    switch result { case .success: exit(0); case .failure: exit(1) }
}
dispatchMain()
