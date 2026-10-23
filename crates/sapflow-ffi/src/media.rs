//! Immutable sample handles and bounded, non-blocking native ingress.
use crate::{BindingError, MAX_ENCODED_BUFFER_BYTES};
use sapflow_client::media::{AacSampleTimingViews, AacSourceProvenance, SampleContinuity};
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

/// Unattested caller observations plus the unchanged original validated timing.
/// This is not a native AU count, timing/trim receipt, EOF or codec certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct NativeAacSampleSourceInfo {
    pub producer_generation: u64,
    pub native_pts_us: i64,
    pub original_pts: i64,
    pub original_dts: i64,
    pub original_duration: u32,
    pub original_timescale: u32,
}
impl From<&AacSourceProvenance> for NativeAacSampleSourceInfo {
    fn from(source: &AacSourceProvenance) -> Self {
        let timing = source.original().timing();
        Self {
            producer_generation: source.producer_generation(),
            native_pts_us: source.native_pts_us(),
            original_pts: timing.pts(),
            original_dts: timing.dts(),
            original_duration: timing.duration(),
            original_timescale: timing.timescale(),
        }
    }
}

/// Opaque paired timing views from ONE existing validated original AAC handle.
#[derive(uniffi::Object)]
pub struct NativeAacTimingViews {
    views: AacSampleTimingViews,
    source: NativeAacSampleSourceInfo,
}
#[uniffi::export]
impl NativeAacTimingViews {
    /// Cold inspection only. Opt-in fanout does not require per-unit FFI queries.
    pub fn source_info(&self) -> NativeAacSampleSourceInfo {
        self.source
    }
    pub fn recording_sample(&self) -> Arc<MediaSample> {
        Arc::new(MediaSample {
            sample: self.views.recording().clone(),
        })
    }
    pub fn publishing_sample(&self) -> Option<Arc<MediaSample>> {
        self.views.publishing().map(|sample| {
            Arc::new(MediaSample {
                sample: sample.clone(),
            })
        })
    }
}

#[uniffi::export]
impl MediaSample {
    /// Narrow AAC-LC48kmono1024 scalar timing views. Source context is reported
    /// by the caller; output packing/head/tail facts still need qualification.
    pub fn aac_timing_views(
        &self,
        producer_generation: u64,
        native_pts_us: i64,
        publishing_pts: Option<i64>,
    ) -> Result<Arc<NativeAacTimingViews>, BindingError> {
        let views = AacSampleTimingViews::new(
            self.sample.clone(),
            producer_generation,
            native_pts_us,
            publishing_pts,
        )
        .map_err(|_| BindingError::InvalidMedia)?;
        let original = self.sample.timing();
        Ok(Arc::new(NativeAacTimingViews {
            views,
            source: NativeAacSampleSourceInfo {
                producer_generation,
                native_pts_us,
                original_pts: original.pts(),
                original_dts: original.dts(),
                original_duration: original.duration(),
                original_timescale: original.timescale(),
            },
        }))
    }
    /// Optional cold provenance survives queue pop. Never required per AU by
    /// the paired fanout API and never a native generation receipt.
    pub fn aac_source_info(&self) -> Option<NativeAacSampleSourceInfo> {
        self.sample
            .aac_source()
            .map(NativeAacSampleSourceInfo::from)
    }
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
    aac_timing: Option<AacTimingState>,
    last_now: Option<u64>,
    closed: bool,
}
#[derive(Debug, Default, Clone)]
struct AacTimingState {
    publishing: SampleContinuity,
    recording: SampleContinuity,
    generation: Option<u64>,
    next_recording_tick: Option<i64>,
}
impl AacTimingState {
    fn validate_views(&mut self, views: &AacSampleTimingViews) -> Result<(), BindingError> {
        let recording = views.recording();
        let source = recording.aac_source().ok_or(BindingError::InvalidMedia)?;
        let original = source.original();
        if source.producer_generation() == 0
            || original.aac_source().is_some()
            || recording.timing() != original.timing()
            || !Arc::ptr_eq(recording.payload(), original.payload())
            || !Arc::ptr_eq(recording.config(), original.config())
            || self
                .generation
                .is_some_and(|prior| source.producer_generation() < prior)
            || self
                .next_recording_tick
                .is_some_and(|tick| original.timing().dts() != tick)
        {
            return Err(BindingError::InvalidMedia);
        }
        if let Some(publishing) = views.publishing() {
            let published_source = publishing.aac_source().ok_or(BindingError::InvalidMedia)?;
            if !Arc::ptr_eq(published_source.original(), original)
                || published_source.producer_generation() != source.producer_generation()
                || published_source.native_pts_us() != source.native_pts_us()
                || !Arc::ptr_eq(publishing.payload(), original.payload())
                || !Arc::ptr_eq(publishing.config(), original.config())
            {
                return Err(BindingError::InvalidMedia);
            }
            self.publishing
                .validate(publishing)
                .map_err(|_| BindingError::InvalidMedia)?;
        }
        self.recording
            .validate(recording)
            .map_err(|_| BindingError::InvalidMedia)?;
        self.next_recording_tick = Some(
            original
                .timing()
                .dts()
                .checked_add(1024)
                .ok_or(BindingError::InvalidMedia)?,
        );
        self.generation = Some(source.producer_generation());
        Ok(())
    }
}
impl State {
    fn clock(&mut self, now_ns: u64) -> Result<(), BindingError> {
        if self.last_now.is_some_and(|last| now_ns < last) {
            return Err(BindingError::ClockWentBackwards);
        }
        self.last_now = Some(now_ns);
        Ok(())
    }
    fn check_timing_clock(&self, now_ns: u64) -> Result<(), BindingError> {
        if self.last_now.is_some_and(|last| now_ns < last) {
            Err(BindingError::ClockWentBackwards)
        } else {
            Ok(())
        }
    }
    fn timing_outcome(
        &mut self,
        publishing: Option<EnqueueOutcome>,
        recording: EnqueueOutcome,
    ) -> FanoutOutcome {
        let queued = |value| {
            matches!(
                value,
                EnqueueOutcome::Queued | EnqueueOutcome::QueuedAfterReset { .. }
            )
        };
        FanoutOutcome {
            publishing_queued: publishing.is_some_and(queued),
            recording_queued: queued(recording),
            publishing_waiting_keyframe: self.fanout.publishing().waiting_for_keyframe(),
            publishing_overflows: self.fanout.publishing().stats().overflow_events,
            recording_failed: self.fanout.recording().recording_failure().is_some(),
        }
    }
    fn push_timing_video(
        &mut self,
        sample: &SharedSample,
        now_ns: u64,
    ) -> Result<FanoutOutcome, BindingError> {
        if !sample.is_video() {
            return Err(BindingError::InvalidMedia);
        }
        self.check_timing_clock(now_ns)?;
        let mut next = self
            .aac_timing
            .as_ref()
            .ok_or(BindingError::InvalidMedia)?
            .clone();
        next.publishing
            .validate(sample)
            .map_err(|_| BindingError::InvalidMedia)?;
        next.recording
            .validate(sample)
            .map_err(|_| BindingError::InvalidMedia)?;
        let (publishing, recording) = self
            .fanout
            .push(sample.clone(), now_ns)
            .map_err(|_| BindingError::ClockWentBackwards)?;
        self.aac_timing = Some(next);
        self.last_now = Some(now_ns);
        Ok(self.timing_outcome(Some(publishing), recording))
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
                aac_timing: None,
                last_now: None,
                closed: false,
            }),
        }))
    }
    /// Explicit mode: paired AAC views share the EXISTING two queues. Ordinary
    /// push remains available for shared video; every AAC input requires views.
    /// The default constructor retains its existing shared-timing queue policy.
    #[uniffi::constructor]
    pub fn with_aac_timing_views(
        publishing_bytes: u64,
        recording_bytes: u64,
        max_media_ms: u32,
        has_video: bool,
    ) -> Result<Arc<Self>, BindingError> {
        let queues = Self::new(publishing_bytes, recording_bytes, max_media_ms, has_video)?;
        queues.state()?.aac_timing = Some(AacTimingState::default());
        Ok(queues)
    }
    /// Scalar/profile/clock/continuity checks are transactional: invalid views
    /// cannot advance either queue or its source cursor. Context is unattested.
    pub fn push_aac_timing_views(
        &self,
        views: Arc<NativeAacTimingViews>,
        now_ns: u64,
    ) -> Result<FanoutOutcome, BindingError> {
        let mut state = self.state()?;
        if state.closed {
            return Err(BindingError::Closed);
        }
        state.check_timing_clock(now_ns)?;
        let mut next = state
            .aac_timing
            .as_ref()
            .ok_or(BindingError::InvalidMedia)?
            .clone();
        next.validate_views(&views.views)?;
        let (publishing, recording) = state
            .fanout
            .push_aac_timing_views(&views.views, now_ns)
            .map_err(|_| BindingError::ClockWentBackwards)?;
        state.aac_timing = Some(next);
        state.last_now = Some(now_ns);
        Ok(state.timing_outcome(publishing, recording))
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
        if state.aac_timing.is_some() {
            return state.push_timing_video(&sample.sample, now_ns);
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
        if let Some(timing) = state.aac_timing.as_mut() {
            *timing = AacTimingState::default();
        }
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

    fn mono_views(
        codec: &MediaCodec,
        tick: i64,
        generation: u64,
        publishing: Option<i64>,
    ) -> Arc<NativeAacTimingViews> {
        codec
            .sample(&[1, 2], tick, tick, 1024, 48_000, false)
            .unwrap()
            .aac_timing_views(generation, -17, publishing)
            .unwrap()
    }
    #[test]
    fn paired_timing_pop_retains_source_identity_and_one_payload_after_input_drop() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let original = codec
            .sample(&[1, 2, 3], 4096, 4096, 1024, 48_000, false)
            .unwrap();
        let payload = Arc::downgrade(original.sample.payload());
        let views = original.aac_timing_views(9, i64::MIN, Some(0)).unwrap();
        let info = views.source_info();
        assert_eq!(info.original_pts, 4096);
        assert_eq!(info.native_pts_us, i64::MIN);
        assert!(original.aac_source_info().is_none());
        let queues = NativeSampleFanout::with_aac_timing_views(1024, 1024, 1000, false).unwrap();
        queues.push_aac_timing_views(views.clone(), 0).unwrap();
        drop(original);
        drop(views);
        let publishing = queues.pop_publishing(1).unwrap().unwrap();
        let recording = queues.pop_recording(1).unwrap().unwrap();
        assert_eq!(publishing.pts(), 0);
        assert_eq!(recording.pts(), 4096);
        assert_eq!(publishing.aac_source_info(), Some(info));
        assert_eq!(recording.aac_source_info(), Some(info));
        assert!(Arc::ptr_eq(
            publishing.sample.payload(),
            recording.sample.payload()
        ));
        assert!(Arc::ptr_eq(
            publishing.sample.payload(),
            &payload.upgrade().unwrap()
        ));
        assert!(recording.aac_timing_views(9, 0, Some(0)).is_err());
        drop(publishing);
        drop(recording);
        assert!(payload.upgrade().is_none());
    }
    #[test]
    fn paired_timing_is_opt_in_and_invalid_mode_does_not_advance_clocks() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let original = codec.sample(&[1, 2], 0, 0, 1024, 48_000, false).unwrap();
        let views = original.aac_timing_views(1, -1, Some(0)).unwrap();
        let legacy = NativeSampleFanout::new(1024, 1024, 1000, false).unwrap();
        assert!(matches!(
            legacy.push_aac_timing_views(views.clone(), 100),
            Err(BindingError::InvalidMedia)
        ));
        assert_eq!(legacy.stats().unwrap().recording_bytes, 0);
        assert!(legacy.push(original.clone(), 0).unwrap().publishing_queued);
        let paired = NativeSampleFanout::with_aac_timing_views(1024, 1024, 1000, false).unwrap();
        assert!(matches!(
            paired.push(original, 100),
            Err(BindingError::InvalidMedia)
        ));
        assert_eq!(paired.stats().unwrap().recording_bytes, 0);
        assert!(
            paired
                .push_aac_timing_views(views, 0)
                .unwrap()
                .recording_queued
        );
    }
    #[test]
    fn paired_timing_rejections_are_transactional_for_both_branches_and_source_cursor() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let different = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let queues = NativeSampleFanout::with_aac_timing_views(1024, 1024, 1000, false).unwrap();
        queues
            .push_aac_timing_views(mono_views(&codec, 0, 2, Some(0)), 0)
            .unwrap();
        for bad in [
            mono_views(&codec, 1024, 2, Some(0)),
            mono_views(&codec, 3072, 2, Some(1024)),
            mono_views(&codec, 1024, 1, Some(1024)),
            mono_views(&different, 1024, 2, Some(1024)),
        ] {
            assert!(matches!(
                queues.push_aac_timing_views(bad, 100),
                Err(BindingError::InvalidMedia)
            ));
            let state = queues.stats().unwrap();
            assert_eq!(state.publishing_bytes, 2);
            assert_eq!(state.recording_bytes, 2);
        }
        let next = queues
            .push_aac_timing_views(mono_views(&codec, 1024, 2, Some(1024)), 1)
            .unwrap();
        assert!(next.publishing_queued && next.recording_queued);
        let third = queues
            .push_aac_timing_views(mono_views(&codec, 2048, 3, None), 2)
            .unwrap();
        assert!(!third.publishing_queued && third.recording_queued);
        let state = queues.stats().unwrap();
        assert_eq!(state.publishing_drops, 0);
        assert_eq!(state.recording_bytes, 6);
    }
    #[test]
    fn paired_timing_busy_selective_fences_and_cancel_preserve_accepted_owners() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let queues = NativeSampleFanout::with_aac_timing_views(1024, 1024, 1000, false).unwrap();
        let first = mono_views(&codec, 0, 1, Some(0));
        let held = queues.state.lock().unwrap();
        assert!(matches!(
            queues.push_aac_timing_views(first.clone(), 0),
            Err(BindingError::Busy)
        ));
        drop(held);
        queues.push_aac_timing_views(first, 0).unwrap();
        queues.close_publishing_admission().unwrap();
        let second = queues
            .push_aac_timing_views(mono_views(&codec, 1024, 1, Some(1024)), 1)
            .unwrap();
        assert!(!second.publishing_queued && second.recording_queued);
        queues.close_recording_admission().unwrap();
        let third = queues
            .push_aac_timing_views(mono_views(&codec, 2048, 1, None), 2)
            .unwrap();
        assert!(!third.publishing_queued && !third.recording_queued && !third.recording_failed);
        assert!(queues.pop_publishing(2).unwrap().is_some());
        assert!(queues.pop_publishing(2).unwrap().is_none());
        assert!(queues.pop_recording(2).unwrap().is_some());
        assert!(queues.pop_recording(2).unwrap().is_some());
        assert!(queues.pop_recording(2).unwrap().is_none());
        queues.cancel().unwrap();
        assert!(matches!(
            queues.push_aac_timing_views(mono_views(&codec, 3072, 1, Some(2048)), 3),
            Err(BindingError::Closed)
        ));
    }
    #[test]
    fn paired_timing_profile_errors_and_recording_failure_never_change_publishing_policy() {
        let stereo = MediaCodec::aac(&[0x11, 0x90]).unwrap();
        let stereo_sample = stereo.sample(&[1], 0, 0, 1024, 48_000, false).unwrap();
        assert!(stereo_sample.aac_timing_views(1, 0, Some(0)).is_err());
        let codec = MediaCodec::aac(&[0x11, 0x88]).unwrap();
        let original = codec.sample(&[1, 2], 0, 0, 1024, 48_000, false).unwrap();
        assert!(original.aac_timing_views(0, 0, Some(0)).is_err());
        assert!(original.aac_timing_views(1, 0, Some(-1)).is_err());
        assert!(original.aac_timing_views(1, 0, Some(i64::MAX)).is_err());
        let queues = NativeSampleFanout::with_aac_timing_views(1024, 1, 1000, false).unwrap();
        let outcome = queues
            .push_aac_timing_views(original.aac_timing_views(1, 0, Some(0)).unwrap(), 0)
            .unwrap();
        assert!(outcome.publishing_queued && !outcome.recording_queued && outcome.recording_failed);
        assert_eq!(queues.stats().unwrap().publishing_bytes, 2);
        assert_eq!(queues.stats().unwrap().recording_bytes, 0);
    }
    #[test]
    fn paired_mode_preserves_shared_video_and_rejects_non_idr_before_any_mutation() {
        let codec = MediaCodec::h264(
            &[
                0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0,
                0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
            ],
            &[0x68, 0xef, 0xbc, 0x80],
        )
        .unwrap();
        let queues = NativeSampleFanout::with_aac_timing_views(1024, 1024, 1000, true).unwrap();
        let wrong = codec
            .sample(&[0, 0, 0, 2, 0x41, 0x80], 0, 0, 1, 30, false)
            .unwrap();
        assert!(matches!(
            queues.push(wrong, 100),
            Err(BindingError::InvalidMedia)
        ));
        let idr = codec
            .sample(&[0, 0, 0, 2, 0x65, 0x80], 0, 0, 1, 30, true)
            .unwrap();
        assert!(idr.aac_timing_views(1, 0, Some(0)).is_err());
        assert!(queues.push(idr.clone(), 0).unwrap().publishing_queued);
        let a = queues.pop_publishing(1).unwrap().unwrap();
        let b = queues.pop_recording(1).unwrap().unwrap();
        assert!(Arc::ptr_eq(a.sample.payload(), b.sample.payload()));
        assert!(Arc::ptr_eq(a.sample.payload(), idr.sample.payload()));
        assert!(a.aac_source_info().is_none());
    }
}
