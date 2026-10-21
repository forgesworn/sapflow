//! Durable prepared-session metadata, never runtime admission or credentials.
//!
//! All methods are synchronous and belong on the server's serial blocking
//! transaction worker, outside admission/media locks. Callers must durably
//! replace a draft snapshot before publishing that draft or committing a claim.
//! The caller explicitly creates an existing private (0700) parent directory.
//! `open` returns None only when both the initial file and lock were absent;
//! an interrupted virgin bootstrap requires explicit operator repair. Malformed
//! existing data is never reset. Restoring the returned snapshot disables every
//! runtime admission until a fresh, strictly higher revision is provisioned.
//!
//! Unix directory descriptors anchor every operation. No path component or
//! final/lock file may be a symlink; metadata files must be private regular files
//! with one link. The persistent lock file must never be unlinked on close.
//! SHA-256 detects accidental metadata corruption, not malicious rollback by
//! the owning OS user. Backups and external rollback protection are separate.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{self, AtFlags, FlockOperation, Mode, OFlags};
use sapflow_core::StreamPath;
use sapflow_core::publish_session::{
    Authority, EpochMs, Fingerprint, GrantMetadata, JsPositive, MAX_EVENTS, SessionId,
    SessionStatus, Snapshot, SnapshotRow,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_STORE_BYTES: usize = 4 * 1024 * 1024;
const FORMAT_VERSION: u32 = 1;

#[derive(Debug)]
pub enum StoreError {
    InvalidPath,
    InsecureDirectory,
    InsecureFile,
    Locked,
    TooLarge,
    Corrupt,
    Incompatible,
    InvalidSnapshot,
    Rollback,
    Changed,
    Unusable,
    Randomness,
    Io {
        operation: &'static str,
        source: io::Error,
    },
    /// Rename succeeded; the new snapshot may be present, but durability was
    /// not established. This object refuses all further load/replace calls.
    CommitUncertain(io::Error),
    CleanupFailed(io::Error),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Io { operation, source } => {
                return write!(f, "publish-session store {operation}: {source}");
            }
            Self::CommitUncertain(_) => {
                "publish-session store commit durability is uncertain; restart required"
            }
            Self::CleanupFailed(_) => {
                "publish-session store temporary cleanup failed; restart required"
            }
            Self::InvalidPath => "invalid publish-session store path",
            Self::InsecureDirectory => {
                "publish-session store requires an owned private 0700 directory"
            }
            Self::InsecureFile => {
                "publish-session store requires owned regular 0600 files with one link"
            }
            Self::Locked => "publish-session store is already locked",
            Self::TooLarge => "publish-session store exceeds its byte or row bound",
            Self::Corrupt => "publish-session store is corrupt",
            Self::Incompatible => "publish-session store format is incompatible",
            Self::InvalidSnapshot => "publish-session store metadata is invalid",
            Self::Rollback => "publish-session store refuses a high-water or tombstone rollback",
            Self::Changed => "publish-session store changed outside its owner",
            Self::Unusable => "publish-session store is unusable; restart required",
            Self::Randomness => "publish-session store could not obtain a temporary name",
        };
        f.write_str(text)
    }
}
impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. }
            | Self::CommitUncertain(source)
            | Self::CleanupFailed(source) => Some(source),
            _ => None,
        }
    }
}
fn io_error(operation: &'static str, source: impl Into<io::Error>) -> StoreError {
    StoreError::Io {
        operation,
        source: source.into(),
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    clock_floor_ms: u64,
    #[serde(deserialize_with = "bounded_rows")]
    rows: Vec<Row>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    path: String,
    event_generation: u64,
    session_id: String,
    revision: u64,
    request_fingerprint: [u8; 32],
    status: Status,
    grant: Option<Grant>,
    connection_epoch: u64,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Prepared,
    Revoked,
    Expired,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    expires_at_ms: u64,
    reconnect_grace_ms: u32,
    immutable_fingerprint: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format_version: u32,
    metadata: Document,
    checksum_sha256: [u8; 32],
}
fn bounded_rows<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Row>, D::Error> {
    struct Rows;
    impl<'de> serde::de::Visitor<'de> for Rows {
        type Value = Vec<Row>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("at most 4096 metadata rows")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut rows = Vec::new();
            while rows.len() < MAX_EVENTS {
                let Some(row) = seq.next_element()? else {
                    return Ok(rows);
                };
                rows.push(row);
            }
            if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(serde::de::Error::custom("metadata row bound exceeded"));
            }
            Ok(rows)
        }
    }
    deserializer.deserialize_seq(Rows)
}
impl From<&Snapshot> for Document {
    fn from(value: &Snapshot) -> Self {
        Self {
            version: value.version,
            clock_floor_ms: value.clock_floor_ms,
            rows: value
                .rows
                .iter()
                .map(|row| Row {
                    path: row.path.as_path(),
                    event_generation: row.event_generation.get(),
                    session_id: row.session_id.as_str().to_owned(),
                    revision: row.revision.get(),
                    request_fingerprint: *row.request_fingerprint.as_bytes(),
                    status: match row.status {
                        SessionStatus::Prepared => Status::Prepared,
                        SessionStatus::Revoked => Status::Revoked,
                        SessionStatus::Expired => Status::Expired,
                    },
                    grant: row.grant.as_ref().map(|grant| Grant {
                        expires_at_ms: grant.expires_at_ms.get(),
                        reconnect_grace_ms: grant.reconnect_grace_ms,
                        immutable_fingerprint: *grant.immutable_fingerprint.as_bytes(),
                    }),
                    connection_epoch: row.connection_epoch,
                })
                .collect(),
        }
    }
}
impl Document {
    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        if self.rows.len() > MAX_EVENTS {
            return Err(StoreError::TooLarge);
        }
        let invalid = || StoreError::InvalidSnapshot;
        let rows = self
            .rows
            .iter()
            .map(|row| {
                Ok(SnapshotRow {
                    path: StreamPath::parse(&row.path).ok_or_else(invalid)?,
                    event_generation: JsPositive::new(row.event_generation)
                        .map_err(|_| invalid())?,
                    session_id: SessionId::parse(&row.session_id).map_err(|_| invalid())?,
                    revision: JsPositive::new(row.revision).map_err(|_| invalid())?,
                    request_fingerprint: Fingerprint::new(row.request_fingerprint),
                    status: match row.status {
                        Status::Prepared => SessionStatus::Prepared,
                        Status::Revoked => SessionStatus::Revoked,
                        Status::Expired => SessionStatus::Expired,
                    },
                    grant: row
                        .grant
                        .as_ref()
                        .map(|grant| -> Result<GrantMetadata, StoreError> {
                            Ok(GrantMetadata {
                                expires_at_ms: EpochMs::new(grant.expires_at_ms)
                                    .map_err(|_| invalid())?,
                                reconnect_grace_ms: grant.reconnect_grace_ms,
                                immutable_fingerprint: Fingerprint::new(
                                    grant.immutable_fingerprint,
                                ),
                            })
                        })
                        .transpose()?,
                    connection_epoch: row.connection_epoch,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let snapshot = Snapshot {
            version: self.version,
            clock_floor_ms: self.clock_floor_ms,
            rows,
        };
        Authority::restore(snapshot.clone(), MAX_EVENTS)
            .map_err(|_| StoreError::InvalidSnapshot)?;
        Ok(snapshot)
    }
    fn follows(&self, old: &Self) -> Result<(), StoreError> {
        if self.clock_floor_ms < old.clock_floor_ms {
            return Err(StoreError::Rollback);
        }
        let rows: BTreeMap<_, _> = self
            .rows
            .iter()
            .map(|row| (row.path.as_str(), row))
            .collect();
        for previous in &old.rows {
            let next = rows
                .get(previous.path.as_str())
                .ok_or(StoreError::Rollback)?;
            if next.event_generation < previous.event_generation
                || next.connection_epoch < previous.connection_epoch
            {
                return Err(StoreError::Rollback);
            }
            if next.event_generation > previous.event_generation {
                if next.session_id == previous.session_id {
                    return Err(StoreError::Rollback);
                }
                continue;
            }
            if next.session_id != previous.session_id
                || next.revision < previous.revision
                || (previous.status != Status::Prepared && next.status == Status::Prepared)
                || (previous.status == Status::Revoked && next.status != Status::Revoked)
            {
                return Err(StoreError::Rollback);
            }
            match (&previous.grant, &next.grant) {
                (Some(before), Some(after))
                    if before.immutable_fingerprint == after.immutable_fingerprint
                        && before.reconnect_grace_ms == after.reconnect_grace_ms
                        && (if next.status == Status::Prepared {
                            after.expires_at_ms >= before.expires_at_ms
                        } else {
                            after.expires_at_ms == before.expires_at_ms
                        }) => {}
                (None, None) => {}
                _ => return Err(StoreError::Rollback),
            }
            if next.revision == previous.revision
                && (next.request_fingerprint != previous.request_fingerprint
                    || next.grant != previous.grant
                    || (next.status != previous.status
                        && !(previous.status == Status::Prepared
                            && next.status == Status::Expired)))
            {
                return Err(StoreError::Rollback);
            }
        }
        Ok(())
    }
}
struct LimitedBytes(Vec<u8>);
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_STORE_BYTES - self.0.len() {
            return Err(io::Error::other("metadata byte bound exceeded"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, StoreError> {
    let mut output = LimitedBytes(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| StoreError::TooLarge)?;
    Ok(output.0)
}
/// Strict bounded metadata decoding, independent of filesystem ownership.
fn decode_document(bytes: &[u8]) -> Result<Document, StoreError> {
    if bytes.len() > MAX_STORE_BYTES {
        return Err(StoreError::TooLarge);
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| StoreError::Corrupt)?;
    if envelope.format_version != FORMAT_VERSION {
        return Err(StoreError::Incompatible);
    }
    let checksum: [u8; 32] = Sha256::digest(encode(&envelope.metadata)?).into();
    if checksum != envelope.checksum_sha256 {
        return Err(StoreError::Corrupt);
    }
    envelope.metadata.snapshot()?;
    Ok(envelope.metadata)
}

/// Fuzz-only byte parser: no file access, locking or authority activation.
/// The full byte bound applies before JSON allocation or checksum work.
#[cfg(feature = "fuzzing")]
#[allow(dead_code)] // The external fuzz binary includes this module directly.
pub fn validate_snapshot_bytes(bytes: &[u8]) -> Result<(), StoreError> {
    decode_document(bytes).map(|_| ())
}

fn private_file(file: &File) -> Result<(), StoreError> {
    let meta = file.metadata().map_err(|e| io_error("file metadata", e))?;
    if !meta.is_file()
        || meta.mode() & 0o7777 != 0o600
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.nlink() != 1
    {
        return Err(StoreError::InsecureFile);
    }
    Ok(())
}
fn open_parent(path: &Path) -> Result<(File, OsString), StoreError> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
        .ok_or(StoreError::InvalidPath)?;
    let parent = path.parent().ok_or(StoreError::InvalidPath)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(
        fs::open(
            if path.is_absolute() { "/" } else { "." },
            flags,
            Mode::empty(),
        )
        .map_err(|e| io_error("open root", e))?,
    );
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => {
                directory = File::from(
                    fs::openat(&directory, part, flags, Mode::empty())
                        .map_err(|e| io_error("open parent component", e))?,
                )
            }
            _ => return Err(StoreError::InvalidPath),
        }
    }
    let metadata = directory
        .metadata()
        .map_err(|e| io_error("directory metadata", e))?;
    if metadata.mode() & 0o7777 != 0o700 || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(StoreError::InsecureDirectory);
    }
    Ok((directory, name.into()))
}

/// A single process owner. Keep this object (and its advisory lock) alive for
/// the entire control-plane lifetime. It is not the fast admission gate.
pub struct PublishSessionStore {
    directory: File,
    name: OsString,
    _lock: File,
    current: Option<Document>,
    unusable: bool,
    #[cfg(test)]
    fault: Option<CommitStep>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommitStep {
    #[cfg(test)]
    PartialWrite,
    FileSync,
    Rename,
    DirectorySync,
}
impl PublishSessionStore {
    /// Never creates parent directories or an empty metadata file. None means
    /// both metadata and lock were absent. Immediately persist the initial empty
    /// snapshot before serving. An existing lock with missing metadata refuses
    /// ambiguous bootstrap/deletion rather than resetting high-water marks.
    pub fn open(path: &Path) -> Result<(Self, Option<Snapshot>), StoreError> {
        let (directory, name) = open_parent(path)?;
        let mut lock_name = name.clone();
        lock_name.push(".lock");
        let lock_flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let (lock, new_lock) = match fs::openat(
            &directory,
            &lock_name,
            lock_flags | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => (File::from(fd), true),
            Err(rustix::io::Errno::EXIST) => (
                File::from(
                    fs::openat(&directory, &lock_name, lock_flags, Mode::empty())
                        .map_err(|e| io_error("open existing lock", e))?,
                ),
                false,
            ),
            Err(error) => return Err(io_error("create lock", error)),
        };
        private_file(&lock)?;
        fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                StoreError::Locked
            } else {
                io_error("lock", e)
            }
        })?;
        let mut store = Self {
            directory,
            name,
            _lock: lock,
            current: None,
            unusable: false,
            #[cfg(test)]
            fault: None,
        };
        let loaded = store.load()?;
        if !new_lock && loaded.is_none() {
            return Err(StoreError::Changed);
        }
        Ok((store, loaded))
    }
    fn read_document(&self) -> Result<Option<Document>, StoreError> {
        let fd = match fs::openat(
            &self.directory,
            &self.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io_error("open metadata", e)),
        };
        let file = File::from(fd);
        private_file(&file)?;
        if file
            .metadata()
            .map_err(|e| io_error("metadata length", e))?
            .len()
            > MAX_STORE_BYTES as u64
        {
            return Err(StoreError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_STORE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("read metadata", e))?;
        decode_document(&bytes).map(Some)
    }
    /// Re-reading an existing owner also detects external replacement/removal;
    /// callers cannot use reload to discard high-water marks or clear poison.
    pub fn load(&mut self) -> Result<Option<Snapshot>, StoreError> {
        if self.unusable {
            return Err(StoreError::Unusable);
        }
        let result = (|| {
            let document = self.read_document()?;
            if self.current.is_some() && document != self.current {
                return Err(StoreError::Changed);
            }
            let snapshot = document.as_ref().map(Document::snapshot).transpose()?;
            self.current = document;
            Ok(snapshot)
        })();
        if result.is_err() {
            self.unusable = true;
        }
        result
    }
    fn unchanged(&self) -> Result<(), StoreError> {
        if self.read_document()? != self.current {
            return Err(StoreError::Changed);
        }
        Ok(())
    }
    fn check_fault(&self, step: CommitStep) -> Result<(), StoreError> {
        #[cfg(test)]
        if self.fault == Some(step) {
            return Err(io_error(
                "injected commit failure",
                io::Error::other("test fault"),
            ));
        }
        let _ = step;
        Ok(())
    }
    /// Atomically commits only validated metadata. No rows/tombstones may be
    /// removed, and event/revision/epoch/clock high-water marks cannot decrease.
    /// Post-rename failure is explicitly uncertain and poisons this object.
    pub fn replace(&mut self, snapshot: &Snapshot) -> Result<(), StoreError> {
        if self.unusable {
            return Err(StoreError::Unusable);
        }
        if snapshot.rows.len() > MAX_EVENTS {
            return Err(StoreError::TooLarge);
        }
        let document = Document::from(snapshot);
        document.snapshot()?;
        if let Some(old) = &self.current {
            document.follows(old)?;
        }
        if let Err(error) = self.unchanged() {
            self.unusable = true;
            return Err(error);
        }
        let checksum_sha256 = Sha256::digest(encode(&document)?).into();
        let bytes = encode(&Envelope {
            format_version: FORMAT_VERSION,
            metadata: document.clone(),
            checksum_sha256,
        })?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| StoreError::Randomness)?;
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut temporary = OsString::from(".");
        temporary.push(&self.name);
        temporary.push(format!(".tmp-{suffix}"));
        // CREATE_NEW (CREATE|EXCL), with NOFOLLOW, never truncates a collision.
        let mut file = File::from(
            fs::openat(
                &self.directory,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| io_error("create temporary", e))?,
        );
        let identity = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.unusable = true;
                return Err(StoreError::CleanupFailed(error));
            }
        };
        let mut renamed = false;
        let result = (|| {
            private_file(&file)?;
            #[cfg(test)]
            if self.fault == Some(CommitStep::PartialWrite) {
                file.write_all(&bytes[..bytes.len().min(7)])
                    .map_err(|e| io_error("partial test write", e))?;
                return Err(io_error(
                    "injected partial write",
                    io::Error::other("test fault"),
                ));
            }
            file.write_all(&bytes)
                .map_err(|e| io_error("write temporary", e))?;
            self.check_fault(CommitStep::FileSync)?;
            file.sync_all().map_err(|e| io_error("sync temporary", e))?;
            self.unchanged()?;
            self.check_fault(CommitStep::Rename)?;
            fs::renameat(&self.directory, &temporary, &self.directory, &self.name)
                .map_err(|e| io_error("rename temporary", e))?;
            renamed = true;
            self.check_fault(CommitStep::DirectorySync)?;
            self.directory
                .sync_all()
                .map_err(|e| io_error("sync directory", e))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.current = Some(document);
                Ok(())
            }
            Err(error) if renamed => {
                self.unusable = true;
                let source = match error {
                    StoreError::Io { source, .. } => source,
                    _ => io::Error::other("post-rename failure"),
                };
                Err(StoreError::CommitUncertain(source))
            }
            Err(error) => {
                // Never delete a pre-existing or swapped temporary entry.
                let cleanup = (|| -> io::Result<()> {
                    let actual = File::from(fs::openat(
                        &self.directory,
                        &temporary,
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?);
                    let actual = actual.metadata()?;
                    if actual.dev() != identity.dev() || actual.ino() != identity.ino() {
                        return Err(io::Error::other("temporary identity changed"));
                    }
                    fs::unlinkat(&self.directory, &temporary, AtFlags::empty())?;
                    Ok(())
                })();
                if let Err(cleanup) = cleanup {
                    self.unusable = true;
                    return Err(StoreError::CleanupFailed(cleanup));
                }
                if matches!(
                    error,
                    StoreError::Changed | StoreError::InsecureFile | StoreError::Corrupt
                ) {
                    self.unusable = true;
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
#[path = "publish_session_store_tests.rs"]
mod tests;
