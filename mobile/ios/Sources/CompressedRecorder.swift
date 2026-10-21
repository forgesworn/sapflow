import AVFoundation

/// File work has its own queue and never blocks capture or publishing. Already
/// encoded H.264/AAC goes directly to passthrough inputs: no second encode.
final class CompressedRecorder {
    private let queue = DispatchQueue(label: "sapflow.recording", qos: .utility)
    private let budget = EncodedWorkBudget()
    private let writer: AVAssetWriter
    private var video: AVAssetWriterInput?
    private var audio: AVAssetWriterInput?
    private var staged: [(unit: CompressedUnit, reservation: EncodedWorkBudget.Reservation)] = []
    private var finishing = false
    private var failure: Error?
    private let onFailure: (Error) -> Void
    let url: URL

    init(url: URL, onFailure: @escaping (Error) -> Void) throws {
        self.url = url; self.onFailure = onFailure
        writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        writer.movieFragmentInterval = CMTime(seconds: 1, preferredTimescale: 600)
        writer.shouldOptimizeForNetworkUse = false
    }

    func offer(_ unit: CompressedUnit) {
        guard let reservation = budget.reserve(bytes: unit.byteCount) else {
            if budget.close() { queue.async { self.fail(CaptureFailure(message: "Recording backlog exceeded its 8 MiB / 128 sample / 2 s bound")) } }
            return
        }
        queue.async {
            guard !self.finishing, self.failure == nil else { self.budget.release(reservation); return }
            do {
                if self.writer.status == .unknown {
                    self.staged.append((unit, reservation))
                    if unit.isVideo && self.video == nil {
                        self.video = try self.makeInput(type: .video, unit: unit)
                    } else if !unit.isVideo && self.audio == nil {
                        self.audio = try self.makeInput(type: .audio, unit: unit)
                    }
                    guard self.video != nil && self.audio != nil else { return }
                    guard self.writer.startWriting() else { throw self.writer.error ?? CaptureFailure(message: "Recording could not start") }
                    self.writer.startSession(atSourceTime: .zero)
                    let initial = self.staged
                    self.staged.removeAll(keepingCapacity: false)
                    defer { for pending in initial { self.budget.release(pending.reservation) } }
                    for pending in initial { try self.append(pending.unit) }
                } else {
                    defer { self.budget.release(reservation) }
                    try self.append(unit)
                }
            } catch { self.fail(error) }
        }
    }

    private func makeInput(type: AVMediaType, unit: CompressedUnit) throws -> AVAssetWriterInput {
        let input = AVAssetWriterInput(mediaType: type, outputSettings: nil, sourceFormatHint: CMSampleBufferGetFormatDescription(unit.buffer))
        input.expectsMediaDataInRealTime = true
        guard writer.canAdd(input) else { throw CaptureFailure(message: "Recording cannot accept the encoded format") }
        writer.add(input)
        return input
    }
    private func append(_ unit: CompressedUnit) throws {
        let input = unit.isVideo ? video : audio
        guard let input, writer.status == .writing, input.isReadyForMoreMediaData, input.append(unit.buffer) else {
            throw writer.error ?? CaptureFailure(message: "Recording storage cannot keep up; recording stopped")
        }
    }
    private func fail(_ error: Error) {
        guard failure == nil else { return }
        failure = error
        budget.close()
        for pending in staged { budget.release(pending.reservation) }
        staged.removeAll()
        onFailure(error)
    }
    func finish(_ completion: @escaping (Result<URL, Error>) -> Void) {
        budget.close()
        queue.async {
            guard !self.finishing else { return }
            self.finishing = true
            for pending in self.staged { self.budget.release(pending.reservation) }
            self.staged.removeAll()
            guard self.writer.status == .writing else {
                self.writer.cancelWriting()
                completion(.failure(self.failure ?? CaptureFailure(message: "No complete audio/video recording was captured")))
                return
            }
            self.video?.markAsFinished(); self.audio?.markAsFinished()
            self.writer.finishWriting {
                if self.writer.status == .completed {
                    completion(.success(self.url))
                } else {
                    completion(.failure(self.writer.error ?? self.failure ?? CaptureFailure(message: "Recording finalisation failed")))
                }
            }
        }
    }
}
