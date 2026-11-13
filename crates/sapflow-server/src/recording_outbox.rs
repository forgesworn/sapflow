//! The recording outbox: on disk, the `recordSegmentComplete` each recording
//! still owes the app, so that a recording a SIGKILL, an OOM kill, a crash, a
//! power cut or the stop deadline left unreported is reported at the next
//! start ([`crate::recording_recovery`]).
//!
//! It is the directory `.sapflow-outbox` in `SAPFLOW_RECORDINGS`, with one
//! entry per recording file, named by a digest of the file's path:
//!
//! 1. The writer writes the entry, `open`, once it has created the file and
//!    before it writes any of it.
//! 2. Once the file is finalised and synced, and before its callback is
//!    queued, the writer rewrites the entry with the outcome the callback
//!    carries (`finished`: its state, reason and duration).
//!    The body is decided first: a refused one removes the entry instead.
//! 3. The callback sender removes the entry once the app has answered 2xx.
//!    A refusal, failed retries, a full queue or a stop before delivery leave
//!    it for the next start (whose recovery also retries failures while it
//!    runs). So does any exit of the writer that is not one of its decided
//!    failures (which owe the app nothing, as before).
//!
//! A crash at any point therefore leaves either the file with its entry, or a
//! delivered callback; or a callback delivered but its entry not yet removed,
//! which the next start sends again (at least once, never lost).
//!
//! Entries hold the file's relative path, its tracks and audio rate, and for
//! scoped recordings the callback identity; never the callback URL or its
//! token. The directory is owned by this user and private (0700), entries
//! are private (0600) regular files with one link, nothing is followed
//! through a symbolic link, and each write goes to a new temporary file that
//! is synced, renamed over the entry and then the directory synced. A
//! `.lock` held for the process's lifetime keeps a second sapflow on the same
//! directory from using it. Expiry ([`crate::recorder::expire_forever`])
//! removes an entry whose recording has gone or is older than
//! `SAPFLOW_RECORD_DELETE_AFTER`; the recordings expiry itself only touches
//! `live/*/` and so never this directory.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rustix::fs::{self, AtFlags, FlockOperation, Mode, OFlags};
use sapflow_core::StreamPath;
use sapflow_core::recording::is_recording_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::publish_session_store::private_file;
use crate::recorder::IncompleteReason;

/// The outbox's directory inside the recordings directory.
pub const DIRECTORY: &str = ".sapflow-outbox";
const LOCK: &str = ".lock";
/// The entry format this sapflow writes and reads.
const VERSION: u32 = 1;
/// Far above any entry: paths are at most a couple of hundred bytes.
const MAX_ENTRY_BYTES: u64 = 8 * 1024;
/// The largest JavaScript-safe integer, as the callback contract bounds
/// sequences, epochs and generations.
const MAX_JS_INTEGER: u64 = (1 << 53) - 1;
/// Recordings are hours long; a longer duration is a damaged entry.
const MAX_DURATION_MS: u64 = 366 * 24 * 3600 * 1000;

/// One recording's owed callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub version: u32,
    /// The recording file, relative to the recordings directory:
    /// `live/<eventId>/<name>.mp4`.
    pub file: String,
    pub has_video: bool,
    pub has_audio: bool,
    pub audio_rate: Option<u32>,
    /// The scoped callback identity; absent for the legacy contract.
    pub scoped: Option<ScopedIdentity>,
    /// Set once the file is finalised and synced.
    pub finished: Option<Finished>,
}

/// What a scoped completion carries besides the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopedIdentity {
    pub event_generation: u64,
    pub session_id: String,
    pub connection_epoch: u64,
    /// Reserved from the connection's sequence when the file was created,
    /// since after a restart there is no connection to take one from. The
    /// live completion uses it too, so a resend is the same body. It can be
    /// below telemetry already sent: completions are matched by
    /// `recordingId`, never fenced by sequence.
    pub event_sequence: u64,
    pub recording_id: String,
}

/// The writer's outcome, as its live callback carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Finished {
    pub complete: bool,
    pub reason: Option<String>,
    pub duration_ms: u64,
}

impl Entry {
    pub fn new(
        file: String,
        has_video: bool,
        has_audio: bool,
        audio_rate: Option<u32>,
        scoped: Option<ScopedIdentity>,
    ) -> Self {
        Self {
            version: VERSION,
            file,
            has_video,
            has_audio,
            audio_rate,
            scoped,
            finished: None,
        }
    }

    /// The stage path the file belongs to, if the entry is well formed.
    pub fn stream_path(&self) -> Option<StreamPath> {
        self.check().then(|| self.parts()).flatten().map(|(p, _)| p)
    }

    fn parts(&self) -> Option<(StreamPath, &str)> {
        let (parent, name) = self.file.rsplit_once('/')?;
        let path = StreamPath::parse(parent)?;
        (path.as_path() == parent).then_some((path, name))
    }

    /// Everything a recovered callback relies on, checked strictly.
    fn check(&self) -> bool {
        let Some((path, name)) = self.parts() else {
            return false;
        };
        let lower_hex = |s: &str, len: usize| {
            s.len() == len
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let name_ok = is_recording_name(name)
            && match &self.scoped {
                Some(scoped) => {
                    lower_hex(&scoped.recording_id, 32)
                        && name.ends_with(&format!("-{}.mp4", scoped.recording_id))
                        && lower_hex(&scoped.session_id, 32)
                        && (1..=MAX_JS_INTEGER).contains(&scoped.event_generation)
                        && (1..=MAX_JS_INTEGER).contains(&scoped.event_sequence)
                        && scoped.connection_epoch <= MAX_JS_INTEGER
                }
                None => !name.contains('-'),
            };
        let tracks_ok = (self.has_video || self.has_audio)
            && match self.audio_rate {
                Some(rate) => self.has_audio && rate > 0,
                None => !self.has_audio,
            };
        let finished_ok = self.finished.as_ref().is_none_or(|f| {
            f.duration_ms <= MAX_DURATION_MS
                && match (&f.reason, f.complete) {
                    (None, _) => true,
                    (Some(_), true) => false,
                    (Some(reason), false) => IncompleteReason::from_name(reason).is_some(),
                }
        });
        self.version == VERSION
            && path.event_id().len() <= 128
            && name_ok
            && tracks_ok
            && finished_ok
    }
}

/// The entry's name: a digest of the recording's relative path.
fn entry_name(file: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"sapflow-recording-outbox-v1\0");
    hash.update(file.as_bytes());
    let mut name = String::with_capacity(37);
    for byte in &hash.finalize()[..16] {
        use std::fmt::Write;
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".json");
    name
}

fn is_entry_name(name: &[u8]) -> bool {
    name.strip_suffix(b".json").is_some_and(|stem| {
        stem.len() == 32
            && stem
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    })
}

#[derive(Debug)]
pub enum OutboxError {
    /// The outbox directory is not this user's, or not private (0700).
    InsecureDirectory,
    /// The `.lock` is not a private regular file of this user's.
    InsecureLock,
    /// Another process holds the `.lock`.
    Locked,
    Io {
        operation: &'static str,
        source: io::Error,
    },
}

impl fmt::Display for OutboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsecureDirectory => write!(
                f,
                "{DIRECTORY} must be a directory owned by this user and private (0700)"
            ),
            Self::InsecureLock => write!(
                f,
                "{DIRECTORY}/{LOCK} must be this user's private (0600) regular file"
            ),
            Self::Locked => write!(f, "{DIRECTORY} is in use by another sapflow"),
            Self::Io { operation, source } => write!(f, "{DIRECTORY} {operation}: {source}"),
        }
    }
}

fn io_error(operation: &'static str, source: impl Into<io::Error>) -> OutboxError {
    OutboxError::Io {
        operation,
        source: source.into(),
    }
}

/// Why an entry could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Gone since it was listed.
    Missing,
    /// Written by a newer sapflow.
    Newer,
    /// Not a private regular file, too large, malformed, or not matching
    /// its name.
    Unusable,
    Io(io::ErrorKind),
}

/// The open outbox: the recordings directory and the outbox directory, both
/// as handles, and the lock held while this lives.
pub struct Outbox {
    /// `SAPFLOW_RECORDINGS` as configured, which names files in callbacks.
    recordings_path: PathBuf,
    recordings: File,
    directory: File,
    _lock: File,
    #[cfg(test)]
    pub(crate) fail_writes: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pub(crate) fail_directory_sync: std::sync::atomic::AtomicBool,
}

impl Outbox {
    /// Opens the outbox in `recordings`, creating both directories if they
    /// are missing, and removes temporary files a crash left behind.
    pub fn open(recordings: &Path) -> Result<Self, OutboxError> {
        std::fs::create_dir_all(recordings).map_err(|e| io_error("create recordings", e))?;
        let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let recordings_dir = File::from(
            fs::open(recordings, dir_flags, Mode::empty())
                .map_err(|e| io_error("open recordings", e))?,
        );
        let created = match fs::mkdirat(&recordings_dir, DIRECTORY, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(e) => return Err(io_error("create", e)),
        };
        let directory = File::from(
            fs::openat(
                &recordings_dir,
                DIRECTORY,
                dir_flags | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|e| {
                if e == rustix::io::Errno::LOOP || e == rustix::io::Errno::NOTDIR {
                    OutboxError::InsecureDirectory
                } else {
                    io_error("open", e)
                }
            })?,
        );
        // In a setgid recordings directory Linux makes the new directory
        // setgid too (2700). Set what was asked for; and judge only the
        // permission bits, which say who can read and write it.
        if created {
            fs::fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(|e| io_error("chmod", e))?;
        }
        let meta = directory.metadata().map_err(|e| io_error("metadata", e))?;
        if !meta.is_dir()
            || meta.mode() & 0o777 != 0o700
            || meta.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(OutboxError::InsecureDirectory);
        }
        let lock = File::from(
            fs::openat(
                &directory,
                LOCK,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| io_error("open lock", e))?,
        );
        private_file(&lock).map_err(|_| OutboxError::InsecureLock)?;
        fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                OutboxError::Locked
            } else {
                io_error("lock", e)
            }
        })?;
        let outbox = Self {
            recordings_path: recordings.to_owned(),
            recordings: recordings_dir,
            directory,
            _lock: lock,
            #[cfg(test)]
            fail_writes: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_directory_sync: std::sync::atomic::AtomicBool::new(false),
        };
        outbox.remove_stale_temporaries()?;
        Ok(outbox)
    }

    /// The path a callback names for `file`: as the writer names it.
    pub fn segment_path(&self, file: &str) -> String {
        self.recordings_path.join(file).display().to_string()
    }

    /// The recording's path relative to the recordings directory, for a file
    /// the writer created at `file_path`.
    pub fn relative(&self, file_path: &Path) -> Option<String> {
        file_path
            .strip_prefix(&self.recordings_path)
            .ok()?
            .to_str()
            .map(str::to_owned)
    }

    fn remove_stale_temporaries(&self) -> Result<(), OutboxError> {
        let mut dir = fs::Dir::read_from(&self.directory).map_err(|e| io_error("list", e))?;
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|e| io_error("list", e))?;
            let name = entry.file_name().to_bytes();
            let ours = name
                .strip_prefix(b".")
                .and_then(|rest| {
                    let at = rest.windows(5).position(|w| w == b".tmp-")?;
                    Some((&rest[..at], &rest[at + 5..]))
                })
                .is_some_and(|(stem, nonce)| {
                    is_entry_name(stem)
                        && nonce.len() == 16
                        && nonce.iter().all(u8::is_ascii_hexdigit)
                });
            if ours && entry.file_type() == fs::FileType::RegularFile {
                fs::unlinkat(&self.directory, entry.file_name(), AtFlags::empty())
                    .map_err(|e| io_error("remove stale temporary", e))?;
            }
        }
        Ok(())
    }

    /// Writes `entry` over any entry for the same file, durably: a new
    /// temporary file, synced, renamed over it, then the directory synced.
    /// Once the rename has happened the entry is written: a failed directory
    /// sync after it returns `Ok(false)`, written but perhaps not durable,
    /// never an error. Blocks; call it off the async workers.
    pub fn write(&self, entry: &Entry) -> io::Result<bool> {
        #[cfg(test)]
        if self.fail_writes.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::other("test fault"));
        }
        if !entry.check() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let bytes = serde_json::to_vec(entry).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let name = entry_name(&entry.file);
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|_| io::Error::other("no randomness"))?;
        let temporary = format!(
            ".{name}.tmp-{}",
            nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let mut file = File::from(fs::openat(
            &self.directory,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?);
        let written = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::renameat(&self.directory, &temporary, &self.directory, &name)?;
            Ok::<_, io::Error>(())
        })();
        if let Err(error) = written {
            let _ = fs::unlinkat(&self.directory, &temporary, AtFlags::empty());
            return Err(error);
        }
        #[cfg(test)]
        if self
            .fail_directory_sync
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(false);
        }
        Ok(self.directory.sync_all().is_ok())
    }

    /// Removes the entry for `file`, if there is one. Not synced: should the
    /// removal be lost, the callback is sent again, which the app treats as
    /// the same part. Blocks; call it off the async workers.
    pub fn remove(&self, file: &str) -> io::Result<()> {
        self.remove_name(OsStr::new(&entry_name(file)))
    }

    fn remove_name(&self, name: &OsStr) -> io::Result<()> {
        match fs::unlinkat(&self.directory, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The names of up to `limit` entries, oldest first, and how many more
    /// there are.
    pub fn list(&self, limit: usize) -> io::Result<(Vec<OsString>, usize)> {
        let mut found = Vec::new();
        let mut dir = fs::Dir::read_from(&self.directory)?;
        while let Some(entry) = dir.read() {
            let entry = entry?;
            if !is_entry_name(entry.file_name().to_bytes()) {
                continue;
            }
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            let name = OsString::from(name);
            let Ok(file) = self.open_entry(&name) else {
                continue;
            };
            let Some(modified) = file.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            found.push((modified, name));
        }
        found.sort();
        let more = found.len().saturating_sub(limit);
        found.truncate(limit);
        Ok((found.into_iter().map(|(_, name)| name).collect(), more))
    }

    fn open_entry(&self, name: &OsStr) -> io::Result<File> {
        Ok(File::from(fs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    /// Reads the entry `name`, and when it was last written. Blocks.
    pub fn read(&self, name: &OsStr) -> Result<(Entry, SystemTime), ReadError> {
        let file = self.open_entry(name).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => ReadError::Missing,
            _ if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
                ReadError::Unusable
            }
            kind => ReadError::Io(kind),
        })?;
        private_file(&file).map_err(|_| ReadError::Unusable)?;
        let modified = file
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|e| ReadError::Io(e.kind()))?;
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ReadError::Io(e.kind()))?;
        Ok((decode(&bytes, name)?, modified))
    }

    /// Opens a recording named by an entry, without following a symbolic
    /// link at its end, as a regular file. Blocks.
    pub fn open_recording(&self, file: &str) -> io::Result<File> {
        let opened = File::from(fs::openat(
            &self.recordings,
            file,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if !opened.metadata()?.is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(opened)
    }

    /// Removes entries whose recording has gone or is older than `max_age`,
    /// and entries that cannot be read and are themselves that old. Returns
    /// how many it removed. Blocks.
    pub fn sweep(&self, max_age: Duration, now: SystemTime) -> usize {
        let old = |time: SystemTime| now.duration_since(time).is_ok_and(|age| age > max_age);
        let Ok((names, _)) = self.list(usize::MAX) else {
            return 0;
        };
        let mut removed = 0;
        for name in names {
            let stale = match self.read(&name) {
                Ok((entry, _)) => match self.open_recording(&entry.file) {
                    Ok(file) => file.metadata().and_then(|m| m.modified()).is_ok_and(old),
                    Err(e) => {
                        e.kind() == io::ErrorKind::NotFound
                            || e.kind() == io::ErrorKind::InvalidInput
                            || e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
                    }
                },
                Err(ReadError::Missing) => false,
                Err(_) => self
                    .open_entry(&name)
                    .and_then(|f| f.metadata())
                    .and_then(|m| m.modified())
                    .is_ok_and(old),
            };
            if stale && self.remove_name(&name).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// Strictly what [`Outbox::write`] wrote for the entry `name`.
fn decode(bytes: &[u8], name: &OsStr) -> Result<Entry, ReadError> {
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return Err(ReadError::Unusable);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ReadError::Unusable)?;
    if value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|version| version > u64::from(VERSION))
    {
        return Err(ReadError::Newer);
    }
    let entry: Entry = serde_json::from_value(value).map_err(|_| ReadError::Unusable)?;
    if !entry.check() || name.as_encoded_bytes() != entry_name(&entry.file).as_bytes() {
        return Err(ReadError::Unusable);
    }
    Ok(entry)
}

/// One recording's entry, carried by its writer and then its callback, so
/// that the outcome can be written and the entry removed on delivery.
/// Dropping it leaves the entry for the next start: a writer dropped at the
/// stop deadline, or a callback never delivered, is reported then.
pub struct OutboxEntry {
    outbox: Arc<Outbox>,
    entry: Entry,
}

impl OutboxEntry {
    pub fn new(outbox: Arc<Outbox>, entry: Entry) -> Self {
        Self { outbox, entry }
    }

    /// Writes the entry, `open`: before the writer writes its file. The
    /// handle comes back whatever the write did, so that the outcome is
    /// still written after a transient failure, and delivery or a decided
    /// failure still removes an entry the write did leave (removing a
    /// missing one is harmless).
    pub async fn open(outbox: Arc<Outbox>, entry: Entry) -> Self {
        let this = Self { outbox, entry };
        this.store().await;
        this
    }

    /// The sequence a scoped completion reserved, which its live callback
    /// uses too.
    pub fn reserved_sequence(&self) -> Option<u64> {
        self.entry.scoped.as_ref().map(|s| s.event_sequence)
    }

    async fn store(&self) -> bool {
        let outbox = self.outbox.clone();
        let entry = self.entry.clone();
        let file = entry.file.clone();
        match tokio::task::spawn_blocking(move || outbox.write(&entry)).await {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => {
                tracing::warn!(
                    file,
                    "recording outbox entry written but its directory not synced; a power cut now could lose it"
                );
                true
            }
            result => {
                let error = match result {
                    Ok(Err(error)) => error.to_string(),
                    _ => "task failed".to_owned(),
                };
                tracing::error!(
                    file,
                    error,
                    "recording outbox write failed; a crash now would leave this recording unreported"
                );
                false
            }
        }
    }

    /// Records the writer's outcome before its callback is queued. On a
    /// failed write the `open` entry stays: a crash then reports the file
    /// interrupted rather than not at all.
    pub async fn finish(&mut self, finished: Finished) {
        self.entry.finished = Some(finished);
        self.store().await;
    }

    /// Removes the entry: the callback was delivered, or the writer decided
    /// the file owes nothing.
    pub async fn remove(self) {
        let file = self.entry.file.clone();
        let outbox = self.outbox;
        if !matches!(
            tokio::task::spawn_blocking(move || outbox.remove(&file)).await,
            Ok(Ok(()))
        ) {
            tracing::warn!(
                file = self.entry.file,
                "recording outbox entry not removed; its callback may be sent again at the next start"
            );
        }
    }

    pub fn file(&self) -> &str {
        &self.entry.file
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A recordings directory of the test's own under `target/`.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new(label: &str) -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target")
                .join(format!(
                    "recording-outbox-{label}-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).expect("scratch");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn legacy(file: &str) -> Entry {
        Entry::new(file.into(), true, true, Some(48_000), None)
    }

    fn scoped(file_start: u64) -> Entry {
        let id = format!("{:032x}", 7);
        Entry::new(
            format!("live/event-a/{file_start}-{id}.mp4"),
            false,
            true,
            Some(44_100),
            Some(ScopedIdentity {
                event_generation: 2,
                session_id: format!("{:032x}", 11),
                connection_epoch: 3,
                event_sequence: 4,
                recording_id: id,
            }),
        )
    }

    #[test]
    fn entries_round_trip_and_are_removed() {
        let scratch = Scratch::new("round-trip");
        let outbox = Outbox::open(&scratch.0).expect("open");
        let mut entry = scoped(100);
        outbox.write(&entry).expect("write open");
        entry.finished = Some(Finished {
            complete: false,
            reason: Some("server-stopped".into()),
            duration_ms: 3_210,
        });
        outbox.write(&entry).expect("rewrite finished");
        let legacy = legacy("live/event-b/200.mp4");
        outbox.write(&legacy).expect("write legacy");
        let (names, more) = outbox.list(10).expect("list");
        assert_eq!((names.len(), more), (2, 0));
        let mut read: Vec<Entry> = names
            .iter()
            .map(|name| outbox.read(name).expect("read").0)
            .collect();
        read.sort_by(|a, b| a.file.cmp(&b.file));
        assert_eq!(read, [entry.clone(), legacy.clone()]);
        assert_eq!(outbox.list(1).expect("limited").1, 1);
        outbox.remove(&entry.file).expect("remove");
        outbox
            .remove(&entry.file)
            .expect("removing twice is harmless");
        assert_eq!(outbox.list(10).expect("list").0.len(), 1);
        // Entries are private files in a private directory.
        let dir = scratch.0.join(DIRECTORY);
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(entry_name(&legacy.file))), 0o600);
    }

    #[test]
    fn malformed_entries_are_refused_and_never_written() {
        let mut bad = vec![
            legacy("live/event-a/../200.mp4"),
            legacy("live/event-a/200.mp4.tmp"),
            legacy("other/event-a/200.mp4"),
            legacy("live/event-a/200-0123.mp4"),
            Entry::new("live/event-a/1.mp4".into(), false, false, None, None),
            Entry::new("live/event-a/1.mp4".into(), true, false, Some(1), None),
            Entry::new("live/event-a/1.mp4".into(), false, true, None, None),
        ];
        let mut wrong_id = scoped(1);
        wrong_id.scoped.as_mut().expect("scoped").recording_id = format!("{:032x}", 8);
        bad.push(wrong_id);
        let mut zero_sequence = scoped(1);
        zero_sequence
            .scoped
            .as_mut()
            .expect("scoped")
            .event_sequence = 0;
        bad.push(zero_sequence);
        let mut unknown_reason = legacy("live/event-a/1.mp4");
        unknown_reason.finished = Some(Finished {
            complete: false,
            reason: Some("anything <b>".into()),
            duration_ms: 1,
        });
        bad.push(unknown_reason);
        let mut complete_with_reason = legacy("live/event-a/1.mp4");
        complete_with_reason.finished = Some(Finished {
            complete: true,
            reason: Some("disk-low".into()),
            duration_ms: 1,
        });
        bad.push(complete_with_reason);
        let scratch = Scratch::new("malformed");
        let outbox = Outbox::open(&scratch.0).expect("open");
        for entry in &bad {
            assert!(!entry.check(), "{entry:?}");
            assert!(outbox.write(entry).is_err());
            let bytes = serde_json::to_vec(entry).expect("json");
            let name = entry_name(&entry.file);
            assert!(decode(&bytes, OsStr::new(&name)).is_err());
        }
        assert!(outbox.list(10).expect("list").0.is_empty());
        let good = serde_json::to_vec(&legacy("live/event-a/1.mp4")).expect("json");
        let good_name = entry_name("live/event-a/1.mp4");
        assert!(decode(&good, OsStr::new(&good_name)).is_ok());
        // An entry under another entry's name, or a newer format, is not read
        // as one of ours.
        let dir = scratch.0.join(DIRECTORY);
        let other = entry_name("live/event-a/2.mp4");
        let private = |path: &Path, bytes: &[u8]| {
            std::fs::write(path, bytes).expect("write");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        };
        private(&dir.join(&other), &good);
        assert_eq!(
            outbox.read(OsStr::new(&other)).expect_err("misnamed"),
            ReadError::Unusable
        );
        let newer = br#"{"version":2,"file":"live/event-a/3.mp4"}"#;
        let newer_name = entry_name("live/event-a/3.mp4");
        private(&dir.join(&newer_name), newer);
        assert_eq!(
            outbox.read(OsStr::new(&newer_name)).expect_err("newer"),
            ReadError::Newer
        );
        // A world-readable entry is not ours.
        let open_name = entry_name("live/event-a/1.mp4");
        std::fs::write(dir.join(&open_name), &good).expect("write");
        std::fs::set_permissions(dir.join(&open_name), std::fs::Permissions::from_mode(0o644))
            .expect("mode");
        assert_eq!(
            outbox.read(OsStr::new(&open_name)).expect_err("insecure"),
            ReadError::Unusable
        );
    }

    #[test]
    fn a_second_process_and_an_insecure_directory_are_refused() {
        let scratch = Scratch::new("lock");
        let first = Outbox::open(&scratch.0).expect("open");
        assert!(matches!(Outbox::open(&scratch.0), Err(OutboxError::Locked)));
        drop(first);
        let dir = scratch.0.join(DIRECTORY);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("mode");
        assert!(matches!(
            Outbox::open(&scratch.0),
            Err(OutboxError::InsecureDirectory)
        ));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("mode");
        // A symbolic link in its place is never followed.
        let elsewhere = Scratch::new("lock-elsewhere");
        let linked = Scratch::new("lock-linked");
        std::os::unix::fs::symlink(&elsewhere.0, linked.0.join(DIRECTORY)).expect("symlink");
        assert!(matches!(
            Outbox::open(&linked.0),
            Err(OutboxError::InsecureDirectory)
        ));
        // A crash between creating and renaming a temporary leaves it; the
        // next open removes it, and nothing else.
        let stale = dir.join(format!(".{}.tmp-0123456789abcdef", entry_name("x")));
        std::fs::write(&stale, b"partial").expect("stale");
        std::fs::write(dir.join("notes.txt"), b"kept").expect("other");
        let _outbox = Outbox::open(&scratch.0).expect("reopen");
        assert!(!stale.exists());
        assert!(dir.join("notes.txt").exists());
    }

    #[test]
    fn sweep_removes_entries_whose_recordings_are_gone_or_expired() {
        let scratch = Scratch::new("sweep");
        let outbox = Outbox::open(&scratch.0).expect("open");
        let event = scratch.0.join("live/event-a");
        std::fs::create_dir_all(&event).expect("event dir");
        for (name, age) in [("100.mp4", 3600), ("200.mp4", 0)] {
            let file = std::fs::File::create(event.join(name)).expect("recording");
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .expect("mtime");
        }
        for file in [
            "live/event-a/100.mp4",
            "live/event-a/200.mp4",
            "live/event-a/300.mp4",
        ] {
            outbox.write(&legacy(file)).expect("write");
        }
        // Unreadable, and recent: kept until it is as old as a recording.
        let junk = scratch.0.join(DIRECTORY).join(entry_name("junk"));
        std::fs::write(&junk, b"{").expect("junk");
        std::fs::set_permissions(&junk, std::fs::Permissions::from_mode(0o600)).expect("mode");
        let removed = outbox.sweep(Duration::from_secs(600), SystemTime::now());
        assert_eq!(removed, 2, "the expired and the missing recording");
        let (names, _) = outbox.list(10).expect("list");
        let files: Vec<String> = names
            .iter()
            .filter_map(|name| outbox.read(name).ok().map(|(entry, _)| entry.file))
            .collect();
        assert_eq!(files, ["live/event-a/200.mp4"]);
        assert!(junk.exists());
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(outbox.sweep(Duration::from_secs(600), later), 2);
        assert!(!junk.exists());
        // The recordings themselves are never touched by the outbox.
        assert!(event.join("100.mp4").exists() && event.join("200.mp4").exists());
    }
}

#[cfg(test)]
mod durability_tests {
    use super::tests::{Scratch, legacy};
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn a_setgid_recordings_directory_does_not_turn_recovery_off() {
        // Linux makes a directory created in a setgid one setgid too: 2700.
        // Only Linux exercises that inheritance (CI runs it); macOS does
        // not inherit the bit, so there the 2700 case is set by hand below.
        let scratch = Scratch::new("setgid");
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o2755))
            .expect("setgid recordings");
        let outbox = Outbox::open(&scratch.0).expect("opens in a setgid directory");
        let dir = scratch.0.join(DIRECTORY);
        assert_eq!(mode(&dir) & 0o777, 0o700);
        drop(outbox);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o2700))
            .expect("setgid outbox");
        if mode(&dir) & 0o2000 != 0 {
            let _outbox = Outbox::open(&scratch.0).expect("a 2700 outbox is still private");
        } else {
            eprintln!(
                "this filesystem would not set the setgid bit; the 2700 case is untested here"
            );
        }
        // Group or other access is still refused.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o2750)).expect("mode");
        assert!(matches!(
            Outbox::open(&scratch.0),
            Err(OutboxError::InsecureDirectory)
        ));
    }

    #[test]
    fn a_rename_whose_directory_sync_fails_is_written_but_not_durable() {
        let scratch = Scratch::new("dir-sync");
        let outbox = Outbox::open(&scratch.0).expect("outbox");
        outbox
            .fail_directory_sync
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let entry = legacy("live/event-a/100.mp4");
        assert!(!outbox.write(&entry).expect("written"), "not durable");
        assert_eq!(outbox.list(10).expect("list").0.len(), 1);
    }

    #[tokio::test]
    async fn an_entry_handle_survives_a_failed_write_and_still_writes_and_removes() {
        let scratch = Scratch::new("handle");
        let outbox = Arc::new(Outbox::open(&scratch.0).expect("outbox"));
        let names = |outbox: &Outbox| outbox.list(10).expect("list").0;
        // The directory sync fails after the rename: the entry is there, and
        // its handle comes back, so delivery can still remove it.
        outbox
            .fail_directory_sync
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let handle = OutboxEntry::open(outbox.clone(), legacy("live/event-a/100.mp4")).await;
        assert_eq!(names(&outbox).len(), 1);
        handle.remove().await;
        assert!(names(&outbox).is_empty());
        // A transient failure at open: the handle still comes back, and the
        // outcome is written once writes work again.
        outbox
            .fail_writes
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut handle = OutboxEntry::open(outbox.clone(), legacy("live/event-a/200.mp4")).await;
        assert!(names(&outbox).is_empty());
        outbox
            .fail_writes
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let finished = Finished {
            complete: true,
            reason: None,
            duration_ms: 1_000,
        };
        handle.finish(finished.clone()).await;
        let listed = names(&outbox);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            outbox.read(&listed[0]).expect("entry").0.finished,
            Some(finished)
        );
        handle.remove().await;
        assert!(names(&outbox).is_empty());
    }
}
