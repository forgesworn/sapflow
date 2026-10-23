//! Native ownership boundary. Raw frames and PCM never cross this API.
//!
//! UniFFI borrows the encoder's compressed bytes for the synchronous call.
//! `EncodedBuffer` makes one bounded ownership copy and can be shared by
//! Rust recording and publishing consumers after the foreign call returns.

use std::sync::Arc;
mod media;
mod recording;
mod rtmp;
mod session;
mod srt;
pub use media::{
    FanoutOutcome, MediaCodec, MediaSample, NativeAacSampleSourceInfo, NativeAacTimingViews,
    NativeQueueStats, NativeSampleFanout,
};
pub use recording::{
    DecodedProgrammeCheckpoint, DecodedProgrammeCheckpointInfo, EncodedProgrammeCheckpointInfo,
    NativeProgrammeCheckpoint, NativeProgrammeCheckpointEncoding, NativeProgrammeCheckpointFacts,
    NativeProgrammeChunk, NativeProgrammeCompletion, NativeProgrammeGenerationEnd,
    NativeProgrammePresentationExtent, NativeProgrammeQueuedFrames, NativeProgrammeWindowStart,
    decode_programme_checkpoint, validate_programme_checkpoint_successor,
};
pub use recording::{
    DecodedRecordingCheckpoint, DecodedRecordingCheckpointInfo, EncodedRecordingCheckpointInfo,
    NativeAudioFileReceipt, NativeAudioGenerationReceipt, NativeRecordingCheckpoint,
    NativeRecordingCheckpointEncoding, NativeRecordingCheckpointFacts, NativeRecordingChunk,
    NativeRecordingChunkKind, NativeRecordingCompletion, NativeRecordingError,
    NativeRecordingMuxer, NativeRecordingPresentationExtent, NativeRecordingSourceMapping,
    NativeRecordingStats, RecordingCheckpointError, RecordingWrite, decode_recording_checkpoint,
    validate_recording_checkpoint_successor,
};
pub use rtmp::{
    NativeRtmpError, NativeRtmpEvent, NativeRtmpPublisher, NativeRtmpSendOutput, NativeRtmpState,
};
pub use session::{
    NativeMediaReadiness, NativeRetryBudgetState, NativeSession, NativeSessionAction,
    NativeSessionSnapshot, NativeSessionState, NativeStopResult, NativeTransportRecoverySource,
};
pub use srt::{
    NativeSrtCipher, NativeSrtCloseReason, NativeSrtConfig, NativeSrtDatagram, NativeSrtError,
    NativeSrtEvent, NativeSrtPublisher, NativeSrtState, NativeSrtStats,
};

uniffi::setup_scaffolding!();

pub const MAX_ENCODED_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, uniffi::Error)]
pub enum BindingError {
    EmptyBuffer,
    BufferTooLarge,
    IndexOutOfBounds,
    InvalidMedia,
    InvalidQueueLimits,
    ClockWentBackwards,
    Busy,
    Closed,
    InvalidSession,
    StaleSession,
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EmptyBuffer => "encoded buffer is empty",
            Self::BufferTooLarge => "encoded buffer exceeds its byte limit",
            Self::IndexOutOfBounds => "encoded buffer index is out of bounds",
            Self::InvalidMedia => "invalid or discontinuous compressed media",
            Self::InvalidQueueLimits => "invalid native queue limits",
            Self::ClockWentBackwards => "native monotonic clock went backwards",
            Self::Busy => "native queue is busy; capture callback must not wait",
            Self::Closed => "native sample fanout is closed",
            Self::InvalidSession => "invalid session policy or lifecycle transition",
            Self::StaleSession => "callback belongs to a superseded session",
        })
    }
}

impl std::error::Error for BindingError {}

#[derive(uniffi::Object)]
pub struct EncodedBuffer {
    bytes: Arc<[u8]>,
}

#[uniffi::export]
impl EncodedBuffer {
    #[uniffi::constructor]
    pub fn new(bytes: &[u8]) -> Result<Arc<Self>, BindingError> {
        if bytes.is_empty() {
            return Err(BindingError::EmptyBuffer);
        }
        if bytes.len() > MAX_ENCODED_BUFFER_BYTES {
            return Err(BindingError::BufferTooLarge);
        }
        Ok(Arc::new(Self {
            bytes: Arc::from(bytes),
        }))
    }

    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Bounded diagnostic access; does not copy the sample back over FFI.
    pub fn byte_at(&self, index: u64) -> Result<u8, BindingError> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.bytes.get(index))
            .copied()
            .ok_or(BindingError::IndexOutOfBounds)
    }
}

#[uniffi::export]
pub fn sdk_abi_version() -> u32 {
    1
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn foreign_buffer_can_be_released_or_changed_after_the_call() {
        let mut source = vec![1, 2, 3];
        let owned = EncodedBuffer::new(&source).unwrap();
        source.fill(0);
        drop(source);
        assert_eq!(owned.byte_at(1).unwrap(), 2);
        assert!(owned.byte_at(u64::MAX).is_err());
    }

    #[test]
    fn bounds_are_checked_before_allocating_ownership() {
        assert!(matches!(
            EncodedBuffer::new(&[]),
            Err(BindingError::EmptyBuffer)
        ));
        assert!(matches!(
            EncodedBuffer::new(&vec![0; MAX_ENCODED_BUFFER_BYTES + 1]),
            Err(BindingError::BufferTooLarge)
        ));
    }
}
