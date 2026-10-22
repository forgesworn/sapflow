import Foundation

/// Pure untrusted-input boundary. No settings owner, files, Keychain or sockets.
/// Mode zero exercises link framing; mode one also reaches strict JSON directly.
@_cdecl("LLVMFuzzerTestOneInput")
public func manualDestinationEnvelopeFuzz(_ pointer: UnsafePointer<UInt8>?, _ count: Int) -> Int32 {
    guard let pointer, count > 0, count <= ManualDestinationEnvelope.maximumURLBytes + 1 else { return 0 }
    let input = Data(bytes: pointer, count: count)
    let bytes = input.subdata(in: 1..<input.count)
    do {
        let document: Data
        if input[0] & 1 == 0 {
            guard let text = String(data: bytes, encoding: .utf8) else { return 0 }
            let envelope = try ManualDestinationEnvelope(text)
            precondition(envelope.document.count <= ManualDestinationEnvelope.maximumDocumentBytes)
            precondition(envelope.description == "Manual destination link (redacted)")
            document = envelope.document
        } else {
            guard bytes.count <= ImportedManualDestination.maximumBytes else { return 0 }
            document = bytes
        }
        let profile = try ImportedManualDestination(document)
        precondition(profile.description == "Manual destination import (redacted)")
    } catch { /* Invalid links and profiles must refuse without side effects. */ }
    return 0
}
