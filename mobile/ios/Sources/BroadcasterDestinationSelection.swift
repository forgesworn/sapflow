import Foundation

enum BroadcasterDestinationMode: String, CaseIterable {
    case recording = "Record only", manual = "Manual destination", v4v = "V4V"
}

/// Main/UI command ownership, independent of delayed display snapshots. The
/// camera retains actual native cleanup; selecting a mode never releases it.
final class BroadcasterDestinationSelection {
    private(set) var mode = BroadcasterDestinationMode.recording
    private(set) var preparation: CaptureAdmission.Preparation?
    private var endManual: (() -> Void)?
    private let cancelPending: (CaptureAdmission.Preparation) -> Void

    init(cancelPending: @escaping (CaptureAdmission.Preparation) -> Void) { self.cancelPending = cancelPending }
    func select(_ next: BroadcasterDestinationMode) {
        guard next != mode else { return }
        let ticket = preparation, end = endManual
        mode = next; preparation = nil; endManual = nil
        // Even if Start committed before the UI saw its running snapshot, the
        // previous manual publisher must not continue under a new destination.
        end?()
        if let ticket { cancelPending(ticket) }
    }
    func reserve(_ ticket: CaptureAdmission.Preparation) -> Bool {
        // The caller has just obtained this fresh ticket from the camera's
        // actual cleanup barrier. A failed prior preparation may have had no
        // further UI update; it must not prevent an explicitly admitted retry.
        guard mode != .v4v, preparation != ticket else { return false }
        let old = endManual
        preparation = ticket; endManual = nil; old?(); return true
    }
    func installManual(_ ticket: CaptureAdmission.Preparation, end: @escaping () -> Void) -> Bool {
        guard mode == .manual, preparation == ticket, endManual == nil else { return false }
        endManual = end; return true
    }
    /// Only a factory throw with no constructed owner uses this path. A real
    /// rejected candidate still belongs to CaptureEngine's cleanup barrier.
    func forgetFailedReservation(_ ticket: CaptureAdmission.Preparation) {
        guard preparation == ticket, endManual == nil else { return }
        preparation = nil
    }
    func stop() {
        let end = endManual; endManual = nil; preparation = nil; end?()
    }
    func stop(preparation ticket: CaptureAdmission.Preparation) {
        guard preparation == ticket else { return }; stop()
    }
}
