//! Checked native AAC recording facts, independent of codecs, files and clocks.
//! A file removes only its first measured head and proven final EOF tail.

/// Movie clock exactly representing both 48 kHz audio and 90 kHz video.
pub const MOVIE_TIMESCALE: u32 = 720_000;
/// One AAC-LC access unit in the supported native output format.
pub const AAC_FRAMES: u64 = 1024;

/// Invalid, unsupported or contradictory recording facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingError {
    /// A value or arithmetic result exceeds its checked representation.
    Overflow,
    /// Native format, priming or source mapping is outside this contract.
    Unsupported,
    /// The facts contradict the observed units or prior FIFO events.
    Inconsistent,
    /// A completed file no longer accepts timing events.
    Closed,
}

/// Source-to-48 kHz quantisation used by the native generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMapping {
    /// Existing native 48 kHz callers retain nearest-frame conversion.
    Nearest,
    /// A completed native resampler reports its floor output extent.
    Floor,
}

/// Exact rational source conversion, including the declared quantisation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceExtent {
    /// Representable output frames at 48 kHz.
    pub frames: u64,
    /// Signed output-frame error numerator against the exact source extent.
    pub quantisation_numerator: i64,
    /// Error denominator, the source sample rate.
    pub quantisation_denominator: u32,
}

/// Compute the native ledger's checked source mapping without floating point.
pub fn source_extent(
    input: u64,
    rate: u32,
    mapping: SourceMapping,
) -> Result<SourceExtent, TimingError> {
    if rate == 0 || rate > 192_000 {
        return Err(TimingError::Unsupported);
    }
    let scaled = input.checked_mul(48_000).ok_or(TimingError::Overflow)?;
    let divisor = u64::from(rate);
    let remainder = scaled % divisor;
    let up = mapping == SourceMapping::Nearest && remainder >= divisor.div_ceil(2);
    let frames = (scaled / divisor)
        .checked_add(u64::from(up))
        .ok_or(TimingError::Overflow)?;
    Ok(SourceExtent {
        frames,
        quantisation_numerator: if up {
            (divisor - remainder) as i64
        } else {
            -(remainder as i64)
        },
        quantisation_denominator: rate,
    })
}

/// Actual EOF facts for one native converter generation, delivered after its AUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationReceipt {
    /// One-based, strictly sequential generation within this file.
    pub generation: u64,
    /// Actual source frames consumed by the converter.
    pub input_frames: u64,
    /// Source frames from the exact raw producer.
    pub raw_frames: u64,
    /// Source frames from the exact generated-zero producer.
    pub zero_frames: u64,
    /// Actual source ASBD sample rate.
    pub input_rate: u32,
    /// Measured coded frames, including this generation's prime and padding.
    pub encoded_frames: u64,
    /// Measured native leading frames, never a device-specific constant.
    pub leading_frames: u32,
    /// Measured native final padding for this generation.
    pub trailing_frames: u32,
    /// The generation's explicitly selected source conversion rule.
    pub source_mapping: SourceMapping,
}

/// Constant-space completed generation summary for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileReceipt {
    /// Number of actual sequential generation EOF receipts.
    pub generation_count: u64,
    /// Sum of measured source extents after explicit quantisation.
    pub source_frames_at_48k: u64,
    /// Interior padding/priming deliberately retained as standby coded slots.
    pub intentional_standby_frames: u64,
    /// Sum of actual coded generation extents.
    pub encoded_frames: u64,
    /// Only the first file-leading extent is removed from presentation.
    pub first_leading_frames: u32,
    /// Only the last proven EOF tail is removed from presentation.
    pub last_trailing_frames: u32,
}

/// Whether a file has an actual, complete final native EOF receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// All generation receipts and final file summary have been checked.
    Complete,
    /// Preserve a positive accepted coded prefix without guessing final padding.
    IncompletePrefix,
}

/// Bounded FIFO ledger. Failed operations leave its metadata unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioLedger {
    leading: Option<u32>,
    extent: Option<FileReceipt>,
    previous_extent: Option<FileReceipt>,
    final_receipt: Option<FileReceipt>,
}

fn checked_leading(value: u32) -> Result<(), TimingError> {
    if value / 1024 + 1 > 8 {
        Err(TimingError::Unsupported)
    } else {
        Ok(())
    }
}
fn add(a: u64, b: u64) -> Result<u64, TimingError> {
    a.checked_add(b).ok_or(TimingError::Overflow)
}

impl AudioLedger {
    /// Install first-file native priming before the first AAC sample.
    pub fn begin(&mut self, leading: u32, observed_units: u64) -> Result<(), TimingError> {
        checked_leading(leading)?;
        if self.leading.is_some() || observed_units != 0 {
            return Err(TimingError::Inconsistent);
        }
        self.leading = Some(leading);
        Ok(())
    }
    /// First native head, once known.
    pub fn leading(&self) -> Option<u32> {
        self.leading
    }
    /// The last completed generation aggregate; not an implied final EOF.
    pub fn extent(&self) -> Option<FileReceipt> {
        self.extent
    }
    /// Checked final file receipt, if actually supplied.
    pub fn final_receipt(&self) -> Option<FileReceipt> {
        self.final_receipt
    }
    /// Fold one actual generation receipt after exactly its observed output.
    pub fn append(
        &mut self,
        value: GenerationReceipt,
        observed_units: u64,
    ) -> Result<(), TimingError> {
        if self.final_receipt.is_some() {
            return Err(TimingError::Closed);
        }
        let leading = self.leading.ok_or(TimingError::Inconsistent)?;
        checked_leading(value.leading_frames)?;
        let prior_count = self.extent.map_or(0, |x| x.generation_count);
        if value.generation != add(prior_count, 1)?
            || value.encoded_frames == 0
            || !value.encoded_frames.is_multiple_of(AAC_FRAMES)
            || value.trailing_frames >= 1024
            || add(value.raw_frames, value.zero_frames)? != value.input_frames
            || (prior_count == 0 && leading != value.leading_frames)
        {
            return Err(TimingError::Inconsistent);
        }
        let valid =
            source_extent(value.input_frames, value.input_rate, value.source_mapping)?.frames;
        if add(
            valid,
            add(
                u64::from(value.leading_frames),
                u64::from(value.trailing_frames),
            )?,
        )? != value.encoded_frames
        {
            return Err(TimingError::Inconsistent);
        }
        let interior = self.extent.map_or(0, |x| {
            u64::from(x.last_trailing_frames) + u64::from(value.leading_frames)
        });
        let next = FileReceipt {
            generation_count: value.generation,
            source_frames_at_48k: add(self.extent.map_or(0, |x| x.source_frames_at_48k), valid)?,
            intentional_standby_frames: add(
                self.extent.map_or(0, |x| x.intentional_standby_frames),
                interior,
            )?,
            encoded_frames: add(
                self.extent.map_or(0, |x| x.encoded_frames),
                value.encoded_frames,
            )?,
            first_leading_frames: leading,
            last_trailing_frames: value.trailing_frames,
        };
        if observed_units
            .checked_mul(AAC_FRAMES)
            .ok_or(TimingError::Overflow)?
            != next.encoded_frames
        {
            return Err(TimingError::Inconsistent);
        }
        let trims = u64::from(leading) + u64::from(next.last_trailing_frames);
        if next.encoded_frames <= trims
            || next.encoded_frames.checked_sub(trims)
                != Some(add(
                    next.source_frames_at_48k,
                    next.intentional_standby_frames,
                )?)
        {
            return Err(TimingError::Inconsistent);
        }
        self.previous_extent = self.extent;
        self.extent = Some(next);
        Ok(())
    }
    /// Exact generation coverage for an all-but-one-AU sealed prefix.
    /// A prior receipt is retained explicitly; never infer an unobserved EOF.
    pub fn completed_generations_through(&self, coded_frames: u64) -> Result<u64, TimingError> {
        if let Some(current) = self.extent {
            if current.encoded_frames <= coded_frames {
                return Ok(current.generation_count);
            }
            if current
                .encoded_frames
                .checked_sub(AAC_FRAMES)
                .is_none_or(|x| coded_frames < x)
            {
                return Err(TimingError::Inconsistent);
            }
            if let Some(previous) = self.previous_extent {
                if previous.encoded_frames > coded_frames {
                    return Err(TimingError::Inconsistent);
                }
                return Ok(previous.generation_count);
            }
        }
        Ok(0)
    }
    /// Commit only the identical, observed aggregate after real final EOF.
    pub fn complete(&mut self, value: FileReceipt, observed_units: u64) -> Result<(), TimingError> {
        if self.final_receipt.is_some() {
            return Err(TimingError::Closed);
        }
        if self.extent != Some(value)
            || value.encoded_frames
                != observed_units
                    .checked_mul(AAC_FRAMES)
                    .ok_or(TimingError::Overflow)?
        {
            return Err(TimingError::Inconsistent);
        }
        self.final_receipt = Some(value);
        Ok(())
    }
    /// Return only a proven final tail; interrupted prefixes always retain theirs.
    pub fn trailing(&self, outcome: Completion, observed_units: u64) -> Result<u32, TimingError> {
        self.leading.ok_or(TimingError::Inconsistent)?;
        match outcome {
            Completion::IncompletePrefix => Ok(0),
            Completion::Complete => {
                let value = self.final_receipt.ok_or(TimingError::Inconsistent)?;
                if value.encoded_frames
                    != observed_units
                        .checked_mul(AAC_FRAMES)
                        .ok_or(TimingError::Overflow)?
                {
                    return Err(TimingError::Inconsistent);
                }
                Ok(value.last_trailing_frames)
            }
        }
    }
}

/// Recording-header presentation, using original media clocks and coded extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingPresentation {
    /// First original AAC DTS relative to the frozen epoch, at 48 kHz.
    pub audio_start: u64,
    /// Number of coded AAC frames represented by this header.
    pub audio_frames: u64,
    /// First-file measured native head.
    pub leading_frames: u32,
    /// Proven last EOF padding; zero for unknown-EOF recovered prefixes.
    pub trailing_frames: u32,
    /// First video DTS, if present, at 90 kHz.
    pub video_start: Option<u64>,
    /// Last original video presentation endpoint, at 90 kHz.
    pub video_end: Option<u64>,
}

impl RecordingPresentation {
    /// Check native format-independent bounds before serialising metadata.
    pub fn validate(&self) -> Result<(), TimingError> {
        checked_leading(self.leading_frames)?;
        if !self.audio_frames.is_multiple_of(AAC_FRAMES)
            || self.trailing_frames >= 1024
            || self.video_start.is_some() != self.video_end.is_some()
            || self
                .video_start
                .zip(self.video_end)
                .is_some_and(|(a, b)| a > b)
            || (self.trailing_frames > 0
                && u64::from(self.leading_frames) + u64::from(self.trailing_frames)
                    > self.audio_frames)
        {
            return Err(TimingError::Inconsistent);
        }
        let media = add(self.audio_start, u64::from(self.leading_frames))?;
        if media > i64::MAX as u64 {
            return Err(TimingError::Overflow);
        }
        self.movie_duration()?;
        Ok(())
    }
    /// Positive presented audio frames; a short incomplete coded head stays zero.
    pub fn valid_audio_frames(&self) -> u64 {
        self.audio_frames
            .saturating_sub(u64::from(self.leading_frames) + u64::from(self.trailing_frames))
    }
    /// Exact overall extent in the shared 720 kHz movie clock.
    pub fn movie_duration(&self) -> Result<u64, TimingError> {
        let audio = add(self.audio_start, self.valid_audio_frames())?
            .checked_mul(15)
            .ok_or(TimingError::Overflow)?;
        let video = self
            .video_end
            .unwrap_or(0)
            .checked_mul(8)
            .ok_or(TimingError::Overflow)?;
        Ok(audio.max(video))
    }
}
