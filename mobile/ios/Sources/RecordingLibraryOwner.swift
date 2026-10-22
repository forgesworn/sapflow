import Foundation
import Combine

/// Cancellation is scalar. Coordinator cancellation cannot release an accessor
/// that is already executing; the worker still owns its actual close receipt.
final class RecordingExportCancellation {
    private let lock = NSLock()
    private var cancelled = false
    private var coordinator: NSFileCoordinator?
    var requested: Bool { lock.lock(); defer { lock.unlock() }; return cancelled }
    func bind(_ value: NSFileCoordinator) {
        lock.lock(); coordinator = value; let cancel = cancelled; lock.unlock()
        if cancel { value.cancel() }
    }
    func unbind() { lock.lock(); coordinator = nil; lock.unlock() }
    func cancel() {
        lock.lock(); cancelled = true; let value = coordinator; lock.unlock()
        value?.cancel()
    }
}

/// Platform coordination is injected into generated-file host tests. The live
/// accessor is synchronous and permission access lasts through its real return.
struct RecordingExportAccess {
    let begin: (URL) throws -> (() -> Void)
    let coordinate: (URL, RecordingExportCancellation, @escaping (URL) -> Void) throws -> Void
    static var live: RecordingExportAccess {
        RecordingExportAccess(begin: { folder in
            guard folder.isFileURL else { throw RecordingCatalogue.Failure.unsafe }
            let scoped = folder.startAccessingSecurityScopedResource()
            if !scoped {
                // Already-accessible app-container folders need no scope.
                // This check is cold; a selected external path needs a grant.
                let home = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true).resolvingSymlinksInPath().path
                let path = folder.resolvingSymlinksInPath().path
                guard path == home || path.hasPrefix(home + "/") else { throw RecordingCatalogue.Failure.unsafe }
            }
            return { if scoped { folder.stopAccessingSecurityScopedResource() } }
        }, coordinate: { folder, cancellation, accessor in
            let coordinator = NSFileCoordinator(filePresenter: nil)
            cancellation.bind(coordinator); defer { cancellation.unbind() }
            var error: NSError?
            coordinator.coordinate(writingItemAt: folder, options: .forMerging, error: &error, byAccessor: accessor)
            if let error { throw error }
        })
    }
}

/// One app-lifetime cold IO slot. Published state and modal replies belong to
/// main; catalogue/files belong to the serial worker. Capture owns no file job.
final class RecordingLibraryOwner: ObservableObject {
    typealias Job = () -> Void
    typealias Schedule = (@escaping Job) -> Void
    typealias Installer = (RecordingCatalogue, RecordingLibraryAdmission, @escaping (Bool) -> Void) -> Void
    final class Selection: Identifiable {
        let id = UUID()
        let entry: RecordingCatalogue.Entry
        let allowUnverified: Bool
        fileprivate let ticket: RecordingLibraryAdmission.Ticket
        fileprivate init(entry: RecordingCatalogue.Entry, allowUnverified: Bool, ticket: RecordingLibraryAdmission.Ticket) {
            self.entry = entry; self.allowUnverified = allowUnverified; self.ticket = ticket
        }
    }
    @Published private(set) var entries: [RecordingCatalogue.Entry] = []
    @Published private(set) var busy = false
    @Published private(set) var message: String?
    @Published private(set) var ready = false
    @Published private(set) var picker: Selection?
    @Published private(set) var presentation: Selection?
    let admission = RecordingLibraryAdmission()
    private let schedule: Schedule
    private let deliver: Schedule
    private let configuredRoot: URL?
    private let configuredLegacyRoot: URL?
    private let access: RecordingExportAccess
    private let makeDestination: (URL, String) throws -> RecordingExportDestination
    private var catalogue: RecordingCatalogue? // Worker only.
    private var pendingPicker: Selection? // Main only; remains until actual dismissal.
    private var folderReply: URL?
    private var operation: RecordingLibraryAdmission.Ticket?
    private var cancellation: RecordingExportCancellation?
    private var closed = false // Main only.
    private var installer: Installer?
    private var retainedPresentationOwner: RecordingLibraryOwner? // Main only.
    private struct RetainedExport {
        let source: RecordingCatalogue.Export?
        let destination: RecordingExportDestination?
        let endAccess: () -> Void
    }
    // At most one failed export. Retrying a no-op close cannot establish its
    // real outcome. Keep wrappers and permission ownership through process exit.
    private var retainedExport: RetainedExport? // Worker only.
    private var retainedSelf: RecordingLibraryOwner? // Worker only.

    init(root: URL? = nil, legacyRoot: URL? = nil,
         schedule: Schedule? = nil, deliver: @escaping Schedule = { DispatchQueue.main.async(execute: $0) },
         access: RecordingExportAccess = .live,
         makeDestination: @escaping (URL, String) throws -> RecordingExportDestination = { try RecordingExportDestination(folder: $0, filename: $1) }) {
        let queue = DispatchQueue(label: "sapflow.recording-library", qos: .utility)
        self.schedule = schedule ?? { queue.async(execute: $0) }
        self.deliver = deliver; configuredRoot = root; configuredLegacyRoot = legacyRoot
        self.access = access; self.makeDestination = makeDestination
    }
    var captureAllowed: Bool { ready && admission.withCaptureReservation { true } == true }
    var cleanupRetained: Bool { admission.cleanupRetained }

    func initialise(install: @escaping Installer) {
        guard !closed, !ready, let ticket = admission.reserve(.setup) else { return }
        installer = install
        busy = true; operation = ticket; message = "Opening private recordings"
        schedule {
            guard self.admission.begin(ticket) else { self.finish(ticket, result: .failure(RecordingCatalogue.Failure.cancelled), clean: true); return }
            do {
                let manager = FileManager.default
                let root = try self.configuredRoot ?? manager.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true).appendingPathComponent("Recordings", isDirectory: true)
                let legacy = self.configuredRoot == nil ? manager.urls(for: .documentDirectory, in: .userDomainMask).first : self.configuredLegacyRoot
                let store = try RecordingCatalogue(root: root, legacyRoot: legacy)
                self.catalogue = store
                self.deliver {
                    guard !self.closed, !self.admission.shouldCancel(ticket) else {
                        self.finishSetup(ticket, store: store, accepted: false); return
                    }
                    install(store, self.admission) { accepted in self.finishSetup(ticket, store: store, accepted: accepted) }
                }
            } catch { self.finish(ticket, result: .failure(error), clean: self.catalogue?.cleanupUncertain != true) }
        }
    }
    func retryInitialisation() { if let installer, !busy { initialise(install: installer) } }
    private func finishSetup(_ ticket: RecordingLibraryAdmission.Ticket, store: RecordingCatalogue, accepted: Bool) {
        schedule {
            var clean = !store.cleanupUncertain
            if !accepted {
                do { try store.close(); self.catalogue = nil } catch { clean = false }
            }
            let allowed = self.admission.finish(ticket, cleanupComplete: clean)
            self.deliver {
                self.busy = !self.admission.idle
                if self.operation == ticket { self.operation = nil }
                guard allowed, self.admission.canDeliver(ticket), !self.closed else { return }
                self.ready = accepted
                self.message = accepted ? nil : "The recording library could not attach to capture."
            }
        }
    }
    func reload() {
        guard ready, !closed else { return }
        guard let ticket = admission.reserve(.scan) else { message = "A recording file operation is still finishing."; return }
        operation = ticket; busy = true; message = nil
        schedule {
            guard self.admission.begin(ticket) else { self.finish(ticket, result: .failure(RecordingCatalogue.Failure.cancelled), clean: true); return }
            let result = Result { try self.requireCatalogue().scan() }
            let clean = self.catalogue?.cleanupUncertain != true
            let allowed = self.admission.finish(ticket, cleanupComplete: clean)
            self.deliver {
                self.busy = !self.admission.idle
                if self.operation == ticket { self.operation = nil }
                guard allowed, self.admission.canDeliver(ticket), !self.closed else { return }
                switch result {
                case let .success(entries): self.entries = entries
                case let .failure(error): self.message = Self.describe(error)
                }
            }
        }
    }
    func choose(entry: RecordingCatalogue.Entry, allowUnverified: Bool, canExport: () -> Bool) {
        guard ready, !closed else { return }
        guard entry.state != .writing, entry.bytes > 0, allowUnverified || entry.state == .finalised else {
            message = "Confirm the unverified file before exporting it."; return
        }
        guard let ticket = admission.beginExport(canExport: canExport) else {
            message = "Stop and save capture, then wait for file cleanup before exporting."; return
        }
        let selection = Selection(entry: entry, allowUnverified: allowUnverified, ticket: ticket)
        pendingPicker = selection; folderReply = nil; operation = ticket
        presentation = selection; picker = selection; busy = true; message = "Choose a folder for a new copy. The private original stays here."
    }
    func receivedFolder(_ selection: Selection, folder: URL?) {
        guard pendingPicker === selection, picker === selection else { return }
        folderReply = folder
        picker = nil // Actual dismissal, below, still owns the capture fence.
    }
    func requestPickerDismissal(_ selection: Selection) { if pendingPicker === selection { picker = nil } }
    func pickerDismissed(_ selection: Selection) {
        guard pendingPicker === selection, presentation === selection else { return }
        let folder = folderReply
        pendingPicker = nil; folderReply = nil; picker = nil; presentation = nil
        guard admission.dismissPicker(selection.ticket, selected: folder != nil && !closed), let folder else {
            busy = !admission.idle; operation = nil
            message = "Export cancelled. The private original is unchanged."
            if closed { closeCatalogue(); retainedPresentationOwner = nil }
            return
        }
        let cancellation = RecordingExportCancellation()
        self.cancellation = cancellation
        message = "Exporting a copy. The private original stays on this device."
        schedule { self.export(selection, folder: folder, cancellation: cancellation) }
    }
    func cancelOperation() {
        guard let ticket = operation, ticket.kind != .setup else { return }
        admission.cancel(ticket)
        cancellation?.cancel()
        if pendingPicker != nil { picker = nil }
        message = "Cancellation requested. File cleanup must finish; a partial copy may remain."
    }
    private func requireCatalogue() throws -> RecordingCatalogue {
        guard let catalogue else { throw RecordingCatalogue.Failure.closed }; return catalogue
    }
    private func export(_ selection: Selection, folder: URL, cancellation: RecordingExportCancellation) {
        let ticket = selection.ticket
        guard admission.begin(ticket) else { finish(ticket, result: .failure(RecordingCatalogue.Failure.cancelled), clean: true); return }
        var result: Result<UInt64, Error> = .failure(RecordingCatalogue.Failure.io)
        var clean = true
        var endAccess: (() -> Void)?
        var heldSource: RecordingCatalogue.Export?
        var heldDestination: RecordingExportDestination?
        do {
            endAccess = try access.begin(folder)
            guard !admission.shouldCancel(ticket), !cancellation.requested else { throw RecordingCatalogue.Failure.cancelled }
            var entered = false
            try access.coordinate(folder, cancellation) { coordinated in
                guard !entered, !self.admission.shouldCancel(ticket), !cancellation.requested else {
                    result = .failure(RecordingCatalogue.Failure.cancelled); return
                }
                entered = true
                var source: RecordingCatalogue.Export?
                var destination: RecordingExportDestination?
                do {
                    source = try self.requireCatalogue().openExport(entry: selection.entry, allowUnverified: selection.allowUnverified)
                    heldSource = source
                    guard !self.admission.shouldCancel(ticket), !cancellation.requested else { throw RecordingCatalogue.Failure.cancelled }
                    let filename = "Recording-\(UUID().uuidString).\(selection.entry.fileExtension)"
                    destination = try self.makeDestination(coordinated, filename)
                    heldDestination = destination
                    let bytes = try source!.copy(write: { try destination!.write($0) }, cancelled: { self.admission.shouldCancel(ticket) || cancellation.requested })
                    guard !self.admission.shouldCancel(ticket), !cancellation.requested else { throw RecordingCatalogue.Failure.cancelled }
                    try destination!.finish()
                    result = .success(bytes)
                } catch {
                    if let error = error as? RecordingExportDestination.Failure, error.cleanupUncertain { clean = false }
                    result = .failure(error)
                }
                do { try destination?.close() } catch { clean = false; result = .failure(error) }
                do { try source?.close() } catch { clean = false; result = .failure(error) }
                if destination?.cleanupComplete == false || source?.cleanupComplete == false { clean = false }
            }
            if !entered { result = .failure(RecordingCatalogue.Failure.cancelled) }
        } catch { result = .failure(error) }
        if catalogue?.cleanupUncertain == true { clean = false }
        if let endAccess {
            if clean { endAccess() }
            else {
                retainedExport = RetainedExport(source: heldSource, destination: heldDestination, endAccess: endAccess)
                retainedSelf = self
            }
        }
        finish(ticket, result: result, clean: clean)
    }
    private func finish(_ ticket: RecordingLibraryAdmission.Ticket, result: Result<UInt64, Error>, clean: Bool) {
        let allowed = admission.finish(ticket, cleanupComplete: clean)
        deliver {
            self.busy = !self.admission.idle
            let currentOperation = self.operation == ticket
            if currentOperation { self.operation = nil; self.cancellation = nil }
            if !clean, self.admission.cleanupRetained, self.admission.owns(ticket) {
                self.message = "File cleanup is unconfirmed. Originals are preserved; new capture and file work are blocked."
                return
            }
            guard allowed, self.admission.canDeliver(ticket), !self.closed else {
                if currentOperation, !self.closed, clean {
                    self.message = "File operation cancelled. The private original is unchanged; a partial copy may remain."
                }
                return
            }
            switch result {
            case let .success(bytes): self.message = "Exported \(bytes) bytes. The private original is unchanged."
            case let .failure(error): self.message = Self.describe(error) + (ticket.kind == .export ? " A partial destination copy may remain." : "")
            }
        }
    }
    func close() {
        guard !closed else { return }; closed = true; ready = false
        admission.close(); cancellation?.cancel(); picker = nil
        if pendingPicker != nil {
            // The exact presentation must actually disappear before a fresh
            // owner can acquire this directory and admit capture behind it.
            retainedPresentationOwner = self
            return
        }
        closeCatalogue()
    }
    private func closeCatalogue() {
        schedule {
            // A new app owner must not bypass this unknown export by opening
            // the same directory with a fresh gate. Keep its lifetime lock.
            guard self.retainedExport == nil, !self.admission.cleanupRetained else { return }
            if let store = self.catalogue { try? store.close() }
        }
    }
    private static func describe(_ error: Error) -> String {
        switch error as? RecordingCatalogue.Failure {
        case .capacity: return "The recording catalogue is full. Existing files were preserved."
        case .busy: return "A recording or export still owns these files. Wait for it to finish."
        case .unsafe, .changed: return "The file changed or could not be checked safely. Refresh and choose again."
        case .ineligible: return "This file is not eligible for export."
        case .cancelled: return "Export cancelled. The private original is unchanged."
        default: return "The file operation could not finish. The private original is unchanged."
        }
    }
}
