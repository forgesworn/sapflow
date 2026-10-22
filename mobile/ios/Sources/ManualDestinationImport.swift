import Foundation
import Combine
import Darwin

/// A manual file conveys settings, never creator authority or plaintext consent.
struct ImportedManualDestination: CustomStringConvertible, CustomDebugStringConvertible {
    static let maximumBytes = 16_384
    let name: String, transport: ManualTransport, endpoint: String, streamKey: String, streamID: String, passphrase: String
    let allowPlaintext: Bool, requiresPlaintext: Bool
    let host: String, port: UInt16
    var description: String { "Manual destination import (redacted)" }
    var debugDescription: String { description }
    init(_ data: Data) throws {
        do {
            let fields = try V4vJSON.parse(data, limit: Self.maximumBytes).object(required: ["version", "kind", "name", "transport", "endpoint", "streamKey", "streamID", "passphrase", "allowPlaintext"])
            guard try fields.uint("version", min: 1, max: 1) == 1,
                  try fields.text("kind", max: 48) == "sapflow-manual-destination",
                  let transport = ManualTransport(rawValue: try fields.text("transport", max: 4)) else { throw ManualImportError.invalidFile }
            let name = try fields.text("name", max: Self.maximumBytes).trimmingCharacters(in: CharacterSet(charactersIn: " "))
            guard (1...48).contains(name.utf8.count), name.utf8.allSatisfy({ (32...126).contains($0) }) else { throw ManualImportError.invalidFile }
            let endpoint = try fields.text("endpoint", max: 4096), key = try fields.text("streamKey", max: 4096)
            let streamID = try fields.text("streamID", max: 512), passphrase = try fields.text("passphrase", max: 79)
            guard endpoint == endpoint.trimmingCharacters(in: .whitespacesAndNewlines),
                  !endpoint.contains("%"),
                  !(URLComponents(string: endpoint)?.host.map { $0.contains(":") && $0.contains(".") } ?? false),
                  [endpoint, key, streamID].allSatisfy({ !$0.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) }) else { throw ManualImportError.invalidFile }
            let plain = try fields.flag("allowPlaintext")
            switch transport {
            case .rtmp:
                guard streamID.isEmpty, passphrase.isEmpty else { throw ManualImportError.invalidFile }
                let destination = try ManualDestination(endpoint: endpoint, streamKey: key, allowPlaintext: plain)
                self.endpoint = destination.endpoint; host = destination.host; port = destination.port; requiresPlaintext = !destination.encrypted
            case .srt:
                guard key.isEmpty else { throw ManualImportError.invalidFile }
                let destination = try ManualSrtDestination(endpoint: endpoint, streamID: streamID, passphrase: passphrase, allowPlaintext: plain)
                self.endpoint = destination.endpoint; host = destination.host; port = destination.port; requiresPlaintext = !destination.encrypted
            }
            self.name = name; self.transport = transport; streamKey = key; self.streamID = streamID
            self.passphrase = passphrase; allowPlaintext = plain
        } catch { throw ManualImportError.invalidFile }
    }
}

enum ManualImportError: Error, LocalizedError {
    case invalidFile, invalidLink, unavailable, stale
    var errorDescription: String? {
        switch self {
        case .invalidLink: return "This is not a supported manual destination link. Nothing was changed."
        case .invalidFile: return "This is not a supported manual destination JSON file. Nothing was changed."
        case .unavailable: return "The destination file could not be read and closed safely. Nothing was changed."
        case .stale: return "The destination or capture state changed. Choose the file again."
        }
    }
}

/// One cold reader. Uncertain close retains its handle and security scope; its
/// caller must retain admission until actual cleanup, rather than open another.
final class ManualDestinationFileReader {
    struct Reply { let result: Result<ImportedManualDestination, ManualImportError>; let cleanupComplete: Bool }
    typealias Access = (URL) -> (() -> Void)
    private let access: Access
    private let close: (FileHandle) throws -> Void
    private var uncertain: (FileHandle, () -> Void)?
    init(access: @escaping Access = { url in
        let scoped = url.startAccessingSecurityScopedResource()
        return { if scoped { url.stopAccessingSecurityScopedResource() } }
    }, close: @escaping (FileHandle) throws -> Void = { try $0.close() }) { self.access = access; self.close = close }
    func read(_ url: URL) -> Reply {
        guard uncertain == nil, url.isFileURL else { return Reply(result: .failure(.unavailable), cleanupComplete: uncertain == nil) }
        let release = access(url)
        let handle: FileHandle
        do { handle = try FileHandle(forReadingFrom: url) }
        catch { release(); return Reply(result: .failure(.unavailable), cleanupComplete: true) }
        let result: Result<ImportedManualDestination, ManualImportError>
        do {
            var stat = Darwin.stat()
            guard fstat(handle.fileDescriptor, &stat) == 0, stat.st_mode & S_IFMT == S_IFREG,
                  stat.st_size >= 0, stat.st_size <= ImportedManualDestination.maximumBytes else { throw ManualImportError.invalidFile }
            var bytes = Data(); bytes.reserveCapacity(ImportedManualDestination.maximumBytes + 1)
            while bytes.count <= ImportedManualDestination.maximumBytes {
                let part = try handle.read(upToCount: min(4096, ImportedManualDestination.maximumBytes + 1 - bytes.count)) ?? Data()
                if part.isEmpty { break }
                bytes.append(part)
            }
            result = .success(try ImportedManualDestination(bytes))
        } catch { result = .failure((error as? ManualImportError) ?? .unavailable) }
        do { try close(handle); release(); return Reply(result: result, cleanupComplete: true) }
        catch { uncertain = (handle, release); return Reply(result: .failure(.unavailable), cleanupComplete: false) }
    }
}

/// Main-queue cold model. Availability is read again at every action, not from a
/// delayed UI snapshot. A ticket captures the original edit/capture revision.
final class ManualDestinationSettings: ObservableObject {
    struct Ticket: Identifiable, Equatable { let id: UUID; let revision: UInt64 }
    struct Review: Identifiable {
        let ticket: Ticket, peer: String, security: String, requiresPlaintext: Bool
        var id: UUID { ticket.id }
    }
    @Published var transport = ManualTransport.rtmp { didSet { edited() } }
    @Published var endpoint = "" { didSet { edited() } }
    @Published var streamKey = "" { didSet { edited() } }
    @Published var srtStreamID = "" { didSet { edited() } }
    @Published var srtPassphrase = "" { didSet { edited() } }
    @Published var allowPlaintext = false { didSet { edited() } }
    @Published var srtPlaintext = false { didSet { edited() } }
    @Published var profileName = "" { didSet { edited() } }
    @Published var selectedProfile: UUID? { didSet { edited() } }
    @Published private(set) var profiles: [SavedDestinationProfile] = []
    @Published private(set) var error: String?
    @Published private(set) var presentation: Ticket?
    @Published private(set) var pickerVisible = false
    @Published private(set) var review: Review?
    @Published private(set) var reading = false
    /// Contains no imported label, endpoint, path or secret. One per app model.
    @Published private(set) var invitation: Ticket?
    private let available: () -> Bool
    private let invitationAvailable: () -> Bool
    private let invitationReviewAvailable: () -> Bool
    private let store: ManualDestinationProfileStoring
    private let reader: ManualDestinationFileReader
    private let work: (@escaping () -> Void) -> Void
    private let delivery: (@escaping () -> Void) -> Void
    private var revision: UInt64 = 0
    private var batching = false, exhausted = false
    private var pending: (Ticket, ImportedManualDestination)?
    private var readingTicket: Ticket?
    private var invited: (Ticket, ImportedManualDestination)?
    private var active = false
    private var selectingInvitationMode = false
    private var retainedSelf: ManualDestinationSettings?
    init(store: ManualDestinationProfileStoring = DestinationProfileStore(), available: @escaping () -> Bool,
         reader: ManualDestinationFileReader = ManualDestinationFileReader(),
         invitationAvailable: (() -> Bool)? = nil, invitationReviewAvailable: (() -> Bool)? = nil,
         work: ((@escaping () -> Void) -> Void)? = nil,
         delivery: @escaping (@escaping () -> Void) -> Void = { DispatchQueue.main.async(execute: $0) }) {
        self.store = store; self.available = available; self.reader = reader; self.delivery = delivery
        self.invitationAvailable = invitationAvailable ?? available
        self.invitationReviewAvailable = invitationReviewAvailable ?? available
        let queue = DispatchQueue(label: "sapflow.manual-destination-import", qos: .utility)
        self.work = work ?? { queue.async(execute: $0) }
        do { profiles = try store.load() } catch { self.error = DestinationError.storage.localizedDescription }
    }
    var canEdit: Bool { available() && !reading && !exhausted }
    private func edited() { if !batching { invalidate() } }
    func invalidate() {
        if revision == UInt64.max { exhausted = true } else { revision += 1 }
        presentation = nil; pickerVisible = false; review = nil; pending = nil; invitation = nil; invited = nil
    }
    func captureChanged() {
        if invitation != nil { if !invitationAvailable() { invalidate() } }
        else if !available(), presentation != nil || review != nil { invalidate() }
    }
    func modeChanged() { if !selectingInvitationMode { invalidate() } }
    func sceneChanged(active value: Bool) {
        // False means actual background, not a permission-prompt inactive
        // phase. Even background-before-first-active retires the invitation.
        if !value { active = false; invalidate(); return }
        guard !active else { return }
        active = true
        // A new invitation may precede the first active hook on cold launch,
        // or arrive after backgrounding. Becoming active never reviews it.
        if invitation == nil { invalidate() }
    }
    @discardableResult func receiveInvitation(_ text: String) -> Bool {
        // Incoming links cannot displace a review, picker or its retained IO.
        guard invitation == nil, presentation == nil, review == nil,
              !reading, !exhausted, invitationAvailable() else { return false }
        invalidate()
        do {
            let profile = try ImportedManualDestination(ManualDestinationEnvelope(text).document)
            guard invitationAvailable(), !reading else { return false }
            let ticket = Ticket(id: UUID(), revision: revision)
            invited = (ticket, profile); invitation = ticket; error = nil
            return true
        } catch { self.error = ManualImportError.invalidLink.localizedDescription; return false }
    }
    func cancelInvitation(_ ticket: Ticket) { if invitation == ticket { invalidate() } }
    var canReviewInvitation: Bool { active && !reading && !exhausted && invitationReviewAvailable() }
    @discardableResult func reviewInvitation(_ ticket: Ticket, selectManual: () -> Void) -> Bool {
        guard canReviewInvitation, let (saved, profile) = invited, saved == ticket,
              ticket.revision == revision else { return false }
        // Only this explicit action may select Manual. Mode invalidation is
        // suppressed solely during the synchronous model-owned selection.
        selectingInvitationMode = true
        selectManual()
        selectingInvitationMode = false
        guard invitation == ticket, ticket.revision == revision, canReviewInvitation, canEdit else { return false }
        invitation = nil; invited = nil
        installReview(ticket, profile: profile)
        return true
    }
    private func installReview(_ ticket: Ticket, profile: ImportedManualDestination) {
        pending = (ticket, profile)
        let peer = profile.host.contains(":") ? "[\(profile.host)]:\(profile.port)" : "\(profile.host):\(profile.port)"
        review = Review(ticket: ticket, peer: peer,
            security: profile.requiresPlaintext ? "Plaintext: independent permission required" : (profile.transport == .rtmp ? "RTMPS (TLS)" : "SRT AES-256 media; control and stream ID are clear"),
            requiresPlaintext: profile.requiresPlaintext)
    }
    @discardableResult func beginImport() -> Ticket? {
        guard canEdit, presentation == nil else { return nil }
        invalidate(); let ticket = Ticket(id: UUID(), revision: revision)
        presentation = ticket; pickerVisible = true; error = nil; return ticket
    }
    func pickerDismissed(_ ticket: Ticket) { if presentation == ticket { pickerVisible = false } }
    func received(_ ticket: Ticket, file: Result<URL, Error>) {
        guard presentation == ticket, ticket.revision == revision, canEdit else { return }
        presentation = nil; pickerVisible = false
        guard case let .success(url) = file else { return }
        reading = true; readingTicket = ticket; retainedSelf = self
        work { [self] in
            let answer = reader.read(url)
            delivery { [self] in
                guard readingTicket == ticket else { return }
                if answer.cleanupComplete { reading = false; readingTicket = nil; retainedSelf = nil }
                guard answer.cleanupComplete else { error = ManualImportError.unavailable.localizedDescription; return }
                guard ticket.revision == revision, available() else { return }
                switch answer.result {
                case let .success(profile):
                    installReview(ticket, profile: profile)
                case let .failure(failure): error = failure.localizedDescription
                }
            }
        }
    }
    func cancelReview(_ ticket: Ticket) { if pending?.0 == ticket { invalidate() } }
    func reportInvalidDestination() { error = "The manual destination is invalid. Nothing was started." }
    @discardableResult func apply(_ ticket: Ticket, plaintextConfirmed: Bool) -> Bool {
        guard canEdit, let (saved, profile) = pending, saved == ticket, ticket.revision == revision,
              !profile.requiresPlaintext || plaintextConfirmed else { return false }
        batching = true
        transport = profile.transport; endpoint = profile.endpoint; streamKey = profile.streamKey
        srtStreamID = profile.streamID; srtPassphrase = profile.passphrase
        // A file author may have placed a secret in its nominal display name.
        // Validate the contract name, but require a local name for explicit Save.
        profileName = ""
        // The file's requested flag is not consent. Secure RTMP does not gain
        // permission to send plaintext merely because the file sets the flag.
        allowPlaintext = profile.transport == .rtmp && profile.requiresPlaintext && plaintextConfirmed
        srtPlaintext = profile.transport == .srt && profile.requiresPlaintext && plaintextConfirmed
        selectedProfile = nil
        batching = false; invalidate(); error = nil; return true
    }
    func rtmpDestination() throws -> ManualDestination { try ManualDestination(endpoint: endpoint, streamKey: streamKey, allowPlaintext: allowPlaintext) }
    func srtDestination() throws -> ManualSrtDestination { try ManualSrtDestination(endpoint: endpoint, streamID: srtStreamID, passphrase: srtPassphrase, allowPlaintext: srtPlaintext) }
    func loadProfile(_ id: UUID?) {
        guard canEdit, let id, let profile = profiles.first(where: { $0.id == id }) else { return }
        do {
            // Resolve and validate everything before changing any form field.
            let secret = try store.resolve(expected: profile)
            if profile.transport == "srt" {
                _ = try ManualSrtDestination(endpoint: profile.endpoint, streamID: secret.streamID, passphrase: secret.passphrase, allowPlaintext: profile.allowPlaintext)
            } else {
                _ = try ManualDestination(endpoint: profile.endpoint, streamKey: secret.streamKey, allowPlaintext: profile.allowPlaintext)
            }
            guard canEdit else { return }
            invalidate(); batching = true
            transport = profile.transport == "srt" ? .srt : .rtmp; endpoint = profile.endpoint; profileName = profile.name
            streamKey = secret.streamKey; srtStreamID = secret.streamID; srtPassphrase = secret.passphrase
            allowPlaintext = transport == .rtmp && profile.allowPlaintext; srtPlaintext = transport == .srt && profile.allowPlaintext
            selectedProfile = id; batching = false; error = nil
        } catch { self.error = "The saved destination could not be loaded. Nothing was changed." }
    }
    func saveProfile() {
        guard canEdit else { return }
        do {
            let id: UUID
            if transport == .srt { id = try store.saveSrt(id: selectedProfile, name: profileName, destination: srtDestination(), allowPlaintext: srtPlaintext) }
            else { id = try store.save(id: selectedProfile, name: profileName, destination: rtmpDestination(), allowPlaintext: allowPlaintext) }
            profiles = try store.load(); selectedProfile = id; error = nil
        } catch { self.error = "Destination save could not be confirmed. Check saved profiles before retrying." }
    }
    func deleteProfile() {
        guard canEdit, let id = selectedProfile else { return }
        do {
            try store.delete(id); profiles = try store.load()
            invalidate(); batching = true; selectedProfile = nil; streamKey = ""; srtStreamID = ""; srtPassphrase = ""; batching = false; error = nil
        } catch { self.error = "The saved destination could not be deleted." }
    }
}
