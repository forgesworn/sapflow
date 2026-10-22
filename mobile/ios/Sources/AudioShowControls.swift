import Foundation

/// SDK 18.2 has no suitable background microphone-recording Stop control.
/// AVCaptureEventInteraction is foreground/camera-only; MediaPlayer controls
/// represent playback. Do not create playback or pretend either is supported.
enum AudioShowControlCapability: Equatable { case appOnly }

/// One immutable logical-show action. The capture owner rechecks this ticket
/// atomically; a queued action from an old show cannot stop its successor.
struct AudioShowStopControl {
    let preparation: CaptureAdmission.Preparation
    let capability = AudioShowControlCapability.appOnly
    private weak var owner: CaptureAdmission?
    init(preparation: CaptureAdmission.Preparation, admission: CaptureAdmission) {
        self.preparation = preparation; owner = admission
    }
    func perform(on admission: CaptureAdmission) -> CaptureAdmission.Stop? {
        guard owner === admission else { return nil }
        return admission.stop(preparation: preparation)
    }
    /// The actual UI action shares this ordering with its regression: retire
    /// only after the exact capture Stop wins, with no callback under its lock.
    @discardableResult func stop(capture: (AudioShowStopControl) -> Bool,
                                retirePublisher: (CaptureAdmission.Preparation) -> Void) -> Bool {
        guard capture(self) else { return false }
        retirePublisher(preparation); return true
    }
}
