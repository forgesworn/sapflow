import Foundation

/// A bounded, metadata-only edit of an original native recording header.
/// The caller owns file identity, installation, durability and finalisation.
enum QuickTimeAudioPriming {
    static let maximumHeaderBytes = 16 * 1024 * 1024
    static let maximumAccessUnits: UInt64 = 4_000_000
    private static let maximumBoxes = 4096
    private static let maximumEntries: UInt64 = 262_144

    struct Facts: Equatable {
        let audioTrackID: UInt32?
        let accessUnits: UInt64
        let leadingFrames: UInt32
        let validFrames: UInt64

        init(audioTrackID: UInt32? = nil, accessUnits: UInt64, leadingFrames: UInt32, validFrames: UInt64) {
            self.audioTrackID = audioTrackID
            self.accessUnits = accessUnits
            self.leadingFrames = leadingFrames
            self.validFrames = validFrames
        }
    }

    struct Prepared {
        let moov: Data
        let audioTrackID: UInt32
        let accessUnits: UInt64
        let leadingFrames: UInt32
        let validFrames: UInt64
        let trailingFrames: UInt64
        /// Original leading empty edit in the unchanged 48 kHz movie clock.
        let emptyEditDuration: UInt64
    }

    enum Failure: Error, Equatable {
        case invalidHeader, unsupportedLayout, capacity, ambiguousTrack, mismatchedFacts, alreadyApplied
    }

    private struct Box {
        let start: Int
        let end: Int
        let header: Int
        let kind: UInt32
        let children: [Box]
        var payload: Int { start + header }
        var size: Int { end - start }

        func one(_ kind: UInt32) throws -> Box {
            let matches = children.filter { $0.kind == kind }
            guard matches.count == 1 else { throw Failure.invalidHeader }
            return matches[0]
        }
    }

    private static func tag(_ text: String) -> UInt32 {
        text.utf8.reduce(0) { ($0 << 8) | UInt32($1) }
    }

    private struct Reader {
        let data: Data
        var visited = 0

        func integer(_ at: Int, _ width: Int) throws -> UInt64 {
            guard at >= 0, width > 0, width <= 8, at <= data.count, width <= data.count - at else {
                throw Failure.invalidHeader
            }
            var value: UInt64 = 0
            for i in 0..<width { value = (value << 8) | UInt64(data[data.startIndex + at + i]) }
            return value
        }

        func bytes(_ start: Int, _ end: Int) throws -> Data {
            guard start >= 0, start <= end, end <= data.count else { throw Failure.invalidHeader }
            return data.subdata(in: (data.startIndex + start)..<(data.startIndex + end))
        }

        mutating func boxes(_ start: Int, _ end: Int, depth: Int = 0) throws -> [Box] {
            guard start >= 0, start <= end, end <= data.count, depth <= 12 else { throw Failure.capacity }
            let containers = ["moov", "trak", "mdia", "minf", "stbl", "edts", "dinf"].map(QuickTimeAudioPriming.tag)
            let forbidden = ["mdat", "moof", "mvex", "traf", "mfra", "sidx"].map(QuickTimeAudioPriming.tag)
            var cursor = start, result: [Box] = []
            while cursor < end {
                visited += 1
                guard visited <= maximumBoxes else { throw Failure.capacity }
                guard end - cursor >= 8 else { throw Failure.invalidHeader }
                var length = try integer(cursor, 4)
                let kind = UInt32(try integer(cursor + 4, 4))
                var header = 8
                if length == 1 {
                    guard end - cursor >= 16 else { throw Failure.invalidHeader }
                    length = try integer(cursor + 8, 8)
                    header = 16
                }
                // No open-ended boxes or media/fragments in a prepared header.
                guard length >= UInt64(header), length <= UInt64(end - cursor) else { throw Failure.invalidHeader }
                guard !forbidden.contains(kind) else { throw Failure.unsupportedLayout }
                if kind == tag("uuid"), length < UInt64(header + 16) { throw Failure.invalidHeader }
                let finish = cursor + Int(length)
                let children = containers.contains(kind) ? try boxes(cursor + header, finish, depth: depth + 1) : []
                result.append(Box(start: cursor, end: finish, header: header, kind: kind, children: children))
                cursor = finish
            }
            return result
        }

        func full(_ box: Box, minimum: Int, versions: Set<UInt64> = [0], allowFlags: Bool = false) throws -> UInt64 {
            guard box.end - box.payload >= minimum else { throw Failure.invalidHeader }
            let version = try integer(box.payload, 1)
            guard versions.contains(version) else { throw Failure.unsupportedLayout }
            if !allowFlags, try integer(box.payload + 1, 3) != 0 { throw Failure.unsupportedLayout }
            return version
        }

        func clock(_ box: Box) throws -> (scale: UInt64, duration: UInt64) {
            let version = try full(box, minimum: 4, versions: [0, 1])
            let offset = version == 0 ? 12 : 20
            let width = version == 0 ? 4 : 8
            let minimum = box.kind == tag("mvhd") ? (version == 0 ? 100 : 112) : (version == 0 ? 24 : 36)
            guard box.end - box.payload >= minimum else { throw Failure.invalidHeader }
            return (try integer(box.payload + offset, 4), try integer(box.payload + offset + 4, width))
        }

        func trackID(_ track: Box) throws -> UInt32 {
            let header = try track.one(tag("tkhd"))
            let version = try full(header, minimum: 4, versions: [0, 1], allowFlags: true)
            guard header.end - header.payload >= (version == 0 ? 84 : 96) else { throw Failure.invalidHeader }
            let id = UInt32(try integer(header.payload + (version == 0 ? 12 : 20), 4))
            guard id > 0 else { throw Failure.invalidHeader }
            return id
        }

        func selfContained(_ info: Box) throws {
            let references = try info.one(tag("dinf")).one(tag("dref"))
            _ = try full(references, minimum: 8)
            guard try integer(references.payload + 4, 4) == 1, references.end - references.payload == 20 else {
                throw Failure.unsupportedLayout
            }
            let at = references.payload + 8
            let kind = UInt32(try integer(at + 4, 4))
            guard try integer(at, 4) == 12, [tag("alis"), tag("url ")].contains(kind),
                  try integer(at + 8, 4) == 1 else { throw Failure.unsupportedLayout }
        }

        /// Bounds descriptor lengths, including native four-byte length encodings.
        func descriptors(_ start: Int, _ end: Int) throws -> [(tag: UInt64, start: Int, end: Int)] {
            guard start >= 0, start <= end, end <= data.count else { throw Failure.invalidHeader }
            var at = start, found: [(UInt64, Int, Int)] = []
            while at < end {
                guard found.count < 32 else { throw Failure.capacity }
                let kind = try integer(at, 1); at += 1
                var length: UInt64 = 0, ended = false
                for _ in 0..<4 {
                    guard at < end else { throw Failure.invalidHeader }
                    let byte = try integer(at, 1); at += 1
                    length = (length << 7) | (byte & 127)
                    if byte & 128 == 0 { ended = true; break }
                }
                guard ended, length <= UInt64(end - at) else { throw Failure.invalidHeader }
                found.append((kind, at, at + Int(length))); at += Int(length)
            }
            return found
        }

        func aacDescription(_ table: Box, allowNativeV0MonoDefault: Bool = false) throws {
            let descriptions = try table.one(tag("stsd"))
            _ = try full(descriptions, minimum: 8)
            guard try integer(descriptions.payload + 4, 4) == 1 else { throw Failure.unsupportedLayout }
            var nested = self
            let entries = try nested.boxes(descriptions.payload + 8, descriptions.end)
            guard entries.count == 1 else { throw Failure.invalidHeader }
            let entry = entries[0]
            guard entry.kind == tag("mp4a"), entry.end - entry.payload >= 28,
                  try integer(entry.payload + 6, 2) == 1 else { throw Failure.unsupportedLayout }
            let version = try integer(entry.payload + 8, 2)
            guard version <= 1 else { throw Failure.unsupportedLayout }
            let channels = try integer(entry.payload + 16, 2)
            guard channels == 1 || channels == 2, try integer(entry.payload + 24, 4) == 48_000 << 16 else {
                throw Failure.unsupportedLayout
            }
            let extensionStart = entry.payload + (version == 0 ? 28 : 44)
            guard extensionStart <= entry.end else { throw Failure.invalidHeader }
            if version == 1 {
                guard try integer(entry.payload + 28, 4) == 1024 else { throw Failure.unsupportedLayout }
            }
            let extensions = try nested.boxes(extensionStart, entry.end)
            var configs = extensions.filter { $0.kind == tag("esds") }
            for wave in extensions where wave.kind == tag("wave") {
                configs += try nested.boxes(wave.payload, wave.end).filter { $0.kind == tag("esds") }
            }
            guard configs.count == 1 else { throw Failure.invalidHeader }
            let esds = configs[0]
            _ = try full(esds, minimum: 4)
            let roots = try descriptors(esds.payload + 4, esds.end)
            guard roots.count == 1, roots[0].tag == 3, roots[0].end - roots[0].start >= 3 else {
                throw Failure.unsupportedLayout
            }
            let es = roots[0], start = es.start + 3
            let flags = try integer(es.start + 2, 1)
            // Native local samples need no dependent/external ES or OCR stream.
            guard flags & 224 == 0 else { throw Failure.unsupportedLayout }
            let decoders = try descriptors(start, es.end).filter { $0.tag == 4 }
            guard decoders.count == 1, decoders[0].end - decoders[0].start >= 13 else { throw Failure.invalidHeader }
            let decoder = decoders[0]
            guard try integer(decoder.start, 1) == 0x40, try integer(decoder.start + 1, 1) >> 2 == 5 else {
                throw Failure.unsupportedLayout
            }
            let specific = try descriptors(decoder.start + 13, decoder.end).filter { $0.tag == 5 }
            guard specific.count == 1, specific[0].end - specific[0].start == 2 else {
                throw Failure.unsupportedLayout
            }
            let asc = try integer(specific[0].start, 2)
            let canonical = asc == (channels == 1 ? 0x1188 : 0x1190)
            // Observed native M4A v0 entries retain the default channelcount=2
            // while their authoritative AAC configuration is actual mono1188.
            // Admit only through the checked one-audio/no-edit entry; preserve
            // every sample-description byte and the strict existing A/V path.
            let directESDS = extensions.contains { $0.kind == tag("esds") && $0.start == esds.start }
            let nativeMonoDefault = allowNativeV0MonoDefault && version == 0 && channels == 2 && asc == 0x1188 && directESDS
            guard canonical || nativeMonoDefault else {
                throw Failure.unsupportedLayout
            }
        }

        func sampleTables(_ table: Box, units: UInt64) throws {
            let sizes = try table.one(tag("stsz"))
            _ = try full(sizes, minimum: 12)
            let fixed = try integer(sizes.payload + 4, 4), count = try integer(sizes.payload + 8, 4)
            guard count == units else { throw Failure.mismatchedFacts }
            let bytes = fixed == 0 ? 12 + Int(count) * 4 : 12
            guard sizes.end - sizes.payload == bytes else { throw Failure.invalidHeader }
            if fixed == 0 {
                for index in 0..<Int(count) where try integer(sizes.payload + 12 + index * 4, 4) == 0 {
                    throw Failure.invalidHeader
                }
            }
            let timing = try table.one(tag("stts"))
            _ = try full(timing, minimum: 8)
            let entries = try integer(timing.payload + 4, 4)
            guard entries > 0, entries <= maximumEntries, timing.end - timing.payload == 8 + Int(entries) * 8 else {
                throw Failure.invalidHeader
            }
            var samples: UInt64 = 0
            for index in 0..<Int(entries) {
                let n = try integer(timing.payload + 8 + index * 8, 4)
                let delta = try integer(timing.payload + 12 + index * 8, 4)
                guard n > 0, n <= units - samples, delta == 1024 else { throw Failure.mismatchedFacts }
                samples += n
            }
            guard samples == units else { throw Failure.mismatchedFacts }
            let offsets = table.children.filter { $0.kind == tag("stco") || $0.kind == tag("co64") }
            guard offsets.count == 1 else { throw Failure.invalidHeader }
            let chunks = offsets[0]; _ = try full(chunks, minimum: 8)
            let chunkCount = try integer(chunks.payload + 4, 4), width = chunks.kind == tag("stco") ? 4 : 8
            guard chunkCount > 0, chunkCount <= maximumEntries,
                  chunks.end - chunks.payload == 8 + Int(chunkCount) * width else { throw Failure.invalidHeader }
            let mapping = try table.one(tag("stsc")); _ = try full(mapping, minimum: 8)
            let runs = try integer(mapping.payload + 4, 4)
            guard runs > 0, runs <= maximumEntries, mapping.end - mapping.payload == 8 + Int(runs) * 12 else {
                throw Failure.invalidHeader
            }
            var total: UInt64 = 0, expectedFirst: UInt64 = 1
            for index in 0..<Int(runs) {
                let at = mapping.payload + 8 + index * 12
                let first = try integer(at, 4), perChunk = try integer(at + 4, 4), description = try integer(at + 8, 4)
                let next = index + 1 < Int(runs) ? try integer(at + 12, 4) : chunkCount + 1
                guard first == expectedFirst, first <= chunkCount, next > first, next <= chunkCount + 1,
                      perChunk > 0, perChunk <= units, description == 1 else { throw Failure.invalidHeader }
                let added = (next - first) * perChunk
                guard added <= units - total else { throw Failure.mismatchedFacts }
                total += added; expectedFirst = next
            }
            guard total == units else { throw Failure.mismatchedFacts }
        }
    }

    private static func be(_ value: UInt64, width: Int) -> Data {
        Data((0..<width).reversed().map { UInt8(truncatingIfNeeded: value >> ($0 * 8)) })
    }

    /// A native audio-only writer can omit edts despite a measured trimmed
    /// session. Add only the missing neutral edit, then use the same validated
    /// priming transform. Existing A/V preparation and its bytes stay unchanged.
    static func prepareAudioOnly(moov: Data, facts: Facts) throws -> Prepared {
        guard moov.count <= maximumHeaderBytes - 90 else { throw Failure.capacity }
        guard facts.accessUnits > 0, facts.accessUnits <= maximumAccessUnits,
              facts.leadingFrames > 0, facts.leadingFrames <= UInt32(Int32.max),
              facts.validFrames > 0, facts.validFrames <= UInt64(UInt32.max),
              UInt64(facts.leadingFrames) < facts.accessUnits * 1024,
              facts.validFrames <= facts.accessUnits * 1024 - UInt64(facts.leadingFrames),
              facts.audioTrackID != 0 else { throw Failure.mismatchedFacts }
        var reader = Reader(data: moov)
        let roots = try reader.boxes(0, moov.count)
        guard roots.count == 1, roots[0].kind == tag("moov") else { throw Failure.invalidHeader }
        let movie = roots[0], clock = try reader.clock(movie.one(tag("mvhd")))
        guard clock.scale == 48_000 else { throw Failure.unsupportedLayout }
        guard clock.duration == facts.validFrames else { throw Failure.mismatchedFacts }
        let tracks = movie.children.filter { $0.kind == tag("trak") }
        guard tracks.count == 1 else { throw Failure.ambiguousTrack }
        let track = tracks[0], id = try reader.trackID(track)
        guard facts.audioTrackID == nil || facts.audioTrackID == id else { throw Failure.mismatchedFacts }
        guard !track.children.contains(where: { $0.kind == tag("edts") }) else { throw Failure.alreadyApplied }
        let media = try track.one(tag("mdia")), handler = try media.one(tag("hdlr"))
        _ = try reader.full(handler, minimum: 12)
        guard try reader.integer(handler.payload + 8, 4) == UInt64(tag("soun")) else { throw Failure.unsupportedLayout }
        let trackHeader = try track.one(tag("tkhd"))
        let version = try reader.integer(trackHeader.payload, 1)
        let trackDuration = try reader.integer(trackHeader.payload + (version == 0 ? 20 : 28), version == 0 ? 4 : 8)
        guard trackDuration == facts.validFrames else { throw Failure.mismatchedFacts }
        var edit = be(36, width: 4) + Data("edts".utf8) + be(28, width: 4) + Data("elst".utf8)
        edit += be(0, width: 4) + be(1, width: 4) + be(facts.validFrames, width: 4)
        edit += be(0, width: 4) + be(65_536, width: 4)
        var augmented = Data(moov)
        augmented.insert(contentsOf: edit, at: track.end)
        for ancestor in [movie, track] {
            let width = ancestor.header == 8 ? 4 : 8
            let at = ancestor.start + (ancestor.header == 8 ? 0 : 8)
            augmented.replaceSubrange(at..<(at + width), with: be(UInt64(ancestor.size + edit.count), width: width))
        }
        // The original validator checks AAC configuration, tables, media M,
        // facts, edit rate/duration, no prior roll, then applies H and roll once.
        return try prepare(moov: augmented, facts: facts, allowNativeV0MonoDefault: true)
    }

    /// Refuses an already edited header rather than guessing or double-applying.
    static func prepare(moov: Data, facts: Facts) throws -> Prepared {
        try prepare(moov: moov, facts: facts, allowNativeV0MonoDefault: false)
    }

    private static func prepare(moov: Data, facts: Facts, allowNativeV0MonoDefault: Bool) throws -> Prepared {
        guard moov.count <= maximumHeaderBytes else { throw Failure.capacity }
        guard facts.accessUnits > 0, facts.accessUnits <= maximumAccessUnits,
              facts.leadingFrames > 0, facts.validFrames > 0,
              UInt64(facts.leadingFrames) < facts.accessUnits * 1024,
              facts.validFrames <= facts.accessUnits * 1024 - UInt64(facts.leadingFrames),
              facts.audioTrackID != 0 else { throw Failure.mismatchedFacts }
        var reader = Reader(data: moov)
        let roots = try reader.boxes(0, moov.count)
        guard roots.count == 1, roots[0].kind == tag("moov") else { throw Failure.invalidHeader }
        let movie = roots[0]
        let movieClock = try reader.clock(movie.one(tag("mvhd")))
        guard movieClock.scale == 48_000 else { throw Failure.unsupportedLayout }
        let tracks = movie.children.filter { $0.kind == tag("trak") }
        guard !tracks.isEmpty, tracks.count <= 2 else { throw Failure.ambiguousTrack }
        var ids: Set<UInt32> = [], audio: [(Box, Box, Box, UInt32)] = []
        for track in tracks {
            let id = try reader.trackID(track)
            guard ids.insert(id).inserted else { throw Failure.ambiguousTrack }
            let media = try track.one(tag("mdia")), handler = try media.one(tag("hdlr"))
            _ = try reader.full(handler, minimum: 12)
            let kind = UInt32(try reader.integer(handler.payload + 8, 4))
            guard kind == tag("soun") || kind == tag("vide") else { throw Failure.unsupportedLayout }
            let info = try media.one(tag("minf")); try reader.selfContained(info)
            if kind == tag("soun") { audio.append((track, media, try info.one(tag("stbl")), id)) }
        }
        guard audio.count == 1 else { throw Failure.ambiguousTrack }
        let (track, media, table, id) = audio[0]
        guard facts.audioTrackID == nil || facts.audioTrackID == id else { throw Failure.mismatchedFacts }
        let trackHeader = try track.one(tag("tkhd"))
        let trackVersion = try reader.integer(trackHeader.payload, 1)
        let trackDuration = try reader.integer(trackHeader.payload + (trackVersion == 0 ? 20 : 28), trackVersion == 0 ? 4 : 8)
        let mediaClock = try reader.clock(media.one(tag("mdhd")))
        guard mediaClock.scale == 48_000, mediaClock.duration == facts.accessUnits * 1024 else {
            throw Failure.mismatchedFacts
        }
        try reader.aacDescription(table, allowNativeV0MonoDefault: allowNativeV0MonoDefault)
        try reader.sampleTables(table, units: facts.accessUnits)
        for field in table.children where field.kind == tag("sgpd") || field.kind == tag("sbgp") {
            _ = try reader.full(field, minimum: 8, versions: [0, 1, 2])
            if try reader.integer(field.payload + 4, 4) == UInt64(tag("roll")) { throw Failure.alreadyApplied }
        }
        let edit = try track.one(tag("edts")).one(tag("elst"))
        let version = try reader.full(edit, minimum: 8, versions: [0, 1])
        let width = version == 0 ? 4 : 8
        let count = try reader.integer(edit.payload + 4, 4), stride = 2 * width + 4
        guard count == 1 || count == 2, edit.end - edit.payload == 8 + Int(count) * stride else {
            throw Failure.unsupportedLayout
        }
        var emptyDuration: UInt64 = 0
        if count == 2 {
            let at = edit.payload + 8
            emptyDuration = try reader.integer(at, width)
            let noMedia = width == 4 ? UInt64(UInt32.max) : UInt64.max
            guard emptyDuration > 0, try reader.integer(at + width, width) == noMedia,
                  try reader.integer(at + 2 * width, 4) == 65_536 else { throw Failure.unsupportedLayout }
        }
        let (trackExtent, overflow) = emptyDuration.addingReportingOverflow(facts.validFrames)
        guard !overflow, trackDuration == trackExtent, movieClock.duration >= trackExtent else { throw Failure.mismatchedFacts }
        let durationAt = edit.payload + 8 + (count == 2 ? stride : 0), mediaAt = durationAt + width
        guard try reader.integer(durationAt, width) == facts.validFrames else { throw Failure.mismatchedFacts }
        guard try reader.integer(mediaAt, width) == 0 else { throw Failure.alreadyApplied }
        guard try reader.integer(mediaAt + width, 4) == 65_536 else { throw Failure.unsupportedLayout }

        var sgpd = be(26, width: 4) + Data("sgpd".utf8) + Data([1, 0, 0, 0]) + Data("roll".utf8)
        sgpd += be(2, width: 4) + be(1, width: 4) + Data([255, 255])
        let sbgp = be(28, width: 4) + Data("sbgp".utf8) + Data([0, 0, 0, 0]) + Data("roll".utf8)
            + be(1, width: 4) + be(facts.accessUnits, width: 4) + be(1, width: 4)
        let groups = sgpd + sbgp
        guard moov.count <= maximumHeaderBytes - groups.count else { throw Failure.capacity }
        var output = Data(moov)
        output.replaceSubrange(durationAt..<(durationAt + width), with: be(facts.validFrames, width: width))
        output.replaceSubrange(mediaAt..<(mediaAt + width), with: be(UInt64(facts.leadingFrames), width: width))
        output.insert(contentsOf: groups, at: table.end)
        let info = try media.one(tag("minf"))
        for ancestor in [movie, track, media, info, table] {
            let width = ancestor.header == 8 ? 4 : 8
            let at = ancestor.start + (ancestor.header == 8 ? 0 : 8)
            output.replaceSubrange(at..<(at + width), with: be(UInt64(ancestor.size + groups.count), width: width))
        }
        return Prepared(moov: output, audioTrackID: id, accessUnits: facts.accessUnits,
                        leadingFrames: facts.leadingFrames, validFrames: facts.validFrames,
                        trailingFrames: facts.accessUnits * 1024 - UInt64(facts.leadingFrames) - facts.validFrames,
                        emptyEditDuration: emptyDuration)
    }
}
