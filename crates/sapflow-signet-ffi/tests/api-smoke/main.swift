import Foundation

private struct SmokeFailure: Error { let code: String }
private func require(_ condition: @autoclosure () -> Bool, _ code: String) throws {
    if !condition() { throw SmokeFailure(code: code) }
}

private let persona = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
private let uri = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid"
private let credentialBytes = Data("{\"version\":1,\"persona\":\"\(persona)\",\"app_secret_key\":\"\(String(repeating: "1", count: 64))\",\"bunker_uri\":\"\(uri)\"}".utf8)

private func expect(_ expected: SignetError, _ body: () throws -> Void) throws {
    do { try body() }
    catch let error as SignetError {
        try require(error == expected, "wrong_error")
        return
    }
    throw SmokeFailure(code: "refusal_missing")
}

private func close(_ owner: NativeSignet) throws {
    owner.requestClose()
    let closed = try owner.waitClosed(timeoutMs: 2_000)
    try require(closed, "close_timeout")
    try require(owner.isClosed(), "not_closed")
}

private func run() throws {
    var input = credentialBytes
    let credential = try SignetCredential.restore(bytes: input)
    input.resetBytes(in: 0..<input.count)
    var output = credential.encoded()
    try require(output == credentialBytes, "credential_input_alias")
    output.resetBytes(in: 0..<output.count)
    try require(credential.encoded() == credentialBytes, "credential_output_alias")
    try expect(.InvalidCredential) { _ = try SignetCredential.restore(bytes: Data("{}".utf8)) }
    try expect(.InvalidInput) { _ = try NativeSignet.pairBunker(uri: uri, deadlineMs: 0) }
    try expect(.InvalidInput) { _ = try NativeSignet.pairBunker(uri: uri, deadlineMs: 120_001) }
    try expect(.InvalidInput) {
        _ = try NativeSignet.pairBunker(uri: uri.replacingOccurrences(of: "wss:", with: "ws:"), deadlineMs: 1_000)
    }

    let owner = try NativeSignet.pairBunker(uri: uri, deadlineMs: 120_000)
    try expect(.Busy) { _ = try NativeSignet.pairBunker(uri: uri, deadlineMs: 1_000) }
    try expect(.InvalidInput) { try owner.connect(requestId: 0) }
    try expect(.InvalidInput) {
        try owner.signLogin(
            requestId: 1,
            challenge: String(repeating: "a", count: 64),
            origin: "https://fixture.example.invalid",
            appName: "Generated offline fixture",
            createdAtSeconds: 1
        )
    }
    try expect(.InvalidInput) { _ = try owner.waitEvent(timeoutMs: 1_001) }
    try expect(.InvalidInput) { _ = try owner.waitClosed(timeoutMs: 2_001) }
    try close(owner)
    try expect(.Closed) { try owner.connect(requestId: 1) }
    try close(owner)

    let restored = try NativeSignet.restore(credential: credential, deadlineMs: 120_000)
    try close(restored)

    let expiring = try NativeSignet.pairBunker(uri: uri, deadlineMs: 30)
    var event: SignetEvent?
    let until = Date().addingTimeInterval(2)
    while event == nil && Date() < until { event = try expiring.waitEvent(timeoutMs: 100) }
    guard case let .failed(requestId, error)? = event else { throw SmokeFailure(code: "deadline_event") }
    try require(requestId == 0 && error == .Deadline, "deadline_fields")
    let closed = try expiring.waitClosed(timeoutMs: 2_000)
    try require(closed, "deadline_close")
    let replay = try expiring.waitEvent(timeoutMs: 0)
    try require(replay == nil, "event_replay")
    try expect(.Closed) { try expiring.connect(requestId: 1) }

    let replacement = try NativeSignet.pairBunker(uri: uri, deadlineMs: 120_000)
    try close(replacement)
}

do {
    try run()
    print("swift-api-smoke: ok")
} catch {
    FileHandle.standardError.write(Data("swift-api-smoke: \(error)\n".utf8))
    exit(1)
}
