//! Independent publishing/recording backlogs with explicit overflow policy.
//!
//! Time is supplied by the scheduler as monotonic nanoseconds. Call `tick`
//! even when capture stops, so residence deadlines are enforced while idle.

use std::collections::VecDeque;

use crate::SharedSample;
use crate::media::AacSampleTimingViews;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    max_bytes: usize,
    max_samples: usize,
    max_media_ns: u64,
    max_residence_ns: u64,
}
impl QueueLimits {
    pub fn new(
        max_bytes: usize,
        max_samples: usize,
        max_media_ns: u64,
        max_residence_ns: u64,
    ) -> Result<Self, QueueError> {
        if max_bytes == 0
            || max_samples == 0
            || max_samples > 65_536
            || max_media_ns == 0
            || max_residence_ns == 0
        {
            return Err(QueueError::InvalidLimits);
        }
        Ok(Self {
            max_bytes,
            max_samples,
            max_media_ns,
            max_residence_ns,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    InvalidLimits,
    ClockWentBackwards,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Flush stale network media and resume at the next IDR for video.
    Publishing { has_video: bool },
    /// Refuse further recording ingress; keep the accepted prefix for drain.
    Recording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowReason {
    Bytes,
    Samples,
    MediaDuration,
    ResidenceTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// This branch has irreversibly stopped accepting new samples. Its
    /// previously accepted prefix remains available under ordinary drain rules.
    AdmissionClosed,
    Queued,
    QueuedAfterReset {
        discarded: usize,
        reason: OverflowReason,
    },
    WaitingForKeyframe {
        discarded: usize,
        reason: Option<OverflowReason>,
    },
    Rejected {
        reason: OverflowReason,
        discarded: usize,
    },
    RecordingFailed {
        reason: OverflowReason,
    },
    RecordingAlreadyFailed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QueueStats {
    pub queued_samples: usize,
    pub queued_bytes: usize,
    /// Maximum accumulated duration of either track; concurrent A/V is not added.
    pub queued_media_ns: u64,
    pub queued_video_ns: u64,
    pub queued_audio_ns: u64,
    pub high_water_media_ns: u64,
    pub high_water_bytes: usize,
    pub high_water_samples: usize,
    pub accepted_samples: u64,
    pub dropped_samples: u64,
    pub overflow_events: u64,
    pub overflow_bytes: u64,
    pub overflow_samples: u64,
    pub overflow_media_duration: u64,
    pub overflow_residence: u64,
}

#[derive(Debug)]
struct Entry {
    sample: SharedSample,
    enqueued_ns: u64,
    media_ns: u64,
}

#[derive(Debug)]
pub struct BoundedQueue {
    limits: QueueLimits,
    policy: OverflowPolicy,
    entries: VecDeque<Entry>,
    stats: QueueStats,
    last_now: Option<u64>,
    waiting_for_keyframe: bool,
    recording_failure: Option<OverflowReason>,
    admission_closed: bool,
}

impl BoundedQueue {
    pub fn new(limits: QueueLimits, policy: OverflowPolicy) -> Self {
        Self {
            limits,
            policy,
            entries: VecDeque::new(),
            stats: QueueStats::default(),
            last_now: None,
            waiting_for_keyframe: matches!(policy, OverflowPolicy::Publishing { has_video: true }),
            recording_failure: None,
            admission_closed: false,
        }
    }
    pub fn stats(&self) -> QueueStats {
        self.stats
    }
    pub fn recording_failure(&self) -> Option<OverflowReason> {
        self.recording_failure
    }
    pub fn waiting_for_keyframe(&self) -> bool {
        self.waiting_for_keyframe
    }

    /// Stop only future ingress. This is idempotent and does not clear the
    /// accepted prefix, reset clocks or convert intentional closure to loss.
    /// Construct a new queue to begin a new admission lifetime.
    pub fn close_admission(&mut self) {
        self.admission_closed = true;
    }

    pub fn admission_closed(&self) -> bool {
        self.admission_closed
    }

    fn check_clock(&mut self, now_ns: u64) -> Result<(), QueueError> {
        if self.last_now.is_some_and(|last| now_ns < last) {
            return Err(QueueError::ClockWentBackwards);
        }
        self.last_now = Some(now_ns);
        Ok(())
    }
    fn discard(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.stats.queued_samples = 0;
        self.stats.queued_bytes = 0;
        self.stats.queued_media_ns = 0;
        self.stats.queued_video_ns = 0;
        self.stats.queued_audio_ns = 0;
        self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(count as u64);
        count
    }
    fn overflow(&mut self, reason: OverflowReason) -> usize {
        self.stats.overflow_events = self.stats.overflow_events.saturating_add(1);
        let counter = match reason {
            OverflowReason::Bytes => &mut self.stats.overflow_bytes,
            OverflowReason::Samples => &mut self.stats.overflow_samples,
            OverflowReason::MediaDuration => &mut self.stats.overflow_media_duration,
            OverflowReason::ResidenceTime => &mut self.stats.overflow_residence,
        };
        *counter = counter.saturating_add(1);
        match self.policy {
            OverflowPolicy::Publishing { has_video } => {
                self.waiting_for_keyframe = has_video;
                self.discard()
            }
            OverflowPolicy::Recording => {
                self.recording_failure = Some(reason);
                0
            }
        }
    }
    fn expired(&self, now_ns: u64) -> bool {
        self.entries
            .front()
            .is_some_and(|entry| now_ns - entry.enqueued_ns >= self.limits.max_residence_ns)
    }
    /// Enforces age without requiring another captured sample. Recording keeps
    /// its accepted prefix on failure; publishing discards expired media.
    pub fn tick(&mut self, now_ns: u64) -> Result<Option<OverflowReason>, QueueError> {
        self.check_clock(now_ns)?;
        if self.recording_failure.is_none() && self.expired(now_ns) {
            self.overflow(OverflowReason::ResidenceTime);
            Ok(Some(OverflowReason::ResidenceTime))
        } else {
            Ok(None)
        }
    }

    pub fn push(
        &mut self,
        sample: SharedSample,
        now_ns: u64,
    ) -> Result<EnqueueOutcome, QueueError> {
        self.check_clock(now_ns)?;
        if self.admission_closed {
            self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(1);
            return Ok(EnqueueOutcome::AdmissionClosed);
        }
        if self.recording_failure.is_some() {
            self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(1);
            return Ok(EnqueueOutcome::RecordingAlreadyFailed);
        }
        let media_ns = u64::from(sample.timing().duration()) * 1_000_000_000
            / u64::from(sample.timing().timescale());
        let reason = if self.expired(now_ns) {
            Some(OverflowReason::ResidenceTime)
        } else if sample.payload().len()
            > self
                .limits
                .max_bytes
                .saturating_sub(self.stats.queued_bytes)
        {
            Some(OverflowReason::Bytes)
        } else if self.entries.len() >= self.limits.max_samples {
            Some(OverflowReason::Samples)
        } else if media_ns
            > self
                .limits
                .max_media_ns
                .saturating_sub(if sample.is_video() {
                    self.stats.queued_video_ns
                } else {
                    self.stats.queued_audio_ns
                })
        {
            Some(OverflowReason::MediaDuration)
        } else {
            None
        };
        let discarded = reason.map_or(0, |reason| self.overflow(reason));
        if let Some(reason) = self.recording_failure {
            self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(1);
            return Ok(EnqueueOutcome::RecordingFailed { reason });
        }
        // Even an empty queue cannot fit an oversized unit.
        if sample.payload().len() > self.limits.max_bytes || media_ns > self.limits.max_media_ns {
            self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(1);
            return Ok(EnqueueOutcome::Rejected {
                reason: reason.unwrap_or(OverflowReason::Bytes),
                discarded,
            });
        }
        if self.waiting_for_keyframe && !(sample.is_video() && sample.is_keyframe()) {
            self.stats.dropped_samples = self.stats.dropped_samples.saturating_add(1);
            return Ok(EnqueueOutcome::WaitingForKeyframe { discarded, reason });
        }
        self.waiting_for_keyframe = false;
        self.stats.queued_bytes += sample.payload().len();
        self.stats.queued_samples += 1;
        if sample.is_video() {
            self.stats.queued_video_ns += media_ns;
        } else {
            self.stats.queued_audio_ns += media_ns;
        }
        self.stats.queued_media_ns = self.stats.queued_video_ns.max(self.stats.queued_audio_ns);
        self.stats.high_water_media_ns = self
            .stats
            .high_water_media_ns
            .max(self.stats.queued_media_ns);
        self.stats.high_water_bytes = self.stats.high_water_bytes.max(self.stats.queued_bytes);
        self.stats.high_water_samples =
            self.stats.high_water_samples.max(self.stats.queued_samples);
        self.stats.accepted_samples = self.stats.accepted_samples.saturating_add(1);
        self.entries.push_back(Entry {
            sample,
            enqueued_ns: now_ns,
            media_ns,
        });
        Ok(match reason {
            Some(reason) => EnqueueOutcome::QueuedAfterReset { discarded, reason },
            None => EnqueueOutcome::Queued,
        })
    }

    /// Drain accepted recording media even after recording failure. Publishing
    /// pop enforces residence deadlines before returning network media.
    pub fn pop(&mut self, now_ns: u64) -> Result<Option<SharedSample>, QueueError> {
        self.tick(now_ns)?;
        let Some(entry) = self.entries.pop_front() else {
            return Ok(None);
        };
        self.stats.queued_bytes -= entry.sample.payload().len();
        self.stats.queued_samples -= 1;
        if entry.sample.is_video() {
            self.stats.queued_video_ns -= entry.media_ns;
        } else {
            self.stats.queued_audio_ns -= entry.media_ns;
        }
        self.stats.queued_media_ns = self.stats.queued_video_ns.max(self.stats.queued_audio_ns);
        Ok(Some(entry.sample))
    }

    /// Cancellation drops all references held by this queue immediately.
    /// A failed recording queue stays failed; construct a new one for a new file.
    pub fn clear(&mut self) -> usize {
        self.waiting_for_keyframe =
            matches!(self.policy, OverflowPolicy::Publishing { has_video: true });
        self.discard()
    }
}

/// Single encode, two independent owners. Neither sink borrows or locks the
/// other; callers drain them on their own scheduled workers.
#[derive(Debug)]
pub struct SampleFanout {
    publishing: BoundedQueue,
    recording: BoundedQueue,
}
impl SampleFanout {
    pub fn new(publishing: QueueLimits, recording: QueueLimits, has_video: bool) -> Self {
        Self {
            publishing: BoundedQueue::new(publishing, OverflowPolicy::Publishing { has_video }),
            recording: BoundedQueue::new(recording, OverflowPolicy::Recording),
        }
    }
    pub fn push(
        &mut self,
        sample: SharedSample,
        now_ns: u64,
    ) -> Result<(EnqueueOutcome, EnqueueOutcome), QueueError> {
        // Preflight both clocks before either side changes.
        if [&self.publishing, &self.recording]
            .iter()
            .any(|q| q.last_now.is_some_and(|last| now_ns < last))
        {
            return Err(QueueError::ClockWentBackwards);
        }
        let publishing = self.publishing.push(sample.clone(), now_ns)?;
        let recording = self.recording.push(sample, now_ns)?;
        Ok((publishing, recording))
    }
    /// Explicit opt-in paired views use these same two bounded queues. The
    /// opaque pair was derived from one validated original, never two payloads.
    pub fn push_aac_timing_views(
        &mut self,
        views: &AacSampleTimingViews,
        now_ns: u64,
    ) -> Result<(Option<EnqueueOutcome>, EnqueueOutcome), QueueError> {
        if [&self.publishing, &self.recording]
            .iter()
            .any(|q| q.last_now.is_some_and(|last| now_ns < last))
        {
            return Err(QueueError::ClockWentBackwards);
        }
        let publishing = if let Some(sample) = views.publishing() {
            Some(self.publishing.push(sample.clone(), now_ns)?)
        } else {
            // Existing accepted backlog still ages. Withholding this input
            // never manufactures AdmissionClosed or a dropped-input counter.
            self.publishing.tick(now_ns)?;
            None
        };
        let recording = self.recording.push(views.recording().clone(), now_ns)?;
        Ok((publishing, recording))
    }
    pub fn publishing(&mut self) -> &mut BoundedQueue {
        &mut self.publishing
    }
    pub fn recording(&mut self) -> &mut BoundedQueue {
        &mut self.recording
    }
    pub fn clear(&mut self) {
        self.publishing.clear();
        self.recording.clear();
    }
}
