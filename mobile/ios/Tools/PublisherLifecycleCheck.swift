// No camera, codec, media, protocol peer or live origin is involved. This drives
// the production SRT NWConnection owner to ready and observes actual cancellation.
import Foundation
import Network

func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw CaptureFailure(message: message) }
}
func waitUntil(_ predicate: () -> Bool) -> Bool {
    let deadline = DispatchTime.now().uptimeNanoseconds + 2_000_000_000
    while DispatchTime.now().uptimeNanoseconds < deadline {
        if predicate() { return true }
        Thread.sleep(forTimeInterval: 0.001)
    }
    return predicate()
}
func run() throws {
    let parameters = NWParameters.udp
    parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
    let listener = try NWListener(using: parameters)
    let ready = DispatchSemaphore(value: 0)
    listener.stateUpdateHandler = { state in if case .ready = state { ready.signal() } }
    listener.newConnectionHandler = { $0.cancel() }
    listener.start(queue: DispatchQueue(label: "sapflow.lifecycle-listener"))
    defer { listener.cancel() }
    try require(ready.wait(timeout: .now() + 2) == .success, "Loopback listener did not become ready")
    guard let port = listener.port else { throw CaptureFailure(message: "Loopback listener has no port") }
    for _ in 0..<3 {
        let destination = try ManualSrtDestination(endpoint: "srt://127.0.0.1:\(port.rawValue)",
            streamID: "publish:live/lifecycle", passphrase: "", allowPlaintext: true)
        let sink = ManualSrtPublisher(destination: destination)
        try sink.start()
        try require(waitUntil { sink.status.hasPrefix("Waiting for encoded video/audio") }, "Production UDP owner did not reach ready")
        try require(!sink.cleanupComplete, "Live UDP owner incorrectly reported cleanup")
        let first = DispatchSemaphore(value: 0), second = DispatchSemaphore(value: 0)
        sink.stop { first.signal() }
        sink.stop { second.signal() }
        try require(first.wait(timeout: .now() + 2) == .success, "Actual cancellation completion was suppressed")
        try require(second.wait(timeout: .now() + 2) == .success, "Duplicate Stop completion was lost")
        try require(sink.cleanupComplete, "Cancellation callback did not release the owner")
    }
    print("Production SRT NW cancellation PASS: 3 ready/cancel cycles, 6 actual completion callbacks; no media or handshake")
}
do { try run() } catch { fputs("Publisher lifecycle check failed\n", stderr); exit(1) }
