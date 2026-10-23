import Foundation
import Darwin
import CryptoKit

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
    /// Stable append incarnation, independent of changing size/mtime/ctime.
    /// Only beginAppend creates an owner from actual retained-descriptor stat.
    struct AppendSource: Codable, Equatable {
        let rootDevice: Int32
        let rootInode: UInt64
        let device: Int32
        let inode: UInt64
        let birthSeconds: Int64
        let birthNanoseconds: Int64
        fileprivate var valid: Bool {
            rootInode > 0 && inode > 0 && birthSeconds > 0 &&
                birthNanoseconds >= 0 && birthNanoseconds < 1_000_000_000
        }
        /// Versioned, domain-separated, length-framed UTF8 and big-endian
        /// integers. A digest is an equality binding, never authentication.
        func bindings(identity: Identity) throws -> AppendBindings {
            guard valid, (try? Identity(segmentID: identity.segmentID, showID: identity.showID,
                                        segmentIndex: identity.segmentIndex, profileID: identity.profileID,
                                        container: identity.container)) != nil else { throw Failure.unsafe }
            var recording = Data()
            Self.text("sapflow.checkpoint-recording.v1", into: &recording)
            Self.text(identity.segmentID, into: &recording)
            Self.text(identity.showID, into: &recording)
            Self.integer(UInt64(identity.segmentIndex), into: &recording)
            Self.text(identity.profileID, into: &recording)
            // Preserve absence versus an explicitly declared MOV container.
            Self.integer(identity.container == nil ? UInt8(0) : identity.container == .mov ? UInt8(1) : UInt8(2), into: &recording)
            let recordingDigest = Data(SHA256.hash(data: recording))
            var file = Data()
            Self.text("sapflow.checkpoint-file.v1", into: &file)
            file.append(recordingDigest)
            Self.integer(UInt32(bitPattern: rootDevice), into: &file)
            Self.integer(rootInode, into: &file)
            Self.integer(UInt32(bitPattern: device), into: &file)
            Self.integer(inode, into: &file)
            Self.integer(UInt64(birthSeconds), into: &file)
            Self.integer(UInt32(birthNanoseconds), into: &file)
            return AppendBindings(recording: recordingDigest, sourceFile: Data(SHA256.hash(data: file)))
        }
        private static func text(_ value: String, into bytes: inout Data) {
            let encoded = Data(value.utf8)
            integer(UInt32(encoded.count), into: &bytes); bytes.append(encoded)
        }
        private static func integer<T: FixedWidthInteger>(_ value: T, into bytes: inout Data) {
            var encoded = value.bigEndian
            withUnsafeBytes(of: &encoded) { bytes.append(contentsOf: $0) }
        }
    }
    struct AppendBindings: Equatable { let recording: Data; let sourceFile: Data }
    private struct Receipt: Codable {
        let version: Int
        let identity: Identity
        let state: String
        let createdAtMs: Int64
        let durationMs: Int64?
        let file: Fingerprint?
        let appendSource: AppendSource?
        init(version: Int, identity: Identity, state: String, createdAtMs: Int64,
             durationMs: Int64?, file: Fingerprint?, appendSource: AppendSource? = nil) {
            self.version = version; self.identity = identity; self.state = state
            self.createdAtMs = createdAtMs; self.durationMs = durationMs
            self.file = file; self.appendSource = appendSource
        }
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
    /// Only syscall receipts are injected; all ownership/identity checks remain
    /// real. This path uses anchored free blocks, not purgeable-volume estimates.
    struct AppendIO {
        let write: (Int32, UnsafeRawPointer, Int) -> Int
        let synchronize: (Int32) -> Int32
        let availableBytes: (Int32) throws -> UInt64?
        static let native = AppendIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync, availableBytes: { fd in
            var volume = statfs()
            guard fstatfs(fd, &volume) == 0, volume.f_bsize > 0 else { throw Failure.io }
            guard let blocks = UInt64(exactly: volume.f_bavail) else { throw Failure.io }
            let result = blocks.multipliedReportingOverflow(by: UInt64(volume.f_bsize))
            guard !result.overflow else { throw Failure.capacity }; return result.partialValue
        })
    }
    private let appendIO: AppendIO
    /// Checkpoint syscalls only. No portable parser or native capability enters
    /// this Foundation-only file; callback validation owns that separate gate.
    struct CheckpointIO {
        let write: (Int32, UnsafeRawPointer, Int) -> Int
        let synchronize: (Int32) -> Int32
        let rename: (Int32, String, String) -> Int32
        let synchronizeDirectory: (Int32) -> Int32
        let read: (Int32, UnsafeMutableRawPointer, Int) -> Int
        static let native = CheckpointIO(write: { Darwin.write($0, $1, $2) }, synchronize: Darwin.fsync,
            rename: { Darwin.renameat($0, $1, $0, $2) }, synchronizeDirectory: Darwin.fsync,
            read: { Darwin.read($0, $1, $2) })
    }
    private let checkpointIO: CheckpointIO
    // Strongly own the sole actual append child even if its caller drops it.
    // Clear only after checked close and all receipt work, together with token.
    private var appendOwner: AppendWriter?
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
         appendIO: AppendIO = .native,
         checkpointIO: CheckpointIO = .native,
         closeFileDescriptor: @escaping (Int32) -> Int32 = Darwin.close) throws {
        self.closeFileDescriptor = closeFileDescriptor
        self.mediaUpdateIO = mediaUpdateIO; self.appendIO = appendIO; self.checkpointIO = checkpointIO
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
              receipt.appendSource.map({ $0.valid }) ?? true,
              (try? encode(receipt)) == bytes else { return nil }
        return receipt
    }
    private func receiptName(_ identity: Identity) -> String { "Sapflow-\(identity.segmentID).receipt.json" }
    private func writeReceipt(_ receipt: Receipt, fixedTemporary: String? = nil) throws {
        try check(root)
        let data = try encode(receipt), name = receiptName(receipt.identity)
        _ = try info(root, name) // Refuse unsafe existing destinations.
        let temporary = fixedTemporary ?? ".recording-\(UUID().uuidString.lowercased()).tmp"
        let fd = openat(root.fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw Failure.io }
        var needsClose = true
        // Legacy receipt cleanup is unchanged. New append temps survive failed
        // installation/unknown close; a later begin must refuse occupied names.
        defer { if fixedTemporary == nil { unlinkat(root.fd, temporary, 0) } }
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
    /// Six flat entries are reserved under the single catalogue child token.
    /// This primitive owns files, never native media/presentation authority.
    func beginAppend(identity: Identity, createdAtMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000),
                     payloadLimit: UInt64) throws -> AppendWriter {
        guard createdAtMs > 0, payloadLimit > 0, payloadLimit <= 16 * 1024 * 1024,
              (try? Identity(segmentID: identity.segmentID, showID: identity.showID, segmentIndex: identity.segmentIndex,
                             profileID: identity.profileID, container: identity.container)) != nil,
              let profile = CaptureIntent(rawValue: identity.profileID) else { throw Failure.unsafe }
        let token = try reserve(.writer(identity.segmentID))
        var child: AppendWriter?
        do {
            guard try scan().count < Self.maxSegments else { throw Failure.capacity }
            var entries = 0
            _ = try names(root, count: &entries)
            if let legacy { _ = try names(legacy, count: &entries) }
            guard entries <= Self.maxEntries - 6 else { throw Failure.capacity }
            let budget = try RecordingStorage.checkpointBudget(profile: profile)
            guard let available = try appendIO.availableBytes(root.fd) else { throw Failure.io }
            guard available >= budget.requiredBytes,
                  available > 200 * 1024 * 1024 + RecordingStorage.checkpointMetadataBytes else { throw Failure.capacity }
            try check(root)
            let prefix = "Sapflow-\(identity.segmentID)"
            for name in [prefix + ".mov", prefix + ".m4a", receiptName(identity),
                         prefix + ".checkpoint.0", prefix + ".checkpoint.1",
                         prefix + ".checkpoint.tmp", prefix + ".receipt.tmp"] {
                guard try info(root, name) == nil else { throw Failure.changed }
            }
            let fd = openat(root.fd, identity.filename, O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
            guard fd >= 0 else { throw errno == EEXIST ? Failure.changed : Failure.io }
            let value = AppendWriter(catalogue: self, token: token, identity: identity,
                                     createdAtMs: createdAtMs, fd: fd, outputLimit: payloadLimit + 131_072)
            child = value
            lock.lock(); appendOwner = value; lock.unlock()
            try value.prepare()
            return value
        } catch {
            let original = error
            if let child { try child.abortBegin() }
            else if !cleanupUncertain { try release(token) }
            throw original
        }
    }
    private func appendFile(_ s: stat) throws -> (source: AppendSource, file: Fingerprint) {
        guard (s.st_mode & S_IFMT) == S_IFREG, s.st_nlink == 1, s.st_uid == getuid(),
              s.st_mode & 0o777 == 0o600, s.st_size >= 0 else { throw Failure.unsafe }
        let source = AppendSource(rootDevice: root.device, rootInode: root.inode, device: s.st_dev, inode: s.st_ino,
                                  birthSeconds: Int64(s.st_birthtimespec.tv_sec), birthNanoseconds: Int64(s.st_birthtimespec.tv_nsec))
        guard source.valid else { throw Failure.unsafe }
        return (source, Fingerprint(s))
    }
    private func appendDescriptor(_ fd: Int32) throws -> (source: AppendSource, file: Fingerprint) {
        var s = stat(); guard fstat(fd, &s) == 0 else { throw Failure.io }; return try appendFile(s)
    }
    private func appendPath(_ name: String) throws -> (source: AppendSource, file: Fingerprint) {
        try check(root)
        var s = stat()
        guard fstatat(root.fd, name, &s, AT_SYMLINK_NOFOLLOW) == 0 else { throw Failure.changed }
        return try appendFile(s)
    }
    private func releaseAppend(_ token: UUID, child: AppendWriter) throws {
        lock.lock(); defer { lock.unlock() }
        guard !uncertain, owner?.token == token, appendOwner === child else { throw Failure.closed }
        owner = nil; appendOwner = nil
    }
    final class AppendWriter {
        struct InitialAppend { let bytes: UInt64; let sha256: Data }
        struct FragmentAppend { let end: UInt64 }
        struct MediaSyncReceipt { let end: UInt64 }
        struct ClosedAppend { let bytes: UInt64 }
        /// Minted only after actual installed readback close. Scalar filesystem
        /// observations only, not a native checkpoint or whole-show certificate.
        final class InstalledMetadataReceipt {
            fileprivate let owner: UUID
            let sequence: UInt32
            let end: UInt64
            let slot: Int
            fileprivate init(owner: UUID, sequence: UInt32, end: UInt64, slot: Int) {
                self.owner = owner; self.sequence = sequence; self.end = end; self.slot = slot
            }
        }
        private struct SlotSnapshot { let source: AppendSource; let file: Fingerprint }
        private let checkpointOwner = UUID()
        private var checkpointSequence: UInt32 = 0
        private var checkpointSlots: [SlotSnapshot?] = [nil, nil]
        private let catalogue: RecordingCatalogue
        private let token: UUID
        let identity: Identity
        private let createdAtMs: Int64
        private let outputLimit: UInt64
        private var descriptor: Int32?
        private var source: AppendSource?
        private var initial: InitialAppend?
        private var appendedEnd: UInt64 = 0
        private let lock = NSLock()
        private enum Phase { case open, closing, retained, closed }
        private var phase = Phase.open
        private var busy = false
        private var admissionClosed = false
        private var closedResult: ClosedAppend?
        fileprivate init(catalogue: RecordingCatalogue, token: UUID, identity: Identity,
                         createdAtMs: Int64, fd: Int32, outputLimit: UInt64) {
            self.catalogue = catalogue; self.token = token; self.identity = identity
            self.createdAtMs = createdAtMs; descriptor = fd; self.outputLimit = outputLimit
        }
        var cleanupComplete: Bool { lock.lock(); defer { lock.unlock() }; return phase == .closed }
        func cancel() { lock.lock(); admissionClosed = true; lock.unlock() }
        /// Immutable equality digests only. No descriptor/path escapes.
        func bindings() throws -> AppendBindings {
            try operation {
                _ = try inspect(expected: appendedEnd)
                guard let source else { throw Failure.closed }; return try source.bindings(identity: identity)
            }
        }
        fileprivate func prepare() throws {
            guard let descriptor else { throw Failure.closed }
            source = try catalogue.appendDescriptor(descriptor).source
            _ = try inspect(expected: 0)
            try catalogue.writeReceipt(Receipt(version: 1, identity: identity, state: "writing", createdAtMs: createdAtMs,
                                               durationMs: nil, file: nil, appendSource: source), fixedTemporary: temporaryName)
            _ = try inspect(expected: 0)
        }
        private var temporaryName: String { "Sapflow-\(identity.segmentID).receipt.tmp" }
        /// Pure bound check is also used by generated-byte boundary tests.
        static func checkedFragmentEnd(position: UInt64, byteCount: UInt64, expectedEnd: UInt64, outputLimit: UInt64) throws -> UInt64 {
            guard byteCount > 0, byteCount <= outputLimit, byteCount <= 16_908_288 else { throw Failure.capacity }
            guard position <= UInt64(Int64.max), byteCount <= UInt64(Int64.max) - position else { throw Failure.unsafe }
            let end = position + byteCount
            guard end == expectedEnd else { throw Failure.changed }; return end
        }
        func writeInitial(_ bytes: Data) throws -> InitialAppend {
            try operation {
                guard initial == nil, !bytes.isEmpty, bytes.count <= 65_536 else { throw Failure.unsafe }
                _ = try inspect(expected: 0)
                try writeAll(bytes)
                _ = try inspect(expected: UInt64(bytes.count))
                let receipt = InitialAppend(bytes: UInt64(bytes.count), sha256: Data(SHA256.hash(data: bytes)))
                initial = receipt; appendedEnd = receipt.bytes; return receipt
            }
        }
        func appendFragment(_ bytes: Data, expectedEnd: UInt64) throws -> FragmentAppend {
            try operation {
                guard initial != nil else { throw Failure.ineligible }
                let position = try inspect(expected: appendedEnd)
                let end = try Self.checkedFragmentEnd(position: position, byteCount: UInt64(bytes.count), expectedEnd: expectedEnd, outputLimit: outputLimit)
                try writeAll(bytes)
                _ = try inspect(expected: end)
                appendedEnd = end; return FragmentAppend(end: end)
            }
        }
        func synchronizeMedia() throws -> MediaSyncReceipt {
            try operation {
                guard initial != nil, let descriptor else { throw Failure.ineligible }
                _ = try inspect(expected: appendedEnd)
                guard catalogue.appendIO.synchronize(descriptor) == 0 else { throw Failure.io }
                _ = try inspect(expected: appendedEnd)
                // fsync receipt only; this alone does not install a checkpoint.
                return MediaSyncReceipt(end: appendedEnd)
            }
        }
        /// One admission guard covers append through callback and actual close.
        /// Neither caller scalars nor callback alone can publish native progress.
        func appendCheckpointFragment(_ bytes: Data, expectedEnd: UInt64, sequence: UInt32,
                                      checkpointBytes: Data, initialLength: UInt32, initialHash: Data,
                                      expectedBindings: AppendBindings,
                                      validateInstalled: (Data) throws -> Void) throws -> InstalledMetadataReceipt {
            try operation {
                guard let initial, let descriptor, let source,
                      initial.bytes == UInt64(initialLength), initial.sha256 == initialHash,
                      try source.bindings(identity: identity) == expectedBindings,
                      (545...66_080).contains(checkpointBytes.count),
                      sequence > 0, sequence < UInt32.max,
                      UInt64(sequence) == UInt64(checkpointSequence) + 1 else { throw Failure.changed }
                let position = try inspect(expected: appendedEnd)
                let end = try Self.checkedFragmentEnd(position: position, byteCount: UInt64(bytes.count), expectedEnd: expectedEnd, outputLimit: outputLimit)
                let slot = Int(sequence & 1), stable = checkpointName(slot)
                let temporary = "Sapflow-\(identity.segmentID).checkpoint.tmp"
                try checkSlots()
                guard try catalogue.info(catalogue.root, temporary) == nil else { throw Failure.changed }
                var temporaryFD: Int32?
                var readbackFD: Int32?
                do {
                    try writeAll(bytes)
                    _ = try inspect(expected: end); appendedEnd = end
                    guard catalogue.appendIO.synchronize(descriptor) == 0 else { throw Failure.io }
                    _ = try inspect(expected: end)
                    let fd = openat(catalogue.root.fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
                    guard fd >= 0 else { throw Failure.io }; temporaryFD = fd
                    let created = try catalogue.appendDescriptor(fd)
                    guard created.file.size == 0 else { throw Failure.changed }
                    try checkpointBytes.withUnsafeBytes { buffer in
                        var offset = 0
                        while offset < buffer.count {
                            let remaining = buffer.count - offset
                            let count = catalogue.checkpointIO.write(fd, buffer.baseAddress!.advanced(by: offset), remaining)
                            if count < 0 && errno == EINTR { continue }
                            guard count > 0, count <= remaining else { throw Failure.io }; offset += count
                        }
                    }
                    guard catalogue.checkpointIO.synchronize(fd) == 0 else { throw Failure.io }
                    let written = try catalogue.appendDescriptor(fd), namedTemp = try catalogue.appendPath(temporary)
                    guard written.source == created.source, written.source == namedTemp.source,
                          written.file == namedTemp.file, written.file.size == Int64(checkpointBytes.count),
                          lseek(fd, 0, SEEK_CUR) == written.file.size else { throw Failure.changed }
                    // Retain actual temp incarnation BEFORE consuming its close.
                    let installedSource = written.source
                    temporaryFD = nil; try catalogue.closeFD(fd)
                    try checkSlots(); try catalogue.check(catalogue.root)
                    let stillTemp = try catalogue.appendPath(temporary)
                    guard stillTemp.source == installedSource, stillTemp.file == written.file else { throw Failure.changed }
                    guard catalogue.checkpointIO.rename(catalogue.root.fd, temporary, stable) == 0 else { throw Failure.io }
                    let renamed = try catalogue.appendPath(stable)
                    guard renamed.source == installedSource, renamed.file.size == Int64(checkpointBytes.count) else { throw Failure.changed }
                    guard catalogue.checkpointIO.synchronizeDirectory(catalogue.root.fd) == 0 else { throw Failure.io }
                    guard try catalogue.appendPath(stable).file == renamed.file else { throw Failure.changed }
                    let readFD = openat(catalogue.root.fd, stable, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
                    guard readFD >= 0 else { throw Failure.io }; readbackFD = readFD
                    guard try catalogue.appendDescriptor(readFD).source == installedSource,
                          try catalogue.descriptorInfo(readFD) == renamed.file else { throw Failure.changed }
                    var readback = Data(count: checkpointBytes.count)
                    try readback.withUnsafeMutableBytes { buffer in
                        var offset = 0
                        while offset < buffer.count {
                            let remaining = buffer.count - offset
                            let count = catalogue.checkpointIO.read(readFD, buffer.baseAddress!.advanced(by: offset), remaining)
                            if count < 0 && errno == EINTR { continue }
                            guard count > 0, count <= remaining else { throw Failure.io }; offset += count
                        }
                    }
                    var suffix: UInt8 = 0
                    var extra: Int
                    repeat { extra = Darwin.read(readFD, &suffix, 1) } while extra < 0 && errno == EINTR
                    guard extra == 0, readback == checkpointBytes,
                          try catalogue.descriptorInfo(readFD) == renamed.file,
                          try catalogue.appendPath(stable).file == renamed.file else { throw Failure.changed }
                    try validateInstalled(readback)
                    // Callback cannot release this busy owner or mint a receipt.
                    guard try catalogue.descriptorInfo(readFD) == renamed.file,
                          try catalogue.appendPath(stable).source == installedSource,
                          try catalogue.appendPath(stable).file == renamed.file else { throw Failure.changed }
                    readbackFD = nil; try catalogue.closeFD(readFD)
                    guard try catalogue.appendPath(stable).source == installedSource,
                          try catalogue.appendPath(stable).file == renamed.file else { throw Failure.changed }
                    _ = try inspect(expected: end)
                    try checkSlots(excluding: slot)
                    checkpointSlots[slot] = SlotSnapshot(source: installedSource, file: renamed.file)
                    checkpointSequence = sequence
                    return InstalledMetadataReceipt(owner: checkpointOwner, sequence: sequence, end: end, slot: slot)
                } catch {
                    var problem = error
                    if let fd = readbackFD { readbackFD = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                    if let fd = temporaryFD { temporaryFD = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                    if catalogue.cleanupUncertain { catalogue.poison(self) }
                    // Preserve prior active slot, actual original and fixed temp.
                    throw problem
                }
            }
        }
        /// Scalar acknowledgement, never IO/FFI under this lock.
        func confirms(_ receipt: InstalledMetadataReceipt) -> Bool {
            lock.lock(); defer { lock.unlock() }
            return phase == .open && !busy && !admissionClosed && receipt.owner == checkpointOwner &&
                receipt.sequence == checkpointSequence && receipt.end == appendedEnd
        }
        private func checkpointName(_ slot: Int) -> String { "Sapflow-\(identity.segmentID).checkpoint.\(slot)" }
        private func checkSlots(excluding omitted: Int? = nil) throws {
            for slot in 0..<2 {
                if slot == omitted { continue }
                if let previous = checkpointSlots[slot] {
                    let actual = try catalogue.appendPath(checkpointName(slot))
                    guard actual.source == previous.source, actual.file == previous.file else { throw Failure.changed }
                } else {
                    guard try catalogue.info(catalogue.root, checkpointName(slot)) == nil else { throw Failure.changed }
                }
            }
        }
        private func operation<T>(_ body: () throws -> T) throws -> T {
            lock.lock()
            guard phase == .open, !admissionClosed else { lock.unlock(); throw Failure.closed }
            guard !busy else { lock.unlock(); throw Failure.busy }
            busy = true; lock.unlock()
            defer { lock.lock(); busy = false; lock.unlock() }
            do { return try body() }
            catch { lock.lock(); admissionClosed = true; lock.unlock(); throw error }
        }
        private func inspect(expected: UInt64?) throws -> UInt64 {
            guard let descriptor, let source else { throw Failure.closed }
            try catalogue.check(catalogue.root)
            let before = try catalogue.appendDescriptor(descriptor)
            let position = lseek(descriptor, 0, SEEK_CUR)
            guard position >= 0 else { throw Failure.io }
            let after = try catalogue.appendDescriptor(descriptor), path = try catalogue.appendPath(identity.filename)
            guard before.source == source, after.source == source, path.source == source,
                  before.file == after.file, path.file == after.file, after.file.size == position,
                  expected.map({ $0 == UInt64(position) }) ?? true else { throw Failure.changed }
            try catalogue.check(catalogue.root); return UInt64(position)
        }
        private func writeAll(_ bytes: Data) throws {
            guard let descriptor else { throw Failure.closed }
            try bytes.withUnsafeBytes { buffer in
                var position = 0
                while position < buffer.count {
                    let remaining = buffer.count - position
                    let count = catalogue.appendIO.write(descriptor, buffer.baseAddress!.advanced(by: position), remaining)
                    if count < 0 && errno == EINTR { continue }
                    guard count > 0, count <= remaining else { throw Failure.io }; position += count
                }
            }
        }
        /// Closed-header installation is still only submitted-input storage.
        /// Every receipt in this path remains explicitly incomplete.
        struct StoredInputsClosure { let bytes: UInt64; let durationMs: Int64 }
        private var storedInputsResult: StoredInputsClosure?
        func finishStoredInputs(header: Data, durationMs: Int64, checkpoint: InstalledMetadataReceipt,
                                willCommit: () throws -> Void) throws -> StoredInputsClosure {
            lock.lock()
            if let storedInputsResult { lock.unlock(); return storedInputsResult }
            guard phase == .open else { let active = phase == .closing; lock.unlock(); throw active ? Failure.busy : Failure.closed }
            guard !busy else { lock.unlock(); throw Failure.busy }
            phase = .closing; admissionClosed = true; busy = true; lock.unlock()
            var headerFD: Int32?
            defer { lock.lock(); busy = false; lock.unlock() }
            do {
                guard let initial, let source, durationMs >= 0,
                      header.count == Int(initial.bytes), !header.isEmpty, header.count <= 65_536,
                      checkpoint.owner == checkpointOwner, checkpoint.sequence == checkpointSequence,
                      checkpoint.sequence > 0, checkpoint.end == appendedEnd else { throw Failure.ineligible }
                let end = appendedEnd
                _ = try inspect(expected: end); try checkSlots()
                guard try catalogue.info(catalogue.root, temporaryName) == nil,
                      try catalogue.info(catalogue.root, "Sapflow-\(identity.segmentID).checkpoint.tmp") == nil else { throw Failure.changed }
                let receiptBefore = try catalogue.appendPath(catalogue.receiptName(identity))
                guard let descriptor, catalogue.appendIO.synchronize(descriptor) == 0 else { throw Failure.io }
                _ = try inspect(expected: end)
                // A failed actual close must NEVER permit anchored reopen/pwrite.
                self.descriptor = nil; try catalogue.closeFD(descriptor)
                try checkClosedSource(end: end); try checkSlots()
                let fd = openat(catalogue.root.fd, identity.filename, O_RDWR | O_NOFOLLOW | O_CLOEXEC)
                guard fd >= 0 else { throw Failure.io }; headerFD = fd
                let before = try catalogue.appendDescriptor(fd), path = try catalogue.appendPath(identity.filename)
                guard before.source == source, path.source == source, before.file == path.file,
                      UInt64(before.file.size) == end else { throw Failure.changed }
                // Only this first in-process rewrite checks current INITIAL.
                // Future explicit recovery uses the saved slot header instead.
                guard Data(SHA256.hash(data: try readHeader(fd, count: header.count))) == initial.sha256 else { throw Failure.changed }
                let ready = try catalogue.appendDescriptor(fd), readyPath = try catalogue.appendPath(identity.filename)
                guard ready.source == source, readyPath.source == source,
                      ready.file == before.file, readyPath.file == before.file else { throw Failure.changed }
                try checkSlots()
                try header.withUnsafeBytes { buffer in
                    var offset = 0
                    while offset < buffer.count {
                        let remaining = buffer.count - offset
                        let written = catalogue.mediaUpdateIO.write(fd, buffer.baseAddress!.advanced(by: offset), remaining, off_t(offset))
                        if written < 0 && errno == EINTR { continue }
                        guard written > 0, written <= remaining else { throw Failure.io }; offset += written
                    }
                }
                guard catalogue.mediaUpdateIO.synchronize(fd) == 0,
                      try readHeader(fd, count: header.count) == header else { throw Failure.io }
                let installed = try catalogue.appendDescriptor(fd), installedPath = try catalogue.appendPath(identity.filename)
                guard installed.source == source, installedPath.source == source, installed.file == installedPath.file,
                      UInt64(installed.file.size) == end else { throw Failure.changed }
                headerFD = nil; try catalogue.closeFD(fd)
                try checkClosedSource(end: end)
                guard try catalogue.appendPath(identity.filename).file == installed.file else { throw Failure.changed }
                try checkSlots()
                // Scalar decision only. No publication, SDK or IO in callback.
                try willCommit()
                try installStoredReceipt(Receipt(version: 1, identity: identity, state: "incomplete", createdAtMs: createdAtMs,
                    durationMs: durationMs, file: installed.file, appendSource: source), previous: receiptBefore)
                try checkClosedSource(end: end); try checkSlots()
                guard try catalogue.appendPath(identity.filename).file == installed.file else { throw Failure.changed }
                try catalogue.releaseAppend(token, child: self) // LAST fallible action.
                let result = StoredInputsClosure(bytes: end, durationMs: durationMs)
                lock.lock(); storedInputsResult = result; closedResult = ClosedAppend(bytes: end); phase = .closed; lock.unlock()
                return result
            } catch {
                var problem = error
                if let fd = headerFD { headerFD = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                if let fd = descriptor { descriptor = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                lock.lock(); phase = .retained; lock.unlock(); catalogue.poison(self); throw problem
            }
        }
        private func readHeader(_ fd: Int32, count: Int) throws -> Data {
            guard count > 0, count <= 65_536, lseek(fd, 0, SEEK_SET) == 0 else { throw Failure.io }
            var bytes = Data(count: count)
            try bytes.withUnsafeMutableBytes { buffer in
                var offset = 0
                while offset < count {
                    let remaining = count - offset
                    let read = catalogue.checkpointIO.read(fd, buffer.baseAddress!.advanced(by: offset), remaining)
                    if read < 0 && errno == EINTR { continue }
                    guard read > 0, read <= remaining else { throw Failure.io }; offset += read
                }
            }
            return bytes
        }
        private func installStoredReceipt(_ receipt: Receipt, previous: (source: AppendSource, file: Fingerprint)) throws {
            let bytes = try catalogue.encode(receipt), name = catalogue.receiptName(identity)
            var temporaryFD: Int32?, readbackFD: Int32?
            do {
                try catalogue.check(catalogue.root)
                let current = try catalogue.appendPath(name)
                guard current.source == previous.source, current.file == previous.file,
                      try catalogue.info(catalogue.root, temporaryName) == nil else { throw Failure.changed }
                let fd = openat(catalogue.root.fd, temporaryName, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
                guard fd >= 0 else { throw Failure.io }; temporaryFD = fd
                try bytes.withUnsafeBytes { buffer in
                    var offset = 0
                    while offset < buffer.count {
                        let remaining = buffer.count - offset
                        let written = catalogue.checkpointIO.write(fd, buffer.baseAddress!.advanced(by: offset), remaining)
                        if written < 0 && errno == EINTR { continue }
                        guard written > 0, written <= remaining else { throw Failure.io }; offset += written
                    }
                }
                guard catalogue.checkpointIO.synchronize(fd) == 0 else { throw Failure.io }
                let written = try catalogue.appendDescriptor(fd)
                guard written.file.size == Int64(bytes.count) else { throw Failure.changed }
                temporaryFD = nil; try catalogue.closeFD(fd)
                let closedTemporary = try catalogue.appendPath(temporaryName), destination = try catalogue.appendPath(name)
                guard closedTemporary.source == written.source, closedTemporary.file == written.file,
                      destination.source == previous.source, destination.file == previous.file else { throw Failure.changed }
                guard catalogue.checkpointIO.rename(catalogue.root.fd, temporaryName, name) == 0 else { throw Failure.io }
                let renamed = try catalogue.appendPath(name)
                guard renamed.source == written.source, renamed.file.size == Int64(bytes.count) else { throw Failure.changed }
                guard catalogue.checkpointIO.synchronizeDirectory(catalogue.root.fd) == 0 else { throw Failure.io }
                guard try catalogue.appendPath(name).file == renamed.file else { throw Failure.changed }
                let readFD = openat(catalogue.root.fd, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
                guard readFD >= 0 else { throw Failure.io }; readbackFD = readFD
                guard try catalogue.appendDescriptor(readFD).source == written.source,
                      try catalogue.descriptorInfo(readFD) == renamed.file else { throw Failure.changed }
                var actual = Data(count: bytes.count)
                try actual.withUnsafeMutableBytes { buffer in
                    var offset = 0
                    while offset < buffer.count {
                        let remaining = buffer.count - offset
                        let read = catalogue.checkpointIO.read(readFD, buffer.baseAddress!.advanced(by: offset), remaining)
                        if read < 0 && errno == EINTR { continue }
                        guard read > 0, read <= remaining else { throw Failure.io }; offset += read
                    }
                }
                var suffix: UInt8 = 0
                var extra: Int
                repeat { extra = Darwin.read(readFD, &suffix, 1) } while extra < 0 && errno == EINTR
                guard extra == 0, actual == bytes, catalogue.decode(actual, identity.segmentID) != nil,
                      try catalogue.descriptorInfo(readFD) == renamed.file else { throw Failure.changed }
                readbackFD = nil; try catalogue.closeFD(readFD)
                let installed = try catalogue.appendPath(name)
                guard installed.source == written.source, installed.file == renamed.file else { throw Failure.changed }
            } catch {
                var problem = error
                if let fd = readbackFD { readbackFD = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                if let fd = temporaryFD { temporaryFD = nil; do { try catalogue.closeFD(fd) } catch { problem = error } }
                throw problem // Fixed temp/originals and exact owner are preserved.
            }
        }
        /// A cancelled/failed append can only close as explicitly incomplete.
        /// All fallible work, including real close and receipt readback, precedes
        /// token release. This close does not rewrite a header or mark finalised.
        func closeIncomplete() throws -> ClosedAppend {
            lock.lock()
            if let closedResult { lock.unlock(); return closedResult }
            guard phase == .open else { let active = phase == .closing; lock.unlock(); throw active ? Failure.busy : Failure.closed }
            guard !busy else { lock.unlock(); throw Failure.busy }
            phase = .closing; admissionClosed = true; lock.unlock()
            do {
                var problem: Error?
                var end: UInt64 = 0
                do {
                    end = try inspect(expected: nil)
                    guard let descriptor, catalogue.appendIO.synchronize(descriptor) == 0 else { throw Failure.io }
                    _ = try inspect(expected: end)
                } catch { problem = error }
                // Consume this close attempt before invoking it: ambiguity must
                // never let any later method retry a recycled descriptor number.
                if let descriptor {
                    self.descriptor = nil
                    do { try catalogue.closeFD(descriptor) } catch { problem = error }
                } else if problem == nil { problem = Failure.closed }
                if let problem { throw problem }
                try checkClosedSource(end: end)
                try catalogue.writeReceipt(Receipt(version: 1, identity: identity, state: "incomplete", createdAtMs: createdAtMs,
                                                   durationMs: nil, file: nil, appendSource: source), fixedTemporary: temporaryName)
                try checkClosedSource(end: end)
                try catalogue.releaseAppend(token, child: self)
                let result = ClosedAppend(bytes: end)
                lock.lock(); closedResult = result; phase = .closed; lock.unlock(); return result
            } catch {
                lock.lock(); phase = .retained; lock.unlock()
                catalogue.poison(self); throw error
            }
        }
        private func checkClosedSource(end: UInt64) throws {
            let path = try catalogue.appendPath(identity.filename)
            guard path.source == source, path.file.size >= 0, UInt64(path.file.size) == end else { throw Failure.changed }
        }
        fileprivate func abortBegin() throws {
            // Not published to callers. Preserve any original/temp and close the
            // one real media FD even if metadata installation poisoned the root.
            if let descriptor {
                self.descriptor = nil
                do { try catalogue.closeFD(descriptor) }
                catch { lock.lock(); phase = .retained; lock.unlock(); catalogue.poison(self); throw error }
            }
            guard !catalogue.cleanupUncertain else {
                lock.lock(); phase = .retained; lock.unlock(); catalogue.poison(self); throw Failure.io
            }
            do {
                try catalogue.releaseAppend(token, child: self)
                lock.lock(); phase = .closed; lock.unlock()
            } catch { catalogue.poison(self); throw error }
        }
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
        var receipt = decoded?.identity.filename != name || (decoded?.file != nil && decoded?.file != file) ? nil : decoded
        if let source = receipt?.appendSource, file != nil {
            guard location == .privateRecording else { throw Failure.unsafe }
            if try appendPath(name).source != source { receipt = nil }
        }
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
