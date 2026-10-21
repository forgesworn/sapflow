// macOS interoperability driver for the production native networking adapter.
// Reads genuinely encoded fixture media through AVAssetReader, never fake NALs.
import Foundation
import AVFoundation
import Sapflow

func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CaptureFailure(message: message) }
}

final class PacketReader {
    let output: AVAssetReaderTrackOutput
    private var batch: CMSampleBuffer?
    private var nextIndex = 0
    init(_ output: AVAssetReaderTrackOutput) { self.output = output }
    func next() throws -> CMSampleBuffer? {
        for _ in 0..<16 {
            if batch == nil { batch = output.copyNextSampleBuffer(); nextIndex = 0 }
            guard let batch else { return nil }
            let count = CMSampleBufferGetNumSamples(batch)
            // AVAssetReader may emit an empty edit/discontinuity marker before
            // its first stored video sample. It is not an encoded access unit.
            if count == 0 { self.batch = nil; continue }
            try check(count > 0 && count <= 4096, "Fixture packet batch is outside its bound")
            var packet: CMSampleBuffer?
            let status = CMSampleBufferCopySampleBufferForRange(allocator: kCFAllocatorDefault, sampleBuffer: batch,
                sampleRange: CFRange(location: nextIndex, length: 1), sampleBufferOut: &packet)
            try check(status == noErr, "Could not split fixture compressed samples")
            nextIndex += 1
            if nextIndex == count { self.batch = nil }
            return packet
        }
        throw CaptureFailure(message: "Fixture has too many consecutive empty media markers")
    }
}

func configuration(_ sample: CMSampleBuffer, video: Bool) throws -> CodecDescription {
    guard let format = CMSampleBufferGetFormatDescription(sample) else { throw CaptureFailure(message: "Fixture format unavailable") }
    if video {
        try check(CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_H264, "Fixture video must be H.264 without B-frames")
        func parameter(_ index: Int) throws -> Data {
            var bytes: UnsafePointer<UInt8>?
            var length = 0; var count = 0; var header: Int32 = 0
            let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index,
                parameterSetPointerOut: &bytes, parameterSetSizeOut: &length, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &header)
            guard status == noErr, header == 4, let bytes else { throw CaptureFailure(message: "Fixture needs four-byte AVCC framing") }
            return Data(bytes: bytes, count: length)
        }
        return .h264(sps: try parameter(0), pps: try parameter(1))
    }
    guard let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(format) else { throw CaptureFailure(message: "Fixture audio format unavailable") }
    try check(asbd.pointee.mFormatID == kAudioFormatMPEG4AAC && asbd.pointee.mSampleRate == 48_000 && (1...2).contains(asbd.pointee.mChannelsPerFrame), "Fixture audio must be AAC-LC48k mono/stereo")
    return .aac(asc: Data([0x11, asbd.pointee.mChannelsPerFrame == 1 ? 0x88 : 0x90]))
}

func run() throws {
    let environment = ProcessInfo.processInfo.environment
    guard CommandLine.arguments.count == 2, let endpoint = environment["SAPFLOW_CHECK_ENDPOINT"] else {
        throw CaptureFailure(message: "Supply a fixture file and SAPFLOW_CHECK_ENDPOINT")
    }
    let useSrt = environment["SAPFLOW_CHECK_TRANSPORT"] == "srt"
    let sink: EncodedPublishingSink
    let encrypted: Bool
    if useSrt {
        let destination = try ManualSrtDestination(endpoint: endpoint,
            streamID: environment["SAPFLOW_CHECK_SRT_STREAM_ID"] ?? "",
            passphrase: environment["SAPFLOW_CHECK_SRT_PASSPHRASE"] ?? "",
            allowPlaintext: environment["SAPFLOW_CHECK_ALLOW_PLAINTEXT"] == "1")
        sink = ManualSrtPublisher(destination: destination); encrypted = destination.encrypted
    } else {
        guard let key = environment["SAPFLOW_CHECK_STREAM_KEY"] else { throw CaptureFailure(message: "Supply SAPFLOW_CHECK_STREAM_KEY") }
        let destination = try ManualDestination(endpoint: endpoint, streamKey: key,
            allowPlaintext: environment["SAPFLOW_CHECK_ALLOW_PLAINTEXT"] == "1")
        sink = ManualRtmpPublisher(destination: destination); encrypted = destination.encrypted
    }
    let asset = AVURLAsset(url: URL(fileURLWithPath: CommandLine.arguments[1]))
    try check(asset.duration.isNumeric && asset.duration.seconds > 0 && asset.duration.seconds <= 30, "Use a fixture of at most thirty seconds")
    guard let videoTrack = asset.tracks(withMediaType: .video).first,
          let audioTrack = asset.tracks(withMediaType: .audio).first else { throw CaptureFailure(message: "Fixture needs compressed video and audio") }
    let reader = try AVAssetReader(asset: asset)
    let videoOutput = AVAssetReaderTrackOutput(track: videoTrack, outputSettings: nil)
    let audioOutput = AVAssetReaderTrackOutput(track: audioTrack, outputSettings: nil)
    videoOutput.alwaysCopiesSampleData = false; audioOutput.alwaysCopiesSampleData = false
    reader.add(videoOutput); reader.add(audioOutput)
    try check(reader.startReading(), "Could not read fixture")
    let video = PacketReader(videoOutput); let audio = PacketReader(audioOutput)
    var v = try video.next(); var a = try audio.next()
    guard let firstVideo = v, let firstAudio = a else { throw CaptureFailure(message: "Fixture tracks are empty") }
    let videoConfig = try configuration(firstVideo, video: true)
    let audioConfig = try configuration(firstAudio, video: false)
    let videoStart = CMSampleBufferGetPresentationTimeStamp(firstVideo)
    let audioStart = CMSampleBufferGetPresentationTimeStamp(firstAudio)
    let epoch = CMTimeCompare(videoStart, audioStart) < 0 ? videoStart : audioStart
    let ingress = RustEncodedIngress()
    ingress.destination = sink
    if let value = environment["SAPFLOW_CHECK_PREPARE_DELAY_MS"], let delay = UInt32(value) {
        try check(delay <= 20_000, "Prepare delay exceeds the check bound")
        Thread.sleep(forTimeInterval: Double(delay) / 1000)
        try check(sink.status == (useSrt ? "SRT destination ready · single attempt" : "Manual destination ready · single attempt"), "A prepared destination started without explicit Start")
        print("Dormant destination remained ready for \(delay) ms before explicit Start.")
    }
    try sink.start()
    try ingress.prepare(video: videoConfig, audio: audioConfig)
    defer { sink.stop(); reader.cancelReading() }
    let deadline = ProcessInfo.processInfo.systemUptime + 20
    if environment["SAPFLOW_CHECK_EXPECT_TLS_REJECTION"] == "1" {
        try check(!useSrt && encrypted, "TLS rejection mode requires RTMPS")
        while !sink.status.contains("recording continues") {
            try check(ProcessInfo.processInfo.systemUptime < deadline, "TLS rejection was not observed")
            try check(!sink.status.hasPrefix("Destination accepted") && !sink.status.hasPrefix("RTMP handshake"), "Untrusted TLS destination was unexpectedly accepted")
            Thread.sleep(forTimeInterval: 0.02)
        }
        print("Native TLS attempt failed before RTMP publication: \(sink.status). Require the independent server certificate-alert receipt as well.")
        return
    }
    if environment["SAPFLOW_CHECK_EXPECT_SRT_REJECTION"] == "1" {
        try check(useSrt && encrypted, "SRT rejection mode requires encrypted SRT")
        while !sink.status.contains("recording continues") {
            try check(ProcessInfo.processInfo.systemUptime < deadline, "SRT rejection was not observed")
            try check(!sink.status.hasPrefix("SRT connected"), "Wrong-secret SRT peer was unexpectedly accepted")
            Thread.sleep(forTimeInterval: 0.02)
        }
        print("Encrypted SRT attempt refused before media submission. Require the independent listener rejection receipt as well.")
        return
    }
    while !sink.status.hasPrefix(useSrt ? "SRT connected" : "Destination accepted") {
        try check(ProcessInfo.processInfo.systemUptime < deadline, "Native destination startup timed out")
        try check(!sink.status.contains("recording continues"), "Native destination failed before publication: \(sink.status)")
        Thread.sleep(forTimeInterval: 0.02)
    }
    let wallStart = ProcessInfo.processInfo.systemUptime
    var videoPackets = 0; var audioPackets = 0
    while v != nil || a != nil {
        let isVideo = a == nil || (v != nil && CMTimeCompare(CMSampleBufferGetPresentationTimeStamp(v!), CMSampleBufferGetPresentationTimeStamp(a!)) <= 0)
        guard let sample = isVideo ? v : a else { break }
        let time = CMTimeSubtract(CMSampleBufferGetPresentationTimeStamp(sample), epoch)
        let dts = CMSampleBufferGetDecodeTimeStamp(sample)
        try check(!isVideo || !dts.isNumeric || CMTimeCompare(dts, CMSampleBufferGetPresentationTimeStamp(sample)) == 0, "Fixture B-frames are unsupported by this app's profile")
        while ProcessInfo.processInfo.systemUptime - wallStart < time.seconds { Thread.sleep(forTimeInterval: 0.005) }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        let keyframe = isVideo && (attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true
        let duration = isVideo ? CMSampleBufferGetDuration(sample) : CMTime(value: 1024, timescale: 48_000)
        try ingress.accept(CompressedUnit(buffer: sample, isVideo: isVideo, keyframe: keyframe, time: time,
            duration: duration, configuration: isVideo ? videoConfig : audioConfig))
        try check(!sink.status.contains("recording continues"), "Native adapter failed while sending fixture: \(sink.status)")
        if isVideo { videoPackets += 1; v = try video.next() } else { audioPackets += 1; a = try audio.next() }
    }
    if useSrt {
        // SRT explicit stop is cancellation, so let the paced fixture's final
        // datagrams complete first. This wait is only in the desktop driver.
        Thread.sleep(forTimeInterval: 0.25)
        try check(!sink.status.contains("recording continues"), "SRT failed while draining fixture tail")
    }
    let stopped = DispatchGroup()
    // Rotation must await actual transport teardown. Repeated stop requests
    // complete against the same bound rather than extending its deadline.
    stopped.enter(); sink.stop(completion: { stopped.leave() })
    stopped.enter(); sink.stop(completion: { stopped.leave() })
    try check(stopped.wait(timeout: .now() + 4) == .success, "Native publisher teardown callbacks exceeded their bound")
    let stoppedBy = ProcessInfo.processInfo.systemUptime + 4
    while !sink.status.hasPrefix(useSrt ? "SRT publisher stopped" : "Publisher stopped") && ProcessInfo.processInfo.systemUptime < stoppedBy { Thread.sleep(forTimeInterval: 0.02) }
    try check(sink.status.hasPrefix(useSrt ? "SRT publisher stopped" : "Publisher stopped"), "Native publisher did not stop within its bound")
    print("Native Network.framework publication completed: video=\(videoPackets), audio=\(audioPackets). Both teardown callbacks completed within four seconds. Viewer decode must be checked independently.")
}

do { try run() } catch {
    // No destination URL/key or peer-controlled protocol error is printed.
    if let failure = error as? CaptureFailure { fputs("Native fixture check: \(failure.message)\n", stderr) }
    else { fputs("Native publisher fixture check failed. Inspect adapter/receiver evidence.\n", stderr) }
    exit(1)
}
