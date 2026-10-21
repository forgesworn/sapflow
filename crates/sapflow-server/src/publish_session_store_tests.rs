use super::*;
use std::fs::{self as stdfs, DirBuilder, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let path = root.join(format!(
            "publish-session-store-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("task-owned private directory");
        Self(path.canonicalize().expect("canonical task-owned directory"))
    }
    fn path(&self) -> PathBuf {
        self.0.join("metadata.json")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = stdfs::remove_dir_all(&self.0);
    }
}
fn snapshot() -> Snapshot {
    Snapshot {
        version: sapflow_core::publish_session::SNAPSHOT_VERSION,
        clock_floor_ms: 1000,
        rows: vec![SnapshotRow {
            path: StreamPath::parse("live/event-fixture").expect("fixture"),
            event_generation: JsPositive::new(2).expect("fixture"),
            session_id: SessionId::parse("0123456789abcdef0123456789abcdef").expect("fixture"),
            revision: JsPositive::new(3).expect("fixture"),
            request_fingerprint: Fingerprint::new([17; 32]),
            status: SessionStatus::Prepared,
            grant: Some(GrantMetadata {
                expires_at_ms: EpochMs::new(100_000).expect("fixture"),
                reconnect_grace_ms: 90_000,
                immutable_fingerprint: Fingerprint::new([29; 32]),
            }),
            connection_epoch: 7,
        }],
    }
}
fn next_snapshot() -> Snapshot {
    let mut next = snapshot();
    next.rows[0].revision = JsPositive::new(4).expect("fixture");
    next.rows[0].request_fingerprint = Fingerprint::new([18; 32]);
    next
}
fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("new fixture file");
    file.write_all(bytes).expect("fixture write");
}
fn seeded(scratch: &Scratch) -> PublishSessionStore {
    let (mut store, loaded) = PublishSessionStore::open(&scratch.path()).expect("fresh open");
    assert!(loaded.is_none());
    store.replace(&snapshot()).expect("seed");
    store
}
fn encoded_document(snapshot: &Snapshot) -> Vec<u8> {
    let document = Document::from(snapshot);
    let checksum_sha256 = Sha256::digest(encode(&document).expect("encode")).into();
    encode(&Envelope {
        format_version: FORMAT_VERSION,
        metadata: document,
        checksum_sha256,
    })
    .expect("envelope")
}

#[test]
fn byte_decoder_has_the_same_strict_bound_checksum_and_semantic_rules_without_file_access() {
    let bytes = encoded_document(&snapshot());
    assert_eq!(
        decode_document(&bytes)
            .expect("valid metadata")
            .snapshot()
            .expect("snapshot"),
        snapshot()
    );
    assert!(matches!(decode_document(&[]), Err(StoreError::Corrupt)));
    assert!(matches!(
        decode_document(&vec![0; MAX_STORE_BYTES + 1]),
        Err(StoreError::TooLarge)
    ));
    let mut corrupt = bytes.clone();
    corrupt[0] = b'[';
    assert!(matches!(
        decode_document(&corrupt),
        Err(StoreError::Corrupt)
    ));
    let mut invalid = snapshot();
    invalid.rows[0].connection_epoch = sapflow_core::publish_session::MAX_JS_INTEGER + 1;
    assert!(matches!(
        decode_document(&encoded_document(&invalid)),
        Err(StoreError::InvalidSnapshot)
    ));
    #[cfg(feature = "fuzzing")]
    {
        assert!(validate_snapshot_bytes(&bytes).is_ok());
        assert!(matches!(
            validate_snapshot_bytes(&corrupt),
            Err(StoreError::Corrupt)
        ));
    }
}

#[test]
fn reload_preserves_all_metadata_and_core_keeps_admissions_disabled() {
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    let mut expected = snapshot();
    expected.rows[0].connection_epoch = 8;
    store.replace(&expected).expect("persist reserved epoch");
    drop(store);
    let (_store, loaded) = PublishSessionStore::open(&scratch.path()).expect("reopen");
    let loaded = loaded.expect("metadata present");
    assert_eq!(loaded, expected);
    let key = loaded.rows[0].key();
    let mut authority = Authority::restore(loaded, MAX_EVENTS).expect("restore");
    assert_eq!(
        authority.reserve_claim(&key),
        Err(sapflow_core::publish_session::Error::AdmissionDisabled)
    );
    assert_eq!(
        stdfs::metadata(scratch.path()).expect("metadata").mode() & 0o777,
        0o600
    );
    assert_eq!(
        stdfs::metadata(scratch.0.join("metadata.json.lock"))
            .expect("lock")
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn independent_store_owners_are_excluded_until_lock_holder_drops() {
    let scratch = Scratch::new();
    let store = seeded(&scratch);
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::Locked)
    ));
    drop(store);
    assert!(PublishSessionStore::open(&scratch.path()).is_ok());
}

#[test]
fn initial_empty_snapshot_is_persisted_before_control_plane_bootstrap() {
    let scratch = Scratch::new();
    let (mut store, loaded) = PublishSessionStore::open(&scratch.path()).expect("virgin");
    assert!(loaded.is_none());
    let initial = Snapshot {
        version: sapflow_core::publish_session::SNAPSHOT_VERSION,
        clock_floor_ms: 0,
        rows: Vec::new(),
    };
    store.replace(&initial).expect("durable initial bootstrap");
    drop(store);
    let (_store, loaded) = PublishSessionStore::open(&scratch.path()).expect("reopen");
    assert_eq!(loaded, Some(initial));
}

#[test]
fn missing_initial_file_is_explicit_and_missing_previously_opened_store_never_resets() {
    let scratch = Scratch::new();
    let (store, loaded) = PublishSessionStore::open(&scratch.path()).expect("virgin");
    assert!(loaded.is_none());
    assert!(!scratch.path().exists());
    drop(store);
    // Ambiguous interrupted bootstrap also fails closed; operator must repair.
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::Changed)
    ));
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    stdfs::remove_file(scratch.path()).expect("simulate lost final file");
    assert!(matches!(store.load(), Err(StoreError::Changed)));
    assert!(matches!(
        store.replace(&snapshot()),
        Err(StoreError::Unusable)
    ));
    drop(store);
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::Changed)
    ));
}

#[test]
fn corrupt_truncated_and_incompatible_existing_files_are_never_replaced() {
    let valid = encoded_document(&snapshot());
    let mut invalid_version: serde_json::Value = serde_json::from_slice(&valid).expect("fixture");
    invalid_version["format_version"] = 2.into();
    for bytes in [
        b"not-json".to_vec(),
        valid[..valid.len() / 2].to_vec(),
        serde_json::to_vec(&invalid_version).expect("fixture"),
    ] {
        let scratch = Scratch::new();
        write_private(&scratch.path(), &bytes);
        assert!(PublishSessionStore::open(&scratch.path()).is_err());
        assert_eq!(stdfs::read(scratch.path()).expect("original"), bytes);
        assert!(PublishSessionStore::open(&scratch.path()).is_err());
    }
}

#[test]
fn valid_json_bit_flip_checksum_and_semantically_invalid_snapshots_fail_closed() {
    let valid = encoded_document(&snapshot());
    let mut changed: serde_json::Value = serde_json::from_slice(&valid).expect("fixture");
    changed["metadata"]["rows"][0]["connection_epoch"] = 6.into();
    let scratch = Scratch::new();
    write_private(
        &scratch.path(),
        &serde_json::to_vec(&changed).expect("fixture"),
    );
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::Corrupt)
    ));
    let mut invalid = snapshot();
    invalid.clock_floor_ms = 0;
    let scratch = Scratch::new();
    write_private(&scratch.path(), &encoded_document(&invalid));
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::InvalidSnapshot)
    ));
}

#[test]
fn unknown_secret_fields_are_rejected_without_including_their_value_in_errors() {
    let raw_credential = "NEVER-PERSIST-RAW-ADMISSION-OR-SRT-SECRET";
    let mut fixture: serde_json::Value =
        serde_json::from_slice(&encoded_document(&snapshot())).expect("fixture");
    fixture["metadata"]["rows"][0]["admission_credential"] = raw_credential.into();
    let scratch = Scratch::new();
    write_private(
        &scratch.path(),
        &serde_json::to_vec(&fixture).expect("fixture"),
    );
    let error = match PublishSessionStore::open(&scratch.path()) {
        Err(error) => error,
        Ok(_) => panic!("unknown secret field accepted"),
    };
    assert!(!error.to_string().contains(raw_credential));
    assert!(!format!("{error:?}").contains(raw_credential));
    let scratch = Scratch::new();
    let _store = seeded(&scratch);
    let bytes = stdfs::read(scratch.path()).expect("metadata");
    let text = String::from_utf8(bytes).expect("JSON");
    for forbidden in [
        raw_credential,
        "passphrase",
        "admission_credential",
        "password",
        "secret",
        "enabled",
        "active_owner",
    ] {
        assert!(!text.contains(forbidden));
    }
    let data: serde_json::Value = serde_json::from_str(&text).expect("metadata");
    assert_eq!(data["metadata"]["rows"][0]["connection_epoch"], 7);
    assert_eq!(
        data["metadata"]["rows"][0]["grant"]["immutable_fingerprint"]
            .as_array()
            .expect("digest")
            .len(),
        32
    );
}

#[test]
fn private_parent_and_regular_private_single_link_files_are_required() {
    let scratch = Scratch::new();
    stdfs::set_permissions(&scratch.0, Permissions::from_mode(0o755)).expect("mode");
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::InsecureDirectory)
    ));
    stdfs::set_permissions(&scratch.0, Permissions::from_mode(0o700)).expect("restore mode");
    write_private(&scratch.path(), &encoded_document(&snapshot()));
    stdfs::set_permissions(scratch.path(), Permissions::from_mode(0o644)).expect("mode");
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::InsecureFile)
    ));
    stdfs::set_permissions(scratch.path(), Permissions::from_mode(0o600)).expect("restore mode");
    let extra = scratch.0.join("extra");
    stdfs::hard_link(scratch.path(), &extra).expect("hard link");
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::InsecureFile)
    ));
}

#[test]
fn symlink_parent_final_and_lock_cannot_follow_or_modify_an_outside_file() {
    for location in ["parent", "final", "lock"] {
        let scratch = Scratch::new();
        let outside = Scratch::new();
        let target = outside.0.join("outside");
        write_private(&target, b"unchanged outside");
        let path = match location {
            "parent" => {
                let link = scratch.0.join("redirect");
                symlink(&outside.0, &link).expect("link");
                link.join("metadata.json")
            }
            "final" => {
                symlink(&target, scratch.path()).expect("link");
                scratch.path()
            }
            _ => {
                symlink(&target, scratch.0.join("metadata.json.lock")).expect("link");
                scratch.path()
            }
        };
        assert!(PublishSessionStore::open(&path).is_err());
        assert_eq!(stdfs::read(&target).expect("outside"), b"unchanged outside");
        assert!(!outside.path().exists());
    }
}

#[test]
fn pre_rename_write_sync_and_rename_faults_preserve_old_bytes_and_only_remove_own_temps() {
    for point in [
        CommitStep::PartialWrite,
        CommitStep::FileSync,
        CommitStep::Rename,
    ] {
        let scratch = Scratch::new();
        let mut store = seeded(&scratch);
        let old = stdfs::read(scratch.path()).expect("old");
        let foreign = scratch.0.join(".metadata.json.tmp-foreign");
        write_private(&foreign, b"foreign owner");
        store.fault = Some(point);
        assert!(matches!(
            store.replace(&next_snapshot()),
            Err(StoreError::Io { .. })
        ));
        assert_eq!(stdfs::read(scratch.path()).expect("old"), old);
        assert_eq!(stdfs::read(&foreign).expect("foreign"), b"foreign owner");
        let temps = stdfs::read_dir(&scratch.0)
            .expect("directory")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".metadata.json.tmp-")
            })
            .count();
        assert_eq!(temps, 1);
        store.fault = None;
        store
            .replace(&next_snapshot())
            .expect("retry pre-commit failure");
    }
}

#[test]
fn post_rename_directory_sync_failure_is_uncertain_and_cannot_be_retried_in_process() {
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    store.fault = Some(CommitStep::DirectorySync);
    assert!(matches!(
        store.replace(&next_snapshot()),
        Err(StoreError::CommitUncertain(_))
    ));
    assert!(matches!(store.load(), Err(StoreError::Unusable)));
    assert!(matches!(
        store.replace(&snapshot()),
        Err(StoreError::Unusable)
    ));
    drop(store);
    let (_store, loaded) = PublishSessionStore::open(&scratch.path()).expect("explicit reopen");
    assert_eq!(loaded.expect("new final file"), next_snapshot());
}

#[test]
fn rollback_row_removal_revision_generation_epoch_clock_and_same_generation_resurrection_are_refused()
 {
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    for field in [
        "remove",
        "revision",
        "generation",
        "epoch",
        "clock",
        "session",
        "fingerprint",
        "grant",
    ] {
        let mut next = snapshot();
        match field {
            "remove" => next.rows.clear(),
            "revision" => next.rows[0].revision = JsPositive::new(2).expect("fixture"),
            "generation" => next.rows[0].event_generation = JsPositive::new(1).expect("fixture"),
            "epoch" => next.rows[0].connection_epoch = 6,
            "clock" => next.clock_floor_ms = 999,
            "session" => {
                next.rows[0].session_id =
                    SessionId::parse("1123456789abcdef0123456789abcdef").expect("fixture")
            }
            "fingerprint" => next.rows[0].request_fingerprint = Fingerprint::new([2; 32]),
            _ => {
                next.rows[0].grant.as_mut().expect("grant").expires_at_ms =
                    EpochMs::new(200_000).expect("fixture")
            }
        }
        assert!(
            matches!(store.replace(&next), Err(StoreError::Rollback)),
            "{field}"
        );
    }
    let mut terminal = next_snapshot();
    terminal.rows[0].status = SessionStatus::Revoked;
    store.replace(&terminal).expect("terminal");
    let mut resurrect = terminal.clone();
    resurrect.rows[0].status = SessionStatus::Prepared;
    resurrect.rows[0].revision = JsPositive::new(5).expect("fixture");
    assert!(matches!(
        store.replace(&resurrect),
        Err(StoreError::Rollback)
    ));
    let mut fresh = resurrect;
    fresh.rows[0].event_generation = JsPositive::new(3).expect("fixture");
    fresh.rows[0].session_id =
        SessionId::parse("1123456789abcdef0123456789abcdef").expect("fixture");
    fresh.rows[0].revision = JsPositive::new(1).expect("fixture");
    store
        .replace(&fresh)
        .expect("fresh higher generation retains epoch high-water");
}

#[test]
fn expiry_and_epoch_reservation_can_persist_without_rewriting_last_idempotent_operation() {
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    let mut expired = snapshot();
    expired.clock_floor_ms = 100_000;
    expired.rows[0].status = SessionStatus::Expired;
    store.replace(&expired).expect("clock transition");
    let mut revoked = expired.clone();
    revoked.rows[0].revision = JsPositive::new(4).expect("fixture");
    revoked.rows[0].status = SessionStatus::Revoked;
    revoked.rows[0].request_fingerprint = Fingerprint::new([18; 32]);
    store
        .replace(&revoked)
        .expect("terminal higher revision remains terminal");
    assert_eq!(store.load().expect("load"), Some(revoked));
}

#[test]
fn higher_revision_cannot_mutate_immutable_policy_reduce_expiry_or_erase_grant() {
    let scratch = Scratch::new();
    let mut store = seeded(&scratch);
    for field in ["digest", "grace", "expiry", "erase", "terminal-expiry"] {
        let mut next = next_snapshot();
        match field {
            "digest" => {
                next.rows[0]
                    .grant
                    .as_mut()
                    .expect("grant")
                    .immutable_fingerprint = Fingerprint::new([1; 32]);
            }
            "grace" => {
                next.rows[0]
                    .grant
                    .as_mut()
                    .expect("grant")
                    .reconnect_grace_ms = 1
            }
            "expiry" => {
                next.rows[0].grant.as_mut().expect("grant").expires_at_ms =
                    EpochMs::new(99_999).expect("fixture");
            }
            "erase" => {
                next.rows[0].status = SessionStatus::Revoked;
                next.rows[0].grant = None;
            }
            _ => {
                next.rows[0].status = SessionStatus::Revoked;
                next.rows[0].grant.as_mut().expect("grant").expires_at_ms =
                    EpochMs::new(200_000).expect("fixture");
            }
        }
        assert!(
            matches!(store.replace(&next), Err(StoreError::Rollback)),
            "{field}"
        );
    }
    let mut extended = next_snapshot();
    extended.rows[0]
        .grant
        .as_mut()
        .expect("grant")
        .expires_at_ms = EpochMs::new(200_000).expect("fixture");
    store.replace(&extended).expect("valid monotonic refresh");
    let mut revoked = extended.clone();
    revoked.rows[0].status = SessionStatus::Revoked;
    revoked.rows[0].revision = JsPositive::new(5).expect("fixture");
    revoked.rows[0].request_fingerprint = Fingerprint::new([19; 32]);
    store.replace(&revoked).expect("retain policy on revoke");
    let mut changed_terminal = revoked;
    changed_terminal.clock_floor_ms = 200_000;
    changed_terminal.rows[0].status = SessionStatus::Expired;
    changed_terminal.rows[0].revision = JsPositive::new(6).expect("fixture");
    changed_terminal.rows[0].request_fingerprint = Fingerprint::new([20; 32]);
    assert!(matches!(
        store.replace(&changed_terminal),
        Err(StoreError::Rollback)
    ));
}

#[test]
fn cap_4096_has_no_eviction_and_oversized_files_and_extra_rows_fail_closed() {
    let scratch = Scratch::new();
    let (mut store, _) = PublishSessionStore::open(&scratch.path()).expect("open");
    let mut full = snapshot();
    let template = full.rows.remove(0);
    full.rows = (0..MAX_EVENTS)
        .map(|index| {
            let mut row = template.clone();
            row.path = StreamPath::parse(&format!("live/event-{index}")).expect("fixture");
            row.session_id = SessionId::parse(&format!("{index:032x}")).expect("fixture");
            row
        })
        .collect();
    store.replace(&full).expect("all rows fit");
    assert!(stdfs::metadata(scratch.path()).expect("size").len() <= MAX_STORE_BYTES as u64);
    let mut excess = full.clone();
    let mut row = template;
    row.path = StreamPath::parse("live/event-extra").expect("fixture");
    excess.rows.push(row);
    assert!(matches!(store.replace(&excess), Err(StoreError::TooLarge)));
    assert_eq!(
        store
            .load()
            .expect("all tombstones retained")
            .expect("snapshot")
            .rows
            .len(),
        MAX_EVENTS
    );
    let scratch = Scratch::new();
    write_private(&scratch.path(), &encoded_document(&excess));
    assert!(PublishSessionStore::open(&scratch.path()).is_err());
    let scratch = Scratch::new();
    write_private(&scratch.path(), &vec![b' '; MAX_STORE_BYTES + 1]);
    assert!(matches!(
        PublishSessionStore::open(&scratch.path()),
        Err(StoreError::TooLarge)
    ));
}
