#if targetEnvironment(simulator) && SAPFLOW_SIMULATOR_TEST_SOURCE
import AVFoundation
import SwiftUI
import WebKit

/// A separate generated-source app. Never included in the broadcaster target.
/// No camera, microphone, input file, FFmpeg or recording is opened.
final class SimulatorBroadcastCheck: ObservableObject {
    @Published var summary = "Generated-source simulator check"
    let viewerURL: URL?
    private let owner = DispatchQueue(label: "sapflow.simulator-source")
    private let delivery = DispatchQueue(label: "sapflow.simulator-ingress")
    private let lock = NSLock()
    private var facts: [String: Any] = ["state": "waiting", "source": "generated picture and PCM", "softwareVideo": true]
    private var sink: ManualRtmpPublisher?
    private var video: HardwareVideoEncoder?
    private var audio: NativeAudioEncoder?
    private var timer: DispatchSourceTimer?
    private var frame = 0
    private var stopped = false
    private let ingress = RustEncodedIngress()
    private var lastKeyframe: CMTime? // delivery queue only

    init() {
        let raw = ProcessInfo.processInfo.environment["SAPFLOW_SIMULATOR_VIEWER_URL"] ?? ""
        let url = URL(string: raw)
        viewerURL = url?.host == "127.0.0.1" && url?.scheme == "http" ? url : nil
    }
    func set(_ key: String, _ value: Any) { lock.lock(); facts[key] = value; lock.unlock() }
    private func increment(_ key: String) {
        lock.lock(); facts[key] = (facts[key] as? Int ?? 0) + 1; lock.unlock()
    }
    private func report() {
        lock.lock(); let snapshot = facts; lock.unlock()
        do {
            let data = try JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys])
            let directory = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            try data.write(to: directory.appendingPathComponent("simulator-broadcast-report.json"), options: .atomic)
        } catch { DispatchQueue.main.async { self.summary = "Could not save check report" } }
        DispatchQueue.main.async { self.summary = "\(snapshot["state"] ?? "unknown") · video \(snapshot["videoUnits"] ?? 0) · audio \(snapshot["audioUnits"] ?? 0)" }
    }
    func start() {
        owner.async {
            guard self.timer == nil, !self.stopped else { return }
            do {
                let env = ProcessInfo.processInfo.environment
                guard CommandLine.arguments.contains("--sapflow-simulator-broadcast-check"),
                      let endpoint = env["SAPFLOW_SIMULATOR_ENDPOINT"], let url = URL(string: endpoint),
                      url.scheme == "rtmp", url.host == "127.0.0.1", url.user == nil, url.password == nil,
                      let key = env["SAPFLOW_SIMULATOR_STREAM_KEY"], self.viewerURL != nil else {
                    throw CaptureFailure(message: "Explicit simulator arguments and loopback destinations are required")
                }
                // The test flag must not relax the ordinary capture initialiser.
                do {
                    let ordinary = try HardwareVideoEncoder(orientation: .landscapeRight,
                        output: { _ in }, failure: { _ in })
                    ordinary.finish()
                    self.set("ordinaryHardwareGuard", "Hardware selected by ordinary initialiser")
                } catch let error as CaptureFailure {
                    guard error.message == "No explicitly identified hardware H.264 encoder is available" else { throw error }
                    self.set("ordinaryHardwareGuard", "Ordinary initialiser rejected absent hardware")
                }
                let sink = ManualRtmpPublisher(destination: try ManualDestination(endpoint: endpoint, streamKey: key, allowPlaintext: true))
                self.sink = sink; self.ingress.destination = sink
                self.video = try HardwareVideoEncoder(simulatorTestSource: .landscapeRight, profile: .hd720p30,
                    output: { [weak self] unit in self?.accept(unit) }, failure: { [weak self] _ in self?.fail("Video encoder failed") })
                self.audio = try NativeAudioEncoder(output: { [weak self] unit in self?.accept(unit) }, meter: { _ in })
                self.set("encoderImplementation", self.video!.implementation)
                try sink.start()
                self.set("state", "broadcasting")
                let timer = DispatchSource.makeTimerSource(queue: self.owner)
                timer.schedule(deadline: .now(), repeating: 1.0 / 30.0, leeway: .milliseconds(2))
                timer.setEventHandler { [weak self] in self?.tick() }
                self.timer = timer; timer.resume()
                // Independent wall-clock bound also covers a delayed samples queue.
                self.owner.asyncAfter(deadline: .now() + 75) { self.stopOnOwner() }
            } catch { self.fail((error as? CaptureFailure)?.message ?? "Generated source could not start") }
            self.report()
        }
    }
    private func accept(_ unit: CompressedUnit) {
        delivery.async {
            do {
                try self.ingress.accept(unit)
                self.increment(unit.isVideo ? "videoUnits" : "audioUnits")
                if unit.isVideo && unit.keyframe {
                    self.increment("keyframes")
                    if let previous = self.lastKeyframe {
                        let interval = CMTimeSubtract(unit.time, previous).seconds
                        self.lock.lock()
                        self.facts["maxKeyframeSeconds"] = max(self.facts["maxKeyframeSeconds"] as? Double ?? 0, interval)
                        self.lock.unlock()
                    }
                    self.lastKeyframe = unit.time
                }
            } catch { self.fail("Shared encoded ingress rejected generated media") }
        }
    }
    private func tick() {
        guard !stopped, let video, let audio else { return }
        do {
            let time = CMTime(value: Int64(frame), timescale: 30)
            try video.encode(Self.picture(frame: frame, time: time), time: time)
            try audio.encode(Self.pcm(start: Int64(frame) * 1600), time: CMTime(value: Int64(frame) * 1600, timescale: 48_000))
            frame += 1
            if frame % 30 == 0 {
                set("submittedFrames", frame); set("publisherStatus", sink?.status ?? "absent"); report()
            }
            if frame >= 1800 { stopOnOwner() }
        } catch { fail((error as? CaptureFailure)?.message ?? "Native generated media encoding failed") }
    }
    private func fail(_ message: String) {
        set("error", message)
        owner.async { self.stopOnOwner() }
    }
    func stop() { owner.async { self.stopOnOwner() } }
    private func stopOnOwner() {
        guard !stopped else { return }; stopped = true
        timer?.cancel(); timer = nil
        do { try audio?.finish() } catch { set("finalisationError", "Native audio finalisation failed") }
        do { try video?.finishChecked() } catch { set("finalisationError", "Native video finalisation failed") }
        delivery.sync {}
        set("submittedFrames", frame)
        set("state", "stopping"); report()
        guard let sink else { completeStop(); return }
        sink.stop { [weak self] in
            guard let self else { return }
            self.owner.async { self.completeStop() }
        }
    }
    private func completeStop() {
        set("publisherCleanupComplete", sink?.cleanupComplete ?? true)
        lock.lock(); let failed = facts["error"] != nil || facts["finalisationError"] != nil; lock.unlock()
        set("state", failed ? "failed" : "finished"); report()
    }
    private static func picture(frame: Int, time: CMTime) throws -> CMSampleBuffer {
        var image: CVPixelBuffer?
        guard CVPixelBufferCreate(kCFAllocatorDefault, 1280, 720, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &image) == kCVReturnSuccess, let image,
              CVPixelBufferLockBaseAddress(image, []) == kCVReturnSuccess else { throw CaptureFailure(message: "Generated picture allocation failed") }
        for plane in 0..<2 {
            guard let address = CVPixelBufferGetBaseAddressOfPlane(image, plane) else {
                CVPixelBufferUnlockBaseAddress(image, []); throw CaptureFailure(message: "Generated picture plane unavailable")
            }
            let stride = CVPixelBufferGetBytesPerRowOfPlane(image, plane)
            for row in 0..<CVPixelBufferGetHeightOfPlane(image, plane) {
                memset(address.advanced(by: row * stride), plane == 0 ? Int32(16 + (row + frame * 7) % 200) : 128, stride)
            }
        }
        CVPixelBufferUnlockBaseAddress(image, [])
        var format: CMVideoFormatDescription?
        var sample: CMSampleBuffer?
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 30), presentationTimeStamp: time, decodeTimeStamp: time)
        guard CMVideoFormatDescriptionCreateForImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image, formatDescriptionOut: &format) == noErr,
              let format, CMSampleBufferCreateReadyWithImageBuffer(allocator: kCFAllocatorDefault, imageBuffer: image,
                formatDescription: format, sampleTiming: &timing, sampleBufferOut: &sample) == noErr, let sample else {
            throw CaptureFailure(message: "Generated picture sample unavailable")
        }
        return sample
    }
    private static func pcm(start: Int64) throws -> CMSampleBuffer {
        let count = 1600
        var words = (0..<count).map { Int16(6000 * sin(Double(start + Int64($0)) * 2 * .pi * 440 / 48_000)) }
        var block: CMBlockBuffer?; var format: CMAudioFormatDescription?; var sample: CMSampleBuffer?
        guard let pcm = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 48_000, channels: 1, interleaved: true),
              CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: count * 2,
                blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0, dataLength: count * 2,
                flags: 0, blockBufferOut: &block) == noErr, let block else { throw CaptureFailure(message: "Generated PCM allocation failed") }
        let status = words.withUnsafeMutableBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: count * 2) }
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 48_000), presentationTimeStamp: CMTime(value: start, timescale: 48_000), decodeTimeStamp: .invalid)
        var sampleSize = 2
        guard status == noErr, CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: pcm.streamDescription,
            layoutSize: 0, layout: nil, magicCookieSize: 0, magicCookie: nil, extensions: nil, formatDescriptionOut: &format) == noErr,
              let format, CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
                sampleCount: count, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
                sampleSizeArray: &sampleSize, sampleBufferOut: &sample) == noErr, let sample else { throw CaptureFailure(message: "Generated PCM sample unavailable") }
        return sample
    }
}

private struct SimulatorViewer: UIViewRepresentable {
    let check: SimulatorBroadcastCheck
    final class Coordinator: NSObject, WKScriptMessageHandler {
        let check: SimulatorBroadcastCheck
        private var bestFrames = 0
        private var decodedReadings = 0
        init(_ check: SimulatorBroadcastCheck) { self.check = check }
        func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
            guard let facts = message.body as? [String: Any] else { return }
            check.set("viewer", facts)
            let frames = facts["frames"] as? Int ?? 0
            if frames > 0, (facts["width"] as? Int ?? 0) > 0 {
                decodedReadings += 1
                check.set("viewerDecodedReadings", decodedReadings)
                if frames > bestFrames { bestFrames = frames; check.set("viewerBest", facts) }
            }
        }
    }
    func makeCoordinator() -> Coordinator { Coordinator(check) }
    func makeUIView(context: Context) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.allowsInlineMediaPlayback = true
        config.mediaTypesRequiringUserActionForPlayback = [] // Explicit automated test interaction.
        config.userContentController.add(context.coordinator, name: "simulatorCheck")
        config.userContentController.addUserScript(WKUserScript(source: """
        setInterval(() => {
          const button = [...document.querySelectorAll('button')].find(x => x.textContent.trim() === 'Open stream');
          if (button) button.click();
          const v = document.querySelector('video');
          if (!v) return;
          if (v.paused) v.play().catch(() => {});
          const q = v.getVideoPlaybackQuality?.();
          window.webkit.messageHandlers.simulatorCheck.postMessage({time:v.currentTime, paused:v.paused,
            ready:v.readyState, width:v.videoWidth, height:v.videoHeight, frames:q?.totalVideoFrames || 0,
            dropped:q?.droppedVideoFrames || 0, audioTracks:v.audioTracks?.length || 0, error:v.error?.code || 0});
        }, 1000);
        """, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        let web = WKWebView(frame: .zero, configuration: config)
        if let url = check.viewerURL { web.load(URLRequest(url: url)) }
        return web
    }
    func updateUIView(_ view: WKWebView, context: Context) {}
}

@main
private struct SimulatorBroadcastCheckApp: App {
    @StateObject private var check = SimulatorBroadcastCheck()
    var body: some Scene {
        WindowGroup {
            VStack {
                Text("Sapflow simulator test · generated media").font(.headline)
                Text(check.summary).font(.caption)
                Button("Stop generated broadcast") { check.stop() }
                SimulatorViewer(check: check)
            }.task { check.start() }
        }
    }
}
#else
#error("SimulatorBroadcastCheck requires the separate simulator target and SAPFLOW_SIMULATOR_TEST_SOURCE")
#endif
