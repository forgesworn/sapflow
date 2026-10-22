import Foundation
import Darwin

/// App-private local metadata and bounded copies. All filesystem methods run
/// on one cold IO owner. No deletion, repair, media decoding or publication.
final class RecordingCatalogue {
    enum Failure: Error, Equatable { case busy, capacity, unsafe, changed, ineligible, cancelled, io, closed }
    enum State: String { case writing, finalised, incomplete, orphanUnverified }
    enum Completion: String, Codable { case finalised, incomplete }
    enum Location: String { case privateRecording, legacyDocuments }
    enum Container: String, Codable { case mov, m4a }
    struct Identity: Codable, Equatable {
        let segmentID: String
        let showID: String
        let segmentIndex: Int64
        let profileID: String
        // Missing in historical V1 receipts: those exact files remain MOV.
        let container: Container?
        init(segmentID: String, showID: String, segmentIndex: Int64, profileID: String, container: Container? = nil) throws {
            guard Self.uuid(segmentID), Self.uuid(showID), segmentIndex >= 0,
                  let profile = CaptureIntent(rawValue: profileID),
                  container != .m4a || !profile.hasVideo else { throw Failure.unsafe }
            self.segmentID = segmentID; self.showID = showID
            self.segmentIndex = segmentIndex; self.profileID = profileID; self.container = container
        }
        fileprivate static func uuid(_ text: String) -> Bool {
            text.count == 36 && UUID(uuidString: text)?.uuidString.lowercased() == text
        }
        var filename: String { "Sapflow-\(segmentID).\((container ?? .mov).rawValue)" }
    }
    final class Show {
        let showID = UUID().uuidString.lowercased()
        private let profile: CaptureIntent
        private let lock = NSLock()
        private var index: Int64 = 0
        fileprivate init(profile: CaptureIntent) { self.profile = profile }
        func nextIdentity() throws -> Identity {
            lock.lock(); defer { lock.unlock() }
            guard index < Int64.max else { throw Failure.capacity }
            defer { index += 1 }
            return try Identity(segmentID: UUID().uuidString.lowercased(), showID: showID,
                                segmentIndex: index, profileID: profile.rawValue, container: profile.hasVideo ? nil : .m4a)
        }
    }
    /// Opaque exact selection. Metadata is not a claim that the media decodes.
    struct Entry {
        let segmentID: String
        let identity: Identity?
        let state: State
        let location: Location
        let bytes: UInt64
        let durationMs: Int64?
        let createdAtMs: Int64?
        fileprivate let filename: String
        fileprivate let file: Fingerprint?
        fileprivate let receipt: Data?
        fileprivate let receiptFile: Fingerprint?
        var fileExtension: String { filename.hasSuffix(".m4a") ? "m4a" : "mov" }
        var id: String { location.rawValue + ":" + filename }
    }
    fileprivate struct Fingerprint: Codable, Equatable {
        let device: Int32
        let inode: UInt64
        let size: Int64
        let modifiedSeconds: Int64
        let modifiedNanoseconds: Int64
        let changedSeconds: Int64
        let changedNanoseconds: Int64
        init(_ s: stat) {
            device = s.st_dev; inode = s.st_ino; size = s.st_size
            modifiedSeconds = Int64(s.st_mtimespec.tv_sec); modifiedNanoseconds = Int64(s.st_mtimespec.tv_nsec)
            changedSeconds = Int64(s.st_ctimespec.tv_sec); changedNanoseconds = Int64(s.st_ctimespec.tv_nsec)
        }
    }
    private struct Receipt: Codable {
        let version: Int
        let identity: Identity
        let state: String
        let createdAtMs: Int64
        let durationMs: Int64?
        let file: Fingerprint?
    }
    private struct Directory {
        let url: URL
        let fd: Int32
        let device: Int32
        let inode: UInt64
    }
    private enum Owner { case writer(String), export(String), preflight }
    private let root: Directory
    private let legacy: Directory?
    private let closeFileDescriptor: (Int32) -> Int32
    /// Cold metadata writes only. Tests replace syscall receipts, never the
    /// catalogue's ownership, identity checks or mutation order.
    struct MediaUpdateIO {
        let write: (Int32, UnsafeRawPointer, Int, off_t) -> Int
        let synchronize: (Int32) -> Int32
        static let native = MediaUpdateIO(write: { Darwin.pwrite($0, $1, $2, $3) }, synchronize: Darwin.fsync)
    }
    private let mediaUpdateIO: MediaUpdateIO
    private let lock = NSLock()
    private var owner: (token: UUID, kind: Owner)?
    private var uncertain = false
    private var closed = false
    private var closing = false
    // Unknown closure keeps both the real wrappers and the lifetime flock.
    private var retained: [AnyObject] = []
    private var retainedSelf: RecordingCatalogue?
    private static let maxSegments = 256
    private static let maxEntries = 1024
    private static let maxReceipt = 4096

    init(root: URL, legacyRoot: URL? = nil,
         mediaUpdateIO: MediaUpdateIO = .native,
         closeFileDescriptor: @escaping (Int32) -> Int32 = Darwin.close) throws {
        self.closeFileDescriptor = closeFileDescriptor
        self.mediaUpdateIO = mediaUpdateIO
        guard root.isFileURL, !root.path.utf8.contains(0) else { throw Failure.unsafe }
        do { try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700]) }
        catch { throw Failure.io }
        self.root = try Self.openDirectory(root)
        guard flock(self.root.fd, LOCK_EX | LOCK_NB) == 0 else {
            Darwin.close(self.root.fd); throw Failure.busy
        }
        do {
            if let legacyRoot {
                guard legacyRoot.isFileURL, !legacyRoot.path.utf8.contains(0) else { throw Failure.unsafe }
                let fd = Darwin.open(legacyRoot.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                if fd < 0 && errno == ENOENT { legacy = nil }
                else {
                    guard fd >= 0 else { throw Failure.unsafe }
                    do {
                        let candidate = try Self.directory(legacyRoot, fd)
                        guard candidate.device != self.root.device || candidate.inode != self.root.inode else { throw Failure.unsafe }
                        legacy = candidate
                    } catch { Darwin.close(fd); throw error }
                }
            } else { legacy = nil }
        } catch { Darwin.close(self.root.fd); throw error }
    }
    deinit {
        // A live lease strongly retains this object. An uncertain close forms
        // a deliberate retained owner, released only by process termination.
        if !closed { if let legacy { Darwin.close(legacy.fd) }; Darwin.close(root.fd) }
    }
    private static func openDirectory(_ url: URL) throws -> Directory {
        let fd = Darwin.open(url.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw Failure.unsafe }
        do { return try directory(url, fd, privateRoot: true) } catch { Darwin.close(fd); throw error }
    }
    private static func directory(_ url: URL, _ fd: Int32, privateRoot: Bool = false) throws -> Directory {
        var s = stat()
        guard fstat(fd, &s) == 0, s.st_uid == getuid(), (s.st_mode & S_IFMT) == S_IFDIR,
              s.st_mode & (privateRoot ? 0o077 : 0o022) == 0 else { throw Failure.unsafe }
        return Directory(url: url, fd: fd, device: s.st_dev, inode: s.st_ino)
    }
    private func check(_ directory: Directory) throws {
        lock.lock(); let usable = !closed && !closing && !uncertain; lock.unlock()
        guard usable else { throw Failure.closed }
        var s = stat()
        guard lstat(directory.url.path, &s) == 0, (s.st_mode & S_IFMT) == S_IFDIR,
              s.st_dev == directory.device, s.st_ino == directory.inode else { throw Failure.changed }
        guard s.st_uid == getuid(), s.st_mode & (directory.fd == root.fd ? 0o077 : 0o022) == 0 else { throw Failure.unsafe }
    }
    private func poison(_ resource: AnyObject? = nil) {
        lock.lock(); uncertain = true; retainedSelf = self
        if let resource, retained.count < 4 { retained.append(resource) }
        lock.unlock()
    }
    var cleanupRetained: Bool { lock.lock(); defer { lock.unlock() }; return owner != nil || uncertain || closing }
    var cleanupUncertain: Bool { lock.lock(); defer { lock.unlock() }; return uncertain }
    var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return closed }
    /// Recording-worker configuration only. Never a picker/share URL.
    var recordingRootURL: URL { root.url }
    private func reserve(_ kind: Owner) throws -> UUID {
        lock.lock(); defer { lock.unlock() }
        guard !closed, !closing, !uncertain else { throw Failure.closed }
        guard owner == nil else { throw Failure.busy }
        let token = UUID(); owner = (token, kind); return token
    }
    private func release(_ token: UUID) throws {
        lock.lock(); defer { lock.unlock() }
        guard !uncertain, owner?.token == token else { throw Failure.closed }
        owner = nil
    }
    func close() throws {
        lock.lock()
        guard !closed else { lock.unlock(); return }
        guard owner == nil, !closing, !uncertain else { lock.unlock(); throw Failure.busy }
        // Hold admission through the actual closes; no callbacks under lock.
        closing = true; lock.unlock()
        var failed = false
        if let legacy, closeFileDescriptor(legacy.fd) != 0 { failed = true }
        if closeFileDescriptor(root.fd) != 0 { failed = true }
        lock.lock(); closing = false; closed = !failed; lock.unlock()
        if failed { poison(); throw Failure.io }
    }
    func newShow(profile: CaptureIntent) -> Show { Show(profile: profile) }
    func checkRecordingCapacity() throws {
        let token = try reserve(.preflight)
        do {
            guard try scan().count < Self.maxSegments else { throw Failure.capacity }
            try release(token)
        } catch { if !cleanupUncertain { try release(token) }; throw error }
    }
    private func directory(_ location: Location) throws -> Directory {
        if location == .privateRecording { return root }
        guard let legacy else { throw Failure.changed }; return legacy
    }
    private func info(_ directory: Directory, _ name: String) throws -> Fingerprint? {
        try check(directory)
        var s = stat()
        if fstatat(directory.fd, name, &s, AT_SYMLINK_NOFOLLOW) < 0 {
            if errno == ENOENT { return nil }; throw Failure.io
        }
        guard (s.st_mode & S_IFMT) == S_IFREG, s.st_nlink == 1, s.st_uid == getuid(), s.st_size >= 0 else { throw Failure.unsafe }
        return Fingerprint(s)
    }
    private func descriptorInfo(_ fd: Int32) throws -> Fingerprint {
        var s = stat()
        guard fstat(fd, &s) == 0, (s.st_mode & S_IFMT) == S_IFREG,
              s.st_nlink == 1, s.st_uid == getuid(), s.st_size >= 0 else { throw Failure.unsafe }
        return Fingerprint(s)
    }
    private func closeFD(_ fd: Int32) throws {
        // A close error is ambiguous. Never retry the numeric FD: it may
        // already have been recycled. Tests inject only this syscall receipt.
        guard closeFileDescriptor(fd) == 0 else { poison(); throw Failure.io }
    }
    private func readReceipt(_ directory: Directory, _ name: String) throws -> (Data?, Fingerprint?) {
        guard let before = try info(directory, name) else { return (nil, nil) }
        guard before.size > 0, before.size <= Self.maxReceipt else { return (nil, before) }
        let fd = openat(directory.fd, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw Failure.io }
        var needsClose = true
        do {
            guard try descriptorInfo(fd) == before else { throw Failure.changed }
            var bytes = Data(count: Int(before.size))
            try bytes.withUnsafeMutableBytes { buffer in
                var position = 0
                while position < buffer.count {
                    let count = Darwin.read(fd, buffer.baseAddress!.advanced(by: position), buffer.count - position)
                    if count < 0 && errno == EINTR { continue }
                    guard count > 0 else { throw Failure.changed }; position += count
                }
            }
            var extra: UInt8 = 0
            guard Darwin.read(fd, &extra, 1) == 0, try descriptorInfo(fd) == before,
                  try info(directory, name) == before else { throw Failure.changed }
            needsClose = false; try closeFD(fd); return (bytes, before)
        } catch { if needsClose { try closeFD(fd) }; throw error }
    }
    private func encode(_ receipt: Receipt) throws -> Data {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(receipt)
        guard data.count <= Self.maxReceipt else { throw Failure.capacity }; return data
    }
    private func decode(_ bytes: Data, _ id: String) -> Receipt? {
        guard let receipt = try? JSONDecoder().decode(Receipt.self, from: bytes), receipt.version == 1,
              let validated = try? Identity(segmentID: receipt.identity.segmentID, showID: receipt.identity.showID,
                                            segmentIndex: receipt.identity.segmentIndex, profileID: receipt.identity.profileID, container: receipt.identity.container),
              validated.segmentID == id, receipt.createdAtMs > 0,
              ["writing", "finalised", "incomplete"].contains(receipt.state),
              receipt.durationMs.map({ $0 >= 0 }) ?? true,
              (receipt.file == nil) == (receipt.durationMs == nil),
              receipt.state != "finalised" || receipt.file != nil,
              receipt.file.map({ $0.size > 0 }) ?? true,
              (try? encode(receipt)) == bytes else { return nil }
        return receipt
    }
    private func receiptName(_ identity: Identity) -> String { "Sapflow-\(identity.segmentID).receipt.json" }
    private func writeReceipt(_ receipt: Receipt) throws {
        try check(root)
        let data = try encode(receipt), name = receiptName(receipt.identity)
        _ = try info(root, name) // Refuse unsafe existing destinations.
        let temporary = ".recording-\(UUID().uuidString.lowercased()).tmp"
        let fd = openat(root.fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw Failure.io }
        var needsClose = true
        defer { unlinkat(root.fd, temporary, 0) }
        do {
            try data.withUnsafeBytes { buffer in
                var position = 0
                while position < buffer.count {
                    let count = Darwin.write(fd, buffer.baseAddress!.advanced(by: position), buffer.count - position)
                    if count < 0 && errno == EINTR { continue }
                    guard count > 0 else { throw Failure.io }; position += count
                }
            }
            guard fsync(fd) == 0 else { throw Failure.io }
            needsClose = false; try closeFD(fd)
            guard renameat(root.fd, temporary, root.fd, name) == 0, fsync(root.fd) == 0,
                  try readReceipt(root, name).0 == data else { throw Failure.io }
        } catch {
            if needsClose { try? closeFD(fd) }
            // Unknown durable metadata is never adopted by this live owner.
            poison(); throw Failure.io
        }
    }
    func begin(identity: Identity, createdAtMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) throws -> Writer {
        guard createdAtMs > 0 else { throw Failure.unsafe }
        let token = try reserve(.writer(identity.segmentID))
        do {
            guard try scan().count < Self.maxSegments else { throw Failure.capacity }
            guard try info(root, "Sapflow-\(identity.segmentID).mov") == nil,
                  try info(root, "Sapflow-\(identity.segmentID).m4a") == nil,
                  try info(root, receiptName(identity)) == nil else { throw Failure.changed }
            try writeReceipt(Receipt(version: 1, identity: identity, state: "writing", createdAtMs: createdAtMs, durationMs: nil, file: nil))
            return Writer(catalogue: self, token: token, identity: identity, createdAtMs: createdAtMs)
        } catch { if !cleanupUncertain { try release(token) }; throw error }
    }
    final class Writer {
        private let catalogue: RecordingCatalogue
        private let token: UUID
        let identity: Identity
        private let createdAtMs: Int64
        private let lock = NSLock()
        private enum Phase { case writing, updating, cancelled, finishing, closed }
        private var phase = Phase.writing
        private var cancelAfterUpdate = false
        fileprivate init(catalogue: RecordingCatalogue, token: UUID, identity: Identity, createdAtMs: Int64) {
            self.catalogue = catalogue; self.token = token; self.identity = identity; self.createdAtMs = createdAtMs
        }
        var fileURL: URL { catalogue.root.url.appendingPathComponent(identity.filename) }
        var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return phase == .closed }
        func cancel() {
            lock.lock()
            if phase == .writing { phase = .cancelled }
            else if phase == .updating { cancelAfterUpdate = true }
            lock.unlock()
        }
        func retainUncertainCleanup() { catalogue.poison(self) }
        /// A scoped descriptor after AVAssetWriter has actually closed. Neither
        /// its number nor a share URL escapes to the caller. A stale lease can
        /// never touch a recycled descriptor after this method returns.
        final class MediaUpdate {
            private let operation = NSRecursiveLock()
            private let io: MediaUpdateIO
            fileprivate let fd: Int32
            let originalBytes: UInt64
            fileprivate var active = true
            fileprivate var mutationBegan = false
            fileprivate var committed = false
            fileprivate var expectedBytes: UInt64
            fileprivate init(fd: Int32, bytes: UInt64, io: MediaUpdateIO) {
                self.fd = fd; originalBytes = bytes; expectedBytes = bytes; self.io = io
            }
            fileprivate func invalidate() { operation.lock(); active = false; operation.unlock() }
            private static let metadataLimit = 16 * 1024 * 1024 + 4096
            func read(offset: UInt64, count: Int) throws -> Data {
                operation.lock(); defer { operation.unlock() }
                guard active, count >= 0, count <= Self.metadataLimit,
                      offset <= UInt64(Int64.max), UInt64(count) <= UInt64(Int64.max) - offset else { throw Failure.unsafe }
                var bytes = Data(count: count)
                try bytes.withUnsafeMutableBytes { buffer in
                    var position = 0
                    while position < count {
                        let n = pread(fd, buffer.baseAddress!.advanced(by: position), count - position, off_t(offset) + off_t(position))
                        if n < 0 && errno == EINTR { continue }
                        guard n > 0 else { throw Failure.io }
                        position += n
                    }
                }
                return bytes
            }
            private func write(_ bytes: Data, offset: UInt64) throws {
                operation.lock(); defer { operation.unlock() }
                guard active, !bytes.isEmpty, bytes.count <= Self.metadataLimit,
                      offset <= UInt64(Int64.max), UInt64(bytes.count) <= UInt64(Int64.max) - offset else { throw Failure.unsafe }
                try bytes.withUnsafeBytes { buffer in
                    var position = 0
                    while position < bytes.count {
                        let n = io.write(fd, buffer.baseAddress!.advanced(by: position), bytes.count - position, off_t(offset) + off_t(position))
                        if n < 0 && errno == EINTR { continue }
                        guard n > 0 else { throw Failure.io }
                        position += n
                    }
                }
            }
            /// All preparation precedes mutation. The original header remains
            /// usable until the complete replacement is durably appended and
            /// read back. No media byte or chunk offset is relocated.
            func installMovieHeader(offset: UInt64, original: Data, replacement: Data) throws {
                operation.lock(); defer { operation.unlock() }
                guard active, !mutationBegan, original.count >= 8, replacement.count >= 8,
                      original.startIndex == 0, replacement.startIndex == 0,
                      original.count <= Self.metadataLimit, replacement.count <= Self.metadataLimit,
                      originalBytes <= UInt64(Int64.max), UInt64(replacement.count) <= UInt64(Int64.max) - originalBytes,
                      offset <= originalBytes, UInt64(original.count) == originalBytes - offset,
                      original.subdata(in: 4..<8) == Data("moov".utf8),
                      replacement.subdata(in: 4..<8) == Data("moov".utf8),
                      try read(offset: offset, count: original.count) == original else { throw Failure.changed }
                expectedBytes = originalBytes + UInt64(replacement.count)
                mutationBegan = true
                try write(replacement, offset: originalBytes)
                guard io.synchronize(fd) == 0,
                      try read(offset: originalBytes, count: replacement.count) == replacement,
                      try read(offset: offset + 4, count: 4) == Data("moov".utf8) else { throw Failure.io }
                try write(Data("free".utf8), offset: offset + 4)
                guard io.synchronize(fd) == 0,
                      try read(offset: offset + 4, count: 4) == Data("free".utf8),
                      try read(offset: originalBytes, count: replacement.count) == replacement else { throw Failure.io }
                committed = true
            }
        }
        /// Unsupported metadata before the first write may leave the original
        /// prefix explicitly incomplete. Any ambiguous mutation or close keeps
        /// the catalogue owner retained and blocks a new recording.
        func updateClosedMedia<T>(_ body: (MediaUpdate) throws -> T) throws -> T {
            lock.lock()
            guard phase == .writing else { lock.unlock(); throw Failure.busy }
            phase = .updating; lock.unlock()
            var needsClose = false
            var knownClosed = true
            var descriptor: Int32 = -1
            var update: MediaUpdate?
            do {
                try catalogue.check(catalogue.root)
                guard let before = try catalogue.info(catalogue.root, identity.filename), before.size > 0 else { throw Failure.ineligible }
                descriptor = openat(catalogue.root.fd, identity.filename, O_RDWR | O_NOFOLLOW | O_CLOEXEC)
                guard descriptor >= 0 else { throw Failure.io }
                needsClose = true; knownClosed = false
                guard try catalogue.descriptorInfo(descriptor) == before else { throw Failure.changed }
                let lease = MediaUpdate(fd: descriptor, bytes: UInt64(before.size), io: catalogue.mediaUpdateIO); update = lease
                let result = try body(lease)
                lease.invalidate()
                guard !lease.mutationBegan || lease.committed else { throw Failure.io }
                let after = try catalogue.descriptorInfo(descriptor)
                guard after.device == before.device, after.inode == before.inode,
                      after.size >= 0, UInt64(after.size) == lease.expectedBytes,
                      try catalogue.info(catalogue.root, identity.filename) == after else { throw Failure.changed }
                try catalogue.check(catalogue.root)
                needsClose = false // Never retry an ambiguous numeric close.
                try catalogue.closeFD(descriptor); knownClosed = true
                lock.lock(); phase = cancelAfterUpdate ? .cancelled : .writing; lock.unlock()
                return result
            } catch {
                update?.invalidate()
                if needsClose {
                    needsClose = false
                    do { try catalogue.closeFD(descriptor); knownClosed = true } catch { knownClosed = false }
                }
                if !knownClosed || update?.mutationBegan == true { catalogue.poison(self) }
                else { lock.lock(); phase = cancelAfterUpdate ? .cancelled : .writing; lock.unlock() }
                throw error
            }
        }
        /// Invoke only after the real writer completed and owns no further IO.
        /// Prior append/backlog loss must select incomplete even if it completed.
        func complete(durationMs: Int64, outcome: Completion) throws {
            guard durationMs >= 0 else { throw Failure.unsafe }
            lock.lock()
            guard phase == .writing else { lock.unlock(); throw Failure.cancelled }
            phase = .finishing; lock.unlock()
            do {
                let root = catalogue.root
                guard let before = try catalogue.info(root, identity.filename), before.size > 0 else { throw Failure.ineligible }
                let fd = openat(root.fd, identity.filename, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
                guard fd >= 0 else { throw Failure.io }
                var needsClose = true
                do {
                    guard try catalogue.descriptorInfo(fd) == before, fsync(fd) == 0,
                          try catalogue.descriptorInfo(fd) == before,
                          try catalogue.info(root, identity.filename) == before else { throw Failure.changed }
                    needsClose = false; try catalogue.closeFD(fd)
                } catch { if needsClose { try? catalogue.closeFD(fd) }; throw error }
                try catalogue.writeReceipt(Receipt(version: 1, identity: identity, state: outcome.rawValue,
                                                    createdAtMs: createdAtMs, durationMs: durationMs, file: before))
                try catalogue.release(token)
                lock.lock(); phase = .closed; lock.unlock()
            } catch { catalogue.poison(self); throw error }
        }
        /// The caller proves real cancellation/close first. No file is invented.
        func ownersClosed() throws {
            cancel(); lock.lock()
            defer { lock.unlock() }
            if phase == .closed { return }
            guard phase == .cancelled else { throw Failure.busy }
            try catalogue.release(token)
            phase = .closed
        }
    }
    private func names(_ directory: Directory, count: inout Int) throws -> [String] {
        try check(directory)
        // Independent directory position. BSD flock remains on the lifetime
        // root description; opening/closing this one cannot release that lock.
        let copied = openat(directory.fd, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard copied >= 0 else { throw Failure.io }
        guard let stream = fdopendir(copied) else { try closeFD(copied); throw Failure.io }
        var result: [String] = []
        var problem: Error?
        errno = 0
        while let item = readdir(stream) {
            let name = withUnsafePointer(to: item.pointee.d_name) {
                $0.withMemoryRebound(to: CChar.self, capacity: Int(item.pointee.d_namlen) + 1) { String(cString: $0) }
            }
            if name == "." || name == ".." { continue }
            count += 1
            if count > Self.maxEntries { problem = Failure.capacity; break }
            result.append(name)
            errno = 0
        }
        if problem == nil && errno != 0 { problem = Failure.io }
        if closedir(stream) != 0 { poison(); throw Failure.io }
        if let problem { throw problem }
        try check(directory); return result
    }
    private static func mediaID(_ name: String) -> String? {
        guard name.hasPrefix("Sapflow-"), name.hasSuffix(".mov") || name.hasSuffix(".m4a") else { return nil }
        let text = String(name.dropFirst(8).dropLast(4))
        guard text.count == 36, UUID(uuidString: text)?.uuidString.lowercased() == text.lowercased() else { return nil }
        return text.lowercased()
    }
    private func entry(location: Location, name: String, id: String) throws -> Entry {
        let directory = try directory(location), file = try info(directory, name)
        let metadata = location == .privateRecording ? try readReceipt(directory, "Sapflow-\(id).receipt.json") : (nil, nil)
        let decoded = metadata.0.flatMap { decode($0, id) }
        // A known completed-file fingerprint cannot be adopted by a replaced
        // file, including an explicitly incomplete container.
        let receipt = decoded?.identity.filename != name || (decoded?.file != nil && decoded?.file != file) ? nil : decoded
        lock.lock()
        let active: Bool
        if case .writer(let segment)? = owner?.kind { active = segment == id } else { active = false }
        lock.unlock()
        let state: State
        if location == .legacyDocuments || receipt == nil { state = .orphanUnverified }
        else if receipt?.state == "writing" && active { state = .writing }
        else if file == nil || file?.size == 0 { state = .orphanUnverified }
        else if receipt?.state == "finalised" { state = receipt?.file == file ? .finalised : .orphanUnverified }
        else { state = .incomplete }
        return Entry(segmentID: id, identity: receipt?.identity, state: state, location: location,
                     bytes: UInt64(file?.size ?? 0), durationMs: receipt?.durationMs, createdAtMs: receipt?.createdAtMs,
                     filename: name, file: file, receipt: metadata.0, receiptFile: metadata.1)
    }
    func scan() throws -> [Entry] {
        var count = 0, result: [Entry] = []
        let locations: [(Location, Directory)] = [(.privateRecording, root)] + (legacy.map { [(.legacyDocuments, $0)] } ?? [])
        for (location, directory) in locations {
            var files: [String: String] = [:]
            var media: Set<String> = []
            for name in try names(directory, count: &count) {
                if let id = Self.mediaID(name) {
                    guard media.insert(id).inserted else { throw Failure.unsafe }; files[id] = name
                } else if location == .privateRecording, name.hasPrefix("Sapflow-"), name.hasSuffix(".receipt.json") {
                    let id = String(name.dropFirst(8).dropLast(13))
                    if Identity.uuid(id), files[id] == nil {
                        let metadata = try readReceipt(directory, name).0.flatMap { decode($0, id) }
                        files[id] = metadata?.identity.filename ?? "Sapflow-\(id).mov"
                    }
                }
            }
            for (id, name) in files.sorted(by: { $0.key < $1.key }) {
                guard result.count < Self.maxSegments else { throw Failure.capacity }
                result.append(try entry(location: location, name: name, id: id))
            }
        }
        return result
    }
    private func same(_ a: Entry, _ b: Entry) throws {
        guard a.id == b.id, a.segmentID == b.segmentID, a.identity == b.identity, a.state == b.state,
              a.bytes == b.bytes, a.file == b.file, a.receipt == b.receipt, a.receiptFile == b.receiptFile else { throw Failure.changed }
    }
    func openExport(entry selected: Entry, allowUnverified: Bool = false) throws -> Export {
        guard selected.state != .writing, selected.bytes > 0,
              allowUnverified || selected.state == .finalised else { throw Failure.ineligible }
        let token = try reserve(.export(selected.id))
        var export: Export?
        do {
            try same(selected, entry(location: selected.location, name: selected.filename, id: selected.segmentID))
            let directory = try directory(selected.location)
            let fd = openat(directory.fd, selected.filename, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
            guard fd >= 0 else { throw Failure.io }
            let value = Export(catalogue: self, token: token, entry: selected, fd: fd); export = value
            guard try descriptorInfo(fd) == selected.file else { throw Failure.changed }
            try same(selected, entry(location: selected.location, name: selected.filename, id: selected.segmentID))
            return value
        } catch {
            if let export { do { try export.close() } catch { poison(export) } }
            else if !cleanupUncertain { try release(token) }
            throw error
        }
    }
    final class Export {
        private let catalogue: RecordingCatalogue
        private let token: UUID
        private let entry: Entry
        private let fd: Int32
        private let lock = NSLock()
        private var copied = false
        private var copying = false
        private var closing = false
        private var closed = false
        private var failed = false
        fileprivate init(catalogue: RecordingCatalogue, token: UUID, entry: Entry, fd: Int32) {
            self.catalogue = catalogue; self.token = token; self.entry = entry; self.fd = fd
        }
        var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return closed }
        /// The write callback must write the entire chunk or throw. The caller
        /// owns destination creation, sync, close and security-scope lifetime.
        func copy(write: (Data) throws -> Void, cancelled: () -> Bool = { false }) throws -> UInt64 {
            lock.lock()
            guard !copied, !closed, !closing, !failed else { lock.unlock(); throw Failure.closed }
            copied = true; copying = true; lock.unlock()
            defer { lock.lock(); copying = false; lock.unlock() }
            try catalogue.same(entry, catalogue.entry(location: entry.location, name: entry.filename, id: entry.segmentID))
            guard try catalogue.descriptorInfo(fd) == entry.file else { throw Failure.changed }
            var total: UInt64 = 0
            while total < entry.bytes {
                guard !cancelled() else { throw Failure.cancelled }
                var chunk = Data(count: Int(min(65_536, entry.bytes - total)))
                let count = chunk.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress!, $0.count) }
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw Failure.changed }
                chunk.count = count; try write(chunk); total += UInt64(count)
            }
            guard !cancelled() else { throw Failure.cancelled }
            var extra: UInt8 = 0
            guard Darwin.read(fd, &extra, 1) == 0, try catalogue.descriptorInfo(fd) == entry.file else { throw Failure.changed }
            try catalogue.same(entry, catalogue.entry(location: entry.location, name: entry.filename, id: entry.segmentID))
            return total
        }
        func close() throws {
            lock.lock()
            if closed { lock.unlock(); return }
            guard !copying else { lock.unlock(); throw Failure.busy }
            guard !closing, !failed else { lock.unlock(); throw Failure.closed }
            closing = true; lock.unlock()
            do {
                try catalogue.closeFD(fd)
                try catalogue.release(token)
                lock.lock(); closed = true; closing = false; lock.unlock()
            } catch {
                lock.lock(); failed = true; closing = false; lock.unlock()
                catalogue.poison(self); throw error
            }
        }
    }
}
