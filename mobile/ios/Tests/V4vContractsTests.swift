import XCTest
@testable import SapflowBroadcaster

final class V4vContractsTests: XCTestCase {
    private func parse(_ text: String) throws -> V4vJSON { try V4vJSON.parse(Data(text.utf8)) }
    func testEscapedAndNestedDuplicateKeysRejectedBeforeNormalisation() throws {
        for text in [#"{"a":1,"\u0061":2}"#, #"{"x":{"a":1,"a":1}}"#, #"{"x":[{"s":"secret","\u0073":"other"}]}"#] {
            XCTAssertThrowsError(try parse(text))
        }
        XCTAssertNoThrow(try parse(#"{"a":{"key":1},"b":{"key":2}}"#))
    }
    func testIntegerLexemesDoNotCoerceBooleansStringsFloatsOrExponents() throws {
        for literal in ["true", "false", #""1""#, "1.0", "1e0", "1E+0", "-0", "-1", "9007199254740992", "18446744073709551616"] {
            let value = try parse("{\"n\":" + literal + "}")
            XCTAssertThrowsError(try value.object(required: ["n"]).uint("n"), literal)
        }
        XCTAssertEqual(try parse("9007199254740991").uint(), V4vJSON.maxSafe)
        XCTAssertEqual(try parse("0").uint(), 0)
        for invalid in ["01", "+1", "1.", ".1", "NaN", "Infinity", "truefalse"] { XCTAssertThrowsError(try parse(invalid)) }
    }
    func testUtf8AndEscapedUnicodeAreValidatedAndPreserved() throws {
        let value = try parse(#"{"text":"Grüße \ud83d\ude03"}"#)
        XCTAssertEqual(try value.object(required: ["text"]).text("text"), "Grüße 😃")
        XCTAssertThrowsError(try V4vJSON.parse(Data([123,34,120,34,58,34,0xff,34,125])))
        for token in [#""\ud800""#, #""\udc00""#, #""\q""#, "\"raw\nnewline\""] { XCTAssertThrowsError(try parse(token)) }
    }
    func testContainerDepthAndBodyBoundsAreExact() throws {
        let sixteen = String(repeating: "[", count: 16) + "0" + String(repeating: "]", count: 16)
        XCTAssertNoThrow(try parse(sixteen))
        XCTAssertThrowsError(try parse("[" + sixteen + "]"))
        let exact = Data(("{}" + String(repeating: " ", count: V4vJSON.maxBody - 2)).utf8)
        XCTAssertNoThrow(try V4vJSON.parse(exact))
        XCTAssertThrowsError(try V4vJSON.parse(exact + Data([32])))
    }
    func testStrictObjectFieldsAndRoundTripKeepScalarTypes() throws {
        let original = try parse(#"{"flag":true,"n":1,"string":"1","list":[null,"日本語"]}"#)
        XCTAssertEqual(try V4vJSON.parse(original.encoded()), original)
        XCTAssertThrowsError(try original.object(required: ["flag", "n"]))
        XCTAssertEqual(original.description, "V4V JSON (redacted)")
    }
    func testNoImplicitConnectorProfileOrInsecureOrigin() throws {
        for api in ["http://example.invalid", "https://user:secret@example.invalid", "https://example.invalid/api", "https://example.invalid?secret=1", "https://example.invalid/#x", "https://example.invalid\n"] {
            XCTAssertThrowsError(try V4vConnectorProfile(apiOrigin: api, playbackOrigin: "https://media.example.invalid", playbackPrefix: "/"))
        }
        XCTAssertThrowsError(try V4vConnectorProfile(apiOrigin: "https://example.invalid", playbackOrigin: "https://media.example.invalid", playbackPrefix: "/../"))
        let profile = try V4vConnectorProfile(apiOrigin: "https://example.invalid/", playbackOrigin: "https://media.example.invalid", playbackPrefix: "/origin/")
        XCTAssertEqual(profile.apiOrigin, "https://example.invalid")
    }
    func testDirectConstructorsRejectFinalNewlineAndNoncanonicalNumericValues() throws {
        XCTAssertThrowsError(try V4vIdentity.event("event-foundation\n"))
        XCTAssertThrowsError(try V4vIdentity.id(String(repeating: "a", count: 32) + "\n"))
        XCTAssertThrowsError(try V4vJSON.number("1\n").uint())
        XCTAssertThrowsError(try V4vJSON.number("1\n").encoded())
        XCTAssertNoThrow(try V4vIdentity.id(String(repeating: "a", count: 32)))
    }
}
