//! The SRT replay guard's admitted tier on disk (`SAPFLOW_SRT_REPLAY_STORE`),
//! so a restart does not forget which key material already admitted a
//! connection. Without it a recorded handshake replayed after a restart would
//! restore that connection's keys, and the recorded media would decrypt onto
//! the live stage again.
//!
//! It holds stage paths and SHA-256 digests of key material that crossed the
//! network in clear: nothing secret, and no key material. The file is a
//! private (0600) regular file in an owned private (0700) directory, as the
//! publish-session store requires, held by one process through a `.lock`
//! beside it. Each write goes to a new temporary file that is synced, then
//! renamed over the old one, then the directory synced: a failed write
//! leaves the old file as it was.
//!
//! A malformed or unreadable file refuses to start, and so does a missing
//! file once the lock exists (it was there and has gone): never an empty
//! start the operator did not choose. SHA-256 over the contents catches
//! accidental damage, not a rollback by the owning user.

use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use rustix::fs::{self, AtFlags, FlockOperation, Mode, OFlags};
use sha2::{Digest, Sha256};

use crate::publish_session_store::{StoreError, open_parent, private_file};

const MAGIC: &[u8] = b"sapflow srt replay store\n";
const VERSION: u8 = 1;
/// Far beyond what the guard keeps (its bounds, plus connections still open).
pub const MAX_ENTRIES: usize = 65_536;
/// A stage path's longest form (`live/` and a 128-byte event id).
const MAX_PATH: usize = 133;
const MAX_BYTES: usize = MAGIC.len() + 1 + 4 + MAX_ENTRIES * (1 + MAX_PATH + 32) + 32;

/// One admitted caller's key material: its stage path and digest.
pub type Record = (String, [u8; 32]);

#[derive(Debug)]
pub enum ReplayStoreError {
    InvalidPath,
    InsecureDirectory,
    InsecureFile,
    Locked,
    /// The lock is there but the file is not: it was removed.
    Missing,
    Corrupt,
    TooLarge,
    Io {
        operation: &'static str,
        source: io::Error,
    },
}

/// How to start again after a lost or damaged file, at the price it costs.
const RECOVER: &str = "to start without it, move the file and its .lock aside; captures of earlier connections can then be replayed until the app rotates those streams' passphrases";

impl fmt::Display for ReplayStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => f.write_str(
                "invalid SRT replay store path (a file name of letters, digits, '-', '_' or '.')",
            ),
            Self::InsecureDirectory => f.write_str(
                "the SRT replay store's directory must exist, be owned by this user and be private (0700)",
            ),
            Self::InsecureFile => f.write_str(
                "the SRT replay store and its .lock must be this user's private (0600) regular files with one link",
            ),
            Self::Locked => f.write_str("the SRT replay store is in use by another process"),
            Self::Missing => write!(
                f,
                "the SRT replay store is missing but its .lock is there, so it was removed; {RECOVER}"
            ),
            Self::Corrupt => write!(f, "the SRT replay store is malformed; {RECOVER}"),
            Self::TooLarge => write!(f, "the SRT replay store is too large; {RECOVER}"),
            Self::Io { operation, source } => {
                write!(f, "SRT replay store {operation}: {source}")
            }
        }
    }
}

impl std::error::Error for ReplayStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(operation: &'static str, source: impl Into<io::Error>) -> ReplayStoreError {
    ReplayStoreError::Io {
        operation,
        source: source.into(),
    }
}

/// The publish-session store's path and permission checks, in this store's
/// words.
fn from_store(error: StoreError) -> ReplayStoreError {
    match error {
        StoreError::InvalidPath => ReplayStoreError::InvalidPath,
        StoreError::InsecureDirectory => ReplayStoreError::InsecureDirectory,
        StoreError::InsecureFile => ReplayStoreError::InsecureFile,
        StoreError::Io { operation, source } => ReplayStoreError::Io { operation, source },
        _ => ReplayStoreError::Corrupt,
    }
}

/// Encodes records, oldest first: the magic line, a version byte, a count,
/// then each path's length, path and digest, then SHA-256 of all before it.
pub fn encode<'a>(
    records: impl ExactSizeIterator<Item = (&'a str, &'a [u8; 32])>,
) -> Result<Vec<u8>, ReplayStoreError> {
    let count = u32::try_from(records.len())
        .ok()
        .filter(|&n| n as usize <= MAX_ENTRIES)
        .ok_or(ReplayStoreError::TooLarge)?;
    let mut out = Vec::with_capacity(MAGIC.len() + 5 + records.len() * 64 + 32);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&count.to_be_bytes());
    for (path, digest) in records {
        let len = u8::try_from(path.len())
            .ok()
            .filter(|&n| usize::from(n) <= MAX_PATH)
            .ok_or(ReplayStoreError::TooLarge)?;
        out.push(len);
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(digest);
    }
    let checksum = Sha256::digest(&out);
    out.extend_from_slice(&checksum);
    Ok(out)
}

/// Decodes what [`encode`] wrote, strictly: any other byte, a bad stage
/// path or a checksum mismatch is [`ReplayStoreError::Corrupt`].
pub fn decode(bytes: &[u8]) -> Result<Vec<Record>, ReplayStoreError> {
    if bytes.len() > MAX_BYTES {
        return Err(ReplayStoreError::TooLarge);
    }
    let corrupt = || ReplayStoreError::Corrupt;
    let body_len = bytes.len().checked_sub(32).ok_or_else(corrupt)?;
    let (body, checksum) = bytes.split_at(body_len);
    if Sha256::digest(body).as_slice() != checksum {
        return Err(corrupt());
    }
    let rest = body.strip_prefix(MAGIC).ok_or_else(corrupt)?;
    let (&version, rest) = rest.split_first().ok_or_else(corrupt)?;
    if version != VERSION {
        return Err(corrupt());
    }
    let (count, mut rest) = rest.split_first_chunk::<4>().ok_or_else(corrupt)?;
    let count = u32::from_be_bytes(*count) as usize;
    if count > MAX_ENTRIES {
        return Err(corrupt());
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let (&len, after) = rest.split_first().ok_or_else(corrupt)?;
        let len = usize::from(len);
        if len > MAX_PATH || after.len() < len + 32 {
            return Err(corrupt());
        }
        let (path, after) = after.split_at(len);
        let path = std::str::from_utf8(path).map_err(|_| corrupt())?;
        if sapflow_core::StreamPath::parse(path).is_none_or(|p| p.as_path() != path) {
            return Err(corrupt());
        }
        let (digest, after) = after.split_first_chunk::<32>().ok_or_else(corrupt)?;
        records.push((path.to_owned(), *digest));
        rest = after;
    }
    if !rest.is_empty() {
        return Err(corrupt());
    }
    Ok(records)
}

/// The open store: its directory, file name and the lock held for this
/// process's lifetime.
pub struct ReplayStore {
    directory: File,
    name: OsString,
    _lock: File,
    #[cfg(test)]
    pub(crate) fail_before_rename: bool,
}

impl ReplayStore {
    /// Opens the store at `path` and reads its records, oldest first. The
    /// directory must exist. On a first start (neither file nor lock there)
    /// it writes an empty store at once.
    pub fn open(path: &Path) -> Result<(Self, Vec<Record>), ReplayStoreError> {
        let (directory, name) = open_parent(path).map_err(from_store)?;
        let mut lock_name = name.clone();
        lock_name.push(".lock");
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let (lock, new_lock) = match fs::openat(
            &directory,
            &lock_name,
            flags | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => (File::from(fd), true),
            Err(rustix::io::Errno::EXIST) => (
                File::from(
                    fs::openat(&directory, &lock_name, flags, Mode::empty())
                        .map_err(|e| io_error("open lock", e))?,
                ),
                false,
            ),
            Err(e) => return Err(io_error("create lock", e)),
        };
        private_file(&lock).map_err(from_store)?;
        fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                ReplayStoreError::Locked
            } else {
                io_error("lock", e)
            }
        })?;
        let mut store = Self {
            directory,
            name,
            _lock: lock,
            #[cfg(test)]
            fail_before_rename: false,
        };
        match store.read()? {
            Some(records) => Ok((store, records)),
            None if new_lock => {
                store.write(&encode(std::iter::empty())?)?;
                Ok((store, Vec::new()))
            }
            None => Err(ReplayStoreError::Missing),
        }
    }

    fn read(&self) -> Result<Option<Vec<Record>>, ReplayStoreError> {
        let fd = match fs::openat(
            &self.directory,
            &self.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io_error("open", e)),
        };
        let file = File::from(fd);
        private_file(&file).map_err(from_store)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("read", e))?;
        decode(&bytes).map(Some)
    }

    /// Replaces the store with `bytes` ([`encode`]d): a new temporary file,
    /// synced, renamed over the old one, then the directory synced. Until the
    /// rename the old file is untouched; a failed attempt removes its
    /// temporary file.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), ReplayStoreError> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce)
            .map_err(|_| io_error("temporary name", io::Error::other("no randomness")))?;
        let mut temporary = OsString::from(".");
        temporary.push(&self.name);
        temporary.push(".tmp-");
        temporary.push(nonce.iter().map(|b| format!("{b:02x}")).collect::<String>());
        // CREATE with EXCL and NOFOLLOW never truncates or follows anything.
        let mut file = File::from(
            fs::openat(
                &self.directory,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| io_error("create temporary", e))?,
        );
        let written = (|| {
            file.write_all(bytes)
                .map_err(|e| io_error("write temporary", e))?;
            file.sync_all().map_err(|e| io_error("sync temporary", e))?;
            #[cfg(test)]
            if self.fail_before_rename {
                return Err(io_error("rename", io::Error::other("test fault")));
            }
            fs::renameat(&self.directory, &temporary, &self.directory, &self.name)
                .map_err(|e| io_error("rename temporary", e))
        })();
        if let Err(error) = written {
            let _ = fs::unlinkat(&self.directory, &temporary, AtFlags::empty());
            return Err(error);
        }
        self.directory
            .sync_all()
            .map_err(|e| io_error("sync directory", e))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs::DirBuilder;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A private directory of the test's own under `target/`.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
            let path = root.join(format!(
                "srt-replay-store-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .expect("private directory");
            Self(path.canonicalize().expect("canonical"))
        }
        pub(crate) fn path(&self) -> PathBuf {
            self.0.join("srt-replay")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn records(n: u8) -> Vec<Record> {
        (0..n)
            .map(|i| (format!("live/event-{i}"), [i; 32]))
            .collect()
    }

    fn bytes(records: &[Record]) -> Vec<u8> {
        encode(records.iter().map(|(p, d)| (p.as_str(), d))).expect("encode")
    }

    #[test]
    fn records_round_trip_in_order() {
        let r = records(5);
        assert_eq!(decode(&bytes(&r)).expect("decode"), r);
        assert_eq!(decode(&bytes(&[])).expect("empty"), Vec::<Record>::new());
    }

    #[test]
    fn any_damage_is_corrupt() {
        let good = bytes(&records(3));
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0x01;
            assert!(decode(&bad).is_err(), "byte {i}");
        }
        for cut in 0..good.len() {
            assert!(decode(&good[..cut]).is_err(), "cut at {cut}");
        }
        let mut longer = good.clone();
        longer.push(0);
        assert!(decode(&longer).is_err());
        // A valid checksum over a path that is not a stage path.
        let mut body = good[..good.len() - 32].to_vec();
        let at = body.windows(5).position(|w| w == b"live/").expect("a path");
        body[at] = b'L';
        let checksum = Sha256::digest(&body);
        body.extend_from_slice(&checksum);
        assert!(matches!(decode(&body), Err(ReplayStoreError::Corrupt)));
    }

    #[test]
    fn a_first_start_writes_an_empty_store_and_a_restart_reads_it() {
        let scratch = Scratch::new();
        let (mut store, loaded) = ReplayStore::open(&scratch.path()).expect("first start");
        assert!(loaded.is_empty());
        assert!(scratch.path().exists(), "written at once");
        store.write(&bytes(&records(4))).expect("write");
        assert!(matches!(
            ReplayStore::open(&scratch.path()),
            Err(ReplayStoreError::Locked)
        ));
        drop(store);
        let (_store, loaded) = ReplayStore::open(&scratch.path()).expect("restart");
        assert_eq!(loaded, records(4));
    }

    #[test]
    fn a_corrupt_missing_or_insecure_store_refuses_to_open() {
        // Corrupt.
        let scratch = Scratch::new();
        drop(ReplayStore::open(&scratch.path()).expect("first start"));
        std::fs::write(scratch.path(), b"not a store").expect("damage");
        let error = ReplayStore::open(&scratch.path()).err().expect("refused");
        assert!(matches!(error, ReplayStoreError::Corrupt));
        assert!(
            error
                .to_string()
                .contains("move the file and its .lock aside")
        );
        // Removed after a first start: the lock is still there.
        std::fs::remove_file(scratch.path()).expect("remove");
        let error = ReplayStore::open(&scratch.path()).err().expect("refused");
        assert!(matches!(error, ReplayStoreError::Missing), "{error}");
        // No such directory.
        let gone = scratch.0.join("absent").join("srt-replay");
        assert!(matches!(
            ReplayStore::open(&gone),
            Err(ReplayStoreError::Io { .. })
        ));
        // A directory others can read.
        let open_dir = Scratch::new();
        std::fs::set_permissions(
            &open_dir.0,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod");
        assert!(matches!(
            ReplayStore::open(&open_dir.path()),
            Err(ReplayStoreError::InsecureDirectory)
        ));
        // A file others can read.
        let shared = Scratch::new();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(shared.path())
            .expect("file")
            .write_all(&bytes(&[]))
            .expect("write");
        assert!(matches!(
            ReplayStore::open(&shared.path()),
            Err(ReplayStoreError::InsecureFile)
        ));
    }

    #[test]
    fn a_failed_write_leaves_the_old_store_intact() {
        let scratch = Scratch::new();
        let (mut store, _) = ReplayStore::open(&scratch.path()).expect("first start");
        store.write(&bytes(&records(2))).expect("write");
        store.fail_before_rename = true;
        assert!(store.write(&bytes(&records(9))).is_err());
        drop(store);
        let (_store, loaded) = ReplayStore::open(&scratch.path()).expect("reopen");
        assert_eq!(loaded, records(2), "the old store");
        let names: Vec<String> = std::fs::read_dir(&scratch.0)
            .expect("list")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.contains(".tmp-")),
            "temporary removed: {names:?}"
        );
    }
}
