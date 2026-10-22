import Foundation

/// Cold, closed-writer metadata work only. Skip media by its declared extent;
/// only bounded atom headers and the final moov enter memory. The catalogue's
/// scoped lease owns mutation, durability, descriptor close and uncertainty.
enum QuickTimeAudioPrimingFile {
    static func finalise(_ file: RecordingCatalogue.Writer.MediaUpdate, facts: QuickTimeAudioPriming.Facts) throws {
        let movie = try QuickTimeMovieLayout.scan(fileBytes: file.originalBytes) { offset, count in
            try file.read(offset: offset, count: count)
        }.movie
        let original = try file.read(offset: movie.offset, count: movie.bytes)
        let prepared = try QuickTimeAudioPriming.prepare(moov: original, facts: facts)
        try file.installMovieHeader(offset: movie.offset, original: original, replacement: prepared.moov)
    }
}
