import Foundation

/// Display projection of the authenticated, canManage-filtered event list.
/// Roles, stream settings and other presentation extensions are deliberately
/// not retained. None of these display fields authorises a native session.
struct V4vCreatorEvent: Equatable, CustomStringConvertible, CustomDebugStringConvertible {
    let id: String
    let title: String?
    let statusText: String?
    let startsAtText: String?
    let endsAtText: String?

    init(_ value: V4vJSON) throws {
        guard case let .object(fields) = value else { throw V4vFailure.invalidContract }
        id = try V4vIdentity.event(fields.text("id", max: 128))
        // Backend title is at most 160 UTF-16 units. The byte bound also permits
        // non-ASCII titles without splitting or silently truncating them.
        title = try Self.display(fields["title"], max: 640)
        statusText = try Self.display(fields["status"], max: 20)
        // These are inert display text, never timers, capture intent or dates
        // used to grant authority. The backend emits ISO text or an empty string.
        startsAtText = try Self.display(fields["startsAt"], max: 40)
        endsAtText = try Self.display(fields["endsAt"], max: 40)
    }
    private static func display(_ value: V4vJSON?, max: Int) throws -> String? {
        guard let value, !value.isNull else { return nil }
        return try value.text(max: max)
    }
    var description: String { "V4V creator event display (redacted)" }
    var debugDescription: String { description }
}

struct V4vCreatorEvents: CustomStringConvertible, CustomDebugStringConvertible {
    /// Local admission limit, not a server pagination contract. Oversize lists
    /// fail as a whole; callers must not mistake a truncated list for authority.
    static let maximumEvents = 128
    let events: [V4vCreatorEvent]

    init(bytes: Data) throws { try self.init(V4vJSON.parse(bytes)) }
    /// The control client's existing strict 64 KiB JSON parser is the normal
    /// entry point. The bytes convenience uses that same parser, not another.
    init(_ answer: V4vJSON) throws {
        let response = try answer.object(required: ["ok", "events"])
        guard try response.flag("ok"), case let .array(rows) = try response.need("events"),
              rows.count <= Self.maximumEvents else { throw V4vFailure.invalidContract }
        var ids = Set<String>()
        var projected = [V4vCreatorEvent](); projected.reserveCapacity(rows.count)
        for row in rows {
            let event = try V4vCreatorEvent(row)
            guard ids.insert(event.id).inserted else { throw V4vFailure.invalidContract }
            projected.append(event)
        }
        events = projected
    }
    var description: String { "V4V creator events (redacted)" }
    var debugDescription: String { description }
}

/// A read result bound to the captured exact-session request. Reading this DTO
/// does not applyStatus, recover credentials, acknowledge an uncertain request
/// or provide decoded-media proof. Collection reads use V4vCurrentSession.
struct V4vCreatorSessionStatus: CustomStringConvertible, CustomDebugStringConvertible {
    let receipt: V4vSessionReceipt
    init(_ answer: V4vJSON, requested: V4vIdentity) throws {
        let response = try answer.object(required: ["ok", "session"])
        guard try response.flag("ok") else { throw V4vFailure.invalidContract }
        let parsed = try V4vSessionReceipt(response.need("session"))
        guard parsed.identity.sameSession(requested), parsed.identity.revision >= requested.revision else { throw V4vFailure.conflict }
        receipt = parsed
    }
    var description: String { "V4V creator session status (redacted)" }
    var debugDescription: String { description }
}
