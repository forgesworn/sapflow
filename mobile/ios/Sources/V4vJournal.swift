import Foundation
import CryptoKit
import Security
import Darwin

protocol V4vJournalKeySource: AnyObject {
    func load() throws -> Data?
    func create() throws -> Data
}

/// Only the publishing journal participates. The foreground app owner captures
/// one unguessable show identity; neither a signer store nor a restored process
/// implicitly acquires permission to use a key while the device is locked.
protocol V4vJournalShowLeasing: AnyObject {
    func acquireShowLease(owner: UUID) throws
    func releaseShowLease(owner: UUID)
}

/// An app journal encryption key, not a creator Nostr key or signer. Tests use
/// an injected key source and never access a user's Keychain.
final class V4vKeychainJournalKey: V4vJournalKeySource {
    private let service: String
    init(service: String) throws {
        guard service.range(of: #"^[A-Za-z0-9.-]{1,128}\z"#, options: .regularExpression) != nil else { throw V4vFailure.journal }
        self.service = service
    }
    private var query: [String: Any] { [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: "journal-v1"] }
    func load() throws -> Data? {
        var q = query; q[kSecReturnData as String] = true; q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data, data.count == 32 else { throw V4vFailure.journal }
        return data
    }
    func create() throws -> Data {
        var data = Data(count: 32)
        let status = data.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 32, $0.baseAddress!) }
        guard status == errSecSuccess else { throw V4vFailure.journal }
        var q = query; q[kSecValueData as String] = data; q[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        guard SecItemAdd(q as CFDictionary, nil) == errSecSuccess else { throw V4vFailure.journal }
        return data
    }
}

/// Bounded encrypted file in a caller-provided app-private directory. Atomic
/// rename is followed by directory fsync and exact readback BEFORE success.
/// No repair/reset/import API: missing halves and corruption fail closed.
final class V4vEncryptedJournal: V4vJournalStore, V4vJournalShowLeasing {
    private let directory: URL
    private let keys: V4vJournalKeySource
    private let aad: Data
    private var ownership: Int32 = -1
    private var poisoned = false
    private let maxFile = 262_144
    private let name = "session.v1.sealed"
    private var showLease: (owner: UUID, key: Data)? // same serial journal owner, at most 32 key bytes
    func acquireShowLease(owner: UUID) throws {
        guard !poisoned else { throw V4vFailure.journal }
        if let showLease { guard showLease.owner == owner else { throw V4vFailure.busy }; return }
        // Foreground acquisition validates the existing encrypted halves. A
        // lease never creates a key or repairs a missing/corrupt journal.
        guard try load() != nil, let key = try keys.load(), key.count == 32 else { throw V4vFailure.journal }
        showLease = (owner, key)
    }
    func releaseShowLease(owner: UUID) {
        guard showLease?.owner == owner else { return }
        showLease = nil // Data lifetime release, not a byte-zeroisation claim.
    }
    private func currentKey() throws -> Data? { try showLease?.key ?? keys.load() }
    init(directory: URL, profile: V4vConnectorProfile, keys: V4vJournalKeySource) throws {
        guard directory.isFileURL else { throw V4vFailure.journal }
        self.directory = directory; self.keys = keys
        aad = Data(("sapflow-v4v-journal-v1\n" + profile.apiOrigin).utf8)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        } catch { if !FileManager.default.fileExists(atPath: directory.path) { throw V4vFailure.journal } }
        let fd = try openDirectory()
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { Darwin.close(fd); throw V4vFailure.journal }
        ownership = fd
    }
    deinit { if ownership >= 0 { Darwin.close(ownership) } }
    private func openDirectory() throws -> Int32 {
        let fd = Darwin.open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw V4vFailure.journal }
        var s = stat()
        guard fstat(fd, &s) == 0, s.st_uid == getuid(), (s.st_mode & 0o7777) == 0o700 else { Darwin.close(fd); throw V4vFailure.journal }
        if ownership >= 0 {
            var pinned = stat()
            guard fstat(ownership, &pinned) == 0, pinned.st_dev == s.st_dev, pinned.st_ino == s.st_ino else { Darwin.close(fd); throw V4vFailure.journal }
        }
        return fd
    }
    private func read(_ directory: Int32) throws -> Data? {
        let fd = openat(directory, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        if fd < 0 { if errno == ENOENT { return nil }; throw V4vFailure.journal }
        defer { Darwin.close(fd) }
        var s = stat()
        guard fstat(fd, &s) == 0, s.st_uid == getuid(), (s.st_mode & S_IFMT) == S_IFREG, (s.st_mode & 0o7777) == 0o600,
              s.st_nlink == 1, s.st_size > 0, s.st_size <= maxFile else { throw V4vFailure.journal }
        var data = Data(count: Int(s.st_size))
        try data.withUnsafeMutableBytes { buffer in
            var total = 0
            while total < buffer.count {
                let n = Darwin.read(fd, buffer.baseAddress!.advanced(by: total), buffer.count - total)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { throw V4vFailure.journal }; total += n
            }
        }
        var extra: UInt8 = 0
        guard Darwin.read(fd, &extra, 1) == 0 else { throw V4vFailure.journal }
        return data
    }
    func load() throws -> V4vJSON? {
        guard !poisoned else { throw V4vFailure.journal }
        do {
            let fd = try openDirectory(); defer { Darwin.close(fd) }
            let bytes = try read(fd), key = try currentKey()
            if bytes == nil && key == nil { return nil }
            guard let bytes, let key, key.count == 32 else { throw V4vFailure.journal }
            let clear = try AES.GCM.open(AES.GCM.SealedBox(combined: bytes), using: SymmetricKey(data: key), authenticating: aad)
            return try V4vJSON.parse(clear)
        } catch { poisoned = true; throw V4vFailure.journal }
    }
    func save(_ document: V4vJSON) throws {
        guard !poisoned else { throw V4vFailure.journal }
        var temporary: String?
        var dir: Int32 = -1
        do {
            let clear = try document.encoded()
            dir = try openDirectory(); defer { Darwin.close(dir) }
            let old = try read(dir), existingKey = try currentKey()
            guard (old == nil) == (existingKey == nil) else { throw V4vFailure.journal }
            if let old, let existingKey {
                let previous = try AES.GCM.open(AES.GCM.SealedBox(combined: old), using: SymmetricKey(data: existingKey), authenticating: aad)
                _ = try V4vJSON.parse(previous)
            }
            let key = try existingKey ?? keys.create()
            guard key.count == 32, let encrypted = try AES.GCM.seal(clear, using: SymmetricKey(data: key), authenticating: aad).combined, encrypted.count <= maxFile else { throw V4vFailure.journal }
            let filename = ".session-" + UUID().uuidString.lowercased() + ".tmp"
            temporary = filename
            let output = openat(dir, filename, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            guard output >= 0 else { throw V4vFailure.journal }
            var outputOpen = true
            do {
                try encrypted.withUnsafeBytes { buffer in
                    var total = 0
                    while total < buffer.count {
                        let n = Darwin.write(output, buffer.baseAddress!.advanced(by: total), buffer.count - total)
                        if n < 0 && errno == EINTR { continue }
                        guard n > 0 else { throw V4vFailure.journal }; total += n
                    }
                }
                guard fsync(output) == 0 else { throw V4vFailure.journal }
                let result = Darwin.close(output); outputOpen = false
                guard result == 0 else { throw V4vFailure.journal }
            } catch { if outputOpen { Darwin.close(output) }; throw error }
            guard renameat(dir, filename, dir, name) == 0 else { throw V4vFailure.journal }
            temporary = nil
            guard fsync(dir) == 0, try read(dir) == encrypted else { throw V4vFailure.journal }
        } catch {
            // The live owner never continues after an uncertain durable save.
            // Retain the committed prefix; remove only this call's scratch.
            if let temporary, dir >= 0 {
                // The defer above has closed dir, so reopen our pinned directory.
                if let cleanup = try? openDirectory() { unlinkat(cleanup, temporary, 0); Darwin.close(cleanup) }
            }
            poisoned = true; throw V4vFailure.journal
        }
    }
}
