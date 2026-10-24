// Generated AAC-only marginal publishing receipt microbenchmark. No capture.
import Foundation
import AVFoundation
import Darwin
import Sapflow

private func benchmarkRequire(_ value: @autoclosure () throws -> Bool, _ message: String) throws {
    if try !value() { throw CaptureFailure(message: message) }
}
private final class BenchmarkSink: EncodedPublishingSink {
    private(set) var count: UInt64 = 0
    var status: String { "Generated benchmark no-network sink" }
    func start() throws {}
    @inline(never) func offer(_ sample: MediaSample) throws {
        count += 1
        withExtendedLifetime(sample) {}
    }
    func stop() {}
}
private final class BenchmarkGate {
    private let state = NSLock()
    private var open = true
    @inline(never) func allows() -> Bool { state.lock(); defer { state.unlock() }; return open }
}
@main enum EncodedPublishingBenchmark {
    private static let poolCount = 128
    private static let warmupPairs = 32
    private static let measuredPairs = 400
    private struct Timing { let wall: UInt64; let cpu: UInt64 }
    private static var runDeadline: UInt64 = 0

    static func main() async {
        do {
            try benchmarkRequire(CommandLine.arguments.count == 2, "Use generated benchmark launcher")
            let root=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL
            try benchmarkRequire(root == root.resolvingSymlinksInPath(), "Benchmark root symlink")
            try benchmarkRequire(try String(contentsOf:root.appendingPathComponent(".sapflow-publishing-benchmark")) == "generated-publishing-benchmark-v1\n", "Benchmark root marker")
            runDeadline=DispatchTime.now().uptimeNanoseconds+12_000_000_000
            let units=try await load(root.appendingPathComponent("generated.m4a"))
            let (poolIngress,_)=try ingress()
            let samples=try units.map { try poolIngress.validate($0) }
            try benchmarkRequire(poolIngress.accepted == UInt64(poolCount), "Prevalidated pool was not validated once")
            let payloadBytes=units.reduce(0) { $0+$1.byteCount }
            try benchmarkRequire(units.count == poolCount && payloadBytes <= 4*1024*1024, "Prepared pool bound")
            let controls=try correctness(samples:samples,units:units)
            var results:[[String:Any]]=[],stable=true
            for complete in [false,true] {
                var legacy:[Timing]=[],branch:[Timing]=[],pairedCPU:[Double]=[],pairedWall:[Double]=[]
                for index in 0..<(warmupPairs+measuredPairs) {
                    try benchmarkRequire(DispatchTime.now().uptimeNanoseconds < runDeadline,"Predeclared benchmark runtime budget exhausted")
                    let firstNew=index % 2 != 0
                    let first=try batch(units:units,pool:samples,complete:complete,new:firstNew)
                    let second=try batch(units:units,pool:samples,complete:complete,new:!firstNew)
                    if index >= warmupPairs {
                        let old=firstNew ? second:first, fresh=firstNew ? first:second
                        legacy.append(old);branch.append(fresh)
                        pairedCPU.append((Double(fresh.cpu)-Double(old.cpu))/Double(poolCount))
                        pairedWall.append((Double(fresh.wall)-Double(old.wall))/Double(poolCount))
                    }
                }
                let quarters=(0..<4).map { quarter in
                    pairedCPU[(quarter*100)..<((quarter+1)*100)].reduce(0,+)/100
                }
                let spread=quarters.max()!-quarters.min()!
                let stabilityLimit=max(500,abs(quantiles(pairedCPU)["p50"]!)*0.25)
                stable = stable && spread<=stabilityLimit
                let measurement:[String:Any] = ["scope":complete ? "single-production-validation-plus-publishing":"prevalidated-marginal-publishing",
                    "legacy":distribution(legacy),"realBranch":distribution(branch),
                    "pairedDeltaCPU_nsPerAU":quantiles(pairedCPU),"pairedDeltaWall_nsPerAU":quantiles(pairedWall),
                    "pairs":measuredPairs,"batchAUs":poolCount,"warmupPairs":warmupPairs,
                    "stabilityQuarterMeans_nsPerAU":quarters,"stabilitySpread_nsPerAU":spread,"stabilityLimit_nsPerAU":stabilityLimit,
                    "order":"Alternating legacy/new then new/legacy; fresh queue and warm codec outside every timed batch."]
                results.append(measurement)
                let rawRows=zip(legacy,branch).map { old,fresh in ["legacyCPU_ns":old.cpu,"legacyWall_ns":old.wall,"branchCPU_ns":fresh.cpu,"branchWall_ns":fresh.wall] }
                let retained:[String:Any] = ["summary":measurement,"rawPairs":rawRows]
                try JSONSerialization.data(withJSONObject:retained,options:[.sortedKeys]).write(to:root.appendingPathComponent(complete ? "complete-measurements.local.json":"marginal-measurements.local.json"))
            }
            let output:[String:Any]=["passed":stable,"correctnessPassed":true,"predeclaredStabilityPassed":stable,"generatedOnly":true,"audioOnly":true,
                "poolAUs":poolCount,"nativeCompressedPoolBytes":payloadBytes,"sampleBytes":units.map{$0.byteCount},
                "controls":controls,"measurements":results,
                "timedWork":"New includes actual NativeSampleFanout push/pop,2monotonic clock reads,one extra post-pop NSLock gate and popped wrapper destruction. Both include same void no-network sink and pre-submit gate. Complete scope calls actual RustEncodedIngress.validate exactly once per AU on both paths.",
                "excluded":"Cold generation/ASBD/read/codec preparation/fanout construction+unused recording close/correctness queries/serialization/teardown. Initial lazy queue growth is amortised once per128-AU batch, not removed. No recorder/network/worker dispatch/camera/video/phone/power or comparative broadcaster acceptance.",
                "cpuClock":"CLOCK_THREAD_CPUTIME_ID; current serial benchmark thread only, includes synchronous Swift+FFI work, not hardware/media generation or waiting.",
                "distributionUnit":"Quantiles of128-AU batch means per AU, not individual-AU latency quantiles.",
                "allocations":"Not instrumented. Source-derived added pop ownership is a Swift MediaSample wrapper and Rust Arc<MediaSample>; generated push/pop return small RustBuffers. Payload Arc is shared; no extra payload copy. No measured allocation-rate/RSS/whole-app claim."]
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject:output,options:[.prettyPrinted,.sortedKeys]));FileHandle.standardOutput.write(Data([10]));if !stable { exit(1) }
        } catch {
            let result:[String:Any]=["passed":false,"error":String(describing:error),"scope":"Bounded generated host benchmark failure; no tuning or weakened acceptance"]
            if let data=try? JSONSerialization.data(withJSONObject:result,options:[.prettyPrinted,.sortedKeys]) { FileHandle.standardOutput.write(data) }
            exit(1)
        }
    }
    private static func ingress() throws -> (RustEncodedIngress,BenchmarkSink) {
        let value=RustEncodedIngress(),sink=BenchmarkSink();value.reset(hasVideo:false);value.destination=sink
        try value.prepare(video:nil,audio:.aac(asc:Data([0x11,0x88])))
        return (value,sink)
    }
    private static func queue(bytes:UInt64=4*1024*1024) throws -> NativeSampleFanout {
        let value=try NativeSampleFanout(publishingBytes:bytes,recordingBytes:1,maxMediaMs:1000,hasVideo:false)
        try value.closeRecordingAdmission();return value
    }
    @inline(never) private static func old(_ value:RustEncodedIngress,_ validated:RustEncodedIngress.ValidatedUnit,_ gate:BenchmarkGate) throws {
        try benchmarkRequire(gate.allows(),"Legacy gate unexpectedly closed")
        try value.publish(validated)
    }
    @inline(never) private static func new(_ value:RustEncodedIngress,_ validated:RustEncodedIngress.ValidatedUnit,_ gate:BenchmarkGate,_ queue:NativeSampleFanout) throws {
        try benchmarkRequire(gate.allows(),"Branch gate unexpectedly closed")
        let receipt=try queue.push(sample:validated.sample,nowNs:DispatchTime.now().uptimeNanoseconds)
        try benchmarkRequire(receipt.publishingQueued && !receipt.recordingQueued,"Actual bounded receipt refused")
        guard let actual=try queue.popPublishing(nowNs:DispatchTime.now().uptimeNanoseconds) else { throw CaptureFailure(message:"Actual publishing dequeue missing") }
        try benchmarkRequire(gate.allows(),"Post-pop gate unexpectedly closed")
        try value.publish(validated,sample:actual)
    }
    private static func cpu() throws -> UInt64 {
        var value=timespec()
        try benchmarkRequire(clock_gettime(CLOCK_THREAD_CPUTIME_ID,&value)==0 && value.tv_sec>=0 && value.tv_nsec>=0,"Thread CPU clock unavailable")
        return UInt64(value.tv_sec)*1_000_000_000+UInt64(value.tv_nsec)
    }
    private static func batch(units:[CompressedUnit],pool:[RustEncodedIngress.ValidatedUnit],complete:Bool,new isNew:Bool) throws -> Timing {
        let (value,sink)=try ingress(),gate=BenchmarkGate()
        let native=isNew ? try queue():nil
        let cpuStart=try cpu(),wallStart=DispatchTime.now().uptimeNanoseconds
        for index in 0..<poolCount {
            let validated=complete ? try value.validate(units[index]):pool[index]
            if let native { try new(value,validated,gate,native) }
            else { try old(value,validated,gate) }
        }
        let wallEnd=DispatchTime.now().uptimeNanoseconds,cpuEnd=try cpu()
        try benchmarkRequire(wallEnd>=wallStart && cpuEnd>=cpuStart,"Benchmark clock rollback")
        try benchmarkRequire(sink.count==UInt64(poolCount) && value.accepted==UInt64(complete ? poolCount:0),"Validation/sink count differs between paired paths")
        if let native {
            let stats=try native.stats()
            try benchmarkRequire(stats.publishingBytes==0 && stats.recordingBytes==0 && !stats.recordingFailed,"Queue did not actually release batch")
            try native.closePublishingAdmission();try native.cancel()
        }
        return Timing(wall:wallEnd-wallStart,cpu:cpuEnd-cpuStart)
    }
    private static func quantiles(_ values:[Double]) -> [String:Double] {
        let sorted=values.sorted()
        func q(_ p:Double)->Double { sorted[max(0,min(sorted.count-1,Int(ceil(p*Double(sorted.count)))-1))] }
        return ["mean":sorted.reduce(0,+)/Double(sorted.count),"min":sorted[0],"p50":q(0.5),"p95":q(0.95),"p99":q(0.99),"max":sorted.last!]
    }
    private static func distribution(_ values:[Timing])->[String:Any] {
        ["CPU_nsPerAU_batchMean":quantiles(values.map{Double($0.cpu)/Double(poolCount)}),
         "wall_nsPerAU_batchMean":quantiles(values.map{Double($0.wall)/Double(poolCount)}),
         "totalThreadCPU_ns":values.reduce(UInt64(0)){$0+$1.cpu},"totalWall_ns":values.reduce(UInt64(0)){$0+$1.wall}]
    }
    private static func correctness(samples:[RustEncodedIngress.ValidatedUnit],units:[CompressedUnit]) throws -> [String:Any] {
        let native=try queue();var highBytes:UInt64=0
        weak var lastPopped:MediaSample?
        for (index,item) in samples.enumerated() {
            let result=try native.push(sample:item.sample,nowNs:UInt64(index)*2)
            try benchmarkRequire(result.publishingQueued && !result.recordingQueued,"Correctness native queue refused")
            let before=try native.stats();highBytes=max(highBytes,before.publishingBytes)
            try benchmarkRequire(before.publishingBytes==UInt64(units[index].byteCount) && before.recordingBytes==0,"More than one native sample retained")
            do {
                var actual=try native.popPublishing(nowNs:UInt64(index)*2+1)
                try benchmarkRequire(actual != nil && actual!.len()==UInt64(units[index].byteCount) && actual!.pts()==item.pts,"Actual popped metadata/order differs")
                lastPopped=actual
                actual=nil
            }
            try benchmarkRequire(lastPopped==nil && (try native.stats()).publishingBytes==0,"Popped wrapper/queue ownership retained")
        }
        try native.cancel()
        let refused=try queue(bytes:1),first=samples[0]
        try benchmarkRequire(!(try refused.push(sample:first.sample,nowNs:0)).publishingQueued && (try refused.popPublishing(nowNs:1))==nil,"Real one-byte refusal failed")
        try refused.cancel()
        let close=try queue()
        try benchmarkRequire((try close.push(sample:first.sample,nowNs:0)).publishingQueued,"Prefix admission failed")
        try close.closePublishingAdmission();try close.closePublishingAdmission()
        try benchmarkRequire((try close.popPublishing(nowNs:1))?.pts()==first.pts,"Selective close lost prefix")
        try benchmarkRequire(!(try close.push(sample:samples[1].sample,nowNs:2)).publishingQueued,"Selective close reopened")
        try close.cancel()
        let outer=EncodedWorkBudget(bytes:4*1024*1024,count:128,span:1)
        var reservations:[EncodedWorkBudget.Reservation]=[]
        for _ in 0..<128 { guard let ticket=outer.reserve(bytes:1,time:0) else { throw CaptureFailure(message:"Outer128 bound prematurely refused") };reservations.append(ticket) }
        try benchmarkRequire(outer.reserve(bytes:1,time:0)==nil && outer.pendingCount==128,"Outer128 bound failed")
        for ticket in reservations { outer.release(ticket) }
        guard let full=outer.reserve(bytes:4*1024*1024,time:0) else { throw CaptureFailure(message:"Outer4MiB exact bound failed") }
        try benchmarkRequire(outer.reserve(bytes:1,time:0)==nil,"Outer byte bound failed")
        outer.release(full)
        guard let aged=outer.reserve(bytes:1,time:0) else { throw CaptureFailure(message:"Outer age control failed") }
        try benchmarkRequire(outer.reserve(bytes:1,time:1.001)==nil,"Outer1s bound failed")
        outer.release(aged);outer.close()
        return ["actualQueuedAndPopped":128,"maxNativePendingSamples":1,"maxNativePublishingBytes":highBytes,
            "recordingSDKBytes":0,"poppedSwiftWrapperReleased":true,"realByteRefusal":true,"selectiveClosePreservesPrefixAndRefusesFuture":true,
            "outer128_4MiB_1s":true,"prevalidatedPoolValidations":128,"newPathAdditionalCodecValidations":0,"poolPayloadBytes":units.reduce(0){$0+$1.byteCount}]
    }
    private static func load(_ url:URL) async throws -> [CompressedUnit] {
        let asset=AVURLAsset(url:url),tracks=try await asset.loadTracks(withMediaType:.audio)
        try benchmarkRequire(tracks.count==1,"Generated audio track unavailable")
        let reader=try AVAssetReader(asset:asset),output=AVAssetReaderTrackOutput(track:tracks[0],outputSettings:nil)
        output.alwaysCopiesSampleData=false;reader.add(output);try benchmarkRequire(reader.startReading(),"Generated reader refused")
        defer { if reader.status == .reading { reader.cancelReading() } }
        var units:[CompressedUnit]=[],empty=0
        while units.count<poolCount,let group=output.copyNextSampleBuffer() {
            let count=CMSampleBufferGetNumSamples(group)
            if count==0 { empty += 1;try benchmarkRequire(empty<=16,"Generated empty sample bound");continue }
            guard let format=CMSampleBufferGetFormatDescription(group),let asbd=CMAudioFormatDescriptionGetStreamBasicDescription(format) else { throw CaptureFailure(message:"Generated AAC ASBD missing") }
            try benchmarkRequire(asbd.pointee.mFormatID==kAudioFormatMPEG4AAC && asbd.pointee.mSampleRate==48000 && asbd.pointee.mChannelsPerFrame==1,"Generated AAC format differs")
            for index in 0..<count where units.count<poolCount {
                var one:CMSampleBuffer?
                try benchmarkRequire(CMSampleBufferCopySampleBufferForRange(allocator:kCFAllocatorDefault,sampleBuffer:group,sampleRange:CFRange(location:index,length:1),sampleBufferOut:&one)==noErr,"Generated sample reference failed")
                guard let one else { throw CaptureFailure(message:"Generated AU missing") }
                let time=CMTime(value:Int64(units.count)*1024,timescale:48000),duration=CMTime(value:1024,timescale:48000)
                var timing=CMSampleTimingInfo(duration:duration,presentationTimeStamp:time,decodeTimeStamp:time),retimed:CMSampleBuffer?
                try benchmarkRequire(CMSampleBufferCreateCopyWithNewTiming(allocator:kCFAllocatorDefault,sampleBuffer:one,sampleTimingEntryCount:1,sampleTimingArray:&timing,sampleBufferOut:&retimed)==noErr,"Generated independent-run timing failed")
                guard let retimed else { throw CaptureFailure(message:"Generated timing copy missing") }
                units.append(CompressedUnit(buffer:retimed,isVideo:false,keyframe:false,time:time,duration:duration,configuration:.aac(asc:Data([0x11,0x88]))))
            }
        }
        try benchmarkRequire(units.count==poolCount,"Generated128-AU pool incomplete")
        return units
    }
}
