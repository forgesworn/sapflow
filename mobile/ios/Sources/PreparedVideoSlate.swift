import CoreVideo
import Foundation

/// One original procedural picture, created cold and never modified after VT
/// can retain it. This owns no camera input, image asset or per-frame wrapper.
final class PreparedVideoSlate {
    private let image: CVPixelBuffer
    let width: Int
    let height: Int

    func withImage<Result>(_ body: (CVPixelBuffer) throws -> Result) rethrows -> Result { try body(image) }
    func contains(_ other: CVPixelBuffer) -> Bool { image === other }

    init(width: Int, height: Int) throws {
        guard (2...4096).contains(width), (2...4096).contains(height), width % 2 == 0, height % 2 == 0 else {
            throw CaptureFailure(message: "Standby picture has an unsupported canvas")
        }
        var created: CVPixelBuffer?
        guard CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary, &created) == kCVReturnSuccess, let created,
            CVPixelBufferGetPlaneCount(created) == 2 else {
            throw CaptureFailure(message: "Standby picture storage is unavailable")
        }
        self.image = created; self.width = width; self.height = height
        guard CVPixelBufferLockBaseAddress(created, []) == kCVReturnSuccess else {
            throw CaptureFailure(message: "Standby picture could not be prepared")
        }
        defer { CVPixelBufferUnlockBaseAddress(created, []) }
        for plane in 0..<2 {
            let rows = CVPixelBufferGetHeightOfPlane(created, plane)
            let stride = CVPixelBufferGetBytesPerRowOfPlane(created, plane)
            let expectedRows = plane == 0 ? height : height / 2
            guard rows == expectedRows, stride >= width, stride <= 16_384,
                  let address = CVPixelBufferGetBaseAddressOfPlane(created, plane) else {
                throw CaptureFailure(message: "Standby picture layout is unsupported")
            }
            memset(address, plane == 0 ? 16 : 128, stride * rows)
        }
        // Original neutral-grey pause bars: cold procedural marking only.
        guard let y = CVPixelBufferGetBaseAddressOfPlane(created, 0) else {
            throw CaptureFailure(message: "Standby picture marking storage is unavailable")
        }
        let stride = CVPixelBufferGetBytesPerRowOfPlane(created, 0)
        let barWidth = max(1, width / 24), barHeight = max(2, height / 8)
        for row in (height / 2 - barHeight / 2)..<(height / 2 + barHeight / 2) {
            for column in [max(0, width / 2 - 2 * barWidth), min(width - barWidth, width / 2 + barWidth)] {
                memset(y.advanced(by: row * stride + column), 160, barWidth)
            }
        }
    }
}
