import AVFoundation

/// Cold terminal capability. Only the concrete native producer owner below can
/// create it. It retains no media and advances at most once through each owner.
final class CaptureRecordingClosure {
    private let run: CaptureAdmission.MediaRun
    private let ticket: RecordingCaptureAdmission.Ticket
    private let recordingAdmission: RecordingCaptureAdmission
    private let recorder: AnyObject
    private let worker: AnyObject
    private let admission: CaptureAdmission
    private let lock = NSLock()
    private enum Phase { case producers, ingress, recorder, consumed, refused }
    private var phase = Phase.producers
    private var samples: UInt64 = 0
    private var owner: ObjectIdentifier?
    private var loss: CaptureRecordingLoss?
    fileprivate init(run: CaptureAdmission.MediaRun, ticket: RecordingCaptureAdmission.Ticket,
                     recorder: AnyObject, worker: AnyObject, admission: CaptureAdmission, recordingAdmission: RecordingCaptureAdmission) {
        self.run = run; self.ticket = ticket; self.recorder = recorder; self.worker = worker; self.admission = admission; self.recordingAdmission = recordingAdmission
    }
    /// The existing ingress FIFO calls this after its accepted work has run.
    /// Counters describe its original reservations, never inferred codec units.
    func closeIngress(run: CaptureAdmission.MediaRun, worker: AnyObject, recorder: AnyObject,
                      offered: UInt64, forwarded: UInt64, events: UInt64, forwardedEvents: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard phase == .producers, self.run == run, self.worker === worker, self.recorder === recorder,
              offered == forwarded, events == forwardedEvents, offered > 0 else { phase = .refused; return false }
        samples = forwarded; phase = .ingress; return true
    }
    func closeRecorder(_ recorder: AnyObject, owner: RecordingCheckpointOwner, loss: CaptureRecordingLoss) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard phase == .ingress, self.recorder === recorder else { phase = .refused; return false }
        self.owner = ObjectIdentifier(owner); self.loss = loss; phase = .recorder; return true
    }
    /// Called only at the SDK/file owner's final-header commit boundary, after
    /// its exact installed checkpoint and actual drained native stats agree.
    func consume(owner: RecordingCheckpointOwner, frontier: RecordingCheckpointOwner.Frontier) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard phase == .recorder, self.owner == ObjectIdentifier(owner),
              case let .segment(start) = ticket.request, start.preparation == run.preparation,
              recordingAdmission.ownsRecorder(ticket, recorder: recorder) else {
            phase = .refused; return false
        }
        let (units, overflow) = frontier.audioUnits.addingReportingOverflow(frontier.videoUnits)
        guard !overflow, units == samples, frontier.completeAudioEOF, frontier.sequence > 0 else {
            phase = .refused; return false
        }
        guard let loss, loss.commit(admission: admission, run: run, start: start) else { phase = .refused; return false }
        phase = .consumed; return true
    }
}

/// The SAME concrete path is used by CaptureEngine Stop and generated native
/// qualification. No success Boolean, producer callback or proof factory exists.
/// It uses the existing samples queue, native encoders and capture session.
final class CaptureRecordingProducer {
    private let run: CaptureAdmission.MediaRun
    private let ticket: RecordingCaptureAdmission.Ticket
    private let recordingAdmission: RecordingCaptureAdmission
    private let recorder: AnyObject
    private let worker: AnyObject
    private let audio: NativeAudioEncoder
    private let video: HardwareVideoEncoder?
    private let session: AVCaptureSession
    private let samples: DispatchQueue
    private let admission: CaptureAdmission
    private let key = DispatchSpecificKey<UUID>()
    private let identity = UUID()
    private let lock = NSLock()
    private var detached: [AVCaptureOutput]?
    private var closed = false
    private var loss = false
    init(run: CaptureAdmission.MediaRun, ticket: RecordingCaptureAdmission.Ticket,
         recorder: AnyObject, worker: AnyObject, audio: NativeAudioEncoder, video: HardwareVideoEncoder?,
         session: AVCaptureSession, samples: DispatchQueue, admission: CaptureAdmission, recordingAdmission: RecordingCaptureAdmission) {
        self.run = run; self.ticket = ticket; self.recorder = recorder; self.worker = worker
        self.audio = audio; self.video = video; self.session = session; self.samples = samples; self.admission = admission
        self.recordingAdmission = recordingAdmission
        _ = recordingAdmission.bindRecorder(ticket, recorder: recorder)
        samples.setSpecific(key: key, value: identity)
    }
    deinit { samples.setSpecific(key: key, value: nil) }
    /// Control owner only. Native detach and stop complete BEFORE the caller
    /// enqueues the samples barrier. Retain the actual detached output objects.
    func stopInput() {
        let outputs = session.outputs
        var invalid = outputs.count > 2
        for output in outputs {
            if let value = output as? AVCaptureAudioDataOutput { value.setSampleBufferDelegate(nil, queue: nil) }
            else if let value = output as? AVCaptureVideoDataOutput { value.setSampleBufferDelegate(nil, queue: nil) }
            else { invalid = true }
        }
        if session.isRunning { session.stopRunning() }
        let stillRunning = session.isRunning
        lock.lock()
        if detached == nil { detached = Array(outputs.prefix(2)) }
        loss = loss || invalid || stillRunning
        lock.unlock()
    }
    /// Runs after the actual samples FIFO barrier. Each native finish performs
    /// its real converter EOF/callback join. Unknown closure stays incomplete.
    func finish() throws -> CaptureRecordingClosure? {
        guard DispatchQueue.getSpecific(key: key) == identity else {
            throw CaptureFailure(message: "Native producer closure is outside its samples owner")
        }
        lock.lock()
        guard !closed else { lock.unlock(); return nil }
        closed = true
        let detached = self.detached, lost = loss
        lock.unlock()
        var problem: Error?
        do { try audio.finish() } catch { problem = error }
        do { try video?.finishChecked() } catch { problem = problem ?? error }
        if let problem { throw problem }
        guard let detached, !lost, !session.isRunning,
              detached.allSatisfy({ output in
                  if let value = output as? AVCaptureAudioDataOutput { return value.sampleBufferDelegate == nil }
                  if let value = output as? AVCaptureVideoDataOutput { return value.sampleBufferDelegate == nil }
                  return false
              }), session.outputs.allSatisfy({ output in
                  if let value = output as? AVCaptureAudioDataOutput { return value.sampleBufferDelegate == nil }
                  if let value = output as? AVCaptureVideoDataOutput { return value.sampleBufferDelegate == nil }
                  return false
              }), !admission.mediaHasFault(run) else { return nil }
        guard case let .segment(start) = ticket.request, start.preparation == run.preparation,
              admission.hasCommittedMediaRun(run, start: start),
              recordingAdmission.ownsRecorder(ticket, recorder: recorder) else { return nil }
        return CaptureRecordingClosure(run: run, ticket: ticket, recorder: recorder, worker: worker, admission: admission, recordingAdmission: recordingAdmission)
    }
}
