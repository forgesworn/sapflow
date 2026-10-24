//! Checked geometry of a programme selected from original native AAC output.
//!
//! These scalar checks grant no codec, source-consumption, native EOF, file or
//! application authority. The platform owns those facts and the actual barriers.
//! A segment contains one native generation. Legacy converter receipts are
//! deliberately separate: queue acceptance is not converter consumption.

use crate::recording_timing::{
    AAC_FRAMES, MOVIE_TIMESCALE, RecordingPresentationExtent, TimingError,
};

fn add(a: u64, b: u64) -> Result<u64, TimingError> {
    a.checked_add(b).ok_or(TimingError::Overflow)
}

fn media_time(value: u64) -> Result<u64, TimingError> {
    if value > i64::MAX as u64 {
        Err(TimingError::Overflow)
    } else {
        Ok(value)
    }
}

/// Successful native input queue acceptance at 48 kHz, not consumed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedProgrammeFrames {
    /// Measurement, guard and alignment frames before programme admission.
    pub preparation: u64,
    /// Programme frames supplied by the admitted raw producer.
    pub programme_raw: u64,
    /// Programme frames supplied by the intentional-zero producer.
    pub programme_zero: u64,
    /// Generated block alignment after the programme.
    pub block_padding: u64,
    /// Additional generated codec drain after block alignment.
    pub codec_drain: u64,
}

impl QueuedProgrammeFrames {
    /// Raw plus intentional-zero programme; excludes preparation/pad/drain.
    pub fn programme_frames(self) -> Result<u64, TimingError> {
        add(self.programme_raw, self.programme_zero)
    }

    /// All successfully queued physical input, without claiming consumption.
    pub fn total_queued(self) -> Result<u64, TimingError> {
        add(
            add(self.preparation, self.programme_frames()?)?,
            add(self.block_padding, self.codec_drain)?,
        )
    }
}

/// Frozen caller-supplied source/coded/movie mapping for one generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeWindowStart {
    /// Nonzero exact native generation identity, not a count of file joins.
    pub generation: u64,
    /// Original coded-frame origin at 48 kHz, never rebased at selection.
    pub coded_origin: u64,
    /// Actual successfully queued preparation frames before programme Start.
    pub preparation_frames: u64,
    /// Caller-reported measured source-to-coded displacement; not certified here.
    pub measured_displacement: u64,
    /// Whole original coded frames deliberately discarded before this recording.
    pub discard_prefix_frames: u64,
    /// Actual audio programme delay in the 720-kHz movie clock, separate from DTS.
    pub movie_delay_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckedStart {
    value: ProgrammeWindowStart,
    programme_start: u64,
    coded_start: u64,
    head: u32,
}

impl CheckedStart {
    fn new(value: ProgrammeWindowStart) -> Result<Self, TimingError> {
        if value.generation == 0 || !value.discard_prefix_frames.is_multiple_of(AAC_FRAMES) {
            return Err(TimingError::Inconsistent);
        }
        let programme_start = add(value.preparation_frames, value.measured_displacement)?;
        let head = programme_start
            .checked_sub(value.discard_prefix_frames)
            .ok_or(TimingError::Inconsistent)?;
        if head > 8191 {
            return Err(TimingError::Unsupported);
        }
        let coded_start = media_time(add(value.coded_origin, value.discard_prefix_frames)?)?;
        media_time(add(value.coded_origin, programme_start)?)?;
        Ok(Self {
            value,
            programme_start,
            coded_start,
            head: head as u32,
        })
    }
}

/// Exact contiguous retained span, supplied from existing muxer sample counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedProgrammeSpan {
    /// First retained original DTS at 48 kHz.
    pub first_dts: u64,
    /// Exclusive original coded endpoint, including the final coded duration.
    pub end_dts: u64,
    /// Number of actually accepted contiguous original 1,024-frame AUs.
    pub access_units: u64,
}

/// Caller-supplied generation-end facts; native EOF authority is external.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeGenerationEnd {
    /// Exact identity of the native generation begun by this ledger.
    pub generation: u64,
    /// Actual successful queue acceptances, each source phase kept distinct.
    pub queued: QueuedProgrammeFrames,
    /// Actual full original coded extent before prefix/suffix selection.
    pub coded_frames: u64,
}

/// Arithmetic-checked selection, not a native, file or application capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckedProgrammeWindow {
    start: CheckedStart,
    end: ProgrammeGenerationEnd,
    selected_end: u64,
    selected_frames: u64,
    suffix: u64,
    programme_frames: u64,
    tail: u32,
}

impl CheckedProgrammeWindow {
    /// Exact caller-supplied generation identity.
    pub fn generation(self) -> u64 {
        self.start.value.generation
    }
    /// Successful queued-input components; not consumed-source facts.
    pub fn queued(self) -> QueuedProgrammeFrames {
        self.end.queued
    }
    /// Whole original coded generation extent, before any selection.
    pub fn coded_frames(self) -> u64 {
        self.end.coded_frames
    }
    /// Discarded whole original coded prefix.
    pub fn discarded_prefix_frames(self) -> u64 {
        self.start.value.discard_prefix_frames
    }
    /// Exclusive retained endpoint in the generation's coded coordinate.
    pub fn selected_end(self) -> u64 {
        self.selected_end
    }
    /// Retained original coded extent, including preroll and final padding.
    pub fn selected_frames(self) -> u64 {
        self.selected_frames
    }
    /// Original whole coded suffix excluded from this recording.
    pub fn discarded_suffix_frames(self) -> u64 {
        self.suffix
    }
    /// Raw plus intentional-zero programme, excluding all other source phases.
    pub fn programme_frames(self) -> u64 {
        self.programme_frames
    }
    /// Retained preroll before programme presentation, at most 8,191 frames.
    pub fn leading_frames(self) -> u32 {
        self.start.head
    }
    /// Final selected coded padding, below one AU; not all queued drain frames.
    pub fn trailing_frames(self) -> u32 {
        self.tail
    }
    /// Complete checked window presentation, without certifying native EOF.
    pub fn presentation(self) -> ProgrammeRecordingPresentation {
        ProgrammeRecordingPresentation {
            coded_start: self.start.coded_start,
            coded_frames: self.selected_frames,
            leading_frames: self.start.head,
            trailing_frames: self.tail,
            movie_delay_ticks: self.start.value.movie_delay_ticks,
            completion: ProgrammePresentationCompletion::CompleteWindow,
        }
    }

    /// Read-only unknown-tail prefix of this exact completed scalar window.
    /// This does not reopen the ledger or issue another complete window.
    pub fn prefix_presentation(
        self,
        observed: ObservedProgrammeSpan,
    ) -> Result<ProgrammeRecordingPresentation, TimingError> {
        if checked_span(self.start, observed)? > self.selected_frames {
            return Err(TimingError::Inconsistent);
        }
        prefix_presentation(self.start, observed)
    }
}

/// Scalar-only, constant-space ledger for one native generation per segment.
/// Failed operations leave its metadata unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgrammeWindowLedger {
    start: Option<CheckedStart>,
    complete: Option<CheckedProgrammeWindow>,
}

impl ProgrammeWindowLedger {
    /// Freeze the mapping before the first accepted AU. No native fact is minted.
    pub fn begin(
        &mut self,
        start: ProgrammeWindowStart,
        observed_units: u64,
    ) -> Result<(), TimingError> {
        if self.complete.is_some() {
            return Err(TimingError::Closed);
        }
        if self.start.is_some() || observed_units != 0 {
            return Err(TimingError::Inconsistent);
        }
        let checked = CheckedStart::new(start)?;
        self.start = Some(checked);
        Ok(())
    }

    /// Explicitly incomplete retained prefix; no final tail/EOF is inferred.
    pub fn prefix_presentation(
        &self,
        observed: ObservedProgrammeSpan,
    ) -> Result<ProgrammeRecordingPresentation, TimingError> {
        if self.complete.is_some() {
            return Err(TimingError::Closed);
        }
        let start = self.start.ok_or(TimingError::Inconsistent)?;
        prefix_presentation(start, observed)
    }

    /// Check source/window conservation against the actually retained span.
    /// Native EOF, selected-window decoding and source truth remain external.
    pub fn complete(
        &mut self,
        end: ProgrammeGenerationEnd,
        observed: ObservedProgrammeSpan,
    ) -> Result<CheckedProgrammeWindow, TimingError> {
        if self.complete.is_some() {
            return Err(TimingError::Closed);
        }
        let start = self.start.ok_or(TimingError::Inconsistent)?;
        if end.generation != start.value.generation
            || end.queued.preparation != start.value.preparation_frames
            || end.coded_frames == 0
            || !end.coded_frames.is_multiple_of(AAC_FRAMES)
        {
            return Err(TimingError::Inconsistent);
        }
        end.queued.total_queued()?;
        let programme_frames = end.queued.programme_frames()?;
        if programme_frames == 0 {
            return Err(TimingError::Inconsistent);
        }
        let programme_end = add(start.programme_start, programme_frames)?;
        let remainder = programme_end % AAC_FRAMES;
        let selected_end = if remainder == 0 {
            programme_end
        } else {
            add(programme_end, AAC_FRAMES - remainder)?
        };
        let selected_frames = selected_end
            .checked_sub(start.value.discard_prefix_frames)
            .filter(|frames| *frames > 0)
            .ok_or(TimingError::Inconsistent)?;
        let suffix = end
            .coded_frames
            .checked_sub(selected_end)
            .ok_or(TimingError::Inconsistent)?;
        let tail = selected_end
            .checked_sub(programme_end)
            .filter(|frames| *frames < AAC_FRAMES)
            .ok_or(TimingError::Inconsistent)?;
        if checked_span(start, observed)? != selected_frames
            || observed.end_dts != add(start.value.coded_origin, selected_end)?
            || selected_frames
                .checked_sub(u64::from(start.head))
                .and_then(|frames| frames.checked_sub(tail))
                != Some(programme_frames)
            || add(
                add(start.value.discard_prefix_frames, selected_frames)?,
                suffix,
            )? != end.coded_frames
        {
            return Err(TimingError::Inconsistent);
        }
        media_time(add(start.value.coded_origin, end.coded_frames)?)?;
        let checked = CheckedProgrammeWindow {
            start,
            end,
            selected_end,
            selected_frames,
            suffix,
            programme_frames,
            tail: tail as u32,
        };
        checked.presentation().movie_extent()?;
        self.complete = Some(checked);
        Ok(checked)
    }

    /// Last successfully checked window, without implied native/file authority.
    pub fn completed(&self) -> Option<&CheckedProgrammeWindow> {
        self.complete.as_ref()
    }
}

fn prefix_presentation(
    start: CheckedStart,
    observed: ObservedProgrammeSpan,
) -> Result<ProgrammeRecordingPresentation, TimingError> {
    let value = ProgrammeRecordingPresentation {
        coded_start: start.coded_start,
        coded_frames: checked_span(start, observed)?,
        leading_frames: start.head,
        trailing_frames: 0,
        movie_delay_ticks: start.value.movie_delay_ticks,
        completion: ProgrammePresentationCompletion::IncompletePrefix,
    };
    value.movie_extent()?;
    Ok(value)
}

fn checked_span(start: CheckedStart, observed: ObservedProgrammeSpan) -> Result<u64, TimingError> {
    let frames = observed
        .access_units
        .checked_mul(AAC_FRAMES)
        .ok_or(TimingError::Overflow)?;
    let end = media_time(add(start.coded_start, frames)?)?;
    if observed.first_dts != start.coded_start || observed.end_dts != end {
        return Err(TimingError::Inconsistent);
    }
    Ok(frames)
}

/// Distinguish an unknown-EOF prefix from complete scalar window geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgrammePresentationCompletion {
    /// Retained coded prefix; no final padding has been removed.
    IncompletePrefix,
    /// Complete window arithmetic checked, with native truth still external.
    CompleteWindow,
}

/// Header geometry with independent original media and movie programme origins.
/// Private fields prevent an unchecked public struct literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeRecordingPresentation {
    coded_start: u64,
    coded_frames: u64,
    leading_frames: u32,
    trailing_frames: u32,
    movie_delay_ticks: u64,
    completion: ProgrammePresentationCompletion,
}

impl ProgrammeRecordingPresentation {
    /// First retained original coded DTS, never selection-normalised.
    pub fn coded_start(self) -> u64 {
        self.coded_start
    }
    /// Full retained coded frames, including head and final coded padding.
    pub fn coded_frames(self) -> u64 {
        self.coded_frames
    }
    /// Retained measured head removed by the media edit.
    pub fn leading_frames(self) -> u32 {
        self.leading_frames
    }
    /// Checked complete-window tail; zero is only a convention for incomplete.
    pub fn trailing_frames(self) -> u32 {
        self.trailing_frames
    }
    /// Movie delay from the explicit programme origin, in 720-kHz ticks.
    pub fn movie_delay_ticks(self) -> u64 {
        self.movie_delay_ticks
    }
    /// Classification of geometry, not native EOF or file completion authority.
    pub fn completion(self) -> ProgrammePresentationCompletion {
        self.completion
    }
    /// Positive presented extent; a short retained head can present zero frames.
    pub fn presented_frames(self) -> u64 {
        self.coded_frames
            .saturating_sub(u64::from(self.leading_frames) + u64::from(self.trailing_frames))
    }
    /// Original media edit start at 48 kHz, including retained measured head.
    pub fn media_time(self) -> u64 {
        // Construction checks this same sum before the private value escapes.
        self.coded_start + u64::from(self.leading_frames)
    }
    /// Exact header duration with the separate explicit programme movie delay.
    pub fn movie_extent(self) -> Result<RecordingPresentationExtent, TimingError> {
        let audio = self
            .presented_frames()
            .checked_mul(u64::from(MOVIE_TIMESCALE / 48_000))
            .ok_or(TimingError::Overflow)?;
        let duration_ticks = add(self.movie_delay_ticks, audio)?;
        let ticks_per_ms = u64::from(MOVIE_TIMESCALE / 1000);
        let duration_ms_ceil = add(
            duration_ticks / ticks_per_ms,
            u64::from(!duration_ticks.is_multiple_of(ticks_per_ms)),
        )?;
        Ok(RecordingPresentationExtent {
            movie_timescale: MOVIE_TIMESCALE,
            duration_ticks,
            duration_ms_ceil,
        })
    }
}
