import Foundation

/// Untrusted settings bytes only. No URL resolution, credentials, consent or
/// creator authority is conveyed by recognising this envelope.
struct ManualDestinationEnvelope: CustomStringConvertible, CustomDebugStringConvertible {
    static let prefix = "sapflow-manual://import/v1#"
    static let maximumURLBytes = 24_576
    static let maximumDocumentBytes = 16_384
    let document: Data
    var description: String { "Manual destination link (redacted)" }
    var debugDescription: String { description }
    init(_ text: String) throws {
        let bytes = text.utf8
        guard bytes.count <= Self.maximumURLBytes, bytes.allSatisfy({ $0 >= 33 && $0 <= 126 }),
              text.hasPrefix(Self.prefix) else { throw ManualImportError.invalidLink }
        let encoded = String(text.dropFirst(Self.prefix.count))
        guard !encoded.isEmpty, encoded.utf8.count <= (Self.maximumDocumentBytes * 4 + 2) / 3,
              encoded.utf8.count % 4 != 1,
              encoded.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }) else {
            throw ManualImportError.invalidLink
        }
        var base64 = encoded.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.utf8.count % 4) % 4)
        guard let data = Data(base64Encoded: base64), data.count <= Self.maximumDocumentBytes,
              Self.encode(data) == encoded else { throw ManualImportError.invalidLink }
        document = data
    }
    private static func encode(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
}
