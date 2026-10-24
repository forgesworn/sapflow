import Foundation

/// Pure bounded canonical V1/V2 receipt decode only. Accepted metadata grants
/// no file capability, SDK eligibility, source EOF or decoded-playback proof.
@_cdecl("LLVMFuzzerTestOneInput")
public func recordingRecoveryReceiptFuzz(_ pointer: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard let pointer, count > 0, count <= 4096 else { return 0 }
    let original = Data(bytes: pointer, count: count)
    _ = RecordingCatalogue.checkRecoveryReceiptBytes(original)
    return 0
}
