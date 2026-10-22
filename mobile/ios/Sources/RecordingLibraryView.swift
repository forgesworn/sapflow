import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Inline metadata only. The owner revalidates exact selections and actual
/// capture admission; this view never receives a private file URL or handle.
struct RecordingLibraryView: View {
    @ObservedObject var owner: RecordingLibraryOwner
    let canExport: Bool
    let beginExport: (RecordingCatalogue.Entry, Bool) -> Void
    @State private var expanded = false
    @State private var unverified: RecordingCatalogue.Entry?
    @State private var groups: [ShowGroup] = []

    init(owner: RecordingLibraryOwner, canExport: Bool,
         beginExport: @escaping (RecordingCatalogue.Entry, Bool) -> Void) {
        _owner = ObservedObject(wrappedValue: owner)
        self.canExport = canExport; self.beginExport = beginExport
    }

    private struct ShowGroup: Identifiable {
        let id: String
        let title: String
        let date: Date?
        let entries: [RecordingCatalogue.Entry]
    }

    var body: some View {
        DisclosureGroup("Recordings", isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Button("Reload recordings") { owner.reload() }
                        .disabled(!owner.ready || owner.busy)
                        .accessibilityIdentifier("recordings.reload")
                    if owner.busy {
                        ProgressView().accessibilityLabel("Recording library working")
                        Button("Cancel operation") { owner.cancelOperation() }
                            .disabled(!owner.ready)
                            .accessibilityIdentifier("recordings.cancel")
                    } else if !owner.ready {
                        Button("Retry library setup") { owner.retryInitialisation() }
                            .accessibilityIdentifier("recordings.retry")
                    }
                }
                if let message = owner.message {
                    Text(message).font(.callout).accessibilityIdentifier("recordings.message")
                }
                if owner.entries.count > 256 {
                    Text("The recording list exceeds its supported limit. Existing files are preserved.")
                        .font(.callout)
                } else if owner.entries.isEmpty {
                    Text(owner.ready ? "No recordings listed. Reload to check saved segments." : "The recording library is not ready.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(groups) { group in
                            Section {
                                ForEach(group.entries, id: \.id) { entry in row(entry) }
                            } header: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(group.title).font(.headline)
                                    if let date = group.date { Text(date, style: .date).font(.caption).foregroundStyle(.secondary) }
                                }
                            }
                        }
                    }
                }
                Text("Export copies the selected segment and leaves the original in place. It does not repair files or verify playback. Stop capture before choosing an export folder.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .padding(.top, 8)
        }
        .accessibilityIdentifier("recordings.library")
        .onReceive(owner.$entries) { entries in groups = Self.makeGroups(entries) }
        .confirmationDialog("Copy unverified recording?", isPresented: unverifiedPresented, titleVisibility: .visible, presenting: unverified) { entry in
            Button("Copy unverified recording") {
                unverified = nil
                beginExport(entry, true)
            }
            .disabled(!canExport || owner.busy || !owner.ready)
            Button("Cancel", role: .cancel) { unverified = nil }
        } message: { _ in
            Text("This segment may be incomplete or unplayable. Copying preserves its unverified status and does not repair the original.")
        }
        .background {
            // The host survives both a nil visible picker and a collapsed
            // DisclosureGroup until this exact presentation actually dismisses.
            if let selection = owner.presentation {
                RecordingFolderPresentation(owner: owner, selection: selection)
                    .id(selection.id)
            }
        }
    }
    private var unverifiedPresented: Binding<Bool> {
        Binding(get: { unverified != nil }, set: { if !$0 { unverified = nil } })
    }
    private static func makeGroups(_ entries: [RecordingCatalogue.Entry]) -> [ShowGroup] {
        guard entries.count <= 256 else { return [] }
        let grouped = Dictionary(grouping: entries) { entry in
            entry.identity.map { entry.location.rawValue + ":show:" + $0.showID } ?? "unverified:" + entry.id
        }
        return grouped.map { key, values in
            let sorted = values.sorted { left, right in
                let a = left.identity?.segmentIndex ?? Int64.max
                let b = right.identity?.segmentIndex ?? Int64.max
                return a == b ? left.id < right.id : a < b
            }
            let show = sorted.first?.identity?.showID
            let dates = sorted.compactMap { entry -> Date? in
                guard let milliseconds = entry.createdAtMs, milliseconds > 0,
                      milliseconds <= 253_402_300_799_999 else { return nil }
                return Date(timeIntervalSince1970: Double(milliseconds) / 1000)
            }
            return ShowGroup(id: key, title: show.map { "Show \($0.prefix(8))" } ?? "Unverified recording",
                             date: dates.min(), entries: sorted)
        }.sorted { left, right in
            let a = left.date ?? .distantPast, b = right.date ?? .distantPast
            return a == b ? left.id < right.id : a > b
        }
    }
    private func row(_ entry: RecordingCatalogue.Entry) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(segmentTitle(entry)).font(.subheadline.bold())
            Text(status(entry.state)).font(.caption)
            Text(entry.location == .privateRecording ? "App-private" : "Legacy Documents · unverified")
                .font(.caption).foregroundStyle(.secondary)
            Text("\(size(entry.bytes)) · \(duration(entry.durationMs))")
                .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            if let profile = entry.identity?.profileID { Text(profile).font(.caption).foregroundStyle(.secondary) }
            Button(requiresConfirmation(entry) ? "Export unverified segment" : "Export segment") {
                if requiresConfirmation(entry) { unverified = entry }
                else { beginExport(entry, false) }
            }
            .disabled(!canExport || !owner.ready || owner.busy || entry.state == .writing || entry.bytes == 0)
            .accessibilityIdentifier("recordings.export.\(entry.id)")
            Divider()
        }
    }
    private func segmentTitle(_ entry: RecordingCatalogue.Entry) -> String {
        let suffix = String(entry.segmentID.prefix(8))
        guard let index = entry.identity?.segmentIndex, index < Int64.max else { return "Segment \(suffix)" }
        return "Segment \(index + 1) · \(suffix)"
    }
    private func requiresConfirmation(_ entry: RecordingCatalogue.Entry) -> Bool {
        entry.location == .legacyDocuments || entry.state != .finalised
    }
    private func status(_ state: RecordingCatalogue.State) -> String {
        switch state {
        case .writing: return "Recording in progress"
        case .finalised: return "Recorder finalised"
        case .incomplete: return "Incomplete · unverified"
        case .orphanUnverified: return "Unverified file"
        }
    }
    private func size(_ bytes: UInt64) -> String {
        guard bytes <= UInt64(Int64.max) else { return "Size unavailable" }
        return ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
    private func duration(_ milliseconds: Int64?) -> String {
        guard let milliseconds, milliseconds >= 0 else { return "Duration unavailable" }
        let seconds = milliseconds / 1000
        return String(format: "%02lld:%02lld:%02lld", seconds / 3600, seconds / 60 % 60, seconds % 60)
    }
}

private struct RecordingFolderPresentation: View {
    @ObservedObject var owner: RecordingLibraryOwner
    let selection: RecordingLibraryOwner.Selection

    var body: some View {
        Color.clear.frame(width: 0, height: 0)
            .sheet(isPresented: presented, onDismiss: { owner.pickerDismissed(selection) }) {
                RecordingFolderPicker(selection: selection) { captured, folder in
                    owner.receivedFolder(captured, folder: folder)
                }
                .id(selection.id)
            }
    }
    private var presented: Binding<Bool> {
        Binding(get: { owner.picker?.id == selection.id }, set: { visible in
            if !visible { owner.requestPickerDismissal(selection) }
        })
    }
}

private struct RecordingFolderPicker: UIViewControllerRepresentable {
    let selection: RecordingLibraryOwner.Selection
    let received: (RecordingLibraryOwner.Selection, URL?) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(selection: selection, received: received) }
    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
        picker.allowsMultipleSelection = false
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}
    static func dismantleUIViewController(_ controller: UIDocumentPickerViewController, coordinator: Coordinator) {
        controller.delegate = nil // Not a modal-dismissal or resource-cleanup receipt.
    }
    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        private let selection: RecordingLibraryOwner.Selection
        private let received: (RecordingLibraryOwner.Selection, URL?) -> Void
        private var replied = false // UIKit delegate callbacks run on the main thread.
        init(selection: RecordingLibraryOwner.Selection, received: @escaping (RecordingLibraryOwner.Selection, URL?) -> Void) {
            self.selection = selection; self.received = received
        }
        private func reply(_ folder: URL?) {
            guard !replied else { return }
            replied = true; received(selection, folder)
        }
        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
            reply(urls.count == 1 ? urls.first : nil)
        }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { reply(nil) }
    }
}
