import Foundation

enum CameraPosition: String, CaseIterable, Identifiable {
    case rear, front
    var id: String { rawValue }
    var label: String { self == .rear ? "Rear" : "Front" }
    var previewMirrored: Bool { self == .front }
    var encodedMirrored: Bool { false }
}

enum CameraAutomaticMode: String, CaseIterable { case automatic, locked }
struct CameraPoint: Equatable {
    let x: Double
    let y: Double
    var valid: Bool { x.isFinite && y.isFinite && (0...1).contains(x) && (0...1).contains(y) }
}
struct CameraCapabilities: Equatable {
    var minimumZoom: Double = 1
    var maximumZoom: Double = 1
    var focusAuto = false
    var focusLock = false
    var focusPoint = false
    var exposureAuto = false
    var exposureLock = false
    var exposurePoint = false
    var minimumBias: Double = 0
    var maximumBias: Double = 0
    var whiteBalanceAuto = false
    var whiteBalanceLock = false
    var valid: Bool {
        minimumZoom.isFinite && maximumZoom.isFinite && minimumZoom > 0 && maximumZoom >= minimumZoom &&
        minimumBias.isFinite && maximumBias.isFinite && minimumBias <= maximumBias
    }
    func accepts(_ command: CameraCommand) -> Bool {
        guard valid else { return false }
        switch command {
        case let .zoom(value): return value.isFinite && (minimumZoom...maximumZoom).contains(value)
        case let .focus(mode): return mode == .automatic ? focusAuto : focusLock
        case let .focusPoint(point): return focusPoint && focusAuto && point.valid
        case let .exposure(mode): return mode == .automatic ? exposureAuto : exposureLock
        case let .exposurePoint(point): return exposurePoint && exposureAuto && point.valid
        case let .bias(value): return value.isFinite && (minimumBias...maximumBias).contains(value)
        case let .whiteBalance(mode): return mode == .automatic ? whiteBalanceAuto : whiteBalanceLock
        }
    }
}
enum CameraCommand: Equatable {
    case zoom(Double), focus(CameraAutomaticMode), focusPoint(CameraPoint)
    case exposure(CameraAutomaticMode), exposurePoint(CameraPoint), bias(Double), whiteBalance(CameraAutomaticMode)
    var label: String {
        switch self {
        case let .zoom(value): return String(format: "Zoom %.2f×", value)
        case let .bias(value): return String(format: "Exposure compensation %+.1f EV", value)
        case let .focus(mode): return "Focus " + mode.rawValue
        case .focusPoint: return "Focus at preview point"
        case let .exposure(mode): return "Exposure " + mode.rawValue
        case .exposurePoint: return "Exposure at preview point"
        case let .whiteBalance(mode): return "White balance " + mode.rawValue
        }
    }
    var slot: Int {
        switch self { case .zoom: return 0; case .focus, .focusPoint: return 1
        case .exposure, .exposurePoint: return 2; case .bias: return 3; case .whiteBalance: return 4 }
    }
}

/// Scalar source ownership only. The containing admission lock serialises these
/// transitions; no device, delegate, encoder, callback or dispatch work runs here.
struct CameraControlPolicy<Owner: Equatable> {
    static var switchBudgetNs: UInt64 { 6_000_000_000 }
    struct Canvas: Equatable { let width: Int; let height: Int }
    struct Source: Equatable {
        let owner: Owner
        let serial: UInt64
        let position: CameraPosition
        let canvas: Canvas
    }
    struct Switch: Equatable {
        let owner: Owner
        let serial: UInt64
        let position: CameraPosition
        let beganAtNs: UInt64
        let deadlineNs: UInt64
    }
    enum Frame: Equatable { case stale, accepted(first: Bool), failed }
    enum Media: Equatable { case allowed, stale, failed }
    private(set) var owner: Owner?
    private(set) var desired = CameraPosition.rear
    private(set) var source: Source?
    private(set) var confirmed = false
    private(set) var pending: Switch?
    private(set) var retired = false
    private(set) var failed = false
    private var serial: UInt64 = 0
    private mutating func next() -> UInt64? { guard serial < UInt64.max else { return nil }; serial += 1; return serial }
    mutating func prepare(_ owner: Owner, initial: CameraPosition) {
        self.owner = owner; desired = initial; source = nil; pending = nil
        confirmed = false; retired = false; failed = false
    }
    mutating func initial(_ owner: Owner, canvas: Canvas) -> Source? {
        guard self.owner == owner, !failed, pending == nil, canvas.width > 0, canvas.height > 0, let id = next() else { return nil }
        let value = Source(owner: owner, serial: id, position: desired, canvas: canvas)
        source = value; confirmed = false; retired = false; return value
    }
    mutating func reserveSwitch(_ owner: Owner, position: CameraPosition, nowNs: UInt64) -> Switch? {
        guard self.owner == owner, !failed, source != nil, confirmed, pending == nil, position != desired,
              nowNs <= UInt64.max - Self.switchBudgetNs, let id = next() else { return nil }
        let value = Switch(owner: owner, serial: id, position: position, beganAtNs: nowNs, deadlineNs: nowNs + Self.switchBudgetNs)
        pending = value; retired = false; return value
    }
    func permits(_ value: Switch, nowNs: UInt64) -> Bool {
        !failed && owner == value.owner && pending == value && nowNs >= value.beganAtNs && nowNs < value.deadlineNs
    }
    mutating func retire(_ value: Switch, nowNs: UInt64) -> Source? {
        guard permits(value, nowNs: nowNs), !retired, let previous = source, let id = next() else { return nil }
        let result = Source(owner: value.owner, serial: id, position: value.position, canvas: previous.canvas)
        source = result; confirmed = false; retired = true; return result
    }
    mutating func reject(_ value: Switch) -> Bool {
        guard pending == value, !retired else { return false }; pending = nil; return true
    }
    mutating func fail(_ value: Switch) -> Bool {
        guard pending == value else { return false }; failed = true; confirmed = false; return true
    }
    mutating func frame(_ value: Source, width: Int, height: Int, nowNs: UInt64) -> Frame {
        guard source == value, owner == value.owner, !failed else { return .stale }
        if let pending, !permits(pending, nowNs: nowNs) {
            if retired { failed = true; confirmed = false; return .failed }
            self.pending = nil
        }
        guard width == value.canvas.width, height == value.canvas.height else { failed = true; confirmed = false; return .failed }
        let first = !confirmed
        confirmed = true
        if retired { desired = value.position; pending = nil; retired = false }
        return .accepted(first: first)
    }
    mutating func media(_ value: Owner, nowNs: UInt64) -> Media {
        guard owner == value, !failed else { return .stale }
        if let pending, retired, !permits(pending, nowNs: nowNs) {
            failed = true; confirmed = false; return .failed
        }
        return .allowed
    }
    func permitsControls(_ value: Source) -> Bool { owner == value.owner && source == value && confirmed && pending == nil && !failed }
    mutating func close() { owner = nil; source = nil; pending = nil; confirmed = false; retired = false }
}

/// At most one queued cold drain, with one latest command per control family.
/// UI gestures never create an unbounded queue of native operations.
final class CameraCommandMailbox<Source: Equatable> {
    struct Request: Equatable {
        let source: Source
        let command: CameraCommand
        let serial: UInt64
        // Payload validity is separate from identity. In particular NaN is not
        // equal to itself, but its rejected operation must still release capacity.
        static func == (lhs: Self, rhs: Self) -> Bool { lhs.serial == rhs.serial && lhs.source == rhs.source }
    }
    struct Completion { let confirmed: Bool; let schedule: Bool }
    private let lock = NSLock()
    private var values: [Request?] = Array(repeating: nil, count: 5)
    private var latest: [Request?] = Array(repeating: nil, count: 5)
    private var inFlight: [Request?] = Array(repeating: nil, count: 5)
    private var serial: UInt64 = 0
    private var queued = false
    private func scheduleLocked() -> Bool {
        guard !queued, values.indices.contains(where: { values[$0] != nil && inFlight[$0] == nil }) else { return false }
        queued = true; return true
    }
    func offer(_ source: Source, _ command: CameraCommand) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard serial < UInt64.max else { return false }; serial += 1
        let request = Request(source: source, command: command, serial: serial)
        values[command.slot] = request; latest[command.slot] = request
        return scheduleLocked()
    }
    func take() -> [Request] {
        lock.lock(); defer { lock.unlock() }
        var result: [Request] = []
        for index in values.indices where inFlight[index] == nil {
            if let value = values[index] { result.append(value); inFlight[index] = value; values[index] = nil }
        }
        queued = false; return result
    }
    func complete(_ request: Request) -> Completion? {
        lock.lock(); defer { lock.unlock() }
        let slot = request.command.slot
        guard inFlight[slot] == request else { return nil }
        inFlight[slot] = nil
        let confirmed = latest[slot] == request
        if confirmed { latest[slot] = nil }
        return Completion(confirmed: confirmed, schedule: scheduleLocked())
    }
    func pending(_ source: Source) -> Bool {
        lock.lock(); defer { lock.unlock() }; return latest.contains { $0?.source == source }
    }
    func invalidate() {
        lock.lock(); defer { lock.unlock() }
        values = Array(repeating: nil, count: 5); latest = Array(repeating: nil, count: 5)
        inFlight = Array(repeating: nil, count: 5)
    }
}
