import Foundation
import Darwin

/// One exclusively created copy in the user-selected, coordinated directory.
/// All methods run on the library's cold worker. No private input URL is used
/// here and no existing destination can be overwritten or removed.
final class RecordingExportDestination {
    struct Failure: Error {
        enum Reason { case invalidDestination, create, changed, write, sync, close }
        let reason: Reason
        let cleanupUncertain: Bool
    }
    private let output: FileHandle
    private let directory: FileHandle
    private let filename: String
    private let device: dev_t
    private let inode: ino_t
    private let closeHandle: (FileHandle) throws -> Void
    private let writeData: (FileHandle, Data) throws -> Void
    private var closed = false
    private var closeFailure: Failure?
    private var completedBytes: UInt64 = 0
    var cleanupComplete: Bool { closed }

    init(folder: URL, filename: String, closeHandle: @escaping (FileHandle) throws -> Void = { try $0.close() },
         writeData: @escaping (FileHandle, Data) throws -> Void = { try $0.write(contentsOf: $1) }) throws {
        guard folder.isFileURL, !filename.isEmpty, filename.utf8.count <= 160,
              filename != ".", filename != "..", filename.hasSuffix(".mov"),
              filename.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 46, 95].contains($0) }) else {
            throw Failure(reason: .invalidDestination, cleanupUncertain: false)
        }
        let parent = Darwin.open(folder.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard parent >= 0 else { throw Failure(reason: .create, cleanupUncertain: false) }
        var destination: Int32 = -1
        var attributes = stat()
        do {
            var parentAttributes = stat()
            guard Darwin.fstat(parent, &parentAttributes) == 0, parentAttributes.st_mode & S_IFMT == S_IFDIR else {
                throw Failure(reason: .invalidDestination, cleanupUncertain: false)
            }
            destination = filename.withCString { Darwin.openat(parent, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600)) }
            guard destination >= 0, Darwin.fstat(destination, &attributes) == 0,
                  attributes.st_mode & S_IFMT == S_IFREG, attributes.st_nlink == 1, attributes.st_size == 0 else {
                throw Failure(reason: .create, cleanupUncertain: false)
            }
        } catch {
            var clean = true
            if destination >= 0 && Darwin.close(destination) != 0 { clean = false }
            if Darwin.close(parent) != 0 { clean = false }
            throw Failure(reason: .create, cleanupUncertain: !clean)
        }
        output = FileHandle(fileDescriptor: destination, closeOnDealloc: false)
        directory = FileHandle(fileDescriptor: parent, closeOnDealloc: false)
        self.filename = filename; device = attributes.st_dev; inode = attributes.st_ino
        self.closeHandle = closeHandle
        self.writeData = writeData
    }
    func write(_ data: Data) throws {
        guard !closed, closeFailure == nil, data.count <= 64 * 1024 else {
            throw Failure(reason: .write, cleanupUncertain: closeFailure != nil)
        }
        let (next, overflow) = completedBytes.addingReportingOverflow(UInt64(data.count))
        guard !overflow else { throw Failure(reason: .write, cleanupUncertain: false) }
        do { try writeData(output, data); completedBytes = next }
        catch { throw Failure(reason: .write, cleanupUncertain: false) }
    }
    /// A successful copy alone is insufficient: check the exact created inode
    /// and synchronise output/directory before the owner reports success.
    func finish() throws {
        guard !closed, closeFailure == nil else { throw Failure(reason: .sync, cleanupUncertain: closeFailure != nil) }
        var current = stat(), named = stat()
        let matches = filename.withCString { Darwin.fstatat(directory.fileDescriptor, $0, &named, AT_SYMLINK_NOFOLLOW) }
        guard Darwin.fstat(output.fileDescriptor, &current) == 0, matches == 0,
              current.st_dev == device, current.st_ino == inode,
              named.st_dev == device, named.st_ino == inode,
              current.st_nlink == 1, named.st_mode & S_IFMT == S_IFREG,
              current.st_size >= 0, UInt64(current.st_size) == completedBytes,
              named.st_size == current.st_size else {
            throw Failure(reason: .changed, cleanupUncertain: false)
        }
        do { try output.synchronize(); try directory.synchronize() }
        catch { throw Failure(reason: .sync, cleanupUncertain: false) }
    }
    func close() throws {
        if closed { return }
        if let closeFailure { throw closeFailure }
        var failed = false
        do { try closeHandle(output) } catch { failed = true }
        do { try closeHandle(directory) } catch { failed = true }
        if failed {
            let error = Failure(reason: .close, cleanupUncertain: true)
            closeFailure = error
            throw error // Repeated no-op closes cannot manufacture a receipt.
        }
        closed = true
    }
}
