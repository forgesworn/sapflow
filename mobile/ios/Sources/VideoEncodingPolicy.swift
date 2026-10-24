import CoreMedia
import Foundation

/// Fixed native callback credits. The encoder supplies its short state lock;
/// this value never calls native APIs or changes a submitted owner identity.
struct VideoEncodingPolicy<Token: Equatable> {
    enum Failure: Error { case capacity, serialExhausted, clock, closed, unknownCallback, repeatedCallback, inconsistentPTS }
    struct Submission: Equatable {
        let serial: UInt64
        let token: Token
        let time: CMTime
    }
    private struct Entry { let submission: Submission; var ready = false }
    private var entries: [Entry?]
    private var issued: UInt64
    private var delivered: UInt64
    private var lastTime: CMTime?
    private(set) var count = 0
    private(set) var failed = false
    private(set) var closed = false
    var capacity: Int { entries.count }
    var quiescent: Bool { count == 0 }

    init(capacity: Int = 128, startingSerial: UInt64 = 0) throws {
        guard (1...128).contains(capacity), startingSerial < UInt64(Int.max) else { throw Failure.capacity }
        entries = Array(repeating: nil, count: capacity)
        issued = startingSerial; delivered = startingSerial
    }
    mutating func reserve(token: Token, time: CMTime) throws -> Submission {
        guard !closed && !failed else { throw Failure.closed }
        guard count < capacity else { failed = true; throw Failure.capacity }
        guard issued < UInt64(Int.max) else { failed = true; throw Failure.serialExhausted }
        guard time.isNumeric, time.epoch == 0, CMTimeCompare(time, .zero) >= 0,
              lastTime.map({ CMTimeCompare(time, $0) > 0 }) ?? true else { failed = true; throw Failure.clock }
        if let lastTime {
            do { _ = try VideoEncodingTiming.duration(from: lastTime, to: time) }
            catch { failed = true; throw Failure.clock }
        }
        let submission = Submission(serial: issued + 1, token: token, time: time)
        let index = Int(submission.serial % UInt64(capacity))
        guard entries[index] == nil else { failed = true; throw Failure.capacity }
        entries[index] = Entry(submission: submission)
        issued = submission.serial; count += 1; lastTime = time
        return submission
    }
    func submission(_ serial: UInt64) -> Submission? {
        guard let entry = entries[Int(serial % UInt64(capacity))], entry.submission.serial == serial else { return nil }
        return entry.submission
    }
    /// A callback credit remains owned until the single drainer consumes it.
    /// Even a bad timestamp settles that exact native credit before throwing.
    mutating func complete(_ serial: UInt64, time: CMTime?) throws {
        let index = Int(serial % UInt64(capacity))
        guard var entry = entries[index], entry.submission.serial == serial else {
            failed = true; throw Failure.unknownCallback
        }
        guard !entry.ready else { failed = true; throw Failure.repeatedCallback }
        entry.ready = true; entries[index] = entry
        if let time, CMTimeCompare(time, entry.submission.time) != 0 {
            failed = true; throw Failure.inconsistentPTS
        }
    }
    /// Callback arrival can overlap; delivery always follows submitted order.
    mutating func takeNext() -> Submission? {
        guard delivered < issued else { return nil }
        let index = Int((delivered + 1) % UInt64(capacity))
        guard let entry = entries[index], entry.submission.serial == delivered + 1, entry.ready else { return nil }
        entries[index] = nil; delivered = entry.submission.serial; count -= 1
        return entry.submission
    }
    mutating func fail() { failed = true }
    mutating func closeInput() { closed = true }
}

/// Shared callback transaction, used under the encoder's condition lock. The
/// elected callback drains inline; other callbacks only fill reserved slots.
struct VideoCallbackQueue<Token: Equatable, Frame> {
    typealias Submission = VideoEncodingPolicy<Token>.Submission
    private var policy: VideoEncodingPolicy<Token>
    private var frames: [Frame?]
    private(set) var draining = false
    var failed: Bool { policy.failed }
    var quiescent: Bool { policy.quiescent && !draining }
    var count: Int { policy.count }

    init(capacity: Int = 128) throws {
        policy = try .init(capacity: capacity)
        frames = Array(repeating: nil, count: capacity)
    }
    mutating func reserve(token: Token, time: CMTime) throws -> Submission { try policy.reserve(token: token, time: time) }
    func submission(_ serial: UInt64) -> Submission? { policy.submission(serial) }
    mutating func complete(_ serial: UInt64, time: CMTime?, frame: Frame?) throws {
        try policy.complete(serial, time: time)
        frames[Int(serial % UInt64(frames.count))] = frame
    }
    mutating func electDrainer() -> Bool {
        guard !draining else { return false }
        draining = true; return true
    }
    mutating func takeNext() -> (Submission, Frame?)? {
        guard let submission = policy.takeNext() else { draining = false; return nil }
        let index = Int(submission.serial % UInt64(frames.count))
        let frame = frames[index]; frames[index] = nil
        return (submission, frame)
    }
    mutating func fail() { policy.fail() }
    mutating func closeInput() { policy.closeInput() }
}

enum VideoEncodingTiming {
    enum Failure: Error { case clock }
    static func duration(from previous: CMTime, to next: CMTime) throws -> CMTime {
        let delta = CMTimeSubtract(next, previous)
        guard delta.isNumeric, CMTimeCompare(delta, .zero) > 0,
              CMTimeCompare(delta, CMTime(value: 2, timescale: 1)) < 0 else { throw Failure.clock }
        return delta
    }
    static func sealedEnd(last: CMTime, frameRate: Int32) throws -> CMTime {
        guard last.isNumeric, frameRate > 0 else { throw Failure.clock }
        let end = CMTimeAdd(last, CMTime(value: 1, timescale: frameRate))
        guard end.isNumeric, CMTimeCompare(end, last) > 0 else { throw Failure.clock }
        return end
    }
    static func requireNoOverlap(_ time: CMTime, sealedEnd: CMTime?) throws {
        guard time.isNumeric, sealedEnd.map({ CMTimeCompare(time, $0) >= 0 }) ?? true else { throw Failure.clock }
    }
}
