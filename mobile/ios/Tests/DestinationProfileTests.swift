import XCTest
@testable import SapflowBroadcaster

final class DestinationProfileTests: XCTestCase {
    func testTlsIsDefaultAndPlaintextRequiresExplicitChoice() throws {
        let secure = try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "private-key", allowPlaintext: false)
        XCTAssertTrue(secure.encrypted)
        XCTAssertEqual(secure.port, 443)
        XCTAssertEqual(secure.app, "live")
        XCTAssertThrowsError(try ManualDestination(endpoint: "rtmp://stream.example.invalid/live", streamKey: "private-key", allowPlaintext: false))
        let plain = try ManualDestination(endpoint: "rtmp://stream.example.invalid:1936/live", streamKey: "private-key", allowPlaintext: true)
        XCTAssertFalse(plain.encrypted)
        XCTAssertEqual(plain.port, 1936)
    }
    func testCredentialsAndAmbiguousUrlsAreRejected() {
        for endpoint in ["https://stream.example.invalid/live", "rtmps://user:password@stream.example.invalid/live", "rtmps://stream.example.invalid/live?key=secret", "rtmps://stream.example.invalid/live#secret", "rtmps://stream.example.invalid", "rtmps://stream.example.invalid:0/live", "rtmps://stream.example.invalid:/live", "rtmps://stream.example.invalid:65536/live", "rtmps://stream.example.invalid:999999999999999999999999/live", "rtmps://stream.example.invalid/live/../private"] {
            XCTAssertThrowsError(try ManualDestination(endpoint: endpoint, streamKey: "private-key", allowPlaintext: true), endpoint)
        }
    }
    func testProfileBoundsAndKeyQueryAreExplicit() throws {
        XCTAssertThrowsError(try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "", allowPlaintext: false))
        XCTAssertThrowsError(try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: String(repeating: "x", count: 4097), allowPlaintext: false))
        XCTAssertThrowsError(try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "key\r\nother", allowPlaintext: false))
        let allowed = try ManualDestination(endpoint: "rtmps://stream.example.invalid/live", streamKey: "key?destination-token=private", allowPlaintext: false)
        XCTAssertEqual(allowed.streamKey, "key?destination-token=private")
    }
}
