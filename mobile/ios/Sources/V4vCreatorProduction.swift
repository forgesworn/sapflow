import Foundation

/// The app model creates this owner once, not on view redraw or mode changes.
/// Completion means constructed/loading; the owner's snapshot reports actual
/// journal, account and reconcile state. It never means signed in or ready.
enum V4vCreatorProduction {
    static func open(configuration: V4vCreatorConfiguration.Snapshot, capture: V4vCreatorCapturePort,
                     clock: @escaping () -> V4vClock = realtimeClock,
                     changed: @escaping (V4vCreatorOwner.Snapshot) -> Void,
                     completion: @escaping (Result<V4vCreatorOwner, V4vFailure>) -> Void) {
        // Disabled builds never create an owner, directory or Keychain source.
        guard let profile = configuration.profile else {
            DispatchQueue.main.async { completion(.failure(.unavailable)) }
            return
        }
        let owner = DispatchQueue(label: "sapflow.v4v.creator", qos: .userInitiated)
        owner.async {
            do {
                let support = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                                          appropriateFor: nil, create: true)
                // EncryptedJournal creates/validates each private 0700 directory,
                // locks it, and fails closed on corrupt or missing existing halves.
                // Signer client credentials never share the publish/End journal.
                let journal = try V4vEncryptedJournal(
                    directory: support.appendingPathComponent("SapflowV4vSession", isDirectory: true), profile: profile,
                    keys: V4vKeychainJournalKey(service: "dev.forgesworn.sapflow.v4v.publish-journal.v1"))
                let signerJournal = try V4vEncryptedJournal(
                    directory: support.appendingPathComponent("SapflowV4vSigner", isDirectory: true), profile: profile,
                    keys: V4vKeychainJournalKey(service: "dev.forgesworn.sapflow.v4v.signer-client-journal.v1"))
                let creator = V4vCreatorOwner(profile: profile, journal: journal,
                    credentials: V4vSignetCredentialStore(journal: signerJournal), capture: capture,
                    owner: owner, clock: clock, delivery: .main, changed: changed)
                // Initial app mode is local recording. This cold flag wins
                // before init's queued bootstrap can schedule ordinary refresh;
                // exact pending/terminal End reconciliation remains permitted.
                creator.setActive(false)
                DispatchQueue.main.async { completion(.success(creator)) }
            } catch {
                // No OS description, route, journal path, credential or reset.
                DispatchQueue.main.async { completion(.failure(.journal)) }
            }
        }
    }
    static func realtimeClock() -> V4vClock {
        let milliseconds = Date().timeIntervalSince1970 * 1000
        let wall: UInt64 = milliseconds.isFinite && milliseconds > 0 && milliseconds < Double(V4vJSON.maxEpoch)
            ? UInt64(milliseconds) : 0
        return V4vClock(wallMs: wall, monotonicNs: DispatchTime.now().uptimeNanoseconds)
    }
}
