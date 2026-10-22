import AVFoundation

/// One immutable delegate per actual native source/revision. AVFoundation queues
/// this captured token with the callback; an old callback cannot borrow a token
/// minted by a later Resume. No extra dispatch, PCM/pixel copy or per-frame owner.
final class CaptureRawSource: NSObject, AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureVideoDataOutputSampleBufferDelegate {
    let token: CaptureAdmission.MediaToken
    let camera: CaptureAdmission.CameraSource?
    private let frame: (CMSampleBuffer, CaptureAdmission.MediaToken, CaptureAdmission.CameraSource?) -> Void
    private let drop: () -> Void
    init(token: CaptureAdmission.MediaToken, camera: CaptureAdmission.CameraSource? = nil,
         frame: @escaping (CMSampleBuffer, CaptureAdmission.MediaToken, CaptureAdmission.CameraSource?) -> Void,
         drop: @escaping () -> Void = {}) {
        self.token = token; self.camera = camera; self.frame = frame; self.drop = drop
    }
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        frame(sampleBuffer, token, camera)
    }
    func captureOutput(_ output: AVCaptureOutput, didDrop sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) { drop() }
}

/// An opted-in audio-only show retains real native input I/O in Standby.
/// The callback deliberately does not inspect even sample metadata. Synthetic
/// zero cadence is independent of this callback, and no sample is retained.
final class CaptureDiscardAudioSource: NSObject, AVCaptureAudioDataOutputSampleBufferDelegate {
    let run: CaptureAdmission.MediaRun
    init(run: CaptureAdmission.MediaRun) { self.run = run }
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {}
}
