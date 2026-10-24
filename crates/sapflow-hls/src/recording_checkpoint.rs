//! Portable bounded native-recording snapshots, not filesystem authority.
//!
//! A checksum detects corruption, not malicious replacement. Callers own exact
//! recording/file bindings, original media writes, incremental region hashing
//! and all synchronisation/close receipts. Headers remain opaque metadata.

use crate::recording_timing::RecordingPresentation;
use sha2::{Digest, Sha256};

/// Fixed V1 scalar prefix length.
pub const PREFIX_BYTES: usize = 512;
/// Maximum opaque native header length.
pub const MAX_HEADER_BYTES: usize = 65_536;
/// Exact maximum encoded checkpoint length.
pub const MAX_CHECKPOINT_BYTES: usize = PREFIX_BYTES + MAX_HEADER_BYTES + 32;
const MAGIC: &[u8; 8] = b"SFCP0001";

/// Invalid, contradictory or unsupported checkpoint input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    /// Actual input or caller-owned header is outside the bounded envelope.
    Envelope,
    /// Unknown version, flags, format, algorithm or nonzero reserved fields.
    Unsupported,
    /// SHA256 does not match the complete prefix and header.
    Checksum,
    /// A checksum-valid value does not match the caller's expected identity.
    Binding,
    /// Checksum-valid declared lengths or media/offset facts are contradictory.
    Facts,
    /// Two individually valid snapshots are not an exact next prefix.
    Successor,
    /// Checked arithmetic exceeded its representation.
    Overflow,
}
impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "recording checkpoint: {self:?}")
    }
}
impl std::error::Error for CheckpointError {}

/// Opaque identities supplied independently by the platform's retained owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointBinding {
    /// Canonical immutable recording identity digest.
    pub recording: [u8; 32],
    /// Retained root/file incarnation digest; never just a path.
    pub source_file: [u8; 32],
}
/// Immutable original output context, independent of later header rewrites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointContext {
    /// Expected platform identity binding.
    pub binding: CheckpointBinding,
    /// Frozen media epoch, not a grant/authority clock.
    pub media_epoch_ns: u64,
    /// Actual original INITIAL output length.
    pub initial_header_len: u32,
    /// SHA256 of actual original INITIAL output bytes.
    pub initial_header_sha256: [u8; 32],
}
/// Sealed media coverage; no accepted-but-buffered media is represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointFacts {
    /// One-based native fragment sequence, below u32::MAX.
    pub sequence: u32,
    /// Sealed original AAC access-unit count; may be zero with sealed video.
    pub audio_units: u64,
    /// Sealed original video access-unit count.
    pub video_units: u64,
    /// First original AAC DTS relative to media epoch, at48kHz.
    pub audio_start: u64,
    /// Original coded AAC endpoint, including final coded padding.
    pub audio_end: u64,
    /// Container AAC endpoint; shortened only by proven complete EOF tail.
    pub audio_container_end: u64,
    /// Proven final EOF padding, otherwise zero.
    pub final_trailing_frames: u32,
    /// Actual checked native EOF, not file synchronisation/completion.
    pub complete_audio_eof: bool,
    /// First original video DTS at90kHz, paired with video_end.
    pub video_start: Option<u64>,
    /// Last sealed video presentation endpoint at90kHz.
    pub video_end: Option<u64>,
    /// Measured first-file native priming, not a platform constant.
    pub first_leading_frames: u32,
    /// Checked generation EOFs fully covered by these sealed AUs.
    pub completed_generation_count: u64,
}
/// Portable snapshot scalars. Values alone do not certify a file write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointInfo {
    /// Immutable original context and independently supplied identity.
    pub context: CheckpointContext,
    /// Exact sealed native coverage.
    pub facts: CheckpointFacts,
    /// INITIAL length plus actual serialized complete fragment bytes.
    pub committed_offset: u64,
    /// Bytes in the fragment region, excluding INITIAL and header rewrites.
    pub fragment_region_bytes: u64,
    /// Platform's cumulative hash of its actual fragment byte region.
    pub fragment_region_sha256: [u8; 32],
}
/// Borrowed encode view, avoiding an intermediate header clone.
#[derive(Debug, Clone, Copy)]
pub struct CheckpointRef<'a> {
    /// Checked snapshot scalars.
    pub info: &'a CheckpointInfo,
    /// Opaque exact native header,1..65536bytes.
    pub header: &'a [u8],
}
/// Owned decoded outside input; never a live muxer or filesystem capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedCheckpoint {
    /// Untrusted-until-validated snapshot values, not a durability receipt.
    pub info: CheckpointInfo,
    /// Opaque original checkpoint header.
    pub header: Vec<u8>,
}
impl PersistedCheckpoint {
    /// Borrow this snapshot for canonical encoding without cloning its header.
    pub fn as_ref(&self) -> CheckpointRef<'_> {
        CheckpointRef {
            info: &self.info,
            header: &self.header,
        }
    }
}

/// Hash metadata with the selected maintained SHA256 implementation.
/// This does not validate identities or prove the bytes reached storage.
pub fn metadata_sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn validate(info: &CheckpointInfo, header_len: usize) -> Result<(), CheckpointError> {
    let c = info.context;
    let f = info.facts;
    if !(1..=MAX_HEADER_BYTES).contains(&header_len) {
        return Err(CheckpointError::Envelope);
    }
    if usize::try_from(c.initial_header_len).ok() != Some(header_len) {
        return Err(CheckpointError::Facts);
    }
    if f.sequence == 0
        || f.sequence == u32::MAX
        || (f.audio_units == 0 && f.video_units == 0)
        || f.first_leading_frames > 8191
        || f.final_trailing_frames >= 1024
        || f.completed_generation_count > f.audio_units
        || f.video_start.is_some() != f.video_end.is_some()
        || (f.video_start.is_none() && f.video_units != 0)
        || f.video_start.zip(f.video_end).is_some_and(|(a, b)| {
            a > b || (f.video_units > 0 && a == b) || (f.video_units == 0 && a != b)
        })
    {
        return Err(CheckpointError::Facts);
    }
    let frames = f
        .audio_units
        .checked_mul(1024)
        .ok_or(CheckpointError::Overflow)?;
    if f.audio_start
        .checked_add(frames)
        .ok_or(CheckpointError::Overflow)?
        != f.audio_end
        || f.audio_end.checked_sub(u64::from(f.final_trailing_frames))
            != Some(f.audio_container_end)
        || (!f.complete_audio_eof && f.final_trailing_frames != 0)
        || (f.complete_audio_eof
            && (f.completed_generation_count == 0
                || frames
                    <= u64::from(f.first_leading_frames) + u64::from(f.final_trailing_frames)))
    {
        return Err(CheckpointError::Facts);
    }
    RecordingPresentation {
        audio_start: f.audio_start,
        audio_frames: frames,
        leading_frames: f.first_leading_frames,
        trailing_frames: f.final_trailing_frames,
        video_start: f.video_start,
        video_end: f.video_end,
    }
    .validate()
    .map_err(|_| CheckpointError::Facts)?;
    if info.committed_offset > i64::MAX as u64
        || info.fragment_region_bytes == 0
        || u64::from(c.initial_header_len).checked_add(info.fragment_region_bytes)
            != Some(info.committed_offset)
    {
        return Err(CheckpointError::Facts);
    }
    Ok(())
}
fn put<const N: usize>(bytes: &mut [u8], offset: usize, value: [u8; N]) {
    bytes[offset..offset + N].copy_from_slice(&value);
}
fn read<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], CheckpointError> {
    let end = offset.checked_add(N).ok_or(CheckpointError::Overflow)?;
    let part = bytes.get(offset..end).ok_or(CheckpointError::Envelope)?;
    let mut value = [0; N];
    value.copy_from_slice(part);
    Ok(value)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, CheckpointError> {
    Ok(u32::from_be_bytes(read(bytes, at)?))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, CheckpointError> {
    Ok(u64::from_be_bytes(read(bytes, at)?))
}

/// Encode the canonical bounded V1 envelope, with no intermediate header copy.
pub fn encode(value: CheckpointRef<'_>) -> Result<Vec<u8>, CheckpointError> {
    validate(value.info, value.header.len())?;
    let total = PREFIX_BYTES
        .checked_add(value.header.len())
        .and_then(|x| x.checked_add(32))
        .ok_or(CheckpointError::Overflow)?;
    let mut bytes = vec![0; total];
    let i = value.info;
    let c = i.context;
    let f = i.facts;
    put(&mut bytes, 0, *MAGIC);
    put(&mut bytes, 8, 1_u16.to_be_bytes());
    put(&mut bytes, 10, 512_u16.to_be_bytes());
    let flags = 2 | u32::from(f.video_start.is_some()) | (u32::from(f.complete_audio_eof) << 2);
    put(&mut bytes, 12, flags.to_be_bytes());
    put(&mut bytes, 16, (total as u32).to_be_bytes());
    put(&mut bytes, 20, (value.header.len() as u32).to_be_bytes());
    put(&mut bytes, 24, f.sequence.to_be_bytes());
    bytes[28] = 1;
    bytes[29] = 1;
    put(&mut bytes, 32, c.binding.recording);
    put(&mut bytes, 64, c.binding.source_file);
    put(&mut bytes, 96, c.initial_header_len.to_be_bytes());
    for (at, n) in [
        (104, i.committed_offset),
        (112, i.fragment_region_bytes),
        (120, c.media_epoch_ns),
        (128, f.audio_units),
        (136, f.video_units),
        (144, f.audio_start),
        (152, f.audio_end),
        (160, f.audio_container_end),
        (168, f.video_start.unwrap_or(0)),
        (176, f.video_end.unwrap_or(0)),
        (184, f.completed_generation_count),
    ] {
        put(&mut bytes, at, n.to_be_bytes());
    }
    put(&mut bytes, 192, f.first_leading_frames.to_be_bytes());
    put(&mut bytes, 196, f.final_trailing_frames.to_be_bytes());
    put(&mut bytes, 200, 48_000_u32.to_be_bytes());
    put(
        &mut bytes,
        204,
        (if f.video_start.is_some() {
            90_000_u32
        } else {
            0
        })
        .to_be_bytes(),
    );
    put(&mut bytes, 208, c.initial_header_sha256);
    put(&mut bytes, 240, i.fragment_region_sha256);
    bytes[PREFIX_BYTES..total - 32].copy_from_slice(value.header);
    let digest = metadata_sha256(&bytes[..total - 32]);
    put(&mut bytes, total - 32, digest);
    Ok(bytes)
}

/// Decode bounded outside input against independently supplied exact bindings.
/// A successful result certifies syntax/semantics only, not storage or playback.
pub fn decode(
    bytes: &[u8],
    expected: &CheckpointBinding,
) -> Result<PersistedCheckpoint, CheckpointError> {
    if !(PREFIX_BYTES + 1 + 32..=MAX_CHECKPOINT_BYTES).contains(&bytes.len()) {
        return Err(CheckpointError::Envelope);
    }
    // Actual bounded input determines the trailer position. Verify it before
    // trusting declared lengths: a checksum-valid contradiction is a semantic
    // conflict, not corruption that a platform slot policy may ignore.
    let end = bytes.len() - 32;
    if metadata_sha256(&bytes[..end]) != read::<32>(bytes, end)? {
        return Err(CheckpointError::Checksum);
    }
    let header_len = u32_at(bytes, 20)? as usize;
    if !(1..=MAX_HEADER_BYTES).contains(&header_len)
        || u32_at(bytes, 16)? as usize != bytes.len()
        || PREFIX_BYTES
            .checked_add(header_len)
            .and_then(|n| n.checked_add(32))
            != Some(bytes.len())
    {
        return Err(CheckpointError::Facts);
    }
    let flags = u32_at(bytes, 12)?;
    if read::<8>(bytes, 0)? != *MAGIC
        || u16::from_be_bytes(read(bytes, 8)?) != 1
        || u16::from_be_bytes(read(bytes, 10)?) != 512
        || flags & !7 != 0
        || flags & 2 == 0
        || bytes[28] != 1
        || bytes[29] != 1
        || bytes[30..32].iter().any(|b| *b != 0)
        || bytes[100..104].iter().any(|b| *b != 0)
        || bytes[272..512].iter().any(|b| *b != 0)
        || u32_at(bytes, 200)? != 48_000
        || u32_at(bytes, 204)? != if flags & 1 != 0 { 90_000 } else { 0 }
    {
        return Err(CheckpointError::Unsupported);
    }
    let binding = CheckpointBinding {
        recording: read(bytes, 32)?,
        source_file: read(bytes, 64)?,
    };
    if binding != *expected {
        return Err(CheckpointError::Binding);
    }
    let video_start = u64_at(bytes, 168)?;
    let video_end = u64_at(bytes, 176)?;
    if flags & 1 == 0 && (video_start != 0 || video_end != 0) {
        return Err(CheckpointError::Facts);
    }
    let info = CheckpointInfo {
        context: CheckpointContext {
            binding,
            media_epoch_ns: u64_at(bytes, 120)?,
            initial_header_len: u32_at(bytes, 96)?,
            initial_header_sha256: read(bytes, 208)?,
        },
        facts: CheckpointFacts {
            sequence: u32_at(bytes, 24)?,
            audio_units: u64_at(bytes, 128)?,
            video_units: u64_at(bytes, 136)?,
            audio_start: u64_at(bytes, 144)?,
            audio_end: u64_at(bytes, 152)?,
            audio_container_end: u64_at(bytes, 160)?,
            final_trailing_frames: u32_at(bytes, 196)?,
            complete_audio_eof: flags & 4 != 0,
            video_start: (flags & 1 != 0).then_some(video_start),
            video_end: (flags & 1 != 0).then_some(video_end),
            first_leading_frames: u32_at(bytes, 192)?,
            completed_generation_count: u64_at(bytes, 184)?,
        },
        committed_offset: u64_at(bytes, 104)?,
        fragment_region_bytes: u64_at(bytes, 112)?,
        fragment_region_sha256: read(bytes, 240)?,
    };
    validate(&info, header_len)?;
    Ok(PersistedCheckpoint {
        info,
        header: bytes[PREFIX_BYTES..end].to_vec(),
    })
}

/// Check individually valid snapshots form the exact next sealed prefix.
/// Matching media bytes, file ownership and sync receipts remain caller duties.
pub fn validate_successor(
    prior: &CheckpointInfo,
    next: &CheckpointInfo,
) -> Result<(), CheckpointError> {
    validate(prior, prior.context.initial_header_len as usize)?;
    validate(next, next.context.initial_header_len as usize)?;
    let p = prior.facts;
    let n = next.facts;
    if p.complete_audio_eof
        || prior.context != next.context
        || p.sequence.checked_add(1) != Some(n.sequence)
        || next.committed_offset <= prior.committed_offset
        || next.fragment_region_bytes <= prior.fragment_region_bytes
        || p.audio_start != n.audio_start
        || p.video_start != n.video_start
        || p.first_leading_frames != n.first_leading_frames
        || n.audio_units < p.audio_units
        || n.video_units < p.video_units
        || (n.audio_units == p.audio_units && n.video_units == p.video_units)
        || n.audio_end < p.audio_end
        || n.audio_container_end < p.audio_container_end
        || n.video_end < p.video_end
        || n.completed_generation_count < p.completed_generation_count
    {
        return Err(CheckpointError::Successor);
    }
    Ok(())
}
