import XCTest
@testable import SapflowBroadcaster

final class V4vCreatorConfigurationTests: XCTestCase {
    private func info(enabled: String = "YES") -> [String: Any] {
        ["SapflowV4vEnabled": enabled, "SapflowV4vApiOrigin": "https://creator.example.invalid", "SapflowV4vPlaybackOrigin": "https://media.example.invalid", "SapflowV4vPlaybackPrefix": "/origin/live/"]
    }
    private func disabled() -> [String: Any] {
        ["SapflowV4vEnabled": "NO", "SapflowV4vApiOrigin": "", "SapflowV4vPlaybackOrigin": "", "SapflowV4vPlaybackPrefix": ""]
    }
    func testExplicitDisabledDefaultDoesNotRequireSignerOrEnableAProfile() {
        let result = V4vCreatorConfiguration.evaluate(info: disabled(), nativeSignerAvailable: false)
        XCTAssertNil(result.profile); XCTAssertEqual(result.unavailableReason, .disabled)
        XCTAssertEqual(result.message, "V4V sign-in is unavailable in this build.")
    }
    func testDisabledProfileRejectsQualificationLeftoversInsteadOfGuessingDefaults() {
        for key in ["SapflowV4vApiOrigin", "SapflowV4vPlaybackOrigin", "SapflowV4vPlaybackPrefix"] {
            var partial = disabled(); partial[key] = info()[key]
            let result = V4vCreatorConfiguration.evaluate(info: partial, nativeSignerAvailable: true)
            XCTAssertNil(result.profile); XCTAssertEqual(result.unavailableReason, .incompleteProfile)
        }
    }
    func testApprovalMustBeAnExactBuildString() {
        for value: Any in [true, 1, "true", "yes", "YES ", "", "$(SAPFLOW_V4V_ENABLED)"] {
            var invalid = info(); invalid["SapflowV4vEnabled"] = value
            XCTAssertEqual(V4vCreatorConfiguration.evaluate(info: invalid, nativeSignerAvailable: true).unavailableReason, .invalidApproval)
        }
        XCTAssertEqual(V4vCreatorConfiguration.evaluate(info: [:], nativeSignerAvailable: true).unavailableReason, .invalidApproval)
    }
    func testEnabledRequiresEveryExactNonemptyProfileField() {
        for key in ["SapflowV4vApiOrigin", "SapflowV4vPlaybackOrigin", "SapflowV4vPlaybackPrefix"] {
            for value: Any? in [nil, "", 1, NSNull()] {
                var invalid = info(); invalid[key] = value
                let result = V4vCreatorConfiguration.evaluate(info: invalid, nativeSignerAvailable: true)
                XCTAssertNil(result.profile); XCTAssertEqual(result.unavailableReason, .incompleteProfile)
            }
        }
    }
    func testValidatedProfileStillRequiresActualNativeSignerModule() {
        let result = V4vCreatorConfiguration.evaluate(info: info(), nativeSignerAvailable: false)
        XCTAssertNil(result.profile); XCTAssertEqual(result.unavailableReason, .nativeSignerUnavailable)
    }
    func testReviewedDualOriginsAndFullPlaybackPrefixArePreserved() throws {
        let result = V4vCreatorConfiguration.evaluate(info: info(), nativeSignerAvailable: true)
        XCTAssertNil(result.unavailableReason)
        let profile = try XCTUnwrap(result.profile)
        XCTAssertEqual(profile.apiOrigin, "https://creator.example.invalid")
        XCTAssertEqual(profile.playbackOrigin, "https://media.example.invalid")
        XCTAssertEqual(profile.playbackPrefix, "/origin/live/")
    }
    func testBothOriginsUseTheExistingStrictHTTPSProfileValidator() {
        for key in ["SapflowV4vApiOrigin", "SapflowV4vPlaybackOrigin"] {
            for value in ["http://site.example.invalid", "https://user:password@site.example.invalid", "https://site.example.invalid/api", "https://site.example.invalid?q=x", "https://site.example.invalid#x", " https://site.example.invalid", "$(UNSET)"] {
                var invalid = info(); invalid[key] = value
                XCTAssertEqual(V4vCreatorConfiguration.evaluate(info: invalid, nativeSignerAvailable: true).unavailableReason, .invalidProfile)
            }
        }
    }
    func testPlaybackPrefixCannotBeAnAuthorityOrTraversal() {
        for value in ["live/", "//other/", "/live/../", "/live/%2e/", "https://site.example.invalid/", "/live", "/live/ "] {
            var invalid = info(); invalid["SapflowV4vPlaybackPrefix"] = value
            XCTAssertEqual(V4vCreatorConfiguration.evaluate(info: invalid, nativeSignerAvailable: true).unavailableReason, .invalidProfile)
        }
    }
    func testManualAndReceivedMetadataNeverEnableTheConnector() {
        var unrelated = disabled(); unrelated["endpoint"] = "https://creator.example.invalid"
        unrelated["profileName"] = "V4V"; unrelated["playbackUrl"] = "https://media.example.invalid/live/"
        XCTAssertEqual(V4vCreatorConfiguration.evaluate(info: unrelated, nativeSignerAvailable: true).unavailableReason, .disabled)
    }
    func testDisabledConnectorLeavesManualDestinationConstructionIndependent() throws {
        XCTAssertNil(V4vCreatorConfiguration.evaluate(info: disabled(), nativeSignerAvailable: false).profile)
        let manual = try ManualDestination(endpoint: "rtmps://manual.example.invalid/live", streamKey: "generated-test-key", allowPlaintext: false)
        XCTAssertTrue(manual.encrypted); XCTAssertEqual(manual.app, "live")
    }
    func testConfigurationDiagnosticsDoNotPrintRoutingOrInjectedValues() {
        for result in [V4vCreatorConfiguration.evaluate(info: info(), nativeSignerAvailable: true), V4vCreatorConfiguration.evaluate(info: info(enabled: "NO"), nativeSignerAvailable: true)] {
            XCTAssertFalse(String(describing: result).contains("example.invalid"))
            XCTAssertFalse(String(reflecting: result).contains("origin/live"))
            XCTAssertFalse(result.message.contains("https"))
        }
    }
    func testInfoTemplateBindsOnlyTheFourExplicitBuildSettings() throws {
        let root = try XCTUnwrap(ProcessInfo.processInfo.environment["SAPFLOW_CONFIG_TEST_ROOT"])
        let bytes = try Data(contentsOf: URL(fileURLWithPath: root).appendingPathComponent("mobile/ios/Info.plist"))
        let plist = try XCTUnwrap(PropertyListSerialization.propertyList(from: bytes, format: nil) as? [String: Any])
        for (key, setting) in [("SapflowV4vEnabled", "ENABLED"), ("SapflowV4vApiOrigin", "API_ORIGIN"), ("SapflowV4vPlaybackOrigin", "PLAYBACK_ORIGIN"), ("SapflowV4vPlaybackPrefix", "PLAYBACK_PREFIX")] {
            XCTAssertEqual(plist[key] as? String, "$(SAPFLOW_V4V_\(setting))")
        }
    }
    #if os(macOS)
    func testQualificationOverridesCannotConfigureDefaultDebugOrRelease() throws {
        let root = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["SAPFLOW_CONFIG_TEST_ROOT"]))
        let process = Process(); process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["xcodegen", "dump", "--no-env", "--type", "parsed-json", "--spec", root.appendingPathComponent("mobile/ios/project.yml").path]
        let output = Pipe(); process.standardOutput = output
        try process.run()
        let bytes = output.fileHandleForReading.readDataToEndOfFile(); process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0); XCTAssertLessThan(bytes.count, 65_536)
        let spec = try XCTUnwrap(JSONSerialization.jsonObject(with: bytes) as? [String: Any])
        let settings = try XCTUnwrap(spec["settings"] as? [String: Any])
        let base = try XCTUnwrap(settings["base"] as? [String: String])
        XCTAssertEqual(base["SAPFLOW_V4V_ENABLED"], "NO")
        let configs = try XCTUnwrap(settings["configs"] as? [String: [String: String]])
        XCTAssertNil(configs["Debug"]); XCTAssertNil(configs["Release"])
        let qualification = try XCTUnwrap(configs["Qualification"])
        for field in ["ENABLED", "API_ORIGIN", "PLAYBACK_ORIGIN", "PLAYBACK_PREFIX"] {
            XCTAssertEqual(qualification["SAPFLOW_V4V_" + field], "$(SAPFLOW_QUALIFICATION_V4V_" + field + ")")
            XCTAssertEqual(base["SAPFLOW_QUALIFICATION_V4V_" + field], field == "ENABLED" ? "NO" : "")
            if field != "ENABLED" { XCTAssertEqual(base["SAPFLOW_V4V_" + field], "") }
        }
    }
    func testBuiltInfoVerifierRejectsMalformedPartialAndWrongRoutesEvenWithPythonOptimized() throws {
        let root = URL(fileURLWithPath: try XCTUnwrap(ProcessInfo.processInfo.environment["SAPFLOW_CONFIG_TEST_ROOT"]))
        let scratch = root.appendingPathComponent("target/config-verifier-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: scratch) }
        let path = scratch.appendingPathComponent("Info.plist")
        func encoded(_ object: Any) throws -> Data { try PropertyListSerialization.data(fromPropertyList: object, format: .xml, options: 0) }
        var nonString = disabled(); nonString["SapflowV4vEnabled"] = false
        let expected = ["https://creator.example.invalid", "https://media.example.invalid", "/origin/live/"]
        let cases: [(Data, [String], Int32)] = [
            (try encoded(disabled()), ["disabled"], 0),
            (try encoded(info()), ["enabled"] + expected, 0),
            (try encoded(info()), ["disabled"], 1),
            (try encoded(info()), ["enabled", "https://other.example.invalid"] + Array(expected.dropFirst()), 1),
            (try encoded(nonString), ["disabled"], 1),
            (try encoded([String]()), ["disabled"], 1),
            (Data(repeating: 0x78, count: 65_537), ["disabled"], 1),
            (Data("not a plist".utf8), ["disabled"], 1)
        ]
        for (bytes, arguments, expectedExit) in cases {
            try bytes.write(to: path)
            let process = Process(); process.executableURL = URL(fileURLWithPath: "/bin/bash")
            process.arguments = [root.appendingPathComponent("mobile/ios/test-config.sh").path, "--verify-info", arguments[0], path.path] + Array(arguments.dropFirst())
            var environment = ProcessInfo.processInfo.environment; environment["PYTHONOPTIMIZE"] = "1"; process.environment = environment
            let output = Pipe(); process.standardOutput = output; process.standardError = output
            try process.run(); process.waitUntilExit()
            XCTAssertEqual(process.terminationStatus, expectedExit)
            let result = output.fileHandleForReading.readDataToEndOfFile()
            XCTAssertLessThan(result.count, 1024)
            XCTAssertFalse(String(decoding: result, as: UTF8.self).contains("example.invalid"))
        }
    }
    #endif

}
