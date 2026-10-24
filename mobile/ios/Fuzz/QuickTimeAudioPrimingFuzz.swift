import Foundation

/// libFuzzer entry, pure metadata only. Prefix byte chooses fixed or fuzzed facts.
@_cdecl("LLVMFuzzerTestOneInput")
public func quickTimeAudioPrimingFuzz(_ pointer: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard let pointer, count > 0, count <= QuickTimeAudioPriming.maximumHeaderBytes + 25 else { return 0 }
    let input = Data(bytes: pointer, count: count)
    if input[0] & 2 != 0 { return fuzzMovieLayout(input) }
    var offset = 1
    var facts = QuickTimeAudioPriming.Facts(accessUnits: 97, leadingFrames: 2112, validFrames: 96356)
    if input[0] & 1 != 0 {
        guard count >= 25 else { return 0 }
        func number(_ start: Int, _ width: Int) -> UInt64 {
            input[start..<(start + width)].reduce(0) { ($0 << 8) | UInt64($1) }
        }
        let id = UInt32(number(1, 4))
        facts = .init(audioTrackID: id == 0 ? nil : id, accessUnits: number(5, 8),
                      leadingFrames: UInt32(number(13, 4)), validFrames: number(17, 8))
        offset = 25
    }
    let original = input.subdata(in: offset..<input.count)
    let audioOnly = input[0] & 4 != 0
    do {
        let prepared: QuickTimeAudioPriming.Prepared
        if audioOnly { prepared = try QuickTimeAudioPriming.prepareAudioOnly(moov: original, facts: facts) }
        else { prepared = try QuickTimeAudioPriming.prepare(moov: original, facts: facts) }
        precondition(prepared.moov.count == original.count + (audioOnly ? 90 : 54))
        precondition(prepared.moov.count <= QuickTimeAudioPriming.maximumHeaderBytes)
        precondition(prepared.accessUnits == facts.accessUnits && prepared.leadingFrames == facts.leadingFrames
            && prepared.validFrames == facts.validFrames)
        precondition(prepared.accessUnits * 1024 == UInt64(prepared.leadingFrames) + prepared.validFrames + prepared.trailingFrames)
        do {
            if audioOnly { _ = try QuickTimeAudioPriming.prepareAudioOnly(moov: prepared.moov, facts: facts) }
            else { _ = try QuickTimeAudioPriming.prepare(moov: prepared.moov, facts: facts) }
            preconditionFailure("A transformed header was applied twice")
        } catch { /* Explicit double-apply refusal. */ }
    } catch { /* Invalid, conflicting or unsupported headers must refuse. */ }
    return 0
}

/// Contiguous bytes or a bounded sparse header map, including virtual 64-bit files.
private func fuzzMovieLayout(_ input: Data) -> Int32 {
    let sparse = input[0] & 1 != 0
    var total: UInt64 = UInt64(input.count - 1), headers: [UInt64: Data] = [:]
    if sparse {
        guard input.count >= 9, input.count - 9 <= 4096 * 16 else { return 0 }
        total = input[1..<9].reduce(0) { ($0 << 8) | UInt64($1) }
        var at = 9
        while input.count - at >= 16 {
            let offset = input[at..<(at + 8)].reduce(0) { ($0 << 8) | UInt64($1) }
            headers[offset] = input.subdata(in: (at + 8)..<(at + 16)); at += 16
        }
    }
    var requests = 0
    do {
        let layout = try QuickTimeMovieLayout.scan(fileBytes: total) { offset, count in
            requests += 1
            precondition(requests <= 2 * QuickTimeMovieLayout.maximumBoxes)
            precondition(count == 8 && total >= 8 && offset <= total - 8)
            if sparse { return headers[offset] ?? Data() }
            let at = Int(offset) + 1
            return input.subdata(in: at..<(at + count))
        }
        precondition(layout.movie.bytes >= layout.movie.headerBytes && layout.movie.bytes <= QuickTimeMovieLayout.maximumMovieBytes)
        precondition(layout.movie.offset == total - UInt64(layout.movie.bytes))
        precondition(layout.boxCount > 0 && layout.boxCount <= QuickTimeMovieLayout.maximumBoxes)
        precondition(layout.mediaBoxCount > 0 && layout.mediaBoxCount < layout.boxCount)
    } catch { /* Invalid extents and failed/short reads must refuse. */ }
    return 0
}
