// Generated PCM -> ONE actual native AAC encoder -> existing ingress validator
// -> same real sample handle -> existing recordingIO -> actual SDK/filesystem.
// Only transport is a spy. No CaptureEngine proof, decoder, capture or device.
import Foundation
import Darwin
import AVFoundation
import CryptoKit
import Sapflow

private func requireCaptureRecording(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !value() { throw CaptureFailure(message: message) }
}
private final class RecordingReply<Value> {
    private let condition = NSCondition()
    private var result: Value?
    func put(_ value: Value) { condition.lock(); if result == nil { result = value }; condition.broadcast(); condition.unlock() }
    func wait() throws -> Value {
        let end = Date().addingTimeInterval(8)
        condition.lock(); defer { condition.unlock() }
        while result == nil { guard condition.wait(until: end) else { throw CaptureFailure(message: "Generated recording callback deadline") } }
        return result!
    }
    var ready: Bool { condition.lock(); defer { condition.unlock() }; return result != nil }
}
private final class RecordingTrace {
    private let lock = NSLock()
    private var packets: [[String: Any]] = []
    private var packetTimings: [[String: Any]] = [] // Same <=512 bound and index as packets; test tool only.
    private var videoPackets: [[String: Any]] = [] // Bounded <=64 adapter-output callbacks; test tool only.
    private var recordingErrors = 0, publishingErrors = 0, mediaErrors = 0, refused = 0
    private var callbackError: Error?
    private var originalUnit: CompressedUnit?
    private var generations: [AudioRecordingTiming.GenerationExtent] = []
    func packet(_ unit: CompressedUnit) {
        do {
            guard let block = CMSampleBufferGetDataBuffer(unit.buffer) else { throw CaptureFailure(message: "Native AAC payload absent") }
            var contiguous = 0, total = 0
            var pointer: UnsafeMutablePointer<Int8>?
            guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &contiguous,
                totalLengthOut: &total, dataPointerOut: &pointer) == noErr, let pointer,
                contiguous == total, (1...65_536).contains(total) else { throw CaptureFailure(message: "Native AAC payload shape") }
            let view = Data(bytesNoCopy: pointer, count: total, deallocator: .none)
            let hash = SHA256.hash(data: view).map { String(format: "%02x", $0) }.joined()
            // Observe the actual original CMSampleBuffer before worker validation.
            // These are application-assigned coded times, not converter timestamps.
            var timing: [String: Any] = ["size": total, "sha256": hash,
                "sampleCount": CMSampleBufferGetNumSamples(unit.buffer),
                "pts": Self.timeFacts(CMSampleBufferGetPresentationTimeStamp(unit.buffer)),
                "dts": Self.timeFacts(CMSampleBufferGetDecodeTimeStamp(unit.buffer)),
                "duration": Self.timeFacts(CMSampleBufferGetDuration(unit.buffer))]
            lock.lock(); defer { lock.unlock() }
            guard packets.count < 512 else { throw CaptureFailure(message: "Generated packet retention bound") }
            guard packetTimings.count == packets.count else { throw CaptureFailure(message: "Generated parallel timing retention diverged") }
            if originalUnit == nil { originalUnit = unit }
            timing["index"] = packets.count
            packets.append(["size": total, "sha256": hash])
            packetTimings.append(timing)
        } catch { lock.lock(); callbackError = callbackError ?? error; lock.unlock() }
    }
    func observeVideo(_ unit: CompressedUnit) {
        do {
            guard unit.isVideo, let block = CMSampleBufferGetDataBuffer(unit.buffer) else {
                throw CaptureFailure(message: "Adapter video payload absent")
            }
            var contiguous = 0, total = 0
            var pointer: UnsafeMutablePointer<Int8>?
            guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &contiguous,
                totalLengthOut: &total, dataPointerOut: &pointer) == noErr, let pointer,
                contiguous == total, (1...4_194_304).contains(total) else { throw CaptureFailure(message: "Adapter video payload shape") }
            let view = Data(bytesNoCopy: pointer, count: total, deallocator: .none)
            let hash = SHA256.hash(data: view).map { String(format: "%02x", $0) }.joined()
            var observed: [String: Any] = ["size": total, "sha256": hash, "keyframe": unit.keyframe,
                "sampleCount": CMSampleBufferGetNumSamples(unit.buffer),
                "pts": Self.timeFacts(CMSampleBufferGetPresentationTimeStamp(unit.buffer)),
                "dts": Self.timeFacts(CMSampleBufferGetDecodeTimeStamp(unit.buffer)),
                "duration": Self.timeFacts(CMSampleBufferGetDuration(unit.buffer))]
            lock.lock(); defer { lock.unlock() }
            guard videoPackets.count < 64 else { throw CaptureFailure(message: "Generated video witness retention bound") }
            observed["index"] = videoPackets.count; videoPackets.append(observed)
        } catch { lock.lock(); callbackError = callbackError ?? error; lock.unlock() }
    }
    private static func timeFacts(_ value: CMTime) -> [String: Any] {
        ["value": value.value, "timescale": value.timescale, "flags": value.flags.rawValue,
            "epoch": value.epoch, "numeric": value.isNumeric]
    }
    func timing(_ event: AudioRecordingTiming.Event) {
        if case let .generationEnded(value) = event { lock.lock(); generations.append(value); lock.unlock() }
    }
    func error(_ kind: String) {
        lock.lock(); defer { lock.unlock() }
        switch kind { case "recording": recordingErrors += 1; case "publishing": publishingErrors += 1; default: mediaErrors += 1 }
    }
    func refusal() { lock.lock(); refused += 1; lock.unlock() }
    func check() throws { lock.lock(); let error = callbackError; lock.unlock(); if let error { throw error } }
    var firstUnit: CompressedUnit? { lock.lock(); defer { lock.unlock() }; return originalUnit }
    var packetSnapshot: [[String: Any]] { lock.lock(); defer { lock.unlock() }; return packets }
    var packetTimingSnapshot: [[String: Any]] { lock.lock(); defer { lock.unlock() }; return packetTimings }
    var videoPacketSnapshot: [[String: Any]] { lock.lock(); defer { lock.unlock() }; return videoPackets }
    var counters: (recording: Int, publishing: Int, media: Int, refused: Int) {
        lock.lock(); defer { lock.unlock() }; return (recordingErrors, publishingErrors, mediaErrors, refused)
    }
    var receiptSnapshot: [AudioRecordingTiming.GenerationExtent] { lock.lock(); defer { lock.unlock() }; return generations }
}
private final class RecordingTransportSpy: EncodedPublishingSink {
    let failOffer: Bool
    let finishRequested = RecordingReply<Bool>()
    private let lock = NSLock()
    private var close: ((Result<Void, Error>) -> Void)?
    private var closed = false, ended = false
    private var first: MediaSample?
    private var offers = 0, finishes = 0
    init(failOffer: Bool) { self.failOffer = failOffer }
    var status: String { "Generated network-free transport spy" }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return closed }
    func start() throws {}
    func configure(video: MediaCodec?, audio: MediaCodec) throws {}
    func offer(_ sample: MediaSample) throws {
        lock.lock(); defer { lock.unlock() }
        guard !ended else { throw CaptureFailure(message: "Publishing spy received media after End") }
        offers += 1
        if first == nil { first = sample }
        if failOffer { throw CaptureFailure(message: "Deliberate publishing-only fixture failure") }
    }
    func endDestination() { lock.lock(); ended = true; lock.unlock() }
    func stop() { endDestination() }
    func finishEpoch(completion: @escaping (Result<Void, Error>) -> Void) {
        lock.lock(); finishes += 1; close = completion; lock.unlock(); finishRequested.put(true)
    }
    func releaseClose() {
        lock.lock(); closed = true; let callback = close; close = nil; lock.unlock(); callback?(.success(()))
    }
    var counts: (offers: Int, finishes: Int) { lock.lock(); defer { lock.unlock() }; return (offers, finishes) }
    func checkRetainedRealSample() throws {
        lock.lock(); let sample = first; lock.unlock()
        guard let sample else { throw CaptureFailure(message: "No actual publishing sample retained") }
        try requireCaptureRecording(!sample.isVideo() && sample.len() > 0 && sample.pts() == 0,
            "Real shared sample did not outlive native buffer/recording closure")
    }
}

@main
enum CaptureRecordingFixtureCheck {
    private static func admitted(_ gate: CaptureAdmission) throws -> CaptureAdmission.MediaRun {
        guard let preparation = gate.reservePreparation(profile: .audioOnly48kMono) else { throw CaptureFailure(message: "Prepare refused") }
        try requireCaptureRecording(gate.attached(preparation) && gate.beginPermissions(preparation) && gate.prepared(preparation), "Prepare ownership")
        guard let start = gate.reserveStart(preparation), let run = gate.reserveMediaRun(start) else { throw CaptureFailure(message: "Run reservation") }
        try requireCaptureRecording(gate.commitMediaRun(run, start: start, nowNs: DispatchTime.now().uptimeNanoseconds, broadcasting: true), "Run commit")
        return run
    }
    private static func run(_ name: String, root: URL) throws -> [String: Any] {
        let queueFailure = name == "recording-queue-failure", missingEOF = name == "missing-file-EOF"
        let mixed = name == "mixed-zero-raw", publishingFailure = name == "publishing-independent-failure"
        let trace = RecordingTrace(), admission = CaptureAdmission(), mediaRun = try admitted(admission)
        guard let token = admission.mediaToken(mediaRun, kind: .raw) else { throw CaptureFailure(message: "Run token absent") }
        let available = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { fd in var value = stat(); guard fstat(fd, &value) == 0 && value.st_mode & S_IFMT == S_IFDIR else { throw CaptureFailure(message: "Capacity descriptor is not actual root") }; return 1024 * 1024 * 1024 })
        let catalogue = try RecordingCatalogue(root: root, appendIO: available)
        defer { try? catalogue.close() }
        let identity = try catalogue.newShow(profile: .audioOnly48kMono).nextIdentity()
        let recordingIO = DispatchQueue(label: "fixture.existing-recording-io")
        let recorder = try recordingIO.sync {
            try CaptureRecording.open(catalogue: catalogue, identity: identity, queue: recordingIO, selection: .sdkCheckpointed) { _ in trace.error("recording") }
        }
        try requireCaptureRecording(recorder.audioReceiptMode == .fullGenerationLedger, "SDK selected legacy EOF mode")
        let ingress = RustEncodedIngress(); ingress.reset(hasVideo: false)
        let transport = RecordingTransportSpy(failOffer: publishingFailure); ingress.destination = transport
        defer { transport.releaseClose() }
        let ingressQueue = DispatchQueue(label: "fixture.existing-encoded-ingress")
        let terminalRecording = DispatchSemaphore(value: 0)
        let worker = try IngressWorker(run: mediaRun, ingress: ingress, recorder: recorder, admission: admission,
            boundary: { _ in }, terminal: { value in
                _ = admission.sinkTerminated(value)
                if value.sink == .recording { terminalRecording.signal() }
            }, mediaFailure: { _ in trace.error("media") }, failure: { _ in trace.error("publishing") }, statistics: { _, _ in }, queue: ingressQueue)
        try requireCaptureRecording(worker.startPublishing(), "Publishing did not start")
        let hold = DispatchSemaphore(value: 0), entered = DispatchSemaphore(value: 0)
        if queueFailure {
            recordingIO.async { entered.signal(); _ = hold.wait(timeout: .now() + 8) }
            try requireCaptureRecording(entered.wait(timeout: .now() + 5) == .success, "Recording queue was not actually held")
        }
        defer { hold.signal() }
        weak var callbackEncoder: NativeAudioEncoder?
        let encoder = try NativeAudioEncoder(receiptMode: recorder.audioReceiptMode, output: { unit in
            trace.packet(unit)
            if !worker.offer(unit, token: token, audioGeneration: callbackEncoder?.lastOutputGeneration) { trace.refusal() }
        }, meter: { _ in }, recordingTiming: { event in trace.timing(event); worker.audioTiming(event) })
        callbackEncoder = encoder
        try recorder.bindNativeInputs(audio: encoder, video: nil)
        defer { try? encoder.finish() }
        let source = try format(), nativeToken = try unwrap(encoder.inputToken)
        if mixed {
            let zero = try encoder.beginStandby(format: source, at: .zero)
            var remaining: UInt64 = 20_017
            while remaining > 0 {
                let frames = UInt32(min(480, remaining)); try encoder.encodeSilence(frames: frames, token: zero)
                ingressQueue.sync {}; remaining -= UInt64(frames)
            }
            let raw = try encoder.resumeInput(from: zero, at: CMTime(value: 20_017, timescale: 48_000))
            try requireCaptureRecording(raw.generation == zero.generation, "Mixed source recreated converter generation")
            try feed(encoder, token: raw, source: source, start: 20_017, frames: 16_039, ingress: ingressQueue)
        } else {
            try feed(encoder, token: nativeToken, source: source, start: 0,
                frames: queueFailure ? 160_137 : 16_137, ingress: ingressQueue)
        }
        if !missingEOF { try encoder.finish(); ingressQueue.sync {} }
        if !missingEOF {
            try requireCaptureRecording(encoder.lastNativeEOFObservation?.complete == true,
                "Actual converter EOS input delivery and terminal output status were not observed")
        }
        let nativeEOF = nativeEOFFacts(encoder.lastNativeEOFObservation)
        try trace.check()
        let originalPackets = trace.packetSnapshot
        try requireCaptureRecording(ingress.accepted == UInt64(originalPackets.count), "Ingress did not validate each original native AU exactly once")
        if queueFailure {
            let bound = try unwrap(recorder.nativeInputBudgetSnapshot)
            try requireCaptureRecording(bound.failed && bound.sequence == 0 &&
                bound.acceptedEnd.map { CMTimeCompare($0, CMTime(value: 2, timescale: 1)) == 0 } == true,
                "Actual native input bypassed the aggregate budget while recordingIO was held")
            try requireCaptureRecording(terminalRecording.wait(timeout: .now() + 5) == .success, "Real recording admission did not exhaust")
        } else {
            let capacity = RecordingReply<Result<Void, Error>>()
            recorder.checkStorage(profile: .audioOnly48kMono) { capacity.put($0) }
            try capacity.wait().get() // Actual same root/file/child query while active.
        }
        let recording = RecordingReply<CaptureRecording.Completion>(), publishing = RecordingReply<Result<Void, Error>>()
        worker.finish(recording: { recording.put($0) }, publishing: { publishing.put($0) })
        hold.signal()
        let result = try recording.wait()
        _ = try transport.finishRequested.wait()
        try requireCaptureRecording(result.cleanupComplete && !result.finalised && result.fileURL == nil,
            "SDK branch promoted submitted inputs or did not checked-close")
        try requireCaptureRecording(!publishing.ready && !transport.cleanupComplete, "Recording waited for or fabricated transport closure")
        transport.releaseClose(); try publishing.wait().get()
        if missingEOF {
            // Explicit BAD caller order control: real native EOF arrives only
            // AFTER ingress+recording closure. It cannot promote/reopen the file.
            try encoder.finish(); ingressQueue.sync {}; try trace.check()
        }
        let entries = try catalogue.scan(), entry = try unwrap(entries.first { $0.segmentID == identity.segmentID })
        try requireCaptureRecording(entry.state == .incomplete && !catalogue.cleanupRetained, "Wrong closed SDK catalogue state")
        let media = root.appendingPathComponent(identity.filename), physical = try Data(contentsOf: media)
        var directoryStat = stat(), fileStat = stat()
        try requireCaptureRecording(lstat(root.path, &directoryStat) == 0 && lstat(media.path, &fileStat) == 0 &&
            fileStat.st_mode & 0o777 == 0o600 && fileStat.st_nlink == 1 && fileStat.st_size == Int64(physical.count), "Actual file mode/incarnation")
        let binding = try RecordingCatalogue.AppendSource(rootDevice: directoryStat.st_dev, rootInode: directoryStat.st_ino,
            device: fileStat.st_dev, inode: fileStat.st_ino, birthSeconds: Int64(fileStat.st_birthtimespec.tv_sec),
            birthNanoseconds: Int64(fileStat.st_birthtimespec.tv_nsec)).bindings(identity: identity)
        var slots: [DecodedRecordingCheckpoint] = []
        for index in 0..<2 {
            let path = root.appendingPathComponent("Sapflow-\(identity.segmentID).checkpoint.\(index)")
            if FileManager.default.fileExists(atPath: path.path) {
                slots.append(try decodeRecordingCheckpoint(bytes: Data(contentsOf: path), expectedRecordingBinding: binding.recording, expectedSourceFileBinding: binding.sourceFile))
            }
        }
        slots.sort { $0.info().facts.sequence < $1.info().facts.sequence }
        if slots.count == 2 { try validateRecordingCheckpointSuccessor(prior: slots[0], next: slots[1]) }
        let last = try unwrap(slots.last), info = last.info()
        try requireCaptureRecording(UInt64(physical.count) == info.committedOffset &&
            Data(physical.prefix(Int(info.initialHeaderLen))) == last.header() &&
            Data(SHA256.hash(data: physical.dropFirst(Int(info.initialHeaderLen)))) == info.fragmentRegionSha256,
            "Actual installed header/fragment prefix differs from real shared checkpoint")
        try requireCaptureRecording(info.facts.completeAudioEof == (!missingEOF && !queueFailure), "Invented or missing actual SDK EOF")
        if !queueFailure { try requireCaptureRecording(info.facts.audioUnits == UInt64(originalPackets.count), "SDK lost actual admitted AUs") }
        else { try requireCaptureRecording(info.facts.audioUnits > 0 && info.facts.audioUnits < UInt64(originalPackets.count), "Queue failure did not preserve a positive bounded prefix") }
        let beforeLateJoin = SHA256.hash(data: physical)
        let repeated = RecordingReply<CaptureRecording.Completion>()
        worker.finish(recording: { repeated.put($0) }, publishing: { _ in })
        let replay = try repeated.wait()
        try requireCaptureRecording(replay.cleanupComplete && !replay.finalised && transport.counts.finishes == 1 &&
            Data(SHA256.hash(data: try Data(contentsOf: media))) == Data(beforeLateJoin), "Late join repeated IO or changed original")
        try transport.checkRetainedRealSample()
        let counts = trace.counters
        try requireCaptureRecording(counts.media == 0 && counts.publishing == (publishingFailure ? 1 : 0) &&
            counts.recording == (queueFailure ? 1 : 0), "Wrong independent branch failure scope")
        try requireCaptureRecording((result.failure != nil) == (missingEOF || queueFailure), "Incomplete input error was lost or invented")
        if mixed {
            let value = try unwrap(trace.receiptSnapshot.first)
            try requireCaptureRecording(value.generation == 1 && value.rawFrames == 16_039 && value.zeroFrames == 20_017 && value.inputFrames == 36_056,
                "Actual mixed generation provenance changed")
        }
        return ["name": name, "passed": true, "generatedOnly": true, "sdkReceiptState": "incomplete", "actualNativeEOF": nativeEOF,
            "appAdmission": "unknown", "nativeUnitsBeforeClose": originalPackets.count, "validatedExactlyOnce": true,
            "storedAudioUnits": info.facts.audioUnits, "sdkCompleteAudioEOF": info.facts.completeAudioEof,
            "actualCheckedCleanup": result.cleanupComplete, "recordingClosedBeforeTransport": true,
            "recordingFailure": result.failure != nil, "publishingFailures": counts.publishing,
            "lateNativeOutputRefusals": counts.refused, "media": identity.filename,
            "exactPayloadProbeRequired": !queueFailure, "originalPackets": queueFailure ? [] : originalPackets,
            "mixedGeneration": mixed ? trace.receiptSnapshot.map { ["generation": $0.generation,
                "rawFrames": $0.rawFrames, "zeroFrames": $0.zeroFrames, "inputFrames": $0.inputFrames] } : []]
    }
    /// Generated native sources exercise the SAME concrete producer close as
    /// CaptureEngine. The real session is empty/stopped; no camera proof.
    private static func producer(_ name: String, root: URL, selection: CaptureRecording.BackendSelection = .sdkCheckpointed) throws -> [String: Any] {
        let publishFailure = name == "producer-publishing-failure"
        let lateDrop = name == "producer-late-native-drop", recordingLoss = name == "producer-recording-loss"
        let early = name == "producer-input-not-closed", wrong = name == "producer-wrong-worker"
        let wrongTicket = name == "producer-wrong-ticket"
        let lateEncoded = name == "producer-late-encoded-output", lateTiming = name == "producer-late-timing"
        let holdCommit = lateDrop || recordingLoss || lateEncoded || lateTiming
        // Legacy A/V already updates its closed native movie through this real
        // descriptor seam before the source cut. Hold it to exercise a late
        // loss while actual AVAssetWriter finish has returned but publication
        // has not. No new production callback or writer/encoder is introduced.
        let av = name == "producer-av" || selection == .legacy && holdCommit
        let expectedComplete = !holdCommit && !early && !wrong && !wrongTicket
        let gate = CaptureAdmission(), profile: CaptureIntent = av ? .audioVideo(.sd480p30) : .audioOnly48kMono
        let prep = try unwrap(gate.reservePreparation(profile: profile))
        try requireCaptureRecording(gate.attached(prep) && gate.beginPermissions(prep) && gate.prepared(prep), "Producer preparation")
        let start = try unwrap(gate.reserveStart(prep)), run = try unwrap(gate.reserveMediaRun(start))
        let captureAdmission = RecordingCaptureAdmission()
        let staleTicket = try unwrap(captureAdmission.begin(.segment(start)))
        try requireCaptureRecording(captureAdmission.closed(staleTicket, cleanupComplete: true), "Old ticket cleanup")
        let ticket = try unwrap(captureAdmission.begin(.segment(start)))
        try requireCaptureRecording(captureAdmission.activate(ticket), "Actual recording ticket activation")
        try requireCaptureRecording(gate.commitMediaRun(run, start: start, nowNs: DispatchTime.now().uptimeNanoseconds, broadcasting: true), "Producer commit")
        let token = try unwrap(gate.mediaToken(run, kind: .raw))
        let capacity = RecordingCatalogue.AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            availableBytes: { _ in 1024 * 1024 * 1024 })
        let headerEntered = DispatchSemaphore(value: 0), releaseHeader = DispatchSemaphore(value: 0)
        var heldHeader = false // actual recordingIO owner only
        let updates = RecordingCatalogue.MediaUpdateIO(write: { fd, buffer, count, offset in
            if holdCommit && !heldHeader {
                heldHeader = true; headerEntered.signal()
                if releaseHeader.wait(timeout: .now() + 8) != .success { errno = ETIMEDOUT; return -1 }
            }
            return Darwin.pwrite(fd, buffer, count, offset)
        }, synchronize: Darwin.fsync)
        let catalogue = try RecordingCatalogue(root: root, mediaUpdateIO: updates, appendIO: capacity)
        defer { releaseHeader.signal() }
        defer { try? catalogue.close() }
        let identity = try catalogue.newShow(profile: profile).nextIdentity()
        let io = DispatchQueue(label: "fixture.producer.recording"), samples = DispatchQueue(label: "fixture.producer.samples")
        let trace = RecordingTrace(), recorder = try io.sync {
            try CaptureRecording.open(catalogue: catalogue, identity: identity, queue: io, selection: selection) { _ in trace.error("recording") }
        }
        let ingress = RustEncodedIngress(); ingress.reset(hasVideo: av)
        let transport = RecordingTransportSpy(failOffer: publishFailure); ingress.destination = transport
        defer { transport.releaseClose() }
        let encoded = DispatchQueue(label: "fixture.producer.ingress")
        let worker = try IngressWorker(run: run, ingress: ingress, recorder: recorder, admission: gate,
            boundary: { _ in }, terminal: { _ in }, mediaFailure: { _ in trace.error("media") },
            failure: { _ in trace.error("publishing") }, statistics: { _, _ in }, queue: encoded)
        try requireCaptureRecording(worker.startPublishing(), "Producer publishing start")
        weak var callback: NativeAudioEncoder?
        let audio = try NativeAudioEncoder(receiptMode: recorder.audioReceiptMode, output: { unit in
            trace.packet(unit)
            if !worker.offer(unit, token: token, audioGeneration: callback?.lastOutputGeneration) { trace.refusal() }
        }, meter: { _ in }, recordingTiming: { worker.audioTiming($0) })
        callback = audio
        let video: HardwareVideoEncoder?
        if av {
            video = try HardwareVideoEncoder(orientation: .landscapeRight, profile: .sd480p30,
                output: { unit, source in trace.observeVideo(unit); _ = worker.offer(unit, token: source) },
                fault: { source, _ in _ = gate.nativeFault(source ?? token) },
                permitsInput: { gate.allowsInput($0) == .allow })
        } else { video = nil }
        try recorder.bindNativeInputs(audio: audio, video: video)
        let session = AVCaptureSession(), wrongOwner = NSObject()
        let expectedWorker: AnyObject = wrong ? wrongOwner : worker
        let producer = CaptureRecordingProducer(run: run, ticket: wrongTicket ? staleTicket : ticket, recorder: recorder,
            worker: expectedWorker, audio: audio, video: video, session: session, samples: samples, admission: gate, recordingAdmission: captureAdmission)
        let source = try format(), input = try unwrap(audio.inputToken)
        try samples.sync {
            try feed(audio, token: input, source: source, start: 0, frames: 16_137, ingress: encoded)
            if let video {
                let slate = try video.prepareSlate()
                for index in 0..<10 {
                    try requireCaptureRecording(try slate.withImage { try video.encode($0, time: CMTime(value: Int64(index), timescale: 30), token: token) }, "Generated video submission")
                }
            }
        }
        // Real normal Stop cancellation is deliberately present. It must not
        // stand in for media loss or prevent a genuine clean close.
        _ = gate.stop(); captureAdmission.cancel(prep)
        if !early { producer.stopInput() }
        let closure = try samples.sync { try producer.finish() }
        let nativeEOF = try samples.sync { () throws -> [String: Any] in
            try requireCaptureRecording(audio.lastNativeEOFObservation?.complete == true,
                "Producer finalisation did not observe actual converter EOS")
            return nativeEOFFacts(audio.lastNativeEOFObservation)
        }
        let nativeLedger = try samples.sync { try nativeLedgerFacts(audio.generationLedger) }
        try requireCaptureRecording(try samples.sync { try producer.finish() } == nil, "Native producer closure minted twice")
        let actualValidatedIngressSamples = encoded.sync { ingress.accepted }; io.sync {}
        let result = RecordingReply<CaptureRecording.Completion>(), published = RecordingReply<Result<Void, Error>>()
        worker.finish(producers: closure, recording: { result.put($0) }, publishing: { published.put($0) })
        if holdCommit {
            try requireCaptureRecording(headerEntered.wait(timeout: .now() + 8) == .success, "Actual final-header IO was not held")
            if lateDrop { try requireCaptureRecording(gate.nativeDrop(token), "Queued native drop did not latch after Stop") }
            if recordingLoss { recorder.markIncomplete(CaptureFailure(message: "Deliberate recording-only terminal loss")) }
            if lateEncoded {
                let unit = try unwrap(trace.firstUnit)
                try requireCaptureRecording(!worker.offer(unit, token: token, audioGeneration: 1), "Late native output was accepted")
            }
            if lateTiming { worker.audioTiming(.failed) }
            releaseHeader.signal()
        }
        let completed = try result.wait()
        let expectedVisibleFile = selection == .legacy || expectedComplete
        try requireCaptureRecording(completed.cleanupComplete && completed.finalised == expectedComplete &&
            (completed.fileURL != nil) == expectedVisibleFile, "Incorrect concrete producer finalisation outcome")
        try requireCaptureRecording(!published.ready && !transport.cleanupComplete, "Producer recording waited for transport")
        transport.releaseClose(); try published.wait().get()
        let media = root.appendingPathComponent(identity.filename), before = try Data(contentsOf: media)
        let repeated = RecordingReply<CaptureRecording.Completion>()
        worker.finish(producers: closure, recording: { repeated.put($0) }, publishing: { _ in })
        let joined = try repeated.wait()
        try requireCaptureRecording(joined.finalised == expectedComplete && joined.cleanupComplete &&
            (try Data(contentsOf: media)) == before && transport.counts.finishes == 1, "Duplicate finish repeated terminal IO")
        let entry = try unwrap(try catalogue.scan().first { $0.segmentID == identity.segmentID })
        try requireCaptureRecording(entry.state == (expectedComplete ? .finalised : .incomplete), "Producer catalogue classification")
        try trace.check()
        return ["name": name, "passed": true, "finalised": completed.finalised, "file": identity.filename, "actualNativeEOF": nativeEOF,
            "actualNativeAudioLedger": nativeLedger, "originalAudioPackets": trace.packetSnapshot,
            "originalAudioPacketTimings": trace.packetTimingSnapshot,
            "originalAudioPacketClock": "APPLICATION_ASSIGNED_CODED_CMSAMPLEBUFFER; NOT_CONVERTER_NATIVE_TIMESTAMP",
            "originalVideoPackets": trace.videoPacketSnapshot,
            "originalVideoPacketClock": "ADAPTER_OUTPUT_CMSAMPLEBUFFER_AFTER_VT_CALLBACK_AND_90K_RETIMING",
            "actualValidatedIngressSamples": actualValidatedIngressSamples,
            "actualVideoImplementation": (video?.implementation as Any?) ?? NSNull(),
            "sourceAudioUnits": trace.packetSnapshot.count, "hasVideo": av, "nativeVideoFramesSubmitted": av ? 10 : 0,
            "recordingClosedBeforeTransport": true, "normalStopCancellationPresent": true,
            "producerCapabilitySingleUse": true, "duplicateFinishPreservedBytes": true,
            "publishingFailures": trace.counters.publishing, "bytes": before.count,
            "backend": selection == .legacy ? "legacy" : "sdkCheckpointed",
            "scope": "Generated native audio/video producer closure and selected real recorder file, empty stopped session, no camera/device acceptance"]
    }
    /// Cold inspection of the same real encoder after producer finalisation.
    /// Values come from its native prime/input/EOF ledger, never file duration,
    /// decoder length differences or fixture parameters. No receipt is minted.
    private static func nativeLedgerFacts(_ value: AudioRecordingTiming.LedgerSnapshot) throws -> [String: Any] {
        try requireCaptureRecording(value.finished && !value.failed && value.activeGeneration == nil,
            "Actual native producer ledger did not close")
        guard let file = value.completed, let generation = value.lastGeneration else {
            throw CaptureFailure(message: "Actual completed native ledger is unavailable")
        }
        return ["finished": value.finished, "failed": value.failed,
            "completedFile": ["generationCount": file.generationCount,
                "sourceFramesAt48k": file.sourceFramesAt48k, "intentionalStandbyFrames": file.intentionalStandbyFrames,
                "encodedFrames": file.encodedFrames, "firstLeadingFrames": file.leadingFrames,
                "lastTrailingFrames": file.trailingFrames],
            "lastGeneration": ["generation": generation.generation, "inputFrames": generation.inputFrames,
                "rawFrames": generation.rawFrames, "zeroFrames": generation.zeroFrames, "inputRate": generation.inputRate,
                "encodedFrames": generation.encodedFrames, "leadingFrames": generation.leadingFrames,
                "trailingFrames": generation.trailingFrames, "sourceMapping": generation.sourceMapping.rawValue],
            "scope": "Actual closed encoder ledger only; application admission and decoder presentation remain separate"]
    }
    private static func nativeEOFFacts(_ value: AudioRecordingTiming.NativeEOFObservation?) -> [String: Any] {
        guard let value else { return ["observed": false] }
        return ["observed": true, "generation": value.generation, "inputEOSDelivered": value.inputEOSDelivered,
            "converterCalls": value.calls, "lastOutputStatus": value.lastOutputStatus?.rawValue ?? "UNSEEN",
            "lastPacketCount": value.lastPacketCount, "lastByteLength": value.lastByteLength,
            "nativeEOFComplete": value.complete]
    }
    private static func unwrap<T>(_ value: T?) throws -> T {
        guard let value else { throw CaptureFailure(message: "Generated fixture missing actual value") }; return value
    }
    private static func feed(_ encoder: NativeAudioEncoder, token: NativeAudioEncoder.InputToken,
                             source: AVAudioFormat, start: Int64, frames: Int, ingress: DispatchQueue) throws {
        try requireCaptureRecording(frames > 0 && frames <= 200_000, "Generated input bound")
        for offset in stride(from: 0, to: frames, by: 480) {
            let count = min(480, frames - offset), time = CMTime(value: start + Int64(offset), timescale: 48_000)
            try encoder.encode(pcm(source, time: time, frames: count), time: time, token: token)
            ingress.sync {} // Test-only held-recording control, no real-time claim.
        }
    }
    private static func format(_ rate: Int = 48_000, _ channels: UInt32 = 1) throws -> AVAudioFormat {
        guard let value = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(rate), channels: channels, interleaved: true) else {
            throw CaptureFailure(message: "Generated PCM format")
        }; return value
    }
    private static func pcm(_ format: AVAudioFormat, time: CMTime, frames: Int) throws -> CMSampleBuffer {
        try requireCaptureRecording((1...480).contains(frames), "Generated PCM bound")
        let channels = Int(format.channelCount)
        // Deterministic, nonperiodic synthetic waveform. No input file or device.
        var words = [Int16](repeating: 0, count: frames * channels)
        for n in 0..<frames {
            let tick = UInt64(n) &+ UInt64(max(0, time.value))
            let value = Int16(Int((tick &* 1664525 &+ 1013904223) % 20001) - 10000)
            for channel in 0..<channels { words[n * channels + channel] = value }
        }
        let bytes = words.count * MemoryLayout<Int16>.size
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: bytes,
            blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0, dataLength: bytes,
            flags: 0, blockBufferOut: &block) == noErr, let block else { throw CaptureFailure(message: "Generated PCM storage") }
        let copied = words.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: bytes) }
        try requireCaptureRecording(copied == noErr, "Generated PCM copy")
        var description: CMAudioFormatDescription?
        guard CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: format.streamDescription,
            layoutSize: 0, layout: nil, magicCookieSize: 0, magicCookie: nil, extensions: nil,
            formatDescriptionOut: &description) == noErr, let description else { throw CaptureFailure(message: "Generated PCM description") }
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: Int32(format.sampleRate)), presentationTimeStamp: time, decodeTimeStamp: time)
        var sampleSize = channels * MemoryLayout<Int16>.size
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: description,
            sampleCount: frames, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize, sampleBufferOut: &sample) == noErr, let sample else { throw CaptureFailure(message: "Generated PCM sample") }
        return sample
    }
    static func main() {
        guard CommandLine.arguments.count == 2 else { exit(2) }
        let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true).standardizedFileURL
        guard root == root.resolvingSymlinksInPath(),
              (try? String(contentsOf: root.appendingPathComponent(".sapflow-sdk-recorder-fixture"))) == "generated-sdk-recorder-v1\n" else { exit(2) }
        var rows: [[String: Any]] = []
        for name in ["healthy", "mixed-zero-raw", "publishing-independent-failure", "missing-file-EOF", "recording-queue-failure"] {
            do {
                let folder = root.appendingPathComponent(name, isDirectory: true)
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700])
                rows.append(try run(name, root: folder))
            } catch { rows.append(["name": name, "passed": false, "error": String(describing: error)]) }
        }
        var producerRows: [[String: Any]] = []
        for name in ["producer-audio", "producer-av", "producer-publishing-failure", "producer-late-native-drop", "producer-recording-loss", "producer-input-not-closed", "producer-wrong-worker", "producer-late-encoded-output", "producer-late-timing", "producer-wrong-ticket"] {
            do {
                let folder = root.appendingPathComponent(name, isDirectory: true)
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
                producerRows.append(try producer(name, root: folder))
            } catch { producerRows.append(["name": name, "passed": false, "error": String(describing: error)]) }
        }
        var legacyRows: [[String: Any]] = []
        for name in ["producer-audio", "producer-av", "producer-recording-loss", "producer-late-native-drop", "producer-input-not-closed", "producer-wrong-worker", "producer-wrong-ticket"] {
            do {
                let folder = root.appendingPathComponent("legacy-" + name, isDirectory: true)
                try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
                legacyRows.append(try producer(name, root: folder, selection: .legacy))
            } catch { legacyRows.append(["name": name, "passed": false, "error": String(describing: error)]) }
        }
        let passed = rows.count == 5 && rows.allSatisfy { $0["passed"] as? Bool == true }
            && producerRows.count == 10 && producerRows.allSatisfy { $0["passed"] as? Bool == true }
            && legacyRows.count == 7 && legacyRows.allSatisfy { $0["passed"] as? Bool == true }
        do {
            let result: [String: Any] = ["passed": passed, "cases": rows, "producerCases": producerRows, "legacyProducerCases": legacyRows, "generatedOnly": true,
                "scope": "Actual native AAC, existing ingress validated handles, SDK adapter/FS and fake transport only. Five stored-input controls remain incomplete. Separate producer cases execute the same concrete native close as CaptureEngine; no camera/device, app/default switch, presentation-equivalence or aggregate2s proof."]
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.prettyPrinted, .sortedKeys]))
            FileHandle.standardOutput.write(Data([10]))
        } catch { exit(2) }
        exit(passed ? 0 : 1)
    }
}
