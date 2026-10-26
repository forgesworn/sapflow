import Foundation
import Darwin

func processCpu() -> UInt64 {
    var usage = rusage()
    precondition(getrusage(RUSAGE_SELF, &usage) == 0)
    return UInt64(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000_000
        + UInt64(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) * 1_000
}

let rounds = 7
var checksum: UInt64 = 0
var rtmpOutputEvidence: UInt64 = 0
var fmp4OutputEvidence: UInt64 = 0

func measure(_ count: Int, _ body: (Int) throws -> Void) rethrows -> Double {
    let start = DispatchTime.now().uptimeNanoseconds
    for index in 0..<count { try body(index) }
    return Double(DispatchTime.now().uptimeNanoseconds - start) / Double(count)
}

func amfString(_ out: inout Data, _ value: String) {
    let bytes = Data(value.utf8)
    out.append(2)
    out.append(UInt8(bytes.count >> 8))
    out.append(UInt8(bytes.count & 255))
    out.append(bytes)
}

func amfNumber(_ out: inout Data, _ value: Double) {
    out.append(0)
    withUnsafeBytes(of: value.bitPattern.bigEndian) { out.append(contentsOf: $0) }
}

func rtmpResponse(_ name: String, _ transaction: Double, code: String? = nil, streamId: Double? = nil) -> Data {
    var out = Data()
    amfString(&out, name)
    amfNumber(&out, transaction)
    out.append(5)
    if let streamId {
        amfNumber(&out, streamId)
    } else if let code {
        out.append(3)
        for (key, value) in [("level", "status"), ("code", code)] {
            let keyBytes = Data(key.utf8)
            out.append(UInt8(keyBytes.count >> 8))
            out.append(UInt8(keyBytes.count & 255))
            out.append(keyBytes)
            amfString(&out, value)
        }
        out.append(contentsOf: [0, 0, 9])
    }
    return out
}

func rtmpWire(_ body: Data, _ streamId: UInt32) -> Data {
    var out = Data([3, 0, 0, 0])
    out.append(UInt8((body.count >> 16) & 255))
    out.append(UInt8((body.count >> 8) & 255))
    out.append(UInt8(body.count & 255))
    out.append(20)
    withUnsafeBytes(of: streamId.littleEndian) { out.append(contentsOf: $0) }
    out.append(body)
    return out
}

func readyRtmp() throws -> NativeRtmpPublisher {
    let publisher = try NativeRtmpPublisher(app: "live", streamKey: "fixture", tcUrl: "rtmp://origin.invalid/live", mediaEpochNs: 0, nowMs: 0)
    let client = try publisher.takeOutput()
    precondition(client.count == 1537)
    var handshake = Data(repeating: 0x53, count: 3073)
    handshake[0] = 3
    handshake[1] = 0; handshake[2] = 0; handshake[3] = 0; handshake[4] = 123
    for index in 5...8 { handshake[index] = 0 }
    handshake.replaceSubrange(1537..<3073, with: client.dropFirst())
    let receipt = UInt32(456).bigEndian
    withUnsafeBytes(of: receipt) { handshake.replaceSubrange(1541..<1545, with: $0) }
    try publisher.feed(bytes: handshake, nowMs: 1)
    _ = try publisher.takeOutput()
    let responses: [(Data, UInt32, NativeRtmpState)] = [
        (rtmpResponse("_result", 1, code: "NetConnection.Connect.Success"), 0, .creatingStream),
        (rtmpResponse("_result", 2, streamId: 7), 0, .awaitingPublish),
        (rtmpResponse("onStatus", 0, code: "NetStream.Publish.Start"), 7, .publishing),
    ]
    for (body, stream, expected) in responses {
        try publisher.feed(bytes: rtmpWire(body, stream), nowMs: 2)
        let state = try publisher.state()
        precondition(state == expected)
        _ = try publisher.takeOutput()
    }
    return publisher
}

func srtPublisher(_ codec: MediaCodec) throws -> NativeSrtPublisher {
    try NativeSrtPublisher(config: NativeSrtConfig(destination: "127.0.0.1:9000", streamId: "publish:test", cipher: nil, latencyMs: 120, maxBufferedPackets: 4096, maxBufferedBytes: 4 * 1024 * 1024, maxPacketAgeMs: 3000, maxSampleResidenceMs: 1000, bandwidthBytesPerSecond: 1_250_000), video: nil, audio: codec, passphrase: nil, nowUs: 1_000_000)
}

func correctnessCheck() throws -> UInt64 {
    let large = Data(repeating: 0x42, count: 1024 * 1024)
    let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
    let rtmp = try readyRtmp()
    let sample = try codec.sample(bytes: large, pts: 0, dts: 0, duration: 1024, timescale: 48_000, keyframe: false)
    let rtmpOutput = try rtmp.sendAndTakeOutput(sample: sample, nowMs: 10)
    precondition(rtmpOutput.output.count > 1024 * 1024 && rtmpOutput.state == .publishing)
    let next = try codec.sample(bytes: large, pts: 1024, dts: 1024, duration: 1024, timescale: 48_000, keyframe: false)
    let muxer = try NativeRecordingMuxer(hasVideo: false, hasAudio: true, mediaEpochNs: 0, maxFragmentBytes: 1024 * 1024, maxFragmentMs: 100)
    var outputBytes: UInt64 = 0
    var largest = 0
    for media in [sample, next] {
        try muxer.push(sample: media)
        while let output = try muxer.takeOutput() {
            outputBytes += UInt64(output.bytes.count)
            largest = max(largest, output.bytes.count)
        }
    }
    precondition(largest > 1024 * 1024)
    let srt = try srtPublisher(codec)
    try srt.feed(source: "127.0.0.1:9001", bytes: Data(count: 1316), nowUs: 1_000_000)
    try srt.feed(source: "127.0.0.1:9001", bytes: Data(count: 1500), nowUs: 1_000_000)
    let stats = try srt.stats()
    precondition(stats.strayDatagrams == 2)
    return UInt64(rtmpOutput.output.count) + outputBytes + 2816
}

if ProcessInfo.processInfo.environment["SAPFLOW_BENCH_CHECK"] == "1" {
    print("correctness_checksum=\(try correctnessCheck())")
    exit(0)
}

let small = Data(repeating: 0x21, count: 4096)
let large = Data(repeating: 0x42, count: 1024 * 1024)
let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
let first = try codec.sample(bytes: small, pts: 0, dts: 0, duration: 1024, timescale: 48_000, keyframe: false)
let encoded = try EncodedBuffer(bytes: small)
let names = [
    "sample_4kib", "sample_1mib", "fanout_push_pop", "scalar_len", "encoded_buffer_4kib",
    "rtmp_feed", "rtmp_send_1mib", "fmp4_output_1mib", "srt_feed_1316", "srt_feed_1500",
]
let iterations = [1000, 50, 2000, 20000, 5000, 10000, 16, 6, 10000, 10000]
var fanoutSamples = [MediaSample]()
fanoutSamples.reserveCapacity(iterations[2])
for index in 0..<iterations[2] {
    let tick = Int64(index * 1024)
    fanoutSamples.append(try codec.sample(bytes: small, pts: tick, dts: tick, duration: 1024, timescale: 48_000, keyframe: false))
}
let fanout = try NativeSampleFanout(publishingBytes: 64 * 1024 * 1024, recordingBytes: 64 * 1024 * 1024, maxMediaMs: 1000, hasVideo: false)
let rtmpAck = Data([2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0, 0, 0, 0, 0])
let datagram1316 = Data(count: 1316)
let datagram1500 = Data(count: 1500)
var evidenceStats: NativeSrtStats?
for metric in names.indices {
    var values = [Double]()
    var cpuValues = [Double]()
    for round in 0..<rounds {
        let roundFanout = metric == 2 ? try NativeSampleFanout(publishingBytes: 64 * 1024 * 1024, recordingBytes: 64 * 1024 * 1024, maxMediaMs: 1000, hasVideo: false) : fanout
        let rtmp = metric == 5 || metric == 6 ? try readyRtmp() : nil
        let srt = metric >= 8 ? try srtPublisher(codec) : nil
        var samples: [MediaSample]?
        if metric == 6 || metric == 7 {
            let count = metric == 6 ? iterations[metric] : iterations[metric] * 2
            samples = [MediaSample]()
            samples!.reserveCapacity(count)
            for index in 0..<count {
                let tick = Int64(index * 1024)
                samples!.append(try codec.sample(bytes: large, pts: tick, dts: tick, duration: 1024, timescale: 48_000, keyframe: false))
            }
        }
        let cpuStart = processCpu()
        let value = try measure(iterations[metric]) { index in
            switch metric {
            case 0, 1:
                let sample = try codec.sample(bytes: metric == 0 ? small : large, pts: 0, dts: 0, duration: 1024, timescale: 48_000, keyframe: false)
                checksum += sample.len() == 0 ? 0 : 1
            case 2:
                let outcome = try roundFanout.push(sample: fanoutSamples[index], nowNs: UInt64(round))
                precondition(outcome.publishingQueued)
                checksum += try roundFanout.popPublishing(nowNs: UInt64(round))!.len()
            case 3:
                checksum += first.len()
            case 4:
                checksum += try EncodedBuffer(bytes: small).len()
            case 5:
                try rtmp!.feed(bytes: rtmpAck, nowMs: UInt64(index + 10))
                let state = try rtmp!.state()
                precondition(state == .publishing)
                checksum += 1
            case 6:
                let output = try rtmp!.sendAndTakeOutput(sample: samples![index], nowMs: UInt64(index + 10))
                precondition(!output.output.isEmpty && output.state == .publishing)
                checksum += UInt64(output.output.count) + output.queuedOutputBytes
                rtmpOutputEvidence += UInt64(output.output.count)
            case 7:
                let muxer = try NativeRecordingMuxer(hasVideo: false, hasAudio: true, mediaEpochNs: 0, maxFragmentBytes: 1024 * 1024, maxFragmentMs: 100)
                for part in 0..<2 {
                    try muxer.push(sample: samples![index * 2 + part])
                    while let output = try muxer.takeOutput() {
                        checksum += UInt64(output.bytes.count)
                        fmp4OutputEvidence += UInt64(output.bytes.count)
                    }
                }
            default:
                let size = metric == 8 ? 1316 : 1500
                try srt!.feed(source: "127.0.0.1:9001", bytes: size == 1316 ? datagram1316 : datagram1500, nowUs: 1_000_000)
                checksum += UInt64(size)
            }
        }
        values.append(value)
        cpuValues.append(Double(processCpu() - cpuStart) / Double(iterations[metric]))
        if let srt { evidenceStats = try srt.stats() }
        withExtendedLifetime(samples) {}
    }
    values.sort()
    cpuValues.sort()
    print("\"\(names[metric])\":\(String(format: "%.3f", values[rounds / 2]))", terminator: ",")
    print("\"\(names[metric])_cpu\":\(String(format: "%.3f", cpuValues[rounds / 2]))", terminator: ",")
}
precondition(rtmpOutputEvidence > 0 && fmp4OutputEvidence > 0)
let stats = evidenceStats!
precondition(stats.strayDatagrams == iterations[9])
print("\"rtmp_output_bytes\":\(rtmpOutputEvidence),\"fmp4_output_bytes\":\(fmp4OutputEvidence)", terminator: "")
print(",\"srt_stray_datagrams\":\(stats.strayDatagrams),\"srt_returned_datagram_bytes\":\(stats.returnedDatagramBytes)", terminator: "")
print(",\"srt_ts_output_allocations\":\(stats.tsOutputAllocations),\"srt_writer_payload_copy_bytes\":\(stats.writerPayloadCopyBytes)", terminator: "")
withExtendedLifetime((codec, first, encoded, fanoutSamples, fanout)) {
    print(",\"srt_ts_serialised_bytes\":\(stats.tsSerialisedBytes),\"checksum\":\(checksum)")
}
