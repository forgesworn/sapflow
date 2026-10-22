import Foundation

/// Top-level layout only. The read port supplies exact, bounded header bytes;
/// media payloads and the movie header itself are never read by this scanner.
enum QuickTimeMovieLayout {
    static let maximumBoxes = 65_536
    static let maximumMovieBytes = 16 * 1024 * 1024

    struct Extent: Equatable {
        let offset: UInt64
        let bytes: Int
        let headerBytes: Int
    }

    struct Layout: Equatable {
        let movie: Extent
        let boxCount: Int
        let mediaBoxCount: Int
    }

    enum Failure: Error, Equatable {
        case unsupported, excessive, shortRead
    }

    private static func integer(_ bytes: Data) -> UInt64 {
        bytes.reduce(0) { ($0 << 8) | UInt64($1) }
    }

    static func scan(fileBytes total: UInt64, read: (UInt64, Int) throws -> Data) throws -> Layout {
        guard total >= 8 else { throw Failure.unsupported }
        var cursor: UInt64 = 0, boxes = 0, media = 0
        var movie: Extent?
        while cursor < total {
            guard boxes < maximumBoxes else { throw Failure.excessive }
            boxes += 1
            // Subtraction precedes addition or any request to the read port.
            guard total - cursor >= 8 else { throw Failure.unsupported }
            let header = try read(cursor, 8)
            guard header.count == 8 else { throw Failure.shortRead }
            let split = header.startIndex + 4
            let short = integer(header.subdata(in: header.startIndex..<split))
            let kind = UInt32(integer(header.subdata(in: split..<header.endIndex)))
            var size = short, headerBytes = 8
            if short == 1 {
                guard total - cursor >= 16 else { throw Failure.unsupported }
                let extended = try read(cursor + 8, 8)
                guard extended.count == 8 else { throw Failure.shortRead }
                size = integer(extended); headerBytes = 16
            }
            // Open-ended boxes are unsupported by the priming transformer too.
            guard size >= UInt64(headerBytes), size <= total - cursor else { throw Failure.unsupported }
            switch kind {
            case 0x6d6f6f76: // moov
                guard movie == nil, media > 0, size == total - cursor else { throw Failure.unsupported }
                guard size <= UInt64(maximumMovieBytes) else { throw Failure.excessive }
                movie = Extent(offset: cursor, bytes: Int(size), headerBytes: headerBytes)
            case 0x6d646174: // mdat
                guard movie == nil else { throw Failure.unsupported }
                media += 1
            case 0x6d6f6f66, 0x6d667261, 0x73696478: // moof, mfra, sidx
                throw Failure.unsupported
            default:
                break
            }
            // The extent check proves this addition cannot overflow UInt64.
            cursor += size
        }
        guard let movie else { throw Failure.unsupported }
        return Layout(movie: movie, boxCount: boxes, mediaBoxCount: media)
    }
}
