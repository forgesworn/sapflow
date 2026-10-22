// Generated native VT ownership check. No camera, microphone or input media.
import Foundation
import AVFoundation
import VideoToolbox
import CryptoKit
import Sapflow

private func demand(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !value() { throw CaptureFailure(message: message) }
}
/// Generated immutable fixture storage; only the serial samples owner submits
/// it. No native pixel buffer is transferred as a mutable queue capture.
private final class VideoPicture {
    private let image: CVPixelBuffer
    init(_ image: CVPixelBuffer) { self.image = image }
    func withImage<T>(_ work: (CVPixelBuffer) throws -> T) rethrows -> T { try work(image) }
}
private final class VideoResult<T> {
    private let condition = NSCondition()
    private var result: Result<T, Error>?
    func put(_ value: Result<T, Error>) { condition.lock(); if result == nil { result = value }; condition.broadcast(); condition.unlock() }
    func wait() throws -> T {
        let until = Date().addingTimeInterval(10)
        condition.lock(); defer { condition.unlock() }
        while result == nil { guard condition.wait(until: until) else { throw CaptureFailure(message: "Native VT operation deadline") } }
        return try result!.get()
    }
    var ready: Bool { condition.lock(); defer { condition.unlock() }; return result != nil }
}
private final class VideoLedger {
    struct Row { let unit: CompressedUnit; let token: CaptureAdmission.MediaToken }
    private let lock = NSLock()
    private var rows: [Row] = []
    private var failures: [String] = []
    let held = DispatchSemaphore(value: 0)
    let release = DispatchSemaphore(value: 0)
    func output(_ unit: CompressedUnit, token: CaptureAdmission.MediaToken) {
        lock.lock(); let first = rows.isEmpty
        if rows.count < 16 { rows.append(Row(unit: unit, token: token)) }
        else { failures.append("Native output exceeded fixture bound") }
        lock.unlock()
        if first {
            held.signal()
            if release.wait(timeout: .now() + 8) != .success { fault("Held native output deadline") }
        }
    }
    func fault(_ value: String) { lock.lock(); if failures.count < 16 { failures.append(value) }; lock.unlock() }
    var snapshot: [Row] { lock.lock(); defer { lock.unlock() }; return rows }
    var errors: [String] { lock.lock(); defer { lock.unlock() }; return failures }
}

@main enum StandbyVideoOwnershipCheck {
    static func main() {
        do {
            try demand(CommandLine.arguments.count == 2, "Use generated VT launcher")
            let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
            try demand(root == root.resolvingSymlinksInPath(), "Owned root symlink")
            try demand(try String(contentsOf: root.appendingPathComponent(".sapflow-video-ownership")) == "generated-video-ownership-v1\n", "Owned root marker")
            let result = try run(root: root)
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted]))
            FileHandle.standardOutput.write(Data([10]))
        } catch {
            let facts: [String: Any] = ["passed": false, "generatedOnly": true, "error": String(describing: error),
                "scope": "Host native capability failure or assertion failure; no fallback encoder or physical acceptance"]
            if let bytes = try? JSONSerialization.data(withJSONObject: facts, options: [.sortedKeys, .prettyPrinted]) { FileHandle.standardOutput.write(bytes) }
            exit(1)
        }
    }
    private static func setup(_ gate: CaptureAdmission, standby: Bool = false) throws -> (CaptureAdmission.Preparation, CaptureAdmission.MediaRun) {
        try demand(gate.selectInitialStandby(standby), "Initial privacy selection refused")
        guard let prep = gate.reservePreparation(profile: .audioVideo(.sd480p30)) else { throw CaptureFailure(message: "Generated Prepare refused") }
        try demand(gate.attached(prep) && gate.beginPermissions(prep) && gate.prepared(prep), "Generated Prepare sequence refused")
        guard let start = gate.reserveStart(prep), let run = gate.reserveMediaRun(start) else { throw CaptureFailure(message: "Generated native run refused") }
        try demand(gate.commitMediaRun(run, start: start, nowNs: DispatchTime.now().uptimeNanoseconds, broadcasting: false), "Generated Start refused")
        return (prep, run)
    }
    private static func run(root: URL) throws -> [String: Any] {
        let gate = CaptureAdmission(), ledger = VideoLedger()
        let (prep, run) = try setup(gate)
        guard let raw = gate.mediaToken(run, kind: .raw) else { throw CaptureFailure(message: "Captured raw token missing") }
        let owner = DispatchQueue(label: "generated.vt.samples")
        let encoder = try HardwareVideoEncoder(orientation: .landscapeRight, profile: .sd480p30,
            output: { ledger.output($0, token: $1) },
            fault: { token, error in _ = gate.nativeFault(token ?? raw); ledger.fault(String(describing: error)) },
            permitsInput: { gate.allowsInput($0) == .allow })
        let implementation = encoder.implementation
        let slate = try encoder.prepareSlate()
        try demand(try encoder.prepareSlate() === slate, "Slate storage was allocated again")
        let image = VideoPicture(try makeImage(width: slate.width, height: slate.height))
        let oldSeal = VideoResult<CMTime?>()
        owner.async {
            do {
                for n in 0..<3 { try demand(try image.withImage { try encoder.encode($0, time: CMTime(value: Int64(n), timescale: 30), token: raw) }, "Native raw submission refused") }
                oldSeal.put(.success(try encoder.sealForPrivacy()))
            } catch { oldSeal.put(.failure(error)) }
        }
        defer { ledger.release.signal() }
        try demand(ledger.held.wait(timeout: .now() + 8) == .success, "No actual native output callback to hold")
        guard let request = gate.requestStandby(preparation: prep, nowNs: DispatchTime.now().uptimeNanoseconds),
              let synthetic = gate.mediaToken(run, kind: .slate) else { throw CaptureFailure(message: "Standby did not mint exact native source") }
        try demand(gate.allowsInput(raw) == .superseded && gate.outputAdmission(raw) == .acceptedPrefix, "Old submitted raw lost prefix identity")
        try demand(!oldSeal.ready, "Native seal escaped a held actual output callback")
        ledger.release.signal()
        try demand(try oldSeal.wait() == CMTime(value: 1, timescale: 30), "Native old prefix seal differs from nominal final frame")
        let finish = VideoResult<Bool>()
        owner.async {
            do {
                try demand(try !image.withImage { try encoder.encode($0, time: CMTime(value: 3, timescale: 30), token: raw) }, "Stale raw input reached native encoder")
                for n in 3..<6 { try demand(try encoder.encode(slate, time: CMTime(value: Int64(n), timescale: 30), token: synthetic, forceIdr: n == 3), "Native synthetic submission refused") }
                try encoder.finishChecked()
                try encoder.finishChecked() // Exact actual-close repeat is idempotent.
                finish.put(.success(true))
            } catch { finish.put(.failure(error)) }
        }
        try demand(try finish.wait(), "Native checked finish failed")
        let rows = ledger.snapshot
        try demand(rows.count == 6 && ledger.errors.isEmpty, "Native dropped/relabelled output or signalled a fault")
        let ingress = RustEncodedIngress(); ingress.reset(hasVideo: true)
        var annex = Data(), expectedConfig: (Data, Data)?
        var report: [[String: Any]] = []
        for (n, row) in rows.enumerated() {
            try demand(row.token == (n < 3 ? raw : synthetic), "Original immutable callback token/order changed")
            try demand(row.unit.time == CMTime(value: Int64(n) * 3000, timescale: 90_000) && row.unit.duration == CMTime(value: 3000, timescale: 90_000), "Native original PTS/canvas cadence changed")
            let validated = try ingress.validate(row.unit)
            if n == 3 { try demand(validated.codedIdr, "First actual slate output is not a coded IDR") }
            guard case let .h264(sps, pps) = row.unit.configuration else { throw CaptureFailure(message: "Native H264 configuration missing") }
            if let expectedConfig { try demand(expectedConfig.0 == sps && expectedConfig.1 == pps, "Same-session slate changed SPS/PPS") }
            else { expectedConfig = (sps, pps); annex += Data([0,0,0,1]) + sps + Data([0,0,0,1]) + pps }
            let payload = try payload(row.unit.buffer)
            var at = 0
            while at < payload.count {
                try demand(payload.count - at >= 4, "Native AVCC prefix truncated")
                let size = payload[at..<(at+4)].reduce(0) { ($0 << 8) | Int($1) }; at += 4
                try demand(size > 0 && size <= payload.count - at, "Native AVCC NAL truncated")
                annex += Data([0,0,0,1]); annex += payload[at..<(at+size)]; at += size
            }
            report.append(["index": n, "kind": n < 3 ? "raw" : "slate", "revision": row.token.revision,
                "pts90k": row.unit.time.value, "duration90k": row.unit.duration.value,
                "codedIdr": validated.codedIdr, "payloadBytes": payload.count,
                "payloadSHA256": SHA256.hash(data: payload).map { String(format: "%02x", $0) }.joined()])
        }
        try demand(ingress.accepted == 6 && gate.permitsPrivacy(request, nowNs: DispatchTime.now().uptimeNanoseconds), "Native validation/authority changed during finite proof")
        try annex.write(to: root.appendingPathComponent("generated.h264"), options: .withoutOverwriting)
        _ = gate.stop(preparation: prep); gate.mediaRunClosed(run)
        let negative = try refusesRawClaimingSlate()
        return ["passed": true, "generatedOnly": true, "hardwareImplementation": implementation,
            "sessionsInPositiveShow": 1, "width": slate.width, "height": slate.height,
            "nativeOutput": report, "sameSPSPPS": true, "sameCanvasAndOriginalPTS": true,
            "oldRawKeptOriginalTokenWhileCallbackHeld": true, "staleRawNotSubmitted": true,
            "actualSealWaitedHeldOutput": true, "actualFinishCheckedQuiescent": true,
            "rawSampleCannotClaimSlate": negative,
            "scope": "Generated host-native VT and real Admission/SDK validation only; no camera, runtime driver pacing, transport, iPhone or physical privacy acceptance"]
    }
    private static func refusesRawClaimingSlate() throws -> Bool {
        let gate = CaptureAdmission(), fault = VideoResult<Bool>()
        let (_, run) = try setup(gate, standby: true)
        guard let raw = gate.mediaToken(run, kind: .raw), let slate = gate.mediaToken(run, kind: .slate) else { throw CaptureFailure(message: "Negative source tokens missing") }
        let encoder = try HardwareVideoEncoder(orientation: .landscapeRight, profile: .sd480p30,
            output: { _, _ in fault.put(.failure(CaptureFailure(message: "Spoofed source produced output"))) },
            fault: { token, _ in fault.put(.success(token == slate && gate.nativeFault(token ?? raw))) },
            permitsInput: { gate.allowsInput($0) == .allow })
        defer { encoder.finish() }
        let image = try makeImage(width: 640, height: 480)
        var format: CMVideoFormatDescription?, sample: CMSampleBuffer?
        try demand(CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image, formatDescriptionOut: &format) == noErr, "Generated raw format failed")
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 30), presentationTimeStamp: .zero, decodeTimeStamp: .invalid)
        try demand(CMSampleBufferCreateReadyWithImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image, formatDescription: format!, sampleTiming: &timing, sampleBufferOut: &sample) == noErr, "Generated raw sample failed")
        var refused = false
        do { _ = try encoder.encode(sample!, time: .zero, token: slate) } catch { refused = true }
        try demand(refused && (try fault.wait()) && gate.mediaHasFault(run), "Raw source spoof was not synchronously exact-token faulted")
        var finishFailed = false
        do { try encoder.finishChecked() } catch { finishFailed = true }
        try demand(finishFailed, "Native source spoof fault was not sticky through actual cleanup")
        return true
    }
    private static func payload(_ sample: CMSampleBuffer) throws -> Data {
        guard let block = CMSampleBufferGetDataBuffer(sample) else { throw CaptureFailure(message: "Native packet missing") }
        let count = CMBlockBufferGetDataLength(block)
        var data = Data(count: count)
        try demand(data.withUnsafeMutableBytes { CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: count, destination: $0.baseAddress!) } == noErr, "Native packet read failed")
        return data
    }
    private static func makeImage(width: Int, height: Int) throws -> CVPixelBuffer {
        var image: CVPixelBuffer?
        try demand(CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &image) == kCVReturnSuccess, "Generated picture unavailable")
        guard let image else { throw CaptureFailure(message: "Generated picture missing") }
        try demand(CVPixelBufferLockBaseAddress(image, []) == kCVReturnSuccess, "Generated picture lock failed")
        defer { CVPixelBufferUnlockBaseAddress(image, []) }
        for plane in 0..<2 {
            guard let base = CVPixelBufferGetBaseAddressOfPlane(image, plane) else { throw CaptureFailure(message: "Generated picture plane missing") }
            memset(base, plane == 0 ? 80 : 128, CVPixelBufferGetBytesPerRowOfPlane(image, plane) * CVPixelBufferGetHeightOfPlane(image, plane))
        }
        return image
    }
}
