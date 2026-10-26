import Foundation

let rounds = 7
var checksum: UInt64 = 0
let small = Data(repeating: 0x21, count: 4096)
let large = Data(repeating: 0x42, count: 1024 * 1024)
let codec = try MediaCodec.aac(asc: Data([0x11, 0x88]))
let first = try codec.sample(bytes: small, pts: 0, dts: 0, duration: 1024, timescale: 48_000, keyframe: false)
let encoded = try EncodedBuffer(bytes: small)
let names = ["sample_4kib", "sample_1mib", "fanout_push_pop", "scalar_len", "encoded_buffer_4kib"]
let iterations = [1000, 50, 2000, 20000, 5000]
var fanoutSamples = [MediaSample]()
fanoutSamples.reserveCapacity(iterations[2])
for index in 0..<iterations[2] {
    let tick = Int64(index * 1024)
    fanoutSamples.append(try codec.sample(bytes: small, pts: tick, dts: tick, duration: 1024, timescale: 48_000, keyframe: false))
}
func measure(_ count: Int, _ body: (Int) throws -> Void) rethrows -> Double {
    let start = DispatchTime.now().uptimeNanoseconds
    for index in 0..<count { try body(index) }
    return Double(DispatchTime.now().uptimeNanoseconds - start) / Double(count)
}
for metric in names.indices {
    var values = [Double]()
    for round in 0..<rounds {
        let fanout = metric == 2 ? try NativeSampleFanout(publishingBytes: 64 * 1024 * 1024, recordingBytes: 64 * 1024 * 1024, maxMediaMs: 1000, hasVideo: false) : nil
        let value = try measure(iterations[metric]) { index in
            switch metric {
            case 0, 1:
                let sample = try codec.sample(bytes: metric == 0 ? small : large, pts: 0, dts: 0, duration: 1024, timescale: 48_000, keyframe: false)
                checksum += sample.len()
            case 2:
                let outcome = try fanout!.push(sample: fanoutSamples[index], nowNs: UInt64(round))
                precondition(outcome.publishingQueued)
                checksum += try fanout!.popPublishing(nowNs: UInt64(round))!.len()
            case 3:
                checksum += first.len()
            default:
                checksum += try EncodedBuffer(bytes: small).len()
            }
        }
        values.append(value)
    }
    values.sort()
    let rendered = String(format: "%.3f", values[rounds / 2])
    let separator = metric == names.count - 1 ? "" : ","
    print("\"\(names[metric])\":\(rendered)\(separator)", terminator: "")
}
withExtendedLifetime((codec, first, encoded, fanoutSamples)) {
    print(",\"checksum\":\(checksum)")
}
