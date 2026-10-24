// Generated-only actual native AAC -> SDK validation -> recording ownership.
// No microphone, camera, sockets or caller-provided media. The shell creates
// the H.264 colour source and an empty marked private directory.
import Foundation
import AVFoundation
import CryptoKit
import Sapflow

private func requireOwnership(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !value() { throw CaptureFailure(message: message) }
}

private final class OwnershipResult<Value> {
    private let condition = NSCondition()
    private var value: Value?
    private(set) var deliveries = 0
    func put(_ next: Value) { condition.lock(); deliveries += 1; if value == nil { value = next }; condition.broadcast(); condition.unlock() }
    func wait(_ seconds: TimeInterval = 8) throws -> Value {
        let limit = Date().addingTimeInterval(seconds)
        condition.lock(); defer { condition.unlock() }
        while value == nil { guard condition.wait(until: limit) else { throw CaptureFailure(message: "Generated ownership callback deadline") } }
        return value!
    }
    var isReady: Bool { condition.lock(); defer { condition.unlock() }; return value != nil }
}
private final class OwnershipTrace {
    private let lock = NSLock()
    private var errors: [String] = []
    private var receipts: [[String: Any]] = []
    func error(_ value: String) { lock.lock(); defer { lock.unlock() }; if errors.count < 16 { errors.append(value) } }
    func boundary(_ value: CaptureAdmission.MediaBoundary) {
        lock.lock(); defer { lock.unlock() }
        if receipts.count < 128 { receipts.append(["pts":value.pts,"duration":value.duration,
            "generation":value.audioGeneration ?? 0,"sinks":value.acceptedSinks.rawValue,"codedIdr":value.codedIdr]) }
    }
    var failures: [String] { lock.lock(); defer { lock.unlock() }; return errors }
    var boundaries: [[String: Any]] { lock.lock(); defer { lock.unlock() }; return receipts }
}

/// The only fake is transport. Native codecs, Rust payload validation, queue
/// admission, writer and catalogue are the production classes.
private final class OwnershipSink: EncodedPublishingSink {
    private let lock = NSLock()
    private var endObserver: ((UInt64)->Void)?
    private var ended = false
    private var closed = false
    private var completion: ((Result<Void,Error>)->Void)?
    private var configurations = 0
    private var samples = 0
    private var firstSample: MediaSample?
    private var calls = 0
    private var epoch = 1
    private var finishedEpochs: [Int] = []
    let finishRequested = OwnershipResult<Int>()
    var status: String { "Generated network-free destination" }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return closed }
    func start() throws {}
    func configure(video: MediaCodec?, audio: MediaCodec) throws { lock.lock(); configurations += 1; lock.unlock() }
    func offer(_ sample: MediaSample) throws {
        lock.lock(); defer { lock.unlock() }
        guard !ended else { throw CaptureFailure(message: "Publishing consumed a unit after its cold End") }
        if firstSample == nil { firstSample = sample }
        samples += 1
    }
    func observeBroadcastEnd(_ observer: @escaping (UInt64)->Void) {
        lock.lock(); let duplicate = endObserver != nil; endObserver = observer; let replay = ended; lock.unlock()
        precondition(!duplicate, "Exactly one composition observer")
        if replay { observer(DispatchTime.now().uptimeNanoseconds) }
    }
    func endDestination() {
        lock.lock(); let first = !ended; ended = true; let callback = first ? endObserver : nil; lock.unlock()
        callback?(DispatchTime.now().uptimeNanoseconds)
    }
    func stop() { endDestination() }
    func finishEpoch(completion: @escaping (Result<Void, Error>)->Void) {
        lock.lock(); calls += 1; let count = calls; finishedEpochs.append(epoch); self.completion = completion; lock.unlock()
        finishRequested.put(count) // Deliberately withhold actual-close callback.
    }
    func releaseActualClose() {
        lock.lock(); closed = true; let callback = completion; completion = nil; lock.unlock()
        callback?(.success(()))
    }
    func beginSyntheticSuccessorEpoch() { lock.lock(); epoch += 1; closed = false; lock.unlock() }
    var originalFirstSample: MediaSample? { lock.lock(); defer { lock.unlock() }; return firstSample }
    var facts: [String:Any] { lock.lock(); defer { lock.unlock() }; return ["offeredSamples":samples,
        "configurations":configurations,"finishCalls":calls,"finishedEpochs":finishedEpochs,"currentEpoch":epoch] }
}

@main
enum StandbyOutputOwnershipCheck {
    static func main() async {
        do {
            try requireOwnership(CommandLine.arguments.count == 2, "Use generated ownership launcher")
            let root = URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL
            try requireOwnership(root == root.resolvingSymlinksInPath(), "Owned root symlink")
            try requireOwnership(try String(contentsOf:root.appendingPathComponent(".sapflow-output-ownership-fixture")) == "generated-output-ownership-v1\n", "Owned root marker")
            let oldOrder = ProcessInfo.processInfo.environment["SAPFLOW_PROVE_OLD_STOP_ORDER"] == "1"
            let oldPublish = ProcessInfo.processInfo.environment["SAPFLOW_PROVE_OLD_PUBLISH_RECEIPT"] == "1"
            let cases = oldOrder ? ["held-stop"] : oldPublish ? ["publishing-refused"] : ["open-fanout","held-stop","held-auto","end-recording","end-invalid-aac","publishing-refused","publishing-pre-idr"]
            var rows: [[String:Any]] = []
            for name in cases {
                let folder=root.appendingPathComponent(name,isDirectory:true)
                try FileManager.default.createDirectory(at:folder,withIntermediateDirectories:false)
                do { rows.append(try await run(root:folder,fixtureRoot:root,name:name,oldOrder:oldOrder)) }
                catch { rows.append(["name":name,"failed":true,"error":String(describing:error)]) }
            }
            let idr = try await falseIdr(root:root)
            let summary: [String:Any] = ["generatedOnly":true,"reconstructedOldStopOrder":oldOrder,
                "reconstructedOldPublishReceipt":oldPublish,"cases":rows,"codedIdrMismatchRejected":idr,"scope":"Actual native AAC and production queue/Rust/Recorder owners. Transport spy only; no capture driver, physical privacy or real-time pacing acceptance. Independent presentation checked separately by launcher."]
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject:summary,options:[.prettyPrinted,.sortedKeys]))
            FileHandle.standardOutput.write(Data([10]))
            if rows.contains(where:{$0["failed"] as? Bool == true}) { exit(1) }
        } catch { FileHandle.standardError.write(Data(("Generated ownership failure: \(error)\n").utf8)); exit(1) }
    }

    private static func admitted(_ gate:CaptureAdmission, profile:CaptureIntent, initialStandby:Bool) throws -> (CaptureAdmission.Preparation,CaptureAdmission.MediaRun) {
        try requireOwnership(gate.selectInitialStandby(initialStandby),"Initial selection unavailable")
        guard let preparation=gate.reservePreparation(profile:profile) else { throw CaptureFailure(message:"Prepare refused") }
        try requireOwnership(gate.attached(preparation) && gate.beginPermissions(preparation) && gate.prepared(preparation),"Prepare ownership failed")
        guard let start=gate.reserveStart(preparation),let run=gate.reserveMediaRun(start) else { throw CaptureFailure(message:"Native run reservation failed") }
        try requireOwnership(gate.commitMediaRun(run,start:start,nowNs:DispatchTime.now().uptimeNanoseconds,broadcasting:true),"Start did not commit")
        return (preparation,run)
    }

    private static func run(root:URL,fixtureRoot:URL,name:String,oldOrder:Bool) async throws -> [String:Any] {
        let recovery=name == "publishing-pre-idr", refusal=name == "publishing-refused"
        let av=name == "held-auto" || recovery, ending=name.hasPrefix("end-"), invalid=name == "end-invalid-aac"
        let zeroOnly=ending || recovery || refusal, frames=recovery ? 18_513 : 14_417
        var checkpointFailure: Error?
        var preIdrAudio=0, preIdrRefusal=false, recovered=false, refused=false
        let admission=CaptureAdmission()
        let profile:CaptureIntent=av ? .audioVideo(.sd480p30) : .audioOnly48kMono
        let (preparation,run)=try admitted(admission,profile:profile,initialStandby:zeroOnly)
        guard let token=admission.mediaToken(run,kind:zeroOnly ? .zero : .raw),
              let format=AVAudioFormat(commonFormat:.pcmFormatInt16,sampleRate:48_000,channels:1,interleaved:true) else { throw CaptureFailure(message:"Native token/format missing") }
        let catalogue=try RecordingCatalogue(root:root.appendingPathComponent("private",isDirectory:true))
        defer { try? catalogue.close() }
        let identity=try catalogue.newShow(profile:profile).nextIdentity()
        let writing=try catalogue.begin(identity:identity)
        let recordingQueue=DispatchQueue(label:"fixture.real-recording")
        let trace=OwnershipTrace()
        let recorder=try CompressedRecorder(receipt:writing,queue:recordingQueue,hasVideo:av) { trace.error("recorder: \($0)") }
        let ingress=RustEncodedIngress();ingress.reset(hasVideo:av)
        let destination=OwnershipSink();ingress.destination=destination
        let queue=DispatchQueue(label:"fixture.held-encoded-owner")
        let worker=try IngressWorker(run:run,ingress:ingress,recorder:recorder,admission:admission,
            boundary:{ value in trace.boundary(value); _ = admission.recordBoundary(value,nowNs:DispatchTime.now().uptimeNanoseconds) },
            terminal:{ _ = admission.sinkTerminated($0) },mediaFailure:{ trace.error("media: \($0)") },
            failure:{ trace.error("publishing: \($0)") },statistics:{ _,_ in },publishingQueueBytes:refusal ? 1 : 4*1024*1024,queue:queue)
        destination.observeBroadcastEnd { [weak worker] instant in
            _ = worker?.closePublishingAdmission(); admission.broadcastEnded(preparation,nowNs:instant)
        }
        try requireOwnership(worker.startPublishing(),"Spy destination could not start")
        let hold=DispatchSemaphore(value:0)
        if name != "open-fanout" && !recovery { queue.async { _ = hold.wait(timeout:.now()+10) } }
        defer { hold.signal(); destination.releaseActualClose() }
        var nativeUnits:[CompressedUnit]=[],eofEvents=0,outputRefusal=false
        weak var callbackEncoder:NativeAudioEncoder?
        let encoder=try NativeAudioEncoder(output:{ unit in
            nativeUnits.append(unit)
            if nativeUnits.count > 96 || !worker.offer(unit,token:token,audioGeneration:callbackEncoder?.lastOutputGeneration) { outputRefusal=true }
        },meter:{ _ in },recordingTiming:{ event in
            switch event { case .ended,.endedGenerations:eofEvents += 1; default:break }
            worker.audioTiming(event)
        })
        callbackEncoder=encoder
        defer {
            let facts:[String:Any] = ["nativeUnits":nativeUnits.count,"nativeEOFEvents":eofEvents,
                "ledger":String(describing:encoder.generationLedger),"queueRefused":outputRefusal]
            if let bytes=try? JSONSerialization.data(withJSONObject:facts,options:[.sortedKeys]) {
                try? bytes.write(to:root.appendingPathComponent("producer-diagnostics.local.json"))
            }
        }
        var zeroInput: NativeAudioEncoder.InputToken?
        if zeroOnly {
            guard let request=admission.privacyView?.request else { throw CaptureFailure(message:"Initial zero request missing") }
            let zero=try encoder.beginStandby(format:format,at:.zero)
            zeroInput=zero
            try requireOwnership(admission.zeroOwnerReady(request,generation:zero.generation,retiredGeneration:nil,nowNs:DispatchTime.now().uptimeNanoseconds),"Zero generation ownership refused")
            var remaining=frames-(recovery ? 4096 : 0)
            while remaining > 0 { let count=min(4096,remaining);try encoder.encodeSilence(frames:UInt32(count),token:zero);remaining -= count }
        } else {
            guard let native=encoder.inputToken else { throw CaptureFailure(message:"Native raw token missing") }
            try feed(encoder,token:native,format:format,start:.zero,frames:frames,kind:0)
        }
        preIdrAudio=av ? nativeUnits.count : 0
        if recovery {
            queue.sync {}
            preIdrRefusal=preIdrAudio >= 2 && !trace.boundaries.isEmpty &&
                trace.boundaries.allSatisfy { ($0["sinks"] as? UInt8 ?? 0) & CaptureAdmission.SinkMask.publishing.rawValue == 0 } &&
                admission.requiredSinkMask(run:run).contains(.publishing) && admission.privacyView?.phase != .standby &&
                (destination.facts["offeredSamples"] as? Int) == 0
            try requireOwnership(preIdrRefusal,"Audio before actual IDR claimed publishing/standby")
        }
        let videos=av ? try await generatedVideo(root:fixtureRoot) : []
        let videoToken=recovery ? admission.mediaToken(run,kind:.slate) : token
        guard let videoToken else { throw CaptureFailure(message:"Generated video provenance missing") }
        for unit in videos { try requireOwnership(worker.offer(unit,token:videoToken),"Generated video queue refused") }
        if recovery {
            queue.sync {}
            try requireOwnership(admission.privacyView?.phase != .standby,"IDR alone falsely confirmed required audio")
            guard let zeroInput else { throw CaptureFailure(message:"Actual zero owner missing") }
            try encoder.encodeSilence(frames:4096,token:zeroInput)
            queue.sync {}
            recovered=admission.privacyView?.phase == .standby && admission.requiredSinkMask(run:run).contains(.publishing)
            try requireOwnership(recovered,"Actual IDR plus fresh zero AUs did not confirm local admission")
        }
        var rotation:CaptureAdmission.Rotation?
        var stopped:CaptureAdmission.Stop?
        if name == "held-auto" { rotation=admission.beginRotation(preparation);try requireOwnership(rotation != nil,"Actual Auto rotation was refused") }
        else if name == "held-stop" { stopped=admission.stop() }
        try encoder.finish()
        try requireOwnership(eofEvents == 1 && !outputRefusal,"Native EOF/actual queue admission failed")
        if name == "open-fanout" {
            queue.sync {}
            stopped=admission.stop();destination.endDestination()
        }
        if ending { destination.endDestination() }
        if refusal {
            hold.signal(); queue.sync {}
            refused = !trace.boundaries.isEmpty && trace.boundaries.allSatisfy {
                ($0["sinks"] as? UInt8 ?? 0) & CaptureAdmission.SinkMask.publishing.rawValue == 0
            } && admission.requiredSinkMask(run:run).contains(.publishing) && admission.privacyView?.phase != .standby
            if !refused {
                // Historical-worker control: reuse its SAME already validated
                // handle to prove that the real SDK's one-byte branch refuses.
                // There is no additional codec/sample payload validation.
                guard let sample=destination.originalFirstSample else { throw CaptureFailure(message:"Historical branch overclaim lost its original validated sample") }
                let actual=try NativeSampleFanout(publishingBytes:1,recordingBytes:1,maxMediaMs:1000,hasVideo:false)
                try actual.closeRecordingAdmission()
                let outcome=try actual.push(sample:sample,nowNs:DispatchTime.now().uptimeNanoseconds)
                try requireOwnership(!outcome.publishingQueued,"Real one-byte SDK control unexpectedly accepted")
                try actual.cancel()
                checkpointFailure=CaptureFailure(message:"Publishing branch claimed admission despite 1-byte SDK publishing limit")
            }
        }
        if invalid {
            let bad=try malformedAAC(like:nativeUnits[0],time:CMTime(value:Int64(nativeUnits.count * 1024),timescale:48_000))
            try requireOwnership(worker.offer(bad,token:token,audioGeneration:callbackEncoder?.lastOutputGeneration ?? 1),"Malformed test did not reach real validation owner")
        }
        let file=OwnershipResult<CompressedRecorder.Completion>(),network=OwnershipResult<Result<Void,Error>>()
        if oldOrder {
            // Exact former Engine call order, deliberately expected to FAIL the
            // same strict original-payload oracle. No production overlay.
            recorder.finish { file.put($0) }
            _ = try file.wait()
            worker.finish { network.put($0) }
        } else { worker.finish(recording:{file.put($0)},publishing:{network.put($0)}) }
        hold.signal()
        let saved=try file.wait()
        _ = try destination.finishRequested.wait()
        try requireOwnership(!network.isReady && !destination.cleanupComplete,"Transport completion was not actually withheld")
        try requireOwnership(saved.cleanupComplete,"Actual writer/catalogue did not close")
        destination.releaseActualClose()
        if case .failure(let error)=try network.wait() { throw error }
        let rows=try catalogue.scan()
        guard let url=saved.fileURL else { throw CaptureFailure(message:"Recording did not preserve admitted native AAC/EOF prefix") }
        try requireOwnership(saved.finalised == !invalid && rows.count == 1 && rows[0].state == (invalid ? .incomplete : .finalised),"Durable recording outcome differs from real media fault")
        let asset=AVURLAsset(url:url)
        let audio=try await asset.loadTracks(withMediaType:.audio)
        try requireOwnership(audio.count == 1,"Actual recording audio track missing")
        let native=try AudioOnlyFixtureCheck.readEncodedPackets(asset:asset,track:audio[0])
        let expected=try nativeUnits.map { try AudioOnlyFixtureCheck.payloadHash($0.buffer) }
        let stored=try JSONSerialization.data(withJSONObject:native["packetHashes"]!,options:[.sortedKeys])
        let produced=try JSONSerialization.data(withJSONObject:expected,options:[.sortedKeys])
        try requireOwnership(stored == produced,"Original native AAC payloads/order changed or tail was lost")
        let nativePCM=root.appendingPathComponent("native.pcm")
        let decoded=try AudioOnlyFixtureCheck.readPCM(asset:asset,track:audio[0],destination:nativePCM)
        let pcmBytes=try Data(contentsOf:nativePCM)
        let values=pcmBytes.withUnsafeBytes { Array($0.bindMemory(to:Int16.self)) }
        try requireOwnership(values.count >= frames,"Native reader lost original source frames")
        var sourceDot=0.0,sourceEnergy=0.0,decodedEnergy=0.0
        for n in 0..<frames {
            let reference=zeroOnly ? 0.0 : Double(toneValue(frame:n,rate:48_000,kind:0))
            let actual=Double(values[n]);sourceDot += reference*actual;sourceEnergy += reference*reference;decodedEnergy += actual*actual
        }
        let correlation=zeroOnly ? 1.0 : sourceDot / sqrt(sourceEnergy*decodedEnergy)
        try requireOwnership(zeroOnly ? values.allSatisfy({$0 == 0}) : correlation > 0.995,"Actual native source waveform changed")
        let ledger=encoder.generationLedger
        guard let extent=ledger.completed else { throw CaptureFailure(message:"Actual native EOF extent missing") }
        let expectedFrames=invalid ? Int(extent.encodedFrames)-Int(extent.leadingFrames) : frames
        try requireOwnership(decoded == expectedFrames,"Native PCM/EOF presentation extent changed")
        try requireOwnership(ingress.accepted == UInt64(nativeUnits.count+videos.count),"A native unit was unvalidated or validated twice")
        let facts=destination.facts
        // Ordinary Stop/Auto may drain the already accepted compressed prefix;
        // explicit End retires publishing before this held queue is released.
        if let checkpointFailure { throw checkpointFailure }
        let expectedPublished = ending || refusal ? 0 : nativeUnits.count - preIdrAudio + videos.count
        try requireOwnership((facts["offeredSamples"] as? Int) == expectedPublished,"Publishing End/accepted-prefix gate consumed an unexpected unit")
        if ending && !invalid { try requireOwnership(admission.privacyView?.phase == .standby,"Recording-only actual zero boundaries did not confirm") }
        try requireOwnership(invalid ? trace.failures.count >= 1 : trace.failures.isEmpty,"Unexpected/missing real worker media fault")
        if let rotation {
            admission.mediaRunClosed(run)
            guard let start=admission.reserveRotationStart(rotation),let successor=admission.reserveMediaRun(start) else { throw CaptureFailure(message:"Clean actual EOF did not permit Auto child") }
            try requireOwnership(admission.commitMediaRun(successor,start:start,nowNs:DispatchTime.now().uptimeNanoseconds,broadcasting:true),"Auto child did not commit")
        }
        destination.beginSyntheticSuccessorEpoch()
        let lateFile=OwnershipResult<CompressedRecorder.Completion>(),lateNetwork=OwnershipResult<Result<Void,Error>>()
        worker.finish(recording:{lateFile.put($0)},publishing:{lateNetwork.put($0)})
        try requireOwnership(try lateFile.wait().cleanupComplete,"Late file join lost cached cleanup")
        if case .failure(let error)=try lateNetwork.wait() { throw error }
        try requireOwnership((destination.facts["finishCalls"] as? Int) == 1,"Old worker finish retired a successor destination epoch")
        if stopped == nil { stopped=admission.stop() }
        admission.mediaRunClosed(run)
        return ["name":name,"recording":String(url.path.dropFirst(fixtureRoot.path.count+1)),
            "audioPackets":expected,"videoPackets":try videos.map{try AudioOnlyFixtureCheck.payloadHash($0.buffer)},
            "nativePCM":String(nativePCM.path.dropFirst(fixtureRoot.path.count+1)),"nativePCMFrames":decoded,
            "validInputFrames":frames,"nativeSourceCorrelation":correlation,"zeroOnly":zeroOnly,"actualNativeEOFEvents":eofEvents,"nativeLeadingFrames":extent.leadingFrames,
            "nativeTrailingFrames":extent.trailingFrames,"nativeCodedFrames":extent.encodedFrames,
            "validatedExactlyOnce":true,"validatedUnits":ingress.accepted,"actualQueueAdmittedUnits":nativeUnits.count+videos.count,
            "exactNativeStoredAAC":true,"durableState":rows[0].state.rawValue,"recorderClosedBeforeTransport":true,
            "lateJoinDidNotFinishSuccessor":true,"transport":destination.facts,"boundaries":trace.boundaries,
            "expectedMediaFailure":invalid,"failures":trace.failures,"nativePreIdrAudioUnits":preIdrAudio,
            "actualPublishingRefusalVerified":refused,"preIdrRefusalVerified":preIdrRefusal,
            "postIdrLocalConfirmationVerified":recovered]
    }

    private static func malformedAAC(like unit:CompressedUnit,time:CMTime) throws -> CompressedUnit {
        let bytes:[UInt8]=[0xff,0xf1,0x4c,0x40,0x00,0xff,0xfc]
        var block:CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator:kCFAllocatorDefault,memoryBlock:nil,blockLength:bytes.count,blockAllocator:kCFAllocatorDefault,customBlockSource:nil,offsetToData:0,dataLength:bytes.count,flags:0,blockBufferOut:&block) == noErr,let block else { throw CaptureFailure(message:"Generated invalid AU storage") }
        try requireOwnership(bytes.withUnsafeBytes{CMBlockBufferReplaceDataBytes(with:$0.baseAddress!,blockBuffer:block,offsetIntoDestination:0,dataLength:bytes.count)} == noErr,"Generated invalid AU copy")
        var timing=CMSampleTimingInfo(duration:unit.duration,presentationTimeStamp:time,decodeTimeStamp:time),size=bytes.count
        var sample:CMSampleBuffer?
        try requireOwnership(CMSampleBufferCreateReady(allocator:kCFAllocatorDefault,dataBuffer:block,formatDescription:CMSampleBufferGetFormatDescription(unit.buffer),sampleCount:1,sampleTimingEntryCount:1,sampleTimingArray:&timing,sampleSizeEntryCount:1,sampleSizeArray:&size,sampleBufferOut:&sample) == noErr,"Generated invalid AU sample")
        guard let sample else { throw CaptureFailure(message:"Generated invalid AU missing") }
        return CompressedUnit(buffer:sample,isVideo:false,keyframe:false,time:time,duration:unit.duration,configuration:unit.configuration)
    }
    private static func falseIdr(root:URL) async throws -> Bool {
        let source=try await generatedVideo(root:root)
        guard let original=source.first,original.keyframe else { throw CaptureFailure(message:"Generated IDR fixture missing") }
        let ingress=RustEncodedIngress()
        let bad=CompressedUnit(buffer:original.buffer,isVideo:true,keyframe:false,time:original.time,duration:original.duration,configuration:original.configuration)
        var rejected=false
        do { _ = try ingress.validate(bad) } catch { rejected=true }
        try requireOwnership(rejected && ingress.accepted == 0,"Real Rust validator trusted false IDR flag")
        return true
    }
    private static func toneValue(frame: Int, rate: Int32, kind: Int) -> Int16 {
        let t = Double(frame) / Double(rate)
        let value: Double
        if kind == 0 {
            value = 0.28 * sin(2 * .pi * (113 * t + 173 * t * t)) + 0.18 * sin(2 * .pi * (997 * t + 71 * t * t))
        } else {
            value = 0.23 * sin(2 * .pi * (317 * t + 127 * t * t)) + 0.21 * sin(2 * .pi * (1297 * t + 53 * t * t))
        }
        return Int16((value * 32767).rounded())
    }

    private static func pcm(format: AVAudioFormat, start: CMTime, firstFrame: Int, frames: Int, kind: Int) throws -> CMSampleBuffer {
        let channels = Int(format.channelCount)
        var bytes = [Int16](repeating: 0, count: frames * channels)
        for n in 0..<frames {
            let value = toneValue(frame: firstFrame + n, rate: Int32(format.sampleRate), kind: kind)
            for channel in 0..<channels { bytes[n * channels + channel] = value }
        }
        let byteCount = bytes.count * MemoryLayout<Int16>.size
        var block: CMBlockBuffer?
        let allocated = CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil,
            blockLength: byteCount, blockAllocator: kCFAllocatorDefault, customBlockSource: nil,
            offsetToData: 0, dataLength: byteCount, flags: 0, blockBufferOut: &block)
        guard allocated == noErr, let block else { throw CaptureFailure(message: "Generated PCM allocation failed") }
        let copied = bytes.withUnsafeBytes { CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: byteCount) }
        try requireOwnership(copied == noErr, "Generated PCM storage failed")
        var description: CMAudioFormatDescription?
        try requireOwnership(CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault,
            asbd: format.streamDescription, layoutSize: 0, layout: nil, magicCookieSize: 0, magicCookie: nil,
            extensions: nil, formatDescriptionOut: &description) == noErr, "Generated PCM description failed")
        guard let description else { throw CaptureFailure(message: "Generated PCM description unavailable") }
        let rate = Int32(format.sampleRate)
        let time = CMTimeAdd(start, CMTime(value: Int64(firstFrame), timescale: rate))
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: rate), presentationTimeStamp: time, decodeTimeStamp: time)
        var sample: CMSampleBuffer?
        var size = channels * MemoryLayout<Int16>.size
        let made = CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: block,
            formatDescription: description, sampleCount: frames, sampleTimingEntryCount: 1,
            sampleTimingArray: &timing, sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample)
        guard made == noErr, let sample else { throw CaptureFailure(message: "Generated PCM sample failed") }
        return sample
    }

    private static func feed(_ encoder: NativeAudioEncoder, token: NativeAudioEncoder.InputToken,
                             format: AVAudioFormat, start: CMTime, frames: Int, kind: Int) throws {
        for at in stride(from: 0, to: frames, by: 480) {
            let sample = try pcm(format: format, start: start, firstFrame: at, frames: min(480, frames - at), kind: kind)
            try encoder.encode(sample, time: CMSampleBufferGetPresentationTimeStamp(sample), token: token)
        }
    }

    private static func generatedVideo(root: URL) async throws -> [CompressedUnit] {
        // The launcher creates this H.264 colour fixture from lavfi in its
        // empty owned directory. There is no caller-supplied media input.
        let asset = AVURLAsset(url: root.appendingPathComponent("generated-video.mov"))
        let tracks = try await asset.loadTracks(withMediaType: .video)
        try requireOwnership(tracks.count == 1, "Generated video track unavailable")
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: tracks[0], outputSettings: nil)
        output.alwaysCopiesSampleData = false; reader.add(output)
        try requireOwnership(reader.startReading(), "Generated video reader could not start")
        defer { if reader.status == .reading { reader.cancelReading() } }
        var units: [CompressedUnit] = [], emptyMarkers = 0
        while let sample = output.copyNextSampleBuffer() {
            if CMSampleBufferGetNumSamples(sample) == 0 {
                emptyMarkers += 1
                try requireOwnership(emptyMarkers <= 16 && CMSampleBufferGetTotalSampleSize(sample) == 0 &&
                    CMSampleBufferGetDuration(sample) == .zero, "Generated H.264 empty marker bound")
                continue
            }
            guard CMSampleBufferGetNumSamples(sample) == 1, let format = CMSampleBufferGetFormatDescription(sample),
                  CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_H264 else {
                throw CaptureFailure(message: "Generated H.264 sample unavailable")
            }
            func parameter(_ index: Int) throws -> Data {
                var pointer: UnsafePointer<UInt8>?
                var size = 0, count = 0
                var header: Int32 = 0
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index,
                    parameterSetPointerOut: &pointer, parameterSetSizeOut: &size, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &header)
                guard status == noErr, let pointer, size > 0, header == 4 else {
                    throw CaptureFailure(message: "Generated H.264 parameter set unavailable")
                }
                return Data(bytes: pointer, count: size)
            }
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
            let pts = CMSampleBufferGetPresentationTimeStamp(sample)
            let duration = CMSampleBufferGetDuration(sample)
            try requireOwnership(pts.isNumeric && pts == CMTime(value: Int64(units.count), timescale: 30) &&
                duration == CMTime(value: 1, timescale: 30), "Generated H.264 cadence changed")
            units.append(CompressedUnit(buffer: sample, isVideo: true,
                keyframe: (attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true,
                time: pts, duration: duration, configuration: .h264(sps: try parameter(0), pps: try parameter(1))))
            try requireOwnership(units.count <= 64, "Generated H.264 retention bound")
        }
        try requireOwnership(reader.status == .completed && units.count == 15, "Generated H.264 fixture incomplete")
        return units
    }

}
