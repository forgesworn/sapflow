import Foundation
import XCTest
@testable import SapflowBroadcaster

final class QuickTimeAudioPrimingTests: XCTestCase {
    typealias Priming = QuickTimeAudioPriming

    static func be(_ n: UInt64, _ width: Int = 4) -> Data {
        Data((0..<width).reversed().map { UInt8(truncatingIfNeeded: n >> ($0 * 8)) })
    }
    static func box(_ name: String, _ body: Data, wide: Bool = false) -> Data {
        (wide ? be(1) + Data(name.utf8) + be(UInt64(body.count + 16), 8) : be(UInt64(body.count + 8)) + Data(name.utf8)) + body
    }
    static func clock(_ scale: UInt32, _ duration: UInt64, version: UInt8) -> Data {
        var data = Data([version, 0, 0, 0]) + Data(repeating: 0, count: version == 0 ? 8 : 16)
            + be(UInt64(scale)) + be(duration, version == 0 ? 4 : 8)
        data += Data(repeating: 0, count: (version == 0 ? 24 : 36) - data.count)
        return data
    }
    static func full(_ value: Data) -> Data { Data(repeating: 0, count: 4) + value }
    static func descriptor(_ tag: UInt8, _ body: Data) -> Data { Data([tag, UInt8(body.count)]) + body }
    static func track(id: UInt32, audio: Bool, version: UInt8 = 0, wide: Bool = false, units: UInt64 = 97,
                      valid: UInt64 = 96356, mediaTime: UInt64 = 0, asc: UInt16 = 0x1188,
                      existingGroups: Data = Data(), unknown: Data = Data(), empty: UInt64 = 0) -> Data {
        var trackHeader = Data(repeating: 0, count: version == 0 ? 84 : 96)
        trackHeader[0] = version
        let idAt = version == 0 ? 12 : 20, durationAt = version == 0 ? 20 : 28
        trackHeader.replaceSubrange(idAt..<(idAt + 4), with: be(UInt64(id)))
        trackHeader.replaceSubrange(durationAt..<(durationAt + (version == 0 ? 4 : 8)), with: be(valid + empty, version == 0 ? 4 : 8))
        let width = version == 0 ? 4 : 8
        var edit = Data([version, 0, 0, 0]) + be(empty == 0 ? 1 : 2)
        if empty > 0 { edit += be(empty, width) + be(width == 4 ? UInt64(UInt32.max) : UInt64.max, width) + be(65536) }
        edit += be(valid, width) + be(mediaTime, width) + be(65536)
        let handler = full(be(0) + Data((audio ? "soun" : "vide").utf8))
        let reference = box("dinf", box("dref", full(be(1) + box("alis", be(1)))))
        let config = descriptor(3, Data([0, 0, 0]) + descriptor(4, Data([0x40, 0x14]) + Data(repeating: 0, count: 11)
            + descriptor(5, be(UInt64(asc), 2))) + descriptor(6, Data([2])))
        var entry = Data(repeating: 0, count: 44)
        entry.replaceSubrange(6..<8, with: be(1, 2))
        entry.replaceSubrange(8..<10, with: be(1, 2))
        entry.replaceSubrange(16..<18, with: be(asc == 0x1190 ? 2 : 1, 2))
        entry.replaceSubrange(24..<28, with: be(48_000 << 16))
        entry.replaceSubrange(28..<32, with: be(1024))
        entry += box("wave", box("frma", Data("mp4a".utf8)) + box("esds", full(config)) + box("\0\0\0\0", Data()))
        let description = box("stsd", full(be(1) + box("mp4a", entry)))
        let tables = description + box("stsz", full(be(7) + be(units)))
            + box("stts", full(be(1) + be(units) + be(1024)))
            + box("stsc", full(be(1) + be(1) + be(units) + be(1)))
            + box("stco", full(be(1) + be(4096))) + existingGroups + unknown
        let media = box("mdia", box("mdhd", clock(48000, units * 1024, version: version)) + box("hdlr", handler)
            + box("minf", reference + box("stbl", tables, wide: wide), wide: wide), wide: wide)
        return box("trak", box("tkhd", trackHeader) + box("edts", box("elst", edit)) + media, wide: wide)
    }
    static func header(version: UInt8 = 0, wide: Bool = false, video: Bool = true, units: UInt64 = 97,
                       valid: UInt64 = 96356, mediaTime: UInt64 = 0, asc: UInt16 = 0x1188,
                       groups: Data = Data(), unknown: Data = Data(), empty: UInt64 = 0) -> Data {
        var movieClock = clock(48000, 100800, version: version)
        movieClock += Data(repeating: 0, count: (version == 0 ? 100 : 112) - movieClock.count)
        return box("moov", box("mvhd", movieClock)
            + (video ? track(id: 1, audio: false) : Data())
            + track(id: 2, audio: true, version: version, wide: wide, units: units, valid: valid, mediaTime: mediaTime,
                    asc: asc, existingGroups: groups, unknown: unknown, empty: empty), wide: wide)
    }
    static let facts = Priming.Facts(audioTrackID: 2, accessUnits: 97, leadingFrames: 2112, validFrames: 96356)
    private func prepare(_ data: Data, _ facts: Priming.Facts = facts) throws -> Priming.Prepared {
        try Priming.prepare(moov: data, facts: facts)
    }
    private func altered(_ data: Data, marker: String, offset: Int, value: UInt64, width: Int = 4) throws -> Data {
        var copy = data
        let range = try XCTUnwrap(copy.range(of: Data(marker.utf8), options: .backwards))
        let at = range.lowerBound + offset
        copy.replaceSubrange(at..<(at + width), with: Self.be(value, width))
        return copy
    }
    func testCleanAVProducesCompleteGroupsAndMeasuredFacts() throws {
        let original = Self.header(), result = try prepare(original)
        XCTAssertEqual(result.moov.count, original.count + 54)
        XCTAssertEqual(result.audioTrackID, 2); XCTAssertEqual(result.trailingFrames, 860)
        let sgpd = Self.box("sgpd", Data([1, 0, 0, 0]) + Data("roll".utf8) + Self.be(2) + Self.be(1) + Data([255, 255]))
        let sbgp = Self.box("sbgp", Self.full(Data("roll".utf8) + Self.be(1) + Self.be(97) + Self.be(1)))
        XCTAssertEqual(sgpd.count, 26); XCTAssertEqual(sbgp.count, 28)
        XCTAssertNotNil(result.moov.range(of: sgpd + sbgp))
        let edit = try XCTUnwrap(result.moov.range(of: Data("elst".utf8), options: .backwards))
        XCTAssertEqual(result.moov.subdata(in: (edit.lowerBound + 12)..<(edit.lowerBound + 24)), Self.be(96356) + Self.be(2112) + Self.be(65536))
    }
    func testIncompleteRetainsEntirePostLeadingCodedPrefix() throws {
        let facts = Priming.Facts(accessUnits: 97, leadingFrames: 2112, validFrames: 97216)
        let result = try prepare(Self.header(valid: 97216), facts)
        XCTAssertEqual(result.validFrames, 97216); XCTAssertEqual(result.trailingFrames, 0)
    }
    func testAudioOnlyAndCanonicalStereoAreAccepted() throws {
        XCTAssertEqual(try prepare(Self.header(video: false)).accessUnits, 97)
        XCTAssertEqual(try prepare(Self.header(video: false, asc: 0x1190)).audioTrackID, 2)
    }
    func testVersionOneEditClocksAndExtendedAncestorSizes() throws {
        let original = Self.header(version: 1, wide: true), result = try prepare(original)
        XCTAssertEqual(result.moov.prefix(4), Self.be(1)); XCTAssertEqual(result.moov.count, original.count + 54)
        let edit = try XCTUnwrap(result.moov.range(of: Data("elst".utf8), options: .backwards))
        XCTAssertEqual(result.moov.subdata(in: (edit.lowerBound + 12)..<(edit.lowerBound + 28)), Self.be(96356, 8) + Self.be(2112, 8))
    }
    func testVideoUnknownAtomsAndChunkOffsetsAreByteExact() throws {
        let video = Self.track(id: 1, audio: false)
        let unknown = Self.box("uuid", Data(repeating: 0x91, count: 31)) + Self.box("zzzz", Data([0, 1, 2, 3]))
        let original = Self.header(unknown: unknown), result = try prepare(original)
        XCTAssertNotNil(result.moov.range(of: video)); XCTAssertNotNil(result.moov.range(of: unknown))
        let chunk = Self.box("stco", Self.full(Self.be(1) + Self.be(4096)))
        XCTAssertEqual(result.moov.ranges(of: chunk).count, original.ranges(of: chunk).count)
    }
    func testRejectsEveryTruncationOfValidHeader() {
        let original = Self.header(video: false)
        for count in 0..<original.count { XCTAssertThrowsError(try prepare(original.prefix(count))) }
    }
    func testRejectsOversizedOpenEndedAndOverflowBoxes() {
        XCTAssertThrowsError(try prepare(Data(repeating: 0, count: Priming.maximumHeaderBytes + 1)))
        XCTAssertThrowsError(try prepare(Self.be(0) + Data("moov".utf8)))
        XCTAssertThrowsError(try prepare(Self.be(1) + Data("moov".utf8) + Self.be(UInt64.max, 8)))
    }
    func testRejectsMediaAndFragmentLayoutsInsideInput() {
        for kind in ["mdat", "moof", "mvex", "traf", "mfra", "sidx"] {
            XCTAssertThrowsError(try prepare(Self.box("moov", Self.box(kind, Data()))))
        }
        XCTAssertThrowsError(try prepare(Self.header() + Self.box("mdat", Data(), wide: true)))
    }
    func testRejectsAmbiguousDuplicateAndWrongTrackIdentity() throws {
        let duplicate = Self.box("moov", Self.box("mvhd", Self.clock(48000, 100800, version: 0))
            + Self.track(id: 2, audio: true) + Self.track(id: 3, audio: true))
        XCTAssertThrowsError(try prepare(duplicate))
        XCTAssertThrowsError(try prepare(Self.header(), .init(audioTrackID: 1, accessUnits: 97, leadingFrames: 2112, validFrames: 96356)))
        let sameIDs = try altered(Self.header(), marker: "tkhd", offset: 16, value: 1)
        XCTAssertThrowsError(try prepare(sameIDs))
    }
    func testRejectsExistingGroupsAndDoubleApply() throws {
        let result = try prepare(Self.header())
        XCTAssertThrowsError(try prepare(result.moov))
        let group = Self.box("sbgp", Self.full(Data("roll".utf8) + Self.be(1) + Self.be(97) + Self.be(1)))
        XCTAssertThrowsError(try prepare(Self.header(groups: group)))
        XCTAssertThrowsError(try prepare(Self.header(mediaTime: 2112)))
    }
    func testRejectsFactsAndTimingContradictions() throws {
        for facts in [Priming.Facts(accessUnits: 0, leadingFrames: 1, validFrames: 1),
                      .init(accessUnits: UInt64.max, leadingFrames: 1, validFrames: 1),
                      .init(accessUnits: 97, leadingFrames: 0, validFrames: 96356),
                      .init(accessUnits: 97, leadingFrames: 2112, validFrames: 97217),
                      .init(accessUnits: 96, leadingFrames: 2112, validFrames: 96356)] {
            XCTAssertThrowsError(try prepare(Self.header(), facts))
        }
        XCTAssertThrowsError(try prepare(Self.header(valid: 96355)))
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "stts", offset: 16, value: 960)))
    }
    func testRejectsWrongAACRateChannelsAndFrameGrammar() throws {
        for asc: UInt16 in [0x118c, 0x118a, 0x1210, 0x1198] { XCTAssertThrowsError(try prepare(Self.header(asc: asc))) }
        var wrongRate = Self.header(video: false)
        let entry = try XCTUnwrap(wrongRate.range(of: Data("mp4a".utf8))).lowerBound
        wrongRate.replaceSubrange((entry + 28)..<(entry + 32), with: Self.be(44100 << 16))
        XCTAssertThrowsError(try prepare(wrongRate))
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "mdhd", offset: 16, value: 44100)))
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "esds", offset: 4, value: 0x01000000)))
    }
    func testRejectsExternalDataReferencesAndSampleIndex() throws {
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "alis", offset: 4, value: 0)))
        // Last mp4a text occurs in frma; select the actual sample-entry type.
        var original = Self.header(video: false)
        let entry = try XCTUnwrap(original.range(of: Data("mp4a".utf8)))
        original.replaceSubrange((entry.lowerBound + 10)..<(entry.lowerBound + 12), with: Self.be(2, 2))
        XCTAssertThrowsError(try prepare(original))
    }
    func testRejectsMalformedSampleSizeChunkAndDescriptorCounts() throws {
        for (marker, offset, value) in [("stsz", 12, UInt64.max & 0xffff_ffff), ("stco", 8, 262145),
                                       ("stsc", 20, 2), ("stsd", 8, 2), ("elst", 8, 2)] {
            XCTAssertThrowsError(try prepare(altered(Self.header(), marker: marker, offset: offset, value: value)))
        }
        var malformed = Self.header(video: false)
        let at = try XCTUnwrap(malformed.range(of: Data("esds".utf8))).lowerBound + 9
        malformed[at] = 0xff
        XCTAssertThrowsError(try prepare(malformed))
    }
    func testRejectsExcessiveBoxCountsAndNesting() {
        let many = (0..<4097).reduce(into: Data()) { data, _ in data += Self.box("zzzz", Data()) }
        XCTAssertThrowsError(try prepare(Self.box("moov", many)))
        var nested = Data()
        for _ in 0..<14 { nested = Self.box("moov", nested) }
        XCTAssertThrowsError(try prepare(nested))
    }
    func testInputValueAndSliceRemainUnmodified() throws {
        let original = Self.header(), copy = original
        var padded = Data([9, 9, 9]); padded.append(original)
        let result = try prepare(padded.dropFirst(3))
        XCTAssertEqual(original, copy); XCTAssertEqual(result.moov, try prepare(original).moov)
    }
    func testRejectsContradictoryTrackDurationAndFlags() throws {
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "tkhd", offset: 24, value: 96355)))
        XCTAssertThrowsError(try prepare(altered(Self.header(), marker: "mdhd", offset: 4, value: 1)))
    }
    func testLeadingEmptyEditRemainsByteExactForBothVersions() throws {
        for version: UInt8 in [0, 1] {
            let original = Self.header(version: version, empty: 2048), result = try prepare(original)
            let at = try XCTUnwrap(original.range(of: Data("elst".utf8), options: .backwards)).lowerBound + 12
            let stride = version == 0 ? 12 : 20
            XCTAssertEqual(original.subdata(in: at..<(at + stride)), result.moov.subdata(in: at..<(at + stride)))
            XCTAssertEqual(result.emptyEditDuration, 2048); XCTAssertEqual(result.validFrames, 96356)
            XCTAssertEqual(result.trailingFrames, 860)
        }
    }
    func testRejectsOtherEditSequencesAndGapDurationContradictions() throws {
        let original = Self.header(empty: 2048)
        XCTAssertThrowsError(try prepare(altered(original, marker: "elst", offset: 16, value: 0)))
        XCTAssertThrowsError(try prepare(altered(original, marker: "elst", offset: 12, value: 0)))
        XCTAssertThrowsError(try prepare(altered(original, marker: "elst", offset: 20, value: 0)))
        XCTAssertThrowsError(try prepare(altered(original, marker: "elst", offset: 8, value: 3)))
        XCTAssertThrowsError(try prepare(altered(original, marker: "tkhd", offset: 24, value: 96356)))
        XCTAssertThrowsError(try prepare(altered(Self.header(version: 1, empty: 2048), marker: "elst", offset: 12, value: UInt64.max, width: 8)))
    }
    func testRejectsExternalAndDependentESDescriptors() throws {
        for flag: UInt8 in [32, 64, 128] {
            var header = Self.header(video: false)
            let at = try XCTUnwrap(header.range(of: Data("esds".utf8))).lowerBound
            header[at + 12] = flag
            XCTAssertThrowsError(try prepare(header))
        }
    }

    /// Synthetic native audio-only shape: measured movie/track N and no edts.
    /// Existing fixture builders and all twenty previous test bodies stay exact.
    static func audioOnlyHeader(version: UInt8 = 0, wide: Bool = false, asc: UInt16 = 0x1188,
                                unknown: Data = Data()) -> Data {
        var data = header(version: version, wide: wide, video: false, asc: asc, unknown: unknown)
        let marker = data.range(of: Data("edts".utf8))!.lowerBound
        let start = marker - 4
        let length = data[start..<marker].reduce(0) { ($0 << 8) | Int($1) }
        let track = data.range(of: Data("trak".utf8))!.lowerBound - 4
        data.removeSubrange(start..<(start + length))
        for at in [0, track] {
            let width = wide ? 8 : 4, field = at + (wide ? 8 : 0)
            let size = data[field..<(field + width)].reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
            data.replaceSubrange(field..<(field + width), with: be(size - UInt64(length), width))
        }
        let clock = data.range(of: Data("mvhd".utf8))!.lowerBound
        let at = clock + (version == 0 ? 20 : 28), width = version == 0 ? 4 : 8
        data.replaceSubrange(at..<(at + width), with: be(facts.validFrames, width))
        return data
    }

    func testAudioOnlyMissingEditProducesMeasuredEditAndRoll() throws {
        let original = Self.audioOnlyHeader()
        let result = try Priming.prepareAudioOnly(moov: original, facts: Self.facts)
        XCTAssertEqual(result.moov.count, original.count + 90)
        XCTAssertEqual(result.audioTrackID, 2); XCTAssertEqual(result.validFrames, 96356)
        XCTAssertEqual(result.leadingFrames, 2112); XCTAssertEqual(result.trailingFrames, 860)
        XCTAssertEqual(result.emptyEditDuration, 0)
        let edit = Self.box("edts", Self.box("elst", Self.full(Self.be(1) + Self.be(96356) + Self.be(2112) + Self.be(65536))))
        XCTAssertNotNil(result.moov.range(of: edit))
        XCTAssertEqual(result.moov.ranges(of: Data("sgpd".utf8)).count, 1)
        XCTAssertEqual(result.moov.ranges(of: Data("sbgp".utf8)).count, 1)
    }

    func testAudioOnlyPreservesExistingClocksSamplesOffsetsAndUnknownMetadata() throws {
        let unknown = Self.box("uuid", Data(repeating: 0x91, count: 31)) + Self.box("zzzz", Data([0, 1, 2, 3]))
        let original = Self.audioOnlyHeader(unknown: unknown)
        let result = try Priming.prepareAudioOnly(moov: original, facts: Self.facts)
        for kind in ["mvhd", "tkhd", "mdhd", "stts", "stsz", "stco"] {
            let marker = try XCTUnwrap(original.range(of: Data(kind.utf8))).lowerBound
            let start = marker - 4, size = original[start..<marker].reduce(0) { ($0 << 8) | Int($1) }
            XCTAssertNotNil(result.moov.range(of: original.subdata(in: start..<(start + size))))
        }
        XCTAssertNotNil(result.moov.range(of: unknown))
    }

    func testAudioOnlyRefusesExistingEditAndDoubleApply() throws {
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: Self.header(video: false), facts: Self.facts))
        let first = try Priming.prepareAudioOnly(moov: Self.audioOnlyHeader(), facts: Self.facts)
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: first.moov, facts: Self.facts))
    }

    func testAudioOnlyRefusesVideoAndAdditionalTrack() throws {
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: Self.header(), facts: Self.facts))
        let only = Self.audioOnlyHeader()
        let extra = Self.box("moov", Data(only.dropFirst(8)) + Self.track(id: 3, audio: true))
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: extra, facts: Self.facts))
        let wrong = Self.box("moov", Data(only.dropFirst(8)) + Self.track(id: 3, audio: false))
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: wrong, facts: Self.facts))
    }

    func testAudioOnlyRefusesWrongMovieTrackDurationAndScale() throws {
        let original = Self.audioOnlyHeader()
        for (marker, offset, value) in [("mvhd", 20, UInt64(96355)), ("tkhd", 24, 96355), ("mvhd", 16, 44100)] {
            XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: altered(original, marker: marker, offset: offset, value: value), facts: Self.facts))
        }
    }

    func testAudioOnlyRefusesMissingOrUnrepresentableFacts() throws {
        let original = Self.audioOnlyHeader()
        for facts in [Priming.Facts(accessUnits: 0, leadingFrames: 2112, validFrames: 96356),
                      .init(accessUnits: 97, leadingFrames: 0, validFrames: 96356),
                      .init(accessUnits: 97, leadingFrames: 2112, validFrames: 0),
                      .init(accessUnits: 4_000_000, leadingFrames: UInt32(Int32.max) + 1, validFrames: 1),
                      .init(accessUnits: 97, leadingFrames: 2112, validFrames: UInt64(UInt32.max) + 1)] {
            XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: original, facts: facts))
        }
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: original, facts: .init(audioTrackID: 3, accessUnits: 97, leadingFrames: 2112, validFrames: 96356)))
    }

    func testAudioOnlyStillValidatesAACDescriptionsAndSampleTables() throws {
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: Self.audioOnlyHeader(asc: 0x1210), facts: Self.facts))
        for (marker, offset, value) in [("stts", 16, UInt64(960)), ("stsz", 12, 96), ("alis", 4, 0)] {
            XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: altered(Self.audioOnlyHeader(), marker: marker, offset: offset, value: value), facts: Self.facts))
        }
    }

    func testAudioOnlyVersionOneWideAncestorsAndSliceRemainSupported() throws {
        let original = Self.audioOnlyHeader(version: 1, wide: true, asc: 0x1190)
        var padded = Data([9, 9, 9]); padded.append(original)
        let result = try Priming.prepareAudioOnly(moov: padded.dropFirst(3), facts: Self.facts)
        XCTAssertEqual(result.moov.count, original.count + 90); XCTAssertEqual(result.moov.prefix(4), Self.be(1))
        XCTAssertEqual(result.moov, try Priming.prepareAudioOnly(moov: original, facts: Self.facts).moov)
        XCTAssertEqual(original, Self.audioOnlyHeader(version: 1, wide: true, asc: 0x1190))
    }

    func testAudioOnlyRefusesEveryTruncationAndOversizedHeader() {
        let original = Self.audioOnlyHeader()
        for count in 0..<original.count { XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: original.prefix(count), facts: Self.facts)) }
        XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: Data(repeating: 0, count: Priming.maximumHeaderBytes), facts: Self.facts))
    }

    /// Native M4A grammar: 28-byte v0 mp4a, default channels2, direct esds with
    /// native four-byte descriptor lengths and mono ASC. No v1 extension/wave.
    static func nativeV0AudioOnlyHeader(channels: UInt16 = 2, asc: UInt16 = 0x1188,
                                        sampleRate: UInt32 = 48000, entryVersion: UInt16 = 0,
                                        neutralEdit: Bool = false, wrappedESDS: Bool = false) -> Data {
        func nativeDescriptor(_ tag: UInt8, _ body: Data) -> Data {
            Data([tag, 0x80, 0x80, 0x80, UInt8(body.count)]) + body
        }
        let specific = nativeDescriptor(5, be(UInt64(asc), 2))
        let decoder = nativeDescriptor(4, Data([0x40, 0x14, 0, 0x18, 0, 0, 0, 0, 0, 0, 1, 0x77, 0]) + specific)
        let esds = full(nativeDescriptor(3, Data([0, 0, 0]) + decoder + nativeDescriptor(6, Data([2]))))
        var entry = Data(repeating: 0, count: 28)
        entry.replaceSubrange(6..<8, with: be(1, 2))
        entry.replaceSubrange(8..<10, with: be(UInt64(entryVersion), 2))
        entry.replaceSubrange(16..<18, with: be(UInt64(channels), 2))
        entry.replaceSubrange(18..<20, with: be(16, 2))
        entry.replaceSubrange(24..<28, with: be(UInt64(sampleRate) << 16))
        if entryVersion == 1 { entry += be(1024) + Data(repeating: 0, count: 12) }
        entry += wrappedESDS ? box("wave", box("esds", esds)) : box("esds", esds)
        var tables = box("stsd", full(be(1) + box("mp4a", entry)))
        tables += box("stsz", full(be(7) + be(97))) + box("stts", full(be(1) + be(97) + be(1024)))
        tables += box("stsc", full(be(1) + be(1) + be(97) + be(1))) + box("stco", full(be(1) + be(4096)))
        let references = box("dinf", box("dref", full(be(1) + box("url ", be(1)))))
        let media = box("mdia", box("mdhd", clock(48000, 97 * 1024, version: 0))
            + box("hdlr", full(be(0) + Data("soun".utf8) + Data(repeating: 0, count: 12)))
            + box("minf", references + box("stbl", tables)))
        var trackHeader = Data(repeating: 0, count: 84)
        trackHeader.replaceSubrange(12..<16, with: be(2))
        trackHeader.replaceSubrange(20..<24, with: be(facts.validFrames))
        var movieClock = clock(48000, facts.validFrames, version: 0)
        movieClock += Data(repeating: 0, count: 100 - movieClock.count)
        let edit = neutralEdit ? box("edts", box("elst", full(be(1) + be(facts.validFrames) + be(0) + be(65536)))) : Data()
        return box("moov", box("mvhd", movieClock) + box("trak", box("tkhd", trackHeader) + edit + media))
    }

    func testNativeV0DefaultChannelCountMonoIsOnlyAdmittedByAudioOnlyEntry() throws {
        let original = Self.nativeV0AudioOnlyHeader()
        let result = try Priming.prepareAudioOnly(moov: original, facts: Self.facts)
        XCTAssertEqual(result.moov.count, original.count + 90)
        XCTAssertEqual(result.leadingFrames, 2112); XCTAssertEqual(result.validFrames, 96356)
        let marker = try XCTUnwrap(original.range(of: Data("mp4a".utf8))).lowerBound
        let start = marker - 4, length = original[start..<marker].reduce(0) { ($0 << 8) | Int($1) }
        XCTAssertNotNil(result.moov.range(of: original.subdata(in: start..<(start + length))))
        XCTAssertEqual(original[marker + 20..<marker + 22], Self.be(2, 2))
        XCTAssertThrowsError(try prepare(Self.nativeV0AudioOnlyHeader(neutralEdit: true))) { error in
            XCTAssertEqual(error as? Priming.Failure, .unsupportedLayout)
        }
    }

    func testNativeV0MonoAllowanceRefusesOtherRatesChannelsASCAndVersions() {
        let unsupported = [Self.nativeV0AudioOnlyHeader(channels: 0), Self.nativeV0AudioOnlyHeader(channels: 3),
            Self.nativeV0AudioOnlyHeader(asc: 0x1210), Self.nativeV0AudioOnlyHeader(asc: 0x118c),
            Self.nativeV0AudioOnlyHeader(sampleRate: 44100), Self.nativeV0AudioOnlyHeader(entryVersion: 1),
            Self.nativeV0AudioOnlyHeader(entryVersion: 2), Self.nativeV0AudioOnlyHeader(channels: 1, asc: 0x1190),
            Self.nativeV0AudioOnlyHeader(wrappedESDS: true)]
        for header in unsupported { XCTAssertThrowsError(try Priming.prepareAudioOnly(moov: header, facts: Self.facts)) }
    }

    func testNativeV0CanonicalMonoAndStereoDescriptionsStayByteExact() throws {
        let configurations: [(UInt16, UInt16)] = [(1, 0x1188), (2, 0x1190)]
        for (channels, asc) in configurations {
            let original = Self.nativeV0AudioOnlyHeader(channels: channels, asc: asc)
            let result = try Priming.prepareAudioOnly(moov: original, facts: Self.facts)
            let at = try XCTUnwrap(original.range(of: Data("stsd".utf8))).lowerBound
            let start = at - 4, length = original[start..<at].reduce(0) { ($0 << 8) | Int($1) }
            XCTAssertNotNil(result.moov.range(of: original.subdata(in: start..<(start + length))))
            XCTAssertEqual(try prepare(Self.nativeV0AudioOnlyHeader(channels: channels, asc: asc, neutralEdit: true)).moov.count,
                           Self.nativeV0AudioOnlyHeader(channels: channels, asc: asc, neutralEdit: true).count + 54)
        }
    }
}

private extension Data {
    func ranges(of needle: Data) -> [Range<Int>] {
        var result: [Range<Int>] = [], cursor = startIndex
        while cursor < endIndex, let found = range(of: needle, in: cursor..<endIndex) {
            result.append(found); cursor = found.upperBound
        }
        return result
    }
}
