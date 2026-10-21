import Foundation
import Network

enum ManualTransport: String, CaseIterable, Identifiable {
    case rtmp, srt
    var id: String { rawValue }
}

/// The initial manual SRT caller accepts explicit numeric unicast peers. DNS and
/// peer-address changes need a separately owned resolution/session policy.
struct ManualSrtDestination {
    let endpoint: String
    let host: String
    let port: UInt16
    let socketAddress: String
    let streamID: String
    let passphrase: String?
    let encrypted: Bool
    init(endpoint: String, streamID: String, passphrase: String, allowPlaintext: Bool) throws {
        let endpoint = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard endpoint.utf8.count <= 256, let url = URLComponents(string: endpoint), url.string == endpoint,
              url.scheme?.lowercased() == "srt", url.user == nil, url.password == nil, url.query == nil, url.fragment == nil,
              url.path.isEmpty || url.path == "/", let rawHost = url.host,
              let range = url.rangeOfPort, let source = url.string, let port = UInt16(source[range]), port > 0,
              streamID.utf8.count <= 512, !streamID.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              !endpoint.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { throw SrtDestinationError.invalidAddress }
        let host = rawHost.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        let canonical: String
        if let ipv4 = IPv4Address(host) {
            guard Self.unicastV4(Array(ipv4.rawValue)) else { throw SrtDestinationError.invalidAddress }
            canonical = ipv4.debugDescription
            socketAddress = "\(canonical):\(port)"
        } else if let ipv6 = IPv6Address(host), !host.contains("%") {
            let bytes = Array(ipv6.rawValue)
            guard bytes.contains(where: { $0 != 0 }), bytes[0] != 0xff else { throw SrtDestinationError.invalidAddress }
            if bytes.prefix(10).allSatisfy({ $0 == 0 }), bytes[10] == 0xff, bytes[11] == 0xff {
                guard Self.unicastV4(Array(bytes.suffix(4))) else { throw SrtDestinationError.invalidAddress }
            }
            canonical = ipv6.debugDescription
            socketAddress = "[\(canonical)]:\(port)"
        } else { throw SrtDestinationError.invalidAddress }
        if allowPlaintext {
            guard passphrase.isEmpty else { throw SrtDestinationError.invalidPassphrase }
            self.passphrase = nil
        } else {
            guard (10...79).contains(passphrase.utf8.count), passphrase.unicodeScalars.allSatisfy({ $0.isASCII && !CharacterSet.controlCharacters.contains($0) }) else { throw SrtDestinationError.invalidPassphrase }
            self.passphrase = passphrase
        }
        self.endpoint = endpoint; self.host = canonical; self.port = port
        self.streamID = streamID; encrypted = !allowPlaintext
    }
    private static func unicastV4(_ bytes: [UInt8]) -> Bool {
        bytes.count == 4 && bytes[0] != 0 && bytes[0] < 224 && bytes != [255, 255, 255, 255]
    }
}

enum SrtDestinationError: LocalizedError {
    case invalidAddress, invalidPassphrase
    var errorDescription: String? {
        switch self {
        case .invalidAddress: return "Enter srt:// with a numeric unicast IPv4/IPv6 address and explicit port. Keep the stream ID separate; DNS, queries and embedded credentials are not supported yet."
        case .invalidPassphrase: return "Encrypted SRT requires 10 to 79 printable ASCII passphrase bytes. Explicit plaintext requires an empty passphrase."
        }
    }
}
