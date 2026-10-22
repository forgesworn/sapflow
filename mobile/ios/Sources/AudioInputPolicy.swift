import Foundation

/// Bounded, local metadata only. A display name never selects or authorises an input.
struct AudioInputOption: Equatable, Identifiable {
    enum Kind: String { case builtIn = "Built-in", wired = "Wired", usb = "USB" }
    let id: String
    let name: String
    let kind: Kind
    init?(id: String, name: String, kind: Kind) {
        guard Self.validID(id), name.utf8.prefix(1025).count <= 1024 else { return nil }
        self.id = id; self.kind = kind
        let clean = name.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }
        self.name = String(String.UnicodeScalarView(clean)).prefix(80).description
    }
    static func validID(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.prefix(257).count <= 256 &&
            !id.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
    }
    static func inventory(_ values: [AudioInputOption]) -> [AudioInputOption] {
        var seen = Set<String>()
        // Bound both work and the snapshot, without an unbounded filtered scan.
        var result: [AudioInputOption] = []
        result.reserveCapacity(min(32, values.count))
        for value in values.prefix(64) where seen.insert(value.id).inserted {
            result.append(value)
            if result.count == 32 { break }
        }
        return result
    }
}

/// Pure ownership policy. Construct one per preparation, with that preparation's
/// immutable choice and inventory. Native adapters must supply their captured
/// owner, not substitute whichever preparation happens to be current later.
/// Include preparation and epoch/native-owner identity: one preparation can
/// survive multiple rotation epochs.
final class AudioInputPolicy<Owner: Equatable> {
    enum Observation { case accepted, failed, stale, ignored }
    let requestedID: String? // nil means System Default, not permission to switch later.
    let owner: Owner
    private let available: Set<String>
    private let lock = NSLock()
    private var actualID: String?
    private var failed = false
    private var closed = false
    init?(owner: Owner, requestedID: String? = nil, available: [AudioInputOption]) {
        let ids = Set(AudioInputOption.inventory(available).map(\.id))
        if let requestedID, !AudioInputOption.validID(requestedID) || !ids.contains(requestedID) { return nil }
        self.owner = owner; self.requestedID = requestedID; self.available = ids
    }
    func observe(owner: Owner, id: String?) -> Observation {
        lock.lock(); defer { lock.unlock() }
        guard owner == self.owner else { return .stale }
        guard !closed, !failed else { return .ignored }
        guard let id, available.contains(id), requestedID == nil || requestedID == id,
              actualID == nil || actualID == id else {
            failed = true; return .failed
        }
        actualID = id; return .accepted
    }
    func permitsMedia(owner: Owner) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return owner == self.owner && !closed && !failed && actualID != nil
    }
    func observedID(owner: Owner) -> String? {
        lock.lock(); defer { lock.unlock() }
        return owner == self.owner && !closed && !failed ? actualID : nil
    }
    func close(owner: Owner) {
        lock.lock(); defer { lock.unlock() }
        guard owner == self.owner else { return }; closed = true
    }
}
