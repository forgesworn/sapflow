//! Immutable sample handles and bounded, non-blocking native ingress.
use crate::{BindingError, MAX_ENCODED_BUFFER_BYTES};
use sapflow_client::media::SampleContinuity;
use sapflow_client::queue::{EnqueueOutcome, QueueLimits, SampleFanout};
use sapflow_client::{
    AacConfig, CodecConfig, EncodedSample, H264Config, SampleTiming, SharedSample,
};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(uniffi::Object)]
pub struct MediaCodec {
    config: Arc<CodecConfig>,
}

impl MediaCodec {
    pub(crate) fn config(&self) -> &Arc<CodecConfig> {
        &self.config
    }
}

#[uniffi::export]
impl MediaCodec {
    #[uniffi::constructor]
    pub fn h264(sps: &[u8], pps: &[u8]) -> Result<Arc<Self>, BindingError> {
        let config = H264Config::new(sps, pps).map_err(|_| BindingError::InvalidMedia)?;
        Ok(Arc::new(Self {
            config: Arc::new(CodecConfig::H264(config)),
        }))
    }
    #[uniffi::constructor]
    pub fn aac(asc: &[u8]) -> Result<Arc<Self>, BindingError> {
        let config = AacConfig::new(asc).map_err(|_| BindingError::InvalidMedia)?;
        Ok(Arc::new(Self {
            config: Arc::new(CodecConfig::Aac(config)),
        }))
    }
    /// The sole compressed-payload ownership copy; consumers share the result.
    pub fn sample(
        &self,
        bytes: &[u8],
        pts: i64,
        dts: i64,
        duration: u32,
        timescale: u32,
        keyframe: bool,
    ) -> Result<Arc<MediaSample>, BindingError> {
        if bytes.is_empty() {
            return Err(BindingError::EmptyBuffer);
        }
        if bytes.len() > MAX_ENCODED_BUFFER_BYTES {
            return Err(BindingError::BufferTooLarge);
        }
        let timing = SampleTiming::new(pts, dts, duration, timescale)
            .map_err(|_| BindingError::InvalidMedia)?;
        let sample = EncodedSample::new(self.config.clone(), timing, keyframe, Arc::from(bytes))
            .map_err(|_| BindingError::InvalidMedia)?;
        Ok(Arc::new(MediaSample {
            sample: Arc::new(sample),
        }))
    }
}

#[derive(uniffi::Object)]
pub struct MediaSample {
    pub(crate) sample: SharedSample,
}

#[uniffi::export]
impl MediaSample {
    pub fn len(&self) -> u64 {
        self.sample.payload().len() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.sample.payload().is_empty()
    }
    pub fn is_video(&self) -> bool {
        self.sample.is_video()
    }
    pub fn is_keyframe(&self) -> bool {
        self.sample.is_keyframe()
    }
    pub fn pts(&self) -> i64 {
        self.sample.timing().pts()
    }
    pub fn dts(&self) -> i64 {
        self.sample.timing().dts()
    }
    pub fn duration(&self) -> u32 {
        self.sample.timing().duration()
    }
    pub fn timescale(&self) -> u32 {
        self.sample.timing().timescale()
    }
}

#[derive(uniffi::Record)]
pub struct FanoutOutcome {
    pub publishing_queued: bool,
    pub recording_queued: bool,
    pub publishing_waiting_keyframe: bool,
    pub publishing_overflows: u64,
    pub recording_failed: bool,
}

#[derive(uniffi::Record)]
pub struct NativeQueueStats {
    pub publishing_bytes: u64,
    pub recording_bytes: u64,
    pub publishing_drops: u64,
    pub recording_drops: u64,
    pub publishing_high_water_bytes: u64,
    pub recording_high_water_bytes: u64,
    pub publishing_media_ms: u64,
    pub publishing_high_water_media_ms: u64,
    pub publishing_overflows: u64,
    pub publishing_overflow_bytes: u64,
    pub publishing_overflow_samples: u64,
    pub publishing_overflow_media: u64,
    pub publishing_overflow_residence: u64,
    pub publishing_waiting_keyframe: bool,
    pub recording_failed: bool,
}

struct State {
    fanout: SampleFanout,
    continuity: SampleContinuity,
    last_now: Option<u64>,
    closed: bool,
}
impl State {
    fn clock(&mut self, now_ns: u64) -> Result<(), BindingError> {
        if self.last_now.is_some_and(|last| now_ns < last) {
            return Err(BindingError::ClockWentBackwards);
        }
        self.last_now = Some(now_ns);
        Ok(())
    }
}

#[derive(uniffi::Object)]
pub struct NativeSampleFanout {
    state: Mutex<State>,
}
impl NativeSampleFanout {
    fn state(&self) -> Result<MutexGuard<'_, State>, BindingError> {
        // Never wait behind another thread from a codec callback. The caller
        // handles Busy using its explicit drop/retry policy.
        self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => BindingError::Busy,
            std::sync::TryLockError::Poisoned(_) => BindingError::Closed,
        })
    }
}

#[uniffi::export]
impl NativeSampleFanout {
    #[uniffi::constructor]
    pub fn new(
        publishing_bytes: u64,
        recording_bytes: u64,
        max_media_ms: u32,
        has_video: bool,
    ) -> Result<Arc<Self>, BindingError> {
        if publishing_bytes > 64 * 1024 * 1024
            || recording_bytes > 64 * 1024 * 1024
            || max_media_ms == 0
            || max_media_ms > 60_000
        {
            return Err(BindingError::InvalidQueueLimits);
        }
        let limits = |bytes| {
            QueueLimits::new(
                usize::try_from(bytes).map_err(|_| BindingError::InvalidQueueLimits)?,
                8192,
                u64::from(max_media_ms) * 1_000_000,
                u64::from(max_media_ms) * 1_000_000,
            )
            .map_err(|_| BindingError::InvalidQueueLimits)
        };
        let fanout = SampleFanout::new(
            limits(publishing_bytes)?,
            limits(recording_bytes)?,
            has_video,
        );
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                fanout,
                continuity: SampleContinuity::default(),
                last_now: None,
                closed: false,
            }),
        }))
    }
    pub fn push(
        &self,
        sample: Arc<MediaSample>,
        now_ns: u64,
    ) -> Result<FanoutOutcome, BindingError> {
        let mut state = self.state()?;
        if state.closed {
            return Err(BindingError::Closed);
        }
        state.clock(now_ns)?;
        state
            .continuity
            .validate(&sample.sample)
            .map_err(|_| BindingError::InvalidMedia)?;
        let (publishing, recording) = state
            .fanout
            .push(sample.sample.clone(), now_ns)
            .map_err(|_| BindingError::ClockWentBackwards)?;
        let queued = |value| {
            matches!(
                value,
                EnqueueOutcome::Queued | EnqueueOutcome::QueuedAfterReset { .. }
            )
        };
        let waiting = state.fanout.publishing().waiting_for_keyframe();
        let failed = state.fanout.recording().recording_failure().is_some();
        Ok(FanoutOutcome {
            publishing_queued: queued(publishing),
            recording_queued: queued(recording),
            publishing_waiting_keyframe: waiting,
            publishing_overflows: state.fanout.publishing().stats().overflow_events,
            recording_failed: failed,
        })
    }
    pub fn pop_publishing(&self, now_ns: u64) -> Result<Option<Arc<MediaSample>>, BindingError> {
        let mut state = self.state()?;
        state.clock(now_ns)?;
        Ok(state
            .fanout
            .publishing()
            .pop(now_ns)
            .map_err(|_| BindingError::ClockWentBackwards)?
            .map(|sample| Arc::new(MediaSample { sample })))
    }
    pub fn pop_recording(&self, now_ns: u64) -> Result<Option<Arc<MediaSample>>, BindingError> {
        let mut state = self.state()?;
        state.clock(now_ns)?;
        Ok(state
            .fanout
            .recording()
            .pop(now_ns)
            .map_err(|_| BindingError::ClockWentBackwards)?
            .map(|sample| Arc::new(MediaSample { sample })))
    }
    pub fn stats(&self) -> Result<NativeQueueStats, BindingError> {
        let mut state = self.state()?;
        let publishing = state.fanout.publishing().stats();
        let waiting = state.fanout.publishing().waiting_for_keyframe();
        let recording = state.fanout.recording().stats();
        let failed = state.fanout.recording().recording_failure().is_some();
        Ok(NativeQueueStats {
            publishing_bytes: publishing.queued_bytes as u64,
            recording_bytes: recording.queued_bytes as u64,
            publishing_drops: publishing.dropped_samples,
            recording_drops: recording.dropped_samples,
            publishing_high_water_bytes: publishing.high_water_bytes as u64,
            recording_high_water_bytes: recording.high_water_bytes as u64,
            publishing_media_ms: publishing.queued_media_ns / 1_000_000,
            publishing_high_water_media_ms: publishing.high_water_media_ns / 1_000_000,
            publishing_overflows: publishing.overflow_events,
            publishing_overflow_bytes: publishing.overflow_bytes,
            publishing_overflow_samples: publishing.overflow_samples,
            publishing_overflow_media: publishing.overflow_media_duration,
            publishing_overflow_residence: publishing.overflow_residence,
            publishing_waiting_keyframe: waiting,
            recording_failed: failed,
        })
    }
    /// Irreversibly fence this branch's future admission without clearing its
    /// accepted prefix or affecting recording. Successful return is a local
    /// admission receipt, not transport completion or remote erasure.
    pub fn close_publishing_admission(&self) -> Result<(), BindingError> {
        self.state()?.fanout.publishing().close_admission();
        Ok(())
    }
    /// Irreversibly fence future recording admission while keeping the
    /// accepted prefix available to its actual recorder owner.
    pub fn close_recording_admission(&self) -> Result<(), BindingError> {
        self.state()?.fanout.recording().close_admission();
        Ok(())
    }
    pub fn cancel(&self) -> Result<(), BindingError> {
        let mut state = self.state()?;
        state.closed = true;
        state.fanout.clear();
        state.continuity.reset();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn selective_fences_keep_prefix_identity_and_independent_admission() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let queues = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        let sample = codec.sample(&[1, 2, 3], 0, 0, 1024, 48_000, false).unwrap();
        queues.push(sample.clone(), 0).unwrap();
        queues.close_publishing_admission().unwrap();
        queues.close_publishing_admission().unwrap();
        let next = codec
            .sample(&[4, 5], 1024, 1024, 1024, 48_000, false)
            .unwrap();
        let outcome = queues.push(next.clone(), 1).unwrap();
        assert!(
            !outcome.publishing_queued && outcome.recording_queued && !outcome.recording_failed
        );
        let prefix = queues.pop_publishing(2).unwrap().unwrap();
        assert!(Arc::ptr_eq(
            prefix.sample.payload(),
            sample.sample.payload()
        ));
        assert!(queues.pop_publishing(2).unwrap().is_none());
        let first_recorded = queues.pop_recording(2).unwrap().unwrap();
        assert!(Arc::ptr_eq(
            first_recorded.sample.payload(),
            sample.sample.payload()
        ));
        assert!(queues.pop_recording(2).unwrap().is_some());
        queues.close_recording_admission().unwrap();
        let final_sample = codec.sample(&[6], 2048, 2048, 1024, 48_000, false).unwrap();
        let closed = queues.push(final_sample, 3).unwrap();
        assert!(!closed.publishing_queued && !closed.recording_queued && !closed.recording_failed);
        queues.cancel().unwrap();
        queues.close_recording_admission().unwrap();
        let successor = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        queues.close_publishing_admission().unwrap();
        let fresh = successor.push(sample, 0).unwrap();
        assert!(fresh.publishing_queued && fresh.recording_queued);
    }

    #[test]
    fn fence_busy_or_invalid_media_is_not_a_successful_detach_or_clock_reset() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let queues = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        let hold = queues.state.lock().unwrap();
        assert!(matches!(
            queues.close_publishing_admission(),
            Err(BindingError::Busy)
        ));
        drop(hold);
        let sample = codec.sample(&[1], 0, 0, 1024, 48_000, false).unwrap();
        assert!(queues.push(sample.clone(), 10).unwrap().publishing_queued);
        queues.close_publishing_admission().unwrap();
        assert!(matches!(
            queues.push(sample.clone(), 9),
            Err(BindingError::ClockWentBackwards)
        ));
        assert!(matches!(
            queues.push(sample, 11),
            Err(BindingError::InvalidMedia)
        ));
        assert_eq!(queues.stats().unwrap().recording_bytes, 1);
        let next = codec.sample(&[2], 1024, 1024, 1024, 48_000, false).unwrap();
        let accepted = queues.push(next, 11).unwrap();
        assert!(!accepted.publishing_queued && accepted.recording_queued);
    }

    #[test]
    fn native_fanout_shares_payload_and_cancellation_releases_it() {
        let codec = MediaCodec::aac(&[0x11, 0x90]).unwrap();
        let sample = codec.sample(&[1, 2, 3], 0, 0, 1024, 48_000, false).unwrap();
        let queues = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        let outcome = queues.push(sample.clone(), 0).unwrap();
        assert!(outcome.publishing_queued && outcome.recording_queued);
        let a = queues.pop_publishing(1).unwrap().unwrap();
        let b = queues.pop_recording(1).unwrap().unwrap();
        assert!(Arc::ptr_eq(a.sample.payload(), b.sample.payload()));
        queues.cancel().unwrap();
        assert!(matches!(queues.push(sample, 2), Err(BindingError::Closed)));
    }
    #[test]
    fn queue_diagnostics_include_discarded_backlog_and_overflow_reason() {
        let queues = NativeSampleFanout::new(1, 1024, 1000, false).unwrap();
        let codec = MediaCodec::aac(&[0x11, 0x90]).unwrap();
        let unit = |tick| codec.sample(&[1], tick, tick, 1024, 48_000, false).unwrap();
        queues.push(unit(0), 0).unwrap();
        let reset = queues.push(unit(1024), 1).unwrap();
        assert!(reset.publishing_queued);
        assert_eq!(reset.publishing_overflows, 1);
        let stats = queues.stats().unwrap();
        assert_eq!(stats.publishing_drops, 1);
        assert_eq!(stats.publishing_overflow_bytes, 1);
        assert_eq!(stats.publishing_bytes, 1);
        assert_eq!(stats.publishing_high_water_bytes, 1);
        assert_eq!(stats.publishing_media_ms, 21);
        assert!(queues.pop_publishing(1_000_000_001).unwrap().is_none());
        let stats = queues.stats().unwrap();
        assert_eq!(stats.publishing_overflows, 2);
        assert_eq!(stats.publishing_overflow_residence, 1);
        assert_eq!(stats.publishing_drops, 2);
        assert_eq!(stats.publishing_bytes, 0);
        assert_eq!(stats.publishing_media_ms, 0);
        assert_eq!(stats.publishing_high_water_media_ms, 21);
    }

    #[test]
    fn busy_capture_ingress_never_blocks_and_clock_failures_are_atomic() {
        let queues = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        let held = queues.state.lock().unwrap();
        assert!(matches!(queues.stats(), Err(BindingError::Busy)));
        drop(held);
        let codec = MediaCodec::aac(&[0x11, 0x90]).unwrap();
        let sample = codec.sample(&[1], 0, 0, 1024, 48_000, false).unwrap();
        queues.pop_recording(10).unwrap();
        assert!(matches!(
            queues.push(sample.clone(), 9),
            Err(BindingError::ClockWentBackwards)
        ));
        assert!(queues.push(sample, 10).is_ok());
    }
}
