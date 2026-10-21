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
    case invalidProfile, invalidName, tooManyProfiles, keychain
    var errorDescription: String? {
        switch self {
        case .invalidProfile: return "Enter an RTMPS application URL and a separate stream key. Plain RTMP requires explicit permission. Use the application address, without a login, query or fragment."
        case .invalidName: return "Give the profile a name between 1 and 48 characters."
        case .tooManyProfiles: return "Delete a saved profile before adding another (maximum 16)."
        case .keychain: return "Destination secrets could not be read or saved in the device Keychain. Unlock the phone and try again."
        }
    }
}

struct SavedDestinationProfile: Codable, Identifiable {
    let id: UUID
    var name: String
    var endpoint: String
    var allowPlaintext: Bool
    var transport: String? = nil
}

/// Only explicit Save persists a destination. Keys stay out of UserDefaults,
/// exports and logs; Keychain entries are local, unlocked-device-only items.
final class DestinationProfileStore {
    private let defaults: UserDefaults
    private let service = "dev.forgesworn.sapflow.manual-publisher"
    private let storageKey = "sapflow.manual-destinations.v1"
    init(defaults: UserDefaults = .standard) { self.defaults = defaults }
    func load() -> [SavedDestinationProfile] {
        guard let data = defaults.data(forKey: storageKey), data.count <= 128 * 1024,
              let profiles = try? JSONDecoder().decode([SavedDestinationProfile].self, from: data), profiles.count <= 16 else { return [] }
        return profiles
    }
    func key(for id: UUID) throws -> String {
        var query = keyQuery(id)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data, data.count <= 4096, let key = String(data: data, encoding: .utf8) else { throw DestinationError.keychain }
        return key
    }
    func save(id: UUID?, name: String, destination: ManualDestination, allowPlaintext: Bool) throws -> UUID {
        try saveProfile(id: id, name: name, endpoint: destination.endpoint, secret: destination.streamKey, allowPlaintext: allowPlaintext, transport: "rtmp")
    }
    private struct SrtSecret: Codable { let streamID: String; let passphrase: String }
    func srtSecret(for id: UUID) throws -> (streamID: String, passphrase: String) {
        let saved = try key(for: id)
        guard let bytes = saved.data(using: .utf8), let secret = try? JSONDecoder().decode(SrtSecret.self, from: bytes) else { throw DestinationError.keychain }
        return (secret.streamID, secret.passphrase)
    }
    func saveSrt(id: UUID?, name: String, destination: ManualSrtDestination, allowPlaintext: Bool) throws -> UUID {
        let bytes = try JSONEncoder().encode(SrtSecret(streamID: destination.streamID, passphrase: destination.passphrase ?? ""))
        guard let secret = String(data: bytes, encoding: .utf8) else { throw DestinationError.keychain }
        return try saveProfile(id: id, name: name, endpoint: destination.endpoint, secret: secret, allowPlaintext: allowPlaintext, transport: "srt")
    }
    private func saveProfile(id: UUID?, name: String, endpoint: String, secret: String, allowPlaintext: Bool, transport: String) throws -> UUID {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= 48, !name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { throw DestinationError.invalidName }
        var profiles = load()
        guard id.map({ existing in profiles.contains(where: { $0.id == existing }) }) == true || profiles.count < 16 else { throw DestinationError.tooManyProfiles }
        let id = id ?? UUID()
        let query = keyQuery(id)
        let attributes: [String: Any] = [kSecValueData as String: Data(secret.utf8), kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly]
        let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updated == errSecItemNotFound {
            var added = query
            for (key, value) in attributes { added[key] = value }
            guard SecItemAdd(added as CFDictionary, nil) == errSecSuccess else { throw DestinationError.keychain }
        } else if updated != errSecSuccess { throw DestinationError.keychain }
        let profile = SavedDestinationProfile(id: id, name: name, endpoint: endpoint, allowPlaintext: allowPlaintext, transport: transport)
        if let index = profiles.firstIndex(where: { $0.id == id }) { profiles[index] = profile }
        else { profiles.append(profile) }
        defaults.set(try JSONEncoder().encode(profiles), forKey: storageKey)
        return id
    }
    func delete(_ id: UUID) throws {
        let removed = SecItemDelete(keyQuery(id) as CFDictionary)
        guard removed == errSecSuccess || removed == errSecItemNotFound else { throw DestinationError.keychain }
        defaults.set(try JSONEncoder().encode(load().filter { $0.id != id }), forKey: storageKey)
    }
    private func keyQuery(_ id: UUID) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: id.uuidString]
    }
}
