import XCTest
@testable import SapflowBroadcaster

final class SrtDestinationTests: XCTestCase {
    func testEncryptedNumericPeersAndExplicitPlaintext() throws {
        let secure = try ManualSrtDestination(endpoint: "srt://192.0.2.1:9000", streamID: "publish:test", passphrase: "test secret value", allowPlaintext: false)
        XCTAssertTrue(secure.encrypted)
        XCTAssertEqual(secure.socketAddress, "192.0.2.1:9000")
        let ipv6 = try ManualSrtDestination(endpoint: "srt://[2001:db8::1]:9001", streamID: "", passphrase: "", allowPlaintext: true)
        XCTAssertFalse(ipv6.encrypted)
        XCTAssertNil(ipv6.passphrase)
        XCTAssertEqual(ipv6.socketAddress, "[2001:db8::1]:9001")
        XCTAssertThrowsError(try ManualSrtDestination(endpoint: secure.endpoint, streamID: "", passphrase: "", allowPlaintext: false))
        XCTAssertThrowsError(try ManualSrtDestination(endpoint: secure.endpoint, streamID: "", passphrase: "test secret value", allowPlaintext: true))
    }
    func testAmbiguousPeersAndSecretBoundsFailClosed() {
        for endpoint in ["srt://host.example.invalid:9000", "srt://192.0.2.1", "srt://192.0.2.1:0", "srt://192.0.2.1:65536", "srt://192.0.2.1:99999999999999999999", "srt://0.0.0.0:9000", "srt://224.0.0.1:9000", "srt://255.255.255.255:9000", "srt://[::]:9000", "srt://[ff02::1]:9000", "srt://user:secret@192.0.2.1:9000", "srt://192.0.2.1:9000?passphrase=secret", "srt://192.0.2.1:9000/live"] {
            XCTAssertThrowsError(try ManualSrtDestination(endpoint: endpoint, streamID: "", passphrase: "test secret value", allowPlaintext: false), endpoint)
        }
        for secret in ["short", String(repeating: "x", count: 80), "contains\nnewline", "nonascii-é-secret"] {
            XCTAssertThrowsError(try ManualSrtDestination(endpoint: "srt://192.0.2.1:9000", streamID: "", passphrase: secret, allowPlaintext: false))
        }
        XCTAssertThrowsError(try ManualSrtDestination(endpoint: "srt://192.0.2.1:9000", streamID: String(repeating: "x", count: 513), passphrase: "test secret value", allowPlaintext: false))
    }
    func testLegacyRtmpProfileMetadataRemainsReadableWithoutSecrets() throws {
        let id = UUID()
        let json = "{\"id\":\"\(id.uuidString)\",\"name\":\"Existing\",\"endpoint\":\"rtmps://stream.example.invalid/live\",\"allowPlaintext\":false}"
        let profile = try JSONDecoder().decode(SavedDestinationProfile.self, from: Data(json.utf8))
        XCTAssertNil(profile.transport)
        XCTAssertEqual(profile.id, id)
        let encoded = try JSONEncoder().encode(profile)
        XCTAssertFalse(String(decoding: encoded, as: UTF8.self).contains("passphrase"))
    }
}
