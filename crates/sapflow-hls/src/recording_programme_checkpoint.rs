//! SFCP0002 submitted-programme geometry snapshots, not native/file authority.
//! V1 remains a separate codec. Checksums detect corruption, not authentication.

use crate::recording_checkpoint::{
    CheckpointBinding, CheckpointContext, CheckpointError, metadata_sha256,
};
use crate::recording_programme::{
    ObservedProgrammeSpan, ProgrammeGenerationEnd, ProgrammeRecordingPresentation,
    ProgrammeWindowLedger, ProgrammeWindowStart, QueuedProgrammeFrames,
};
use crate::recording_timing::TimingError;

/// Fixed V2 scalar prefix; independent of the V1 format tag.
pub const PREFIX_BYTES: usize = 512;
/// Maximum opaque snapshot header.
pub const MAX_HEADER_BYTES: usize = 65_536;
/// Maximum complete bounded V2 envelope.
pub const MAX_CHECKPOINT_BYTES: usize = PREFIX_BYTES + MAX_HEADER_BYTES + 32;
const MAGIC: &[u8; 8] = b"SFCP0002";

/// Immutable original output identity and source/coded/movie mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeCheckpointContext {
    /// Exact platform binding, original INITIAL metadata and frozen media epoch.
    pub base: CheckpointContext,
    /// Frozen caller-reported programme mapping, not native source authority.
    pub start: ProgrammeWindowStart,
}

/// Coverage of actually sealed original AUs, never accepted future media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeCheckpointFacts {
    /// One-based emitted fragment sequence, below u32::MAX.
    pub sequence: u32,
    /// Exact retained contiguous original coded span from muxer counters.
    pub span: ObservedProgrammeSpan,
    /// Serialized endpoint, shortened only for a complete submitted window.
    pub container_end: u64,
    /// Retained measured head, checked against the frozen mapping.
    pub leading_frames: u32,
    /// Complete selected-window tail; zero for an unknown-tail prefix.
    pub trailing_frames: u32,
    /// Checked complete submitted-window facts; no actual native EOF is certified.
    pub window_end: Option<ProgrammeGenerationEnd>,
}

/// Portable metadata; its values do not certify file ownership or synchronisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgrammeCheckpointInfo {
    /// Frozen identity and programme origin.
    pub context: ProgrammeCheckpointContext,
    /// Exact sealed coded/container coverage.
    pub facts: ProgrammeCheckpointFacts,
    /// INITIAL length plus the actual complete fragment region.
    pub committed_offset: u64,
    /// Exact complete fragment-region bytes.
    pub fragment_region_bytes: u64,
    /// Independently supplied actual cumulative fragment-region hash.
    pub fragment_region_sha256: [u8; 32],
}

/// Borrowed bounded encode input without an intermediate header clone.
#[derive(Debug, Clone, Copy)]
pub struct ProgrammeCheckpointRef<'a> {
    /// Checked snapshot scalars.
    pub info: &'a ProgrammeCheckpointInfo,
    /// Opaque exact snapshot header, not parsed or authenticated here.
    pub header: &'a [u8],
}

/// Decoded outside data, never a live issued muxer or filesystem token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedProgrammeCheckpoint {
    /// Bounded identity/geometry facts only.
    pub info: ProgrammeCheckpointInfo,
    /// Opaque header bytes covered by the envelope checksum.
    pub header: Vec<u8>,
}

impl PersistedProgrammeCheckpoint {
    /// Canonical borrowing view of this outside-data snapshot.
    pub fn as_ref(&self) -> ProgrammeCheckpointRef<'_> {
        ProgrammeCheckpointRef {
            info: &self.info,
            header: &self.header,
        }
    }
}

fn timing(error: TimingError) -> CheckpointError {
    if error == TimingError::Overflow {
        CheckpointError::Overflow
    } else {
        CheckpointError::Facts
    }
}

/// Reconstruct checked presentation from this exact snapshot, not current state.
/// A complete result certifies scalar geometry only, never native/file EOF.
pub fn presentation(
    info: &ProgrammeCheckpointInfo,
) -> Result<ProgrammeRecordingPresentation, CheckpointError> {
    let mut ledger = ProgrammeWindowLedger::default();
    ledger.begin(info.context.start, 0).map_err(timing)?;
    if let Some(end) = info.facts.window_end {
        Ok(ledger
            .complete(end, info.facts.span)
            .map_err(timing)?
            .presentation())
    } else {
        ledger.prefix_presentation(info.facts.span).map_err(timing)
    }
}

fn validate(info: &ProgrammeCheckpointInfo, header_len: usize) -> Result<(), CheckpointError> {
    let base = info.context.base;
    let facts = info.facts;
    if !(1..=MAX_HEADER_BYTES).contains(&header_len)
        || base.initial_header_len as usize != header_len
        || facts.sequence == 0
        || facts.sequence == u32::MAX
        || facts.span.access_units == 0
        || info.fragment_region_bytes == 0
        || info.committed_offset > i64::MAX as u64
        || u64::from(base.initial_header_len).checked_add(info.fragment_region_bytes)
            != Some(info.committed_offset)
    {
        return Err(CheckpointError::Facts);
    }
    let value = presentation(info)?;
    if facts.leading_frames != value.leading_frames()
        || facts.trailing_frames != value.trailing_frames()
        || facts
            .span
            .end_dts
            .checked_sub(u64::from(facts.trailing_frames))
            != Some(facts.container_end)
    {
        return Err(CheckpointError::Facts);
    }
    value.movie_extent().map_err(timing)?;
    Ok(())
}

fn put<const N: usize>(bytes: &mut [u8], at: usize, value: [u8; N]) {
    bytes[at..at + N].copy_from_slice(&value);
}
fn read<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], CheckpointError> {
    let end = at.checked_add(N).ok_or(CheckpointError::Overflow)?;
    bytes
        .get(at..end)
        .ok_or(CheckpointError::Envelope)?
        .try_into()
        .map_err(|_| CheckpointError::Envelope)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, CheckpointError> {
    Ok(u32::from_be_bytes(read(bytes, at)?))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, CheckpointError> {
    Ok(u64::from_be_bytes(read(bytes, at)?))
}

/// Canonical bounded V2 encoding. No native, storage or playback proof is issued.
pub fn encode(value: ProgrammeCheckpointRef<'_>) -> Result<Vec<u8>, CheckpointError> {
    validate(value.info, value.header.len())?;
    let total = PREFIX_BYTES
        .checked_add(value.header.len())
        .and_then(|x| x.checked_add(32))
        .ok_or(CheckpointError::Overflow)?;
    let mut bytes = vec![0; total];
    let info = value.info;
    let base = info.context.base;
    let start = info.context.start;
    let facts = info.facts;
    put(&mut bytes, 0, *MAGIC);
    put(&mut bytes, 8, 2_u16.to_be_bytes());
    put(&mut bytes, 10, 512_u16.to_be_bytes());
    put(
        &mut bytes,
        12,
        (2 | (u32::from(facts.window_end.is_some()) << 2)).to_be_bytes(),
    );
    put(
        &mut bytes,
        16,
        u32::try_from(total)
            .map_err(|_| CheckpointError::Overflow)?
            .to_be_bytes(),
    );
    put(
        &mut bytes,
        20,
        u32::try_from(value.header.len())
            .map_err(|_| CheckpointError::Overflow)?
            .to_be_bytes(),
    );
    put(&mut bytes, 24, facts.sequence.to_be_bytes());
    bytes[28] = 1;
    bytes[29] = 1;
    bytes[30] = 2;
    put(&mut bytes, 32, base.binding.recording);
    put(&mut bytes, 64, base.binding.source_file);
    put(&mut bytes, 96, base.initial_header_len.to_be_bytes());
    for (at, number) in [
        (104, info.committed_offset),
        (112, info.fragment_region_bytes),
        (120, base.media_epoch_ns),
        (128, facts.span.access_units),
        (144, facts.span.first_dts),
        (152, facts.span.end_dts),
        (160, facts.container_end),
        (184, u64::from(facts.window_end.is_some())),
        (272, start.coded_origin),
        (280, start.preparation_frames),
        (288, start.measured_displacement),
        (296, start.discard_prefix_frames),
        (304, start.movie_delay_ticks),
        (312, start.generation),
    ] {
        put(&mut bytes, at, number.to_be_bytes());
    }
    put(&mut bytes, 192, facts.leading_frames.to_be_bytes());
    put(&mut bytes, 196, facts.trailing_frames.to_be_bytes());
    put(&mut bytes, 200, 48_000_u32.to_be_bytes());
    put(&mut bytes, 208, base.initial_header_sha256);
    put(&mut bytes, 240, info.fragment_region_sha256);
    if let Some(end) = facts.window_end {
        for (at, number) in [
            (320, end.coded_frames),
            (328, end.queued.programme_raw),
            (336, end.queued.programme_zero),
            (344, end.queued.block_padding),
            (352, end.queued.codec_drain),
        ] {
            put(&mut bytes, at, number.to_be_bytes());
        }
    }
    bytes[PREFIX_BYTES..total - 32].copy_from_slice(value.header);
    let checksum = metadata_sha256(&bytes[..total - 32]);
    put(&mut bytes, total - 32, checksum);
    Ok(bytes)
}

/// Decode outside V2 bytes against independently retained recording/file bindings.
/// V1 is not accepted or guessed; native/file/application authority stays external.
pub fn decode(
    bytes: &[u8],
    expected: &CheckpointBinding,
) -> Result<PersistedProgrammeCheckpoint, CheckpointError> {
    if !(PREFIX_BYTES + 1 + 32..=MAX_CHECKPOINT_BYTES).contains(&bytes.len()) {
        return Err(CheckpointError::Envelope);
    }
    let end = bytes.len() - 32;
    if metadata_sha256(&bytes[..end]) != read::<32>(bytes, end)? {
        return Err(CheckpointError::Checksum);
    }
    let header_len = u32_at(bytes, 20)? as usize;
    if !(1..=MAX_HEADER_BYTES).contains(&header_len)
        || u32_at(bytes, 16)? as usize != bytes.len()
        || PREFIX_BYTES
            .checked_add(header_len)
            .and_then(|x| x.checked_add(32))
            != Some(bytes.len())
    {
        return Err(CheckpointError::Facts);
    }
    let flags = u32_at(bytes, 12)?;
    let complete = flags & 4 != 0;
    if read::<8>(bytes, 0)? != *MAGIC
        || u16::from_be_bytes(read(bytes, 8)?) != 2
        || u16::from_be_bytes(read(bytes, 10)?) != 512
        || flags & !6 != 0
        || flags & 2 == 0
        || bytes[28..32] != [1, 1, 2, 0]
        || bytes[100..104].iter().any(|x| *x != 0)
        || bytes[136..144].iter().any(|x| *x != 0)
        || bytes[168..184].iter().any(|x| *x != 0)
        || bytes[360..512].iter().any(|x| *x != 0)
        || u32_at(bytes, 200)? != 48_000
        || u32_at(bytes, 204)? != 0
    {
        return Err(CheckpointError::Unsupported);
    }
    if u64_at(bytes, 184)? != u64::from(complete)
        || (!complete && bytes[320..360].iter().any(|x| *x != 0))
    {
        return Err(CheckpointError::Facts);
    }
    let binding = CheckpointBinding {
        recording: read(bytes, 32)?,
        source_file: read(bytes, 64)?,
    };
    if binding != *expected {
        return Err(CheckpointError::Binding);
    }
    let start = ProgrammeWindowStart {
        generation: u64_at(bytes, 312)?,
        coded_origin: u64_at(bytes, 272)?,
        preparation_frames: u64_at(bytes, 280)?,
        measured_displacement: u64_at(bytes, 288)?,
        discard_prefix_frames: u64_at(bytes, 296)?,
        movie_delay_ticks: u64_at(bytes, 304)?,
    };
    let window_end = if complete {
        Some(ProgrammeGenerationEnd {
            generation: start.generation,
            queued: QueuedProgrammeFrames {
                preparation: start.preparation_frames,
                programme_raw: u64_at(bytes, 328)?,
                programme_zero: u64_at(bytes, 336)?,
                block_padding: u64_at(bytes, 344)?,
                codec_drain: u64_at(bytes, 352)?,
            },
            coded_frames: u64_at(bytes, 320)?,
        })
    } else {
        None
    };
    let info = ProgrammeCheckpointInfo {
        context: ProgrammeCheckpointContext {
            base: CheckpointContext {
                binding,
                media_epoch_ns: u64_at(bytes, 120)?,
                initial_header_len: u32_at(bytes, 96)?,
                initial_header_sha256: read(bytes, 208)?,
            },
            start,
        },
        facts: ProgrammeCheckpointFacts {
            sequence: u32_at(bytes, 24)?,
            span: ObservedProgrammeSpan {
                first_dts: u64_at(bytes, 144)?,
                end_dts: u64_at(bytes, 152)?,
                access_units: u64_at(bytes, 128)?,
            },
            container_end: u64_at(bytes, 160)?,
            leading_frames: u32_at(bytes, 192)?,
            trailing_frames: u32_at(bytes, 196)?,
            window_end,
        },
        committed_offset: u64_at(bytes, 104)?,
        fragment_region_bytes: u64_at(bytes, 112)?,
        fragment_region_sha256: read(bytes, 240)?,
    };
    validate(&info, header_len)?;
    Ok(PersistedProgrammeCheckpoint {
        info,
        header: bytes[PREFIX_BYTES..end].to_vec(),
    })
}

/// Validate exact next sealed submitted prefix; no media/file hashing is done here.
pub fn validate_successor(
    prior: &ProgrammeCheckpointInfo,
    next: &ProgrammeCheckpointInfo,
) -> Result<(), CheckpointError> {
    validate(prior, prior.context.base.initial_header_len as usize)?;
    validate(next, next.context.base.initial_header_len as usize)?;
    if prior.facts.window_end.is_some()
        || prior.context != next.context
        || prior.facts.sequence.checked_add(1) != Some(next.facts.sequence)
        || next.committed_offset <= prior.committed_offset
        || next.fragment_region_bytes <= prior.fragment_region_bytes
        || next.facts.span.access_units <= prior.facts.span.access_units
        || next.facts.span.first_dts != prior.facts.span.first_dts
        || next.facts.span.end_dts < prior.facts.span.end_dts
        || next.facts.container_end < prior.facts.container_end
    {
        return Err(CheckpointError::Successor);
    }
    Ok(())
}
