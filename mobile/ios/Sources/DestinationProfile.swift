import Foundation
import Security

struct ManualDestination {
    let endpoint: String
    let host: String
    let port: UInt16
    let app: String
    let streamKey: String
    let encrypted: Bool

    init(endpoint: String, streamKey: String, allowPlaintext: Bool) throws {
        let endpoint = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        // Require an exact component round-trip. URLComponents silently drops
        // a decimal port which overflows Int; that must not select a default.
        guard endpoint.utf8.count <= 4096, !endpoint.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              let components = URLComponents(string: endpoint), components.url != nil, components.string == endpoint, let scheme = components.scheme?.lowercased(),
              scheme == "rtmps" || (scheme == "rtmp" && allowPlaintext),
              let host = components.host, !host.isEmpty, host.utf8.count <= 253,
              host.unicodeScalars.allSatisfy({ $0.isASCII && (CharacterSet.alphanumerics.contains($0) || ".-:[]".unicodeScalars.contains($0)) }),
              components.user == nil, components.password == nil, components.query == nil, components.fragment == nil,
              !streamKey.isEmpty, streamKey.utf8.count <= 4096,
              !streamKey.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw DestinationError.invalidProfile
        }
        let app = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !app.isEmpty, app.utf8.count <= 4096,
              !app.split(separator: "/").contains(where: { $0 == "." || $0 == ".." }),
              !app.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            throw DestinationError.invalidProfile
        }
        let port: UInt16
        if let range = components.rangeOfPort {
            // Foundation's Int-valued port becomes nil for oversized decimal
            // input. Validate the original port component instead of silently
            // treating that malformed explicit port as an absent default.
            guard let source = components.string, let explicit = UInt16(source[range]), explicit > 0 else { throw DestinationError.invalidProfile }
            port = explicit
        } else { port = scheme == "rtmps" ? 443 : 1935 }
        self.endpoint = endpoint; self.host = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]")); self.port = port; self.app = app
        self.streamKey = streamKey; encrypted = scheme == "rtmps"
    }
}

enum DestinationError: LocalizedError {
    case invalidProfile, invalidName, tooManyProfiles, keychain, storage
    var errorDescription: String? {
        switch self {
        case .invalidProfile: return "Enter an RTMPS application URL and a separate stream key. Plain RTMP requires explicit permission. Use the application address, without a login, query or fragment."
        case .invalidName: return "Give the profile a name between 1 and 48 characters."
        case .tooManyProfiles: return "Delete a saved profile before adding another (maximum 16)."
        case .keychain: return "Destination secrets could not be read or saved in the device Keychain. Unlock the phone and try again."
        case .storage: return "Saved destinations could not be read. Existing profiles have been preserved."
        }
    }
}

struct SavedDestinationProfile: Codable, Identifiable, Equatable {
    let id: UUID
    var name: String
    var endpoint: String
    var allowPlaintext: Bool
    var transport: String? = nil
}

/// Only explicit Save persists a destination. Keys stay out of UserDefaults,
/// exports and logs; Keychain entries are local, unlocked-device-only items.
protocol ManualDestinationProfileStoring: AnyObject {
    func load() throws -> [SavedDestinationProfile]
    func resolve(expected: SavedDestinationProfile) throws -> DestinationProfileSecret
    func save(id: UUID?, name: String, destination: ManualDestination, allowPlaintext: Bool) throws -> UUID
    func saveSrt(id: UUID?, name: String, destination: ManualSrtDestination, allowPlaintext: Bool) throws -> UUID
    func delete(_ id: UUID) throws
}
struct DestinationProfileSecret: CustomStringConvertible, CustomDebugStringConvertible {
    let streamKey: String, streamID: String, passphrase: String
    var description: String { "Saved destination secret (redacted)" }
    var debugDescription: String { description }
}
protocol DestinationSecretStoring: AnyObject {
    func read(_ id: UUID) throws -> Data
    func write(_ data: Data, for id: UUID) throws
    func delete(_ id: UUID) throws
}
private final class DestinationKeychainSecrets: DestinationSecretStoring {
    private let service = "dev.forgesworn.sapflow.manual-publisher"
    private func query(_ id: UUID) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id.uuidString]
    }
    func read(_ id: UUID) throws -> Data {
        var query = query(id); query[kSecReturnData as String] = true; query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data, data.count <= 32 * 1024 else { throw DestinationError.keychain }
        return data
    }
    func write(_ data: Data, for id: UUID) throws {
        let query = query(id)
        let attributes: [String: Any] = [kSecValueData as String: data, kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly]
        let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updated == errSecItemNotFound {
            var added = query
            for (key, value) in attributes { added[key] = value }
            guard SecItemAdd(added as CFDictionary, nil) == errSecSuccess else { throw DestinationError.keychain }
        } else if updated != errSecSuccess { throw DestinationError.keychain }
    }
    func delete(_ id: UUID) throws {
        let result = SecItemDelete(query(id) as CFDictionary)
        guard result == errSecSuccess || result == errSecItemNotFound else { throw DestinationError.keychain }
    }
}
final class DestinationProfileStore: ManualDestinationProfileStoring {
    private let defaults: UserDefaults
    private let secrets: DestinationSecretStoring
    private let storageKey = "sapflow.manual-destinations.v1"
    // Newline cannot occur in a validated legacy RTMP key, and a legacy SRT
    // secret is compact JSON beginning with '{'. Never guess from JSON shape.
    private static let prefix = Data("SAPFLOW-MANUAL-DESTINATION:1\n".utf8)
    private struct BoundSecret: Codable {
        let id: UUID, endpoint: String, transport: String, allowPlaintext: Bool, secret: String
    }
    init(defaults: UserDefaults = .standard, secrets: DestinationSecretStoring? = nil) {
        self.defaults = defaults; self.secrets = secrets ?? DestinationKeychainSecrets()
    }
    func load() throws -> [SavedDestinationProfile] {
        guard defaults.object(forKey: storageKey) != nil else { return [] }
        guard let data = defaults.data(forKey: storageKey), data.count <= 128 * 1024,
              let profiles = try? JSONDecoder().decode([SavedDestinationProfile].self, from: data), profiles.count <= 16,
              Set(profiles.map(\.id)).count == profiles.count,
              profiles.allSatisfy({ !$0.name.isEmpty && $0.name.count <= 48 && $0.name.utf8.count <= 192 && $0.endpoint.utf8.count <= 4096 &&
                  !$0.name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) &&
                  !$0.endpoint.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) &&
                  ($0.transport == nil || $0.transport == "rtmp" || $0.transport == "srt") }) else { throw DestinationError.storage }
        return profiles
    }
    private func protectedSecret(for id: UUID, transport: String, expected: SavedDestinationProfile? = nil) throws -> String {
        guard let profile = try load().first(where: { $0.id == id }), (profile.transport ?? "rtmp") == transport,
              expected == nil || expected == profile else { throw DestinationError.invalidProfile }
        let data = try secrets.read(id)
        guard data.count <= 32 * 1024 else { throw DestinationError.keychain }
        if data.starts(with: Self.prefix) {
            let value: BoundSecret
            do { value = try JSONDecoder().decode(BoundSecret.self, from: data.dropFirst(Self.prefix.count)) }
            catch { throw DestinationError.keychain }
            guard value.id == id, value.endpoint == profile.endpoint, value.transport == transport,
                  value.allowPlaintext == profile.allowPlaintext, value.secret.utf8.count <= 4096 else { throw DestinationError.keychain }
            return value.secret
        }
        // Historical raw values carry no endpoint binding. Keep them readable;
        // every subsequent explicit Save writes the bound version above.
        guard data.count <= 4096, let value = String(data: data, encoding: .utf8) else { throw DestinationError.keychain }
        return value
    }
    func key(for id: UUID) throws -> String { try protectedSecret(for: id, transport: "rtmp") }
    private struct SrtSecret: Codable { let streamID: String; let passphrase: String }
    /// Resolve against the exact row selected by the caller. A valid newer
    /// metadata/Keychain pair must not lend its secret to an older cached peer.
    func resolve(expected: SavedDestinationProfile) throws -> DestinationProfileSecret {
        if (expected.transport ?? "rtmp") == "rtmp" {
            return DestinationProfileSecret(streamKey: try protectedSecret(for: expected.id, transport: "rtmp", expected: expected), streamID: "", passphrase: "")
        }
        let saved = try protectedSecret(for: expected.id, transport: "srt", expected: expected)
        guard let bytes = saved.data(using: .utf8), let secret = try? JSONDecoder().decode(SrtSecret.self, from: bytes) else { throw DestinationError.keychain }
        return DestinationProfileSecret(streamKey: "", streamID: secret.streamID, passphrase: secret.passphrase)
    }
    func srtSecret(for id: UUID) throws -> (streamID: String, passphrase: String) {
        let saved = try protectedSecret(for: id, transport: "srt")
        guard let bytes = saved.data(using: .utf8), let secret = try? JSONDecoder().decode(SrtSecret.self, from: bytes) else { throw DestinationError.keychain }
        return (secret.streamID, secret.passphrase)
    }
    func save(id: UUID?, name: String, destination: ManualDestination, allowPlaintext: Bool) throws -> UUID {
        try saveProfile(id: id, name: name, endpoint: destination.endpoint, secret: destination.streamKey, allowPlaintext: allowPlaintext, transport: "rtmp")
    }
    func saveSrt(id: UUID?, name: String, destination: ManualSrtDestination, allowPlaintext: Bool) throws -> UUID {
        let bytes = try JSONEncoder().encode(SrtSecret(streamID: destination.streamID, passphrase: destination.passphrase ?? ""))
        guard let secret = String(data: bytes, encoding: .utf8) else { throw DestinationError.keychain }
        return try saveProfile(id: id, name: name, endpoint: destination.endpoint, secret: secret, allowPlaintext: allowPlaintext, transport: "srt")
    }
    private func saveProfile(id: UUID?, name rawName: String, endpoint: String, secret: String, allowPlaintext: Bool, transport: String) throws -> UUID {
        guard rawName.utf8.prefix(193).count <= 192 else { throw DestinationError.invalidName }
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= 48, !name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { throw DestinationError.invalidName }
        var profiles = try load()
        if let id, !profiles.contains(where: { $0.id == id }) { throw DestinationError.invalidProfile }
        guard id.map({ existing in profiles.contains(where: { $0.id == existing }) }) == true || profiles.count < 16 else { throw DestinationError.tooManyProfiles }
        let id = id ?? UUID()
        let profile = SavedDestinationProfile(id: id, name: name, endpoint: endpoint, allowPlaintext: allowPlaintext, transport: transport)
        if let index = profiles.firstIndex(where: { $0.id == id }) { profiles[index] = profile } else { profiles.append(profile) }
        let metadata = try JSONEncoder().encode(profiles)
        let envelope = BoundSecret(id: id, endpoint: endpoint, transport: transport, allowPlaintext: allowPlaintext, secret: secret)
        var protected = Self.prefix; protected.append(try JSONEncoder().encode(envelope))
        guard metadata.count <= 128 * 1024, protected.count <= 32 * 1024 else { throw DestinationError.storage }
        // If termination occurs between these stores, the old metadata will
        // fail exact envelope matching instead of sending a new key to its peer.
        try secrets.write(protected, for: id)
        defaults.set(metadata, forKey: storageKey)
        return id
    }
    func delete(_ id: UUID) throws {
        let profiles = try load()
        guard profiles.contains(where: { $0.id == id }) else { throw DestinationError.invalidProfile }
        let metadata = try JSONEncoder().encode(profiles.filter { $0.id != id })
        try secrets.delete(id)
        defaults.set(metadata, forKey: storageKey)
    }
}
