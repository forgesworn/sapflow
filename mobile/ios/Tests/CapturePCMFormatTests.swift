import XCTest
import AVFoundation
@testable import SapflowBroadcaster

final class CapturePCMFormatTests: XCTestCase {
    private func description(rate: Double = 48_000, channels: AVAudioChannelCount = 1, interleaved: Bool = false) throws -> CMAudioFormatDescription {
        let format = try XCTUnwrap(AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1, interleaved: interleaved))
        var asbd = format.streamDescription.pointee
        // CoreMedia metadata fixture, including values AVAudioFormat itself
        // may refuse before our production bound can be exercised.
        asbd.mSampleRate = rate; asbd.mChannelsPerFrame = channels
        if interleaved { asbd.mBytesPerFrame = channels * 4; asbd.mBytesPerPacket = channels * 4 }
        var result: CMAudioFormatDescription?
        XCTAssertEqual(CMAudioFormatDescriptionCreate(allocator: kCFAllocatorDefault, asbd: &asbd, layoutSize: 0,
            layout: nil, magicCookieSize: 0, magicCookie: nil, extensions: nil, formatDescriptionOut: &result), noErr)
        return try XCTUnwrap(result)
    }
    func testExactNativePCMMetadataMatchesWithoutReadingPayload() throws {
        let a = try description()
        XCTAssertTrue(try CapturePCMFormat(a).matches(description()))
    }
    func testRateChannelsAndEvenMonoInterleavingRemainExact() throws {
        let original = try CapturePCMFormat(description())
        XCTAssertFalse(try original.matches(description(rate: 44_100)))
        XCTAssertFalse(try original.matches(description(channels: 2)))
        XCTAssertFalse(try original.matches(description(interleaved: true)))
    }
    func testUnsupportedPCMInventoryIsBounded() throws {
        XCTAssertThrowsError(try CapturePCMFormat(description(channels: 9)))
        XCTAssertThrowsError(try CapturePCMFormat(description(rate: 384_000)))
    }
}
