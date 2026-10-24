//! Integration of the real durable store, authority and runtime lease gate.
//! All credentials are synthetic; these tests create no media or sockets.

use super::*;
use crate::stream::{Claim, SourceKind};
use sapflow_hls::MuxerConfig;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;

const SID: &str = "0123456789abcdef0123456789abcdef";
const NEXT_SID: &str = "1123456789abcdef0123456789abcdef";
const EVENT: &str = "event-manager-fixture";
const PASSWORD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TRANSPORT_SECRET: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MAX_BPS: u64 = 40_000_000;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "publish-sessions-test-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("private fixture directory");
        Self(path.canonicalize().expect("canonical private fixture"))
    }
    fn config(&self) -> ControlConfig {
        ControlConfig {
            addr: "127.0.0.1:0".parse().expect("fixture loopback"),
            token: Arc::new(Secret::new(
                "synthetic-control-token-0000000000000000".to_owned(),
            )),
            store: self.0.join("sessions.json"),
        }
    }
    async fn open(&self, callbacks: bool) -> PublishSessions {
        PublishSessions::open(&self.config(), callbacks)
            .await
            .expect("manager open")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn put(session: &str, generation: u64, revision: u64, expires: u64) -> PutSession {
    PutSession::parse(
        &serde_json::to_vec(&serde_json::json!({
            "contractVersion": 1, "eventId": EVENT, "eventGeneration": generation,
            "sessionId": session, "revision": revision,
            "idempotencyKey": "cccccccccccccccccccccccccccccccc",
            "expiresAtUnixMs": expires, "reconnectGraceMs": 90_000,
            "transport": "srt", "streamPath": format!("live/{EVENT}"),
            "publishUser": session, "publishPassword": PASSWORD,
            "srt": {"cipher": "aes256-ctr", "passphrase": TRANSPORT_SECRET},
            "limits": {"maxPublishBps": 4_000_000, "maxQueueBytes": 1_048_576}
        }))
        .expect("synthetic JSON"),
    )
    .expect("strict PUT fixture")
}
fn revoke(session: &str, generation: u64, revision: u64) -> RevokeSession {
    RevokeSession::parse(
        &serde_json::to_vec(&serde_json::json!({
            "contractVersion": 1, "eventId": EVENT, "eventGeneration": generation,
            "sessionId": session, "revision": revision,
            "idempotencyKey": "dddddddddddddddddddddddddddddddd"
        }))
        .expect("synthetic JSON"),
    )
    .expect("strict revoke fixture")
}
fn request(session: &str) -> PublishRequest {
    PublishRequest::parse_srt_stream_id(&format!("publish:live/{EVENT}:{session}:{PASSWORD}"))
        .expect("fixture publish request")
}
async fn prepare(manager: &PublishSessions) -> u64 {
    let expires = manager.now_ms() + 600_000;
    let receipt = manager
        .put(SID, put(SID, 1, 1, expires), MAX_BPS)
        .await
        .expect("prepare");
    assert_eq!(
        (receipt.state, receipt.admission, receipt.connection_epoch),
        ("prepared", "open", 0)
    );
    expires
}
fn persisted(manager: &PublishSessions) -> policy::Snapshot {
    manager
        .0
        .store
        .lock()
        .expect("store owner")
        .load()
        .expect("durable load")
        .expect("initial snapshot persisted")
}

/// A deterministic slow-store boundary. The real save worker queues behind
/// this mutex; runtime gate reads and pre-durability revocation must stay live.
struct BlockStore {
    release: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl BlockStore {
    fn new(manager: &PublishSessions) -> Self {
        let store = manager.0.store.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release, released) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _owner = store.lock().expect("slow-store fixture lock");
            ready_tx.send(()).expect("notify fixture");
            released
                .recv_timeout(Duration::from_secs(5))
                .expect("bounded fixture release");
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("store fixture ready");
        Self {
            release: Some(release),
            thread: Some(thread),
        }
    }
    fn finish(mut self) {
        self.release
            .take()
            .expect("one release")
            .send(())
            .expect("release store");
        self.thread
            .take()
            .expect("one worker")
            .join()
            .expect("store fixture ended");
    }
}
impl Drop for BlockStore {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
async fn operation_started(manager: &PublishSessions) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.0.operation.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned transaction started");
}
async fn await_cancel(receiver: &mut watch::Receiver<bool>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !*receiver.borrow_and_update() {
            receiver.changed().await.expect("owner signal");
        }
    })
    .await
    .expect("idle protocol cancellation");
}

#[tokio::test]
async fn revocation_cancels_idle_owner_before_slow_durability_and_fences_media() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    lease.accepted();
    let mut cancel = lease.cancellation();
    let store = BlockStore::new(&manager);
    let task = tokio::spawn({
        let manager = manager.clone();
        async move { manager.revoke(SID, revoke(SID, 1, 2)).await }
    });
    await_cancel(&mut cancel).await;
    assert!(
        !task.is_finished(),
        "revoke must still await its durable commit"
    );
    let mut touched = false;
    assert!(matches!(
        lease.with_open(|| touched = true),
        Err(Error::Denied)
    ));
    assert!(!touched);
    store.finish();
    let receipt = task.await.expect("join revoke").expect("durable revoke");
    assert_eq!(
        (receipt.state, receipt.admission, receipt.protocol),
        ("revoked", "denied", "stopping")
    );
    lease.protocol_stopped();
    assert_eq!(
        manager.status(SID).await.expect("status").protocol,
        "stopped"
    );
    assert_eq!(persisted(&manager).rows[0].status, SessionStatus::Revoked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn already_admitted_media_finishes_before_revocation_and_no_later_media_enters() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let count = Arc::new(AtomicU64::new(0));
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn({
        let lease = lease.clone();
        let count = count.clone();
        move || {
            lease
                .with_open(|| {
                    entered_tx.send(()).expect("entered media gate");
                    release_rx
                        .recv_timeout(Duration::from_secs(3))
                        .expect("release admitted media");
                    count.fetch_add(1, Ordering::Relaxed);
                })
                .expect("already admitted media finishes");
        }
    });
    entered_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("media inside gate");
    let revoked = tokio::spawn({
        let manager = manager.clone();
        async move { manager.revoke(SID, revoke(SID, 1, 2)).await }
    });
    operation_started(&manager).await;
    assert!(!revoked.is_finished());
    release_tx.send(()).expect("release admitted media");
    worker.join().expect("media worker ended");
    revoked.await.expect("join revoke").expect("revoke");
    assert!(matches!(
        lease.with_open(|| count.fetch_add(1, Ordering::Relaxed)),
        Err(Error::Denied)
    ));
    assert_eq!(count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn revocation_ends_a_bound_held_registry_without_waiting_for_grace() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let registry = Arc::new(Registry::default());
    let (stream, generation) = lease
        .with_open(|| {
            let (stream, generation, claim) = registry
                .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                .expect("claim");
            assert_eq!(claim, Claim::Fresh);
            lease.bind(&registry, &stream, generation);
            (stream, generation)
        })
        .expect("open claim");
    assert!(registry.detach(&stream, generation, true));
    lease.released(true);
    lease.protocol_stopped();
    assert!(registry.get(EVENT).is_some());
    manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("revoke held stream");
    assert!(registry.get(EVENT).is_none());
    assert!(stream.lock().closed);
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
}

#[tokio::test]
async fn released_lease_cannot_use_media_or_credentials_and_cannot_destroy_held_grace() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    lease.accepted();
    let registry = Arc::new(Registry::default());
    let (stream, generation) = lease
        .with_open(|| {
            let (stream, generation, _) = registry
                .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                .expect("claim");
            lease.bind(&registry, &stream, generation);
            (stream, generation)
        })
        .expect("live owner claim");
    assert!(registry.detach(&stream, generation, true));
    lease.released(true);
    lease.protocol_stopped();
    let mut touched = false;
    assert!(matches!(
        lease.with_open(|| touched = true),
        Err(Error::Denied)
    ));
    assert!(!touched);
    assert!(lease.crypto().is_none());
    assert!(matches!(lease.limits(), Err(Error::Denied)));
    assert!(
        !*lease.cancellation().borrow(),
        "a stale release must not cancel valid reconnect grace"
    );
    assert!(!stream.lock().closed);
    assert!(
        registry
            .get(EVENT)
            .is_some_and(|current| Arc::ptr_eq(&current, &stream))
    );
    assert_eq!(
        manager.status(SID).await.expect("held status").state,
        "held"
    );
    let replacement = manager
        .admit(&request(SID))
        .await
        .expect("reconnect credentials preserved");
    replacement.accepted();
    let next_generation = replacement
        .with_open(|| {
            let (current, generation, claim) = registry
                .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                .expect("resume");
            assert_eq!(claim, Claim::Resumed);
            assert!(Arc::ptr_eq(&current, &stream));
            replacement.bind(&registry, &current, generation);
            generation
        })
        .expect("new owner resumes held stream");
    assert!(next_generation > generation);
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
    replacement
        .with_open(|| ())
        .expect("late old lease did not cancel replacement");
    assert_eq!(
        manager
            .status(SID)
            .await
            .expect("new status")
            .connection_epoch,
        2
    );
}

#[tokio::test]
async fn staged_refresh_crossing_old_expiry_never_revives_the_cancelled_owner() {
    for held in [false, true] {
        let scratch = Scratch::new();
        let manager = scratch.open(false).await;
        let old_expiry = prepare(&manager).await;
        let lease = manager.admit(&request(SID)).await.expect("admit");
        lease.accepted();
        let registry = Arc::new(Registry::default());
        let (stream, generation) = lease
            .with_open(|| {
                let (stream, generation, _) = registry
                    .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                    .expect("claim");
                lease.bind(&registry, &stream, generation);
                (stream, generation)
            })
            .expect("bind old owner");
        if held {
            assert!(registry.detach(&stream, generation, true));
            lease.released(true);
            lease.protocol_stopped();
        }
        let store = BlockStore::new(&manager);
        let refreshed_expiry = old_expiry + 60_000;
        let refresh = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .put(SID, put(SID, 1, 2, refreshed_expiry), MAX_BPS)
                    .await
            }
        });
        // This current-thread test cannot resume until put has yielded. With
        // old expiry still far ahead, put's first wait after taking operation
        // is its real blocked store save: the refresh is already staged.
        operation_started(&manager).await;
        assert!(!refresh.is_finished());
        manager
            .0
            .clock
            .forced_now
            .store(old_expiry, Ordering::Release);
        manager
            .gate()
            .grants
            .get_mut(EVENT)
            .expect("grant")
            .deadline = Instant::now();
        let expiry = tokio::spawn({
            let manager = manager.clone();
            async move { manager.run_expiry().await }
        });
        manager.0.changed.notify_one();
        let mut cancel = lease.cancellation();
        await_cancel(&mut cancel).await;
        assert!(
            !refresh.is_finished(),
            "fast cancellation must not wait for fsync"
        );
        assert!(stream.lock().closed);
        assert!(registry.get(EVENT).is_none());
        {
            let state = manager.gate();
            let row = state
                .authority
                .row(&request(SID).path)
                .expect("old authority still serialised");
            assert_eq!(row.revision.get(), 1);
            assert_eq!(row.status, SessionStatus::Prepared);
            assert!(
                state
                    .grants
                    .get(EVENT)
                    .expect("grant")
                    .credentials
                    .is_none()
            );
        }
        store.finish();
        let receipt = refresh
            .await
            .expect("join refresh")
            .expect("staged refresh commits");
        assert_eq!(
            (receipt.revision, receipt.state, receipt.admission),
            (2, "prepared", "open")
        );
        assert_eq!(receipt.protocol, if held { "stopped" } else { "stopping" });
        assert_eq!(receipt.expires_at_unix_ms, refreshed_expiry);
        // Even before the cancelled active protocol has finished releasing its
        // owner, denied late work cannot erase the newly refreshed credentials.
        assert!(matches!(
            lease.with_open(|| panic!("old media must never enter")),
            Err(Error::Denied)
        ));
        assert!(lease.crypto().is_none());
        assert!(matches!(lease.limits(), Err(Error::Denied)));
        assert!(
            manager
                .gate()
                .grants
                .get(EVENT)
                .expect("renewed grant")
                .credentials
                .is_some()
        );
        if !held {
            assert!(matches!(
                manager.admit(&request(SID)).await,
                Err(Error::Busy)
            ));
            lease.released(false);
            lease.protocol_stopped();
        }
        let next = manager
            .admit(&request(SID))
            .await
            .expect("new connection after old owner exits");
        next.accepted();
        next.with_open(|| ())
            .expect("fresh connection is authorised");
        assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
        next.with_open(|| ())
            .expect("stale cleanup cannot cancel fresh connection");
        let saved = persisted(&manager);
        assert_eq!(saved.rows[0].status, SessionStatus::Prepared);
        assert_eq!(saved.rows[0].revision.get(), 2);
        assert_eq!(
            saved.rows[0]
                .grant
                .as_ref()
                .expect("grant")
                .expires_at_ms
                .get(),
            refreshed_expiry
        );
        assert_eq!(saved.rows[0].connection_epoch, 2);
        expiry.abort();
        assert!(
            expiry
                .await
                .expect_err("expiry fixture cancelled")
                .is_cancelled()
        );
    }
}

#[tokio::test]
async fn old_lease_and_delayed_registry_cleanup_cannot_touch_replacement_epoch_or_generation() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    let registry = Arc::new(Registry::default());
    let first = manager.admit(&request(SID)).await.expect("first admit");
    let (stream, generation) = first
        .with_open(|| {
            let (stream, generation, _) = registry
                .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                .expect("claim");
            first.bind(&registry, &stream, generation);
            (stream, generation)
        })
        .expect("first claim");
    assert!(registry.detach(&stream, generation, true));
    first.released(true);
    first.protocol_stopped();
    let second = manager.admit(&request(SID)).await.expect("reconnect");
    second.accepted();
    let resumed_generation = second
        .with_open(|| {
            let (resumed, generation, claim) = registry
                .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
                .expect("resume");
            assert_eq!(claim, Claim::Resumed);
            assert!(Arc::ptr_eq(&stream, &resumed));
            second.bind(&registry, &resumed, generation);
            generation
        })
        .expect("second claim");
    assert!(resumed_generation > generation);
    assert!(!registry.terminate(&stream, generation));
    first.released(false);
    first.protocol_stopped();
    assert!(matches!(first.with_open(|| ()), Err(Error::Denied)));
    assert!(first.crypto().is_none());
    assert!(matches!(first.limits(), Err(Error::Denied)));
    assert!(second.crypto().is_some());
    let (limits, grace) = second.limits().expect("current owner's immutable limits");
    assert_eq!(limits.max_publish_bps, 4_000_000);
    assert_eq!(grace, Duration::from_secs(90));
    second.with_open(|| ()).expect("new epoch stays open");
    assert_eq!(
        manager.status(SID).await.expect("status").connection_epoch,
        2
    );
    manager
        .put(NEXT_SID, put(NEXT_SID, 2, 1, expires), MAX_BPS)
        .await
        .expect("new generation");
    assert!(stream.lock().closed);
    let third = manager
        .admit(&request(NEXT_SID))
        .await
        .expect("new identity");
    let replacement = third
        .with_open(|| {
            let (replacement, generation, _) = registry
                .claim(
                    &request(NEXT_SID).path,
                    MuxerConfig::default(),
                    SourceKind::Ts,
                )
                .expect("new registry stream");
            third.bind(&registry, &replacement, generation);
            replacement
        })
        .expect("third claim");
    assert!(!Arc::ptr_eq(&stream, &replacement));
    assert!(!registry.terminate(&stream, resumed_generation));
    assert!(matches!(
        manager.revoke(SID, revoke(SID, 1, 99)).await,
        Err(Error::Conflict)
    ));
    third
        .with_open(|| ())
        .expect("stale revoke cannot touch new generation");
    assert_eq!(
        manager
            .status(NEXT_SID)
            .await
            .expect("status")
            .connection_epoch,
        3
    );
    assert!(Arc::ptr_eq(
        &registry.get(EVENT).expect("replacement retained"),
        &replacement
    ));
}

#[tokio::test]
async fn restart_idempotent_packet_stays_inactive_until_fresh_revision_and_retains_epoch() {
    let scratch = Scratch::new();
    let manager = scratch.open(true).await;
    let expires = prepare(&manager).await;
    let lease = manager
        .admit(&request(SID))
        .await
        .expect("admit before restart");
    lease.released(false);
    lease.protocol_stopped();
    drop(lease);
    drop(manager);
    let manager = scratch.open(true).await;
    let old = manager
        .put(SID, put(SID, 1, 1, expires), MAX_BPS)
        .await
        .expect("idempotent old packet");
    assert_eq!(
        (old.state, old.admission, old.connection_epoch),
        ("inactive", "denied", 1)
    );
    assert_eq!((old.recording, old.callbacks), ("incomplete", "failed"));
    assert!(matches!(
        manager.admit(&request(SID)).await,
        Err(Error::Denied)
    ));
    manager
        .put(SID, put(SID, 1, 2, expires + 10_000), MAX_BPS)
        .await
        .expect("fresh revision");
    let renewed = manager
        .admit(&request(SID))
        .await
        .expect("renewed admission");
    renewed.with_open(|| ()).expect("fresh authority open");
    assert_eq!(
        manager.status(SID).await.expect("status").connection_epoch,
        2
    );
    let bytes = fs::read(scratch.config().store).expect("metadata bytes");
    for secret in [PASSWORD, TRANSPORT_SECRET] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
}

#[tokio::test]
async fn a_new_session_generation_does_not_inherit_old_session_completion_uncertainty() {
    let scratch = Scratch::new();
    let manager = scratch.open(true).await;
    let expires = prepare(&manager).await;
    let first = manager.admit(&request(SID)).await.expect("first admit");
    let first_scope = first.scope();
    first_scope.observations.callback_ticket().finish(false);
    let recording = first_scope.recording().expect("old session recording");
    recording.active();
    drop(recording);
    let old = manager.status(SID).await.expect("old failure receipt");
    assert_eq!(
        (old.recording, old.callbacks, old.connection_epoch),
        ("incomplete", "failed", 1)
    );
    let fresh = manager
        .put(NEXT_SID, put(NEXT_SID, 2, 1, expires), MAX_BPS)
        .await
        .expect("explicit new session");
    assert_eq!(
        (fresh.recording, fresh.callbacks, fresh.connection_epoch),
        ("never-started", "none", 1)
    );
    first_scope.observations.callback_ticket().finish(false);
    first.released(false);
    first.protocol_stopped();
    let next = manager
        .admit(&request(NEXT_SID))
        .await
        .expect("new session admit");
    next.with_open(|| ()).expect("new session authority");
    let receipt = manager.status(NEXT_SID).await.expect("new receipt");
    assert_eq!(
        (
            receipt.recording,
            receipt.callbacks,
            receipt.connection_epoch
        ),
        ("never-started", "none", 2)
    );
}

#[tokio::test]
async fn terminal_replay_stale_revision_and_wrong_credentials_never_reopen_or_burn_epoch() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    let mut wrong = request(SID);
    wrong.pass = Some(TRANSPORT_SECRET.to_owned());
    assert!(matches!(manager.admit(&wrong).await, Err(Error::Denied)));
    assert!(matches!(
        manager.admit(&request(NEXT_SID)).await,
        Err(Error::Denied)
    ));
    assert_eq!(
        manager.status(SID).await.expect("status").connection_epoch,
        0
    );
    manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("revoke");
    assert!(matches!(
        manager.put(SID, put(SID, 1, 1, expires), MAX_BPS).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        manager
            .put(SID, put(SID, 1, 3, expires + 10_000), MAX_BPS)
            .await,
        Err(Error::Denied)
    ));
    assert!(matches!(
        manager.admit(&request(SID)).await,
        Err(Error::Denied)
    ));
    assert_eq!(manager.status(SID).await.expect("status").state, "revoked");
    assert_eq!(persisted(&manager).rows[0].connection_epoch, 0);
}

#[tokio::test]
async fn receipts_wait_for_protocol_recording_and_callback_completion_after_revocation() {
    let scratch = Scratch::new();
    let manager = scratch.open(true).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    lease.accepted();
    let scope = lease.scope();
    let recording = scope.recording().expect("bounded recording identity");
    recording.active();
    let callback = scope.observations.callback_ticket();
    recording.finalising();
    let stopping = manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("revoke");
    assert_eq!(
        (stopping.protocol, stopping.recording, stopping.callbacks),
        ("stopping", "finalising", "pending")
    );
    assert!(recording.completion_state().is_none());
    lease.protocol_stopped();
    recording.finalised();
    callback.finish(true);
    let completed = manager.status(SID).await.expect("completion status");
    assert_eq!(
        (completed.protocol, completed.recording, completed.callbacks),
        ("stopped", "finalised", "delivered")
    );
}

#[tokio::test]
async fn dropped_completion_work_stays_failed_or_incomplete_in_session_receipts() {
    let scratch = Scratch::new();
    let manager = scratch.open(true).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let scope = lease.scope();
    let recording = scope.recording().expect("recording identity");
    recording.active();
    let callback = scope.observations.callback_ticket();
    drop(recording);
    drop(callback);
    let receipt = manager.status(SID).await.expect("honest incomplete status");
    assert_eq!(
        (receipt.recording, receipt.callbacks),
        ("incomplete", "failed")
    );
    scope.observations.callback_ticket().finish(true);
    assert_eq!(
        manager.status(SID).await.expect("sticky failure").callbacks,
        "failed"
    );
}

#[tokio::test]
async fn aborted_http_admission_future_still_persists_epoch_and_releases_unconsumed_lease() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let store = BlockStore::new(&manager);
    let task = tokio::spawn({
        let manager = manager.clone();
        async move { manager.admit(&request(SID)).await }
    });
    operation_started(&manager).await;
    task.abort();
    assert!(
        task.await
            .expect_err("outer request cancelled")
            .is_cancelled()
    );
    store.finish();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let released = manager
                .gate()
                .grants
                .get(EVENT)
                .and_then(|grant| grant.owner.as_ref())
                .is_some_and(|owner| owner.released.load(Ordering::Acquire));
            if released {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached transaction drops unconsumed lease");
    assert_eq!(persisted(&manager).rows[0].connection_epoch, 1);
    let next = manager
        .admit(&request(SID))
        .await
        .expect("released abandoned result permits reconnect");
    assert_eq!(
        manager.status(SID).await.expect("status").connection_epoch,
        2
    );
    next.with_open(|| ()).expect("next owner open");
}

#[tokio::test]
async fn expiry_during_slow_admission_burns_epoch_without_leaving_a_claimed_owner() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    let store = BlockStore::new(&manager);
    let task = tokio::spawn({
        let manager = manager.clone();
        async move { manager.admit(&request(SID)).await }
    });
    operation_started(&manager).await;
    manager
        .gate()
        .grants
        .get_mut(EVENT)
        .expect("grant")
        .deadline = Instant::now();
    store.finish();
    assert!(matches!(
        task.await.expect("join admission"),
        Err(Error::Denied)
    ));
    assert_eq!(persisted(&manager).rows[0].connection_epoch, 1);
    manager
        .put(SID, put(SID, 1, 2, expires + 10_000), MAX_BPS)
        .await
        .expect("fresh refresh");
    let next = manager
        .admit(&request(SID))
        .await
        .expect("no orphaned committed reservation");
    next.with_open(|| ()).expect("new owner open");
    assert_eq!(
        manager.status(SID).await.expect("status").connection_epoch,
        2
    );
}

#[tokio::test]
async fn rejected_admission_still_durably_expires_authority_before_returning() {
    let scratch = Scratch::new();
    let mut manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    // No runtime lease exists, so this test alone owns Inner and can advance
    // its injected clock floor without sleeping or altering the persisted row.
    Arc::get_mut(&mut manager.0)
        .expect("exclusive test clock")
        .clock
        .base = expires;
    let mut wrong = request(SID);
    wrong.pass = Some("invalid".to_owned());
    assert!(matches!(manager.admit(&wrong).await, Err(Error::Denied)));
    let saved = persisted(&manager);
    assert_eq!(saved.rows[0].status, SessionStatus::Expired);
    assert!(saved.clock_floor_ms >= expires);
    assert_eq!(saved.rows[0].connection_epoch, 0);
}

#[tokio::test]
async fn store_failure_closes_existing_media_and_never_publishes_a_new_draft() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let mut cancel = lease.cancellation();
    fs::write(scratch.config().store, b"corrupt existing metadata").expect("inject store fault");
    assert!(matches!(
        manager
            .put(SID, put(SID, 1, 2, expires + 10_000), MAX_BPS)
            .await,
        Err(Error::Unavailable)
    ));
    await_cancel(&mut cancel).await;
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
    assert!(matches!(manager.status(SID).await, Err(Error::Unavailable)));
    assert!(matches!(
        manager.admit(&request(SID)).await,
        Err(Error::Unavailable)
    ));
    assert_eq!(
        fs::read(scratch.config().store).expect("not reset"),
        b"corrupt existing metadata"
    );
}

#[tokio::test]
async fn delayed_accept_cannot_resurrect_stopped_protocol_after_revocation() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("revoke before accepted callback");
    lease.protocol_stopped();
    lease.accepted();
    let receipt = manager.status(SID).await.expect("status");
    assert_eq!((receipt.admission, receipt.protocol), ("denied", "stopped"));
}

#[tokio::test]
async fn aborted_http_revoke_keeps_its_fence_and_finishes_the_durable_tombstone() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let mut cancel = lease.cancellation();
    let store = BlockStore::new(&manager);
    let task = tokio::spawn({
        let manager = manager.clone();
        async move { manager.revoke(SID, revoke(SID, 1, 2)).await }
    });
    await_cancel(&mut cancel).await;
    task.abort();
    assert!(
        task.await
            .expect_err("cancelled outer request")
            .is_cancelled()
    );
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
    store.finish();
    let receipt = manager
        .status(SID)
        .await
        .expect("owned revoke finishes before status");
    assert_eq!(
        (receipt.state, receipt.admission, receipt.connection_epoch),
        ("revoked", "denied", 1)
    );
    assert_eq!(persisted(&manager).rows[0].status, SessionStatus::Revoked);
}

#[tokio::test]
async fn invalid_semantic_mutations_cannot_skip_durable_expiry() {
    for operation in ["put", "revoke"] {
        let scratch = Scratch::new();
        let mut manager = scratch.open(false).await;
        let expires = prepare(&manager).await;
        Arc::get_mut(&mut manager.0)
            .expect("exclusive test clock")
            .clock
            .base = expires;
        let result = if operation == "put" {
            let mut invalid = put(SID, 1, 2, expires + 10_000);
            invalid.transport = "rtmp".to_owned();
            manager.put(SID, invalid, MAX_BPS).await
        } else {
            manager.revoke("invalid-route", revoke(SID, 1, 2)).await
        };
        assert!(matches!(result, Err(Error::Invalid)), "{operation}");
        let saved = persisted(&manager);
        assert_eq!(saved.rows[0].status, SessionStatus::Expired, "{operation}");
        assert!(saved.clock_floor_ms >= expires);
    }
}

#[tokio::test]
async fn poisoned_media_gate_fails_closed_and_cancels_existing_owners() {
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let lease = manager.admit(&request(SID)).await.expect("admit");
    let mut cancel = lease.cancellation();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = lease.with_open(|| panic!("synthetic failure inside media gate"));
    }));
    assert!(panic.is_err());
    assert!(matches!(manager.status(SID).await, Err(Error::Unavailable)));
    await_cancel(&mut cancel).await;
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
    assert!(lease.crypto().is_none());
    assert!(matches!(lease.limits(), Err(Error::Denied)));
    assert!(matches!(
        manager.admit(&request(SID)).await,
        Err(Error::Unavailable)
    ));
}

fn fresh_media_app(
    manager: &PublishSessions,
) -> (
    Arc<crate::App>,
    tokio::sync::mpsc::Receiver<crate::callback::Queued>,
) {
    let mut config = crate::config::Config::load(
        |key| (key == "SAPFLOW_OPEN_PUBLISH").then(|| "1".into()),
        Vec::new(),
    )
    .expect("synthetic config");
    config.recordings = None;
    let (callbacks, receiver) =
        crate::callback::test_queue(crate::publish_observer::fixture_scope("fresh", 1, 1, 1), 32);
    (
        Arc::new(crate::App {
            config,
            publish_sessions: Some(manager.clone()),
            streams: Arc::new(Registry::default()),
            callbacks,
            auth_throttle: Mutex::new(sapflow_core::limits::AuthThrottle::new(
                10,
                Duration::from_secs(60),
            )),
            refusal_log: Mutex::new(sapflow_core::limits::LogLimiter::new(
                20,
                Duration::from_secs(60),
            )),
        }),
        receiver,
    )
}
fn fresh_events(
    receiver: &mut tokio::sync::mpsc::Receiver<crate::callback::Queued>,
) -> Vec<String> {
    std::iter::from_fn(|| receiver.try_recv().ok())
        .map(|item| {
            let json: serde_json::Value =
                serde_json::from_slice(&item.body).expect("JSON callback");
            json["event"].as_str().expect("event").to_owned()
        })
        .collect()
}
async fn fresh_publication(app: &Arc<crate::App>) -> crate::publish::Publication {
    fresh_publication_request(app, &request(SID)).await
}
async fn fresh_publication_request(
    app: &Arc<crate::App>,
    request: &PublishRequest,
) -> crate::publish::Publication {
    let publication = crate::publish::Publication::begin_srt(
        app,
        request,
        "127.0.0.1".parse().expect("loopback"),
    )
    .await
    .expect("scoped publication");
    publication.scoped_lease().expect("scope").accepted();
    publication
}

#[tokio::test]
async fn revoked_terminal_drain_uses_exact_successor_scope_and_marks_both_epochs_on_loss() {
    use crate::publish::terminal_tests::{ADTS44, one, recording_samples};
    for truncated in [false, true] {
        let scratch = Scratch::new();
        let manager = scratch.open(true).await;
        prepare(&manager).await;
        let (mut app, mut callbacks) = fresh_media_app(&manager);
        let config = &mut Arc::get_mut(&mut app).expect("unshared app fixture").config;
        config.recordings = Some(scratch.0.join("recordings"));
        config.record_min_free = 0;
        let mut old = fresh_publication(&app).await;
        let old_lease = old.scoped_lease().expect("old scope");
        let first_epoch = manager
            .status(SID)
            .await
            .expect("first receipt")
            .connection_epoch;
        feed_generated(
            &mut old,
            include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts"),
        );
        old.end();
        old_lease.protocol_stopped();
        let mut next = fresh_publication(&app).await;
        let next_lease = next.scoped_lease().expect("successor scope");
        let second_epoch = manager
            .status(SID)
            .await
            .expect("next receipt")
            .connection_epoch;
        assert_eq!(second_epoch, first_epoch + 1);
        let mut wire = one(ADTS44);
        if truncated {
            wire.extend(&one(ADTS44)[376..426]);
        }
        feed_generated(&mut next, &wire);
        assert!(
            manager
                .status(SID)
                .await
                .expect("not yet fresh")
                .fresh_media
                .is_none()
        );
        manager
            .revoke(SID, revoke(SID, 1, 2))
            .await
            .expect("End closes gate then drains accepted tail");
        assert!(
            next.push(|_, _| panic!("no new media accepted after revoke"))
                .is_err()
        );
        next.end();
        next_lease.protocol_stopped();
        let mut recordings = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            while recordings.len() != 2 {
                let item = callbacks.recv().await.expect("real writer callback");
                let body: serde_json::Value =
                    serde_json::from_slice(&item.body).expect("scoped JSON");
                if body["event"] == "recordSegmentComplete" {
                    recordings.push(body);
                }
            }
        })
        .await
        .expect("bounded actual writer cleanup");
        recordings.sort_by_key(|body| body["connectionEpoch"].as_u64().expect("captured epoch"));
        assert_eq!(recordings[0]["connectionEpoch"], first_epoch);
        assert_eq!(recordings[1]["connectionEpoch"], second_epoch);
        for body in &recordings {
            assert_eq!(body["sessionId"], SID);
            assert_eq!(body["eventGeneration"], 1);
            assert_eq!(
                body["recordingState"],
                if truncated { "incomplete" } else { "finalised" }
            );
        }
        assert_ne!(recordings[0]["recordingId"], recordings[1]["recordingId"]);
        assert_eq!(
            recording_samples(&recordings[1]),
            1,
            "one terminal AAC in new file only"
        );
        let bytes = fs::read(
            recordings[1]["segmentPath"]
                .as_str()
                .expect("AAC recording"),
        )
        .expect("synced file");
        assert!(!bytes.windows(4).any(|word| word == b"avc1"));
        let ended = manager.status(SID).await.expect("terminal receipt");
        assert_eq!(ended.admission, "denied");
        assert!(ended.fresh_media.is_none());
    }
}

#[tokio::test]
async fn stale_publication_drop_cannot_release_or_drain_the_new_scoped_owner() {
    use crate::publish::terminal_tests::{ADTS44, one};
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let (app, _callbacks) = fresh_media_app(&manager);
    let mut old = fresh_publication(&app).await;
    feed_generated(&mut old, &one(ADTS44));
    let lease = old.scoped_lease().expect("old lease");
    let generation = lease
        .owner
        .binding
        .lock()
        .expect("binding")
        .as_ref()
        .expect("bound stream")
        .generation;
    assert!(app.streams.detach(&old.stream, generation, true));
    lease.released(true);
    lease.protocol_stopped();
    let mut next = fresh_publication(&app).await;
    let current = next.scoped_lease().expect("new lease");
    let epoch = manager
        .status(SID)
        .await
        .expect("new receipt")
        .connection_epoch;
    feed_generated(&mut next, &one(ADTS44));
    let before = next_part(&next);
    old.end();
    assert!(
        current.with_open(|| ()).is_ok(),
        "old released(false) cannot cancel successor"
    );
    assert_eq!(
        next_part(&next),
        before,
        "old drop cannot EOF-drain new parser"
    );
    let status = manager
        .status(SID)
        .await
        .expect("new identity stays attached");
    assert_eq!(status.connection_epoch, epoch);
    assert_eq!(status.protocol, "active");
    assert!(
        status.fresh_media.is_none(),
        "old media cannot confirm this attempt"
    );
    manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("clean up new grant");
    next.end();
    current.protocol_stopped();
}

#[tokio::test]
async fn already_revoked_published_owner_emits_one_captured_unpublish_without_grace() {
    let scratch = Scratch::new();
    let manager = scratch.open(true).await;
    prepare(&manager).await;
    let (app, mut callbacks) = fresh_media_app(&manager);
    let mut publication = fresh_publication(&app).await;
    let lease = publication.scoped_lease().expect("current scope");
    feed_generated(
        &mut publication,
        include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts"),
    );
    assert_eq!(fresh_events(&mut callbacks), ["publish"]);
    let epoch = manager.status(SID).await.expect("live").connection_epoch;
    manager
        .revoke(SID, revoke(SID, 1, 2))
        .await
        .expect("explicit terminal End");
    publication.end();
    lease.protocol_stopped();
    let item = callbacks
        .try_recv()
        .expect("one captured Unpublish after revoke");
    let body: serde_json::Value = serde_json::from_slice(&item.body).expect("scoped callback");
    assert_eq!(body["event"], "unpublish");
    assert_eq!(body["sessionId"], SID);
    assert_eq!(body["eventGeneration"], 1);
    assert_eq!(body["connectionEpoch"], epoch);
    assert!(
        callbacks.try_recv().is_err(),
        "no duplicate lifecycle callback"
    );
    assert!(app.streams.get(EVENT).is_none(), "revoke cannot arm a hold");
    let status = manager
        .status(SID)
        .await
        .expect("no authority resurrection");
    assert_eq!(status.admission, "denied");
    assert_eq!(status.state, "revoked");
}
fn feed_generated(publication: &mut crate::publish::Publication, data: &[u8]) {
    for packet in data.chunks(188) {
        crate::srt::feed(publication, packet).expect("generated feed");
    }
}
fn next_part(publication: &crate::publish::Publication) -> u64 {
    publication
        .stream
        .lock()
        .ingest
        .muxer()
        .expect("muxer")
        .next_part_id()
}

#[tokio::test]
async fn audio_only_receipts_require_current_parts_across_track_and_codec_epochs() {
    const MONO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts");
    const STEREO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/audio-44k1-stereo.ts");
    const VIDEO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts");
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let (app, mut callbacks) = fresh_media_app(&manager);
    let mut retained = None;
    let mut previous = None;
    for (index, data) in [MONO, STEREO, VIDEO, MONO].into_iter().enumerate() {
        let mut publication = fresh_publication(&app).await;
        let empty = manager.status(SID).await.expect("current connection");
        assert_eq!(empty.connection_epoch, index as u64 + 1);
        assert!(
            empty.fresh_media.is_none(),
            "retained parts cannot confirm a successor"
        );
        if let Some(stream) = &retained {
            assert!(Arc::ptr_eq(stream, &publication.stream));
        } else {
            retained = Some(publication.stream.clone());
        }
        // A transport header is neither a completed media part nor decoded
        // audio. It must not manufacture a ready receipt from retained media.
        crate::srt::feed(&mut publication, &data[..188]).expect("generated first packet");
        assert!(
            manager
                .status(SID)
                .await
                .expect("header status")
                .fresh_media
                .is_none()
        );
        feed_generated(&mut publication, &data[188..]);
        let live = manager
            .status(SID)
            .await
            .expect("actual current audio parts");
        assert_eq!(live.state, "live");
        let media = live.fresh_media.expect("current completed part");
        assert_eq!(media.connection_epoch, index as u64 + 1);
        assert_eq!(media.part.has_video, index == 2);
        assert!(media.part.has_audio);
        if let Some((part_id, initialization_id)) = previous {
            assert!(media.part.part_id > part_id);
            assert!(media.part.initialization_id > initialization_id);
        }
        {
            let state = publication.stream.lock();
            let muxer = state.ingest.muxer().expect("current muxer");
            assert_eq!(muxer.video().is_some(), index == 2);
            if index != 2 {
                let master = sapflow_hls::playlist::multivariant(muxer);
                assert!(master.contains("CODECS=\"mp4a.40.2\""));
                assert!(!master.contains("RESOLUTION="));
                assert!(!master.contains("FRAME-RATE="));
            }
        }
        let (tracks, parts) = publication
            .stream
            .parts_after(None)
            .expect("recording epoch");
        assert_eq!(tracks.video().is_some(), index == 2);
        assert!(
            parts
                .iter()
                .all(|part| part.initialization.id == media.part.initialization_id)
        );
        previous = Some((media.part.part_id, media.part.initialization_id));
        publication.end();
        assert!(
            manager
                .status(SID)
                .await
                .expect("held status")
                .fresh_media
                .is_none()
        );
        assert_eq!(fresh_events(&mut callbacks), ["publish", "unpublish"]);
    }
}

#[tokio::test]
async fn changed_codec_first_idr_cannot_announce_old_retained_media_as_current_ready() {
    const PORTRAIT: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
    const LANDSCAPE: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-landscape.ts");
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let (app, mut callbacks) = fresh_media_app(&manager);
    let mut first = fresh_publication(&app).await;
    assert!(
        manager
            .status(SID)
            .await
            .expect("empty status")
            .fresh_media
            .is_none()
    );
    feed_generated(&mut first, PORTRAIT);
    let old = manager.status(SID).await.expect("old media");
    assert_eq!(old.state, "live");
    let old_media = old.fresh_media.expect("completed old media");
    assert_eq!(fresh_events(&mut callbacks), ["publish"]);
    let old_stream = first.stream.clone();
    let before_tail = next_part(&first);
    first.end(); // interrupt seals the previous partial part under claim locks.
    assert_eq!(fresh_events(&mut callbacks), ["unpublish"]);
    let after_tail = old_stream
        .lock()
        .ingest
        .muxer()
        .expect("retained")
        .next_part_id();
    assert!(
        after_tail > before_tail,
        "fixture must have a real pending old tail"
    );
    let held = manager.status(SID).await.expect("held status");
    assert_eq!(held.state, "held");
    assert!(held.fresh_media.is_none());
    let mut resumed = fresh_publication(&app).await;
    assert!(Arc::ptr_eq(&old_stream, &resumed.stream));
    let empty = manager.status(SID).await.expect("new empty epoch");
    assert_eq!(empty.connection_epoch, old_media.connection_epoch + 1);
    assert_eq!(empty.state, "connected");
    assert!(empty.fresh_media.is_none());
    assert!(resumed.stream.has_content(), "old HLS remains available");
    let mut consumed = 0;
    for packet in LANDSCAPE.chunks(188) {
        crate::srt::feed(&mut resumed, packet).expect("landscape packet");
        consumed += packet.len();
        if resumed
            .stream
            .lock()
            .ingest
            .muxer()
            .expect("muxer")
            .current_initialization_id()
            > old_media.part.initialization_id
        {
            break;
        }
    }
    assert!(
        consumed < LANDSCAPE.len(),
        "must stop at first changed-config IDR"
    );
    assert_eq!(
        next_part(&resumed),
        after_tail,
        "first IDR has no completed fresh part"
    );
    let first_idr = manager.status(SID).await.expect("pending IDR status");
    assert_eq!(first_idr.state, "connected");
    assert!(first_idr.fresh_media.is_none());
    assert!(
        fresh_events(&mut callbacks).is_empty(),
        "no premature scoped publish callback"
    );
    feed_generated(&mut resumed, &LANDSCAPE[consumed..]);
    let ready = manager.status(SID).await.expect("fresh completed part");
    assert_eq!(ready.state, "live");
    assert_eq!(fresh_events(&mut callbacks), ["publish"]);
    let encoded = serde_json::to_value(&ready).expect("receipt JSON");
    let object = encoded["freshMedia"].as_object().expect("flat descriptor");
    assert_eq!(object.len(), 7);
    assert_eq!(object["connectionEpoch"], encoded["connectionEpoch"]);
    assert_eq!(object["hasVideo"], true);
    assert_eq!(object["hasAudio"], true);
    let fresh = ready.fresh_media.expect("fresh descriptor");
    assert_eq!(fresh.connection_epoch, ready.connection_epoch);
    assert!(fresh.part.part_id >= after_tail);
    assert!(fresh.part.initialization_id > old_media.part.initialization_id);
    assert!(fresh.part.has_video && fresh.part.has_audio);
    {
        let st = resumed.stream.lock();
        let muxer = st.ingest.muxer().expect("muxer");
        let part = match muxer.part(fresh.part.part_id) {
            sapflow_hls::PartLookup::Found(part) => part,
            _ => panic!("descriptor part retained"),
        };
        assert!(part.independent);
        assert_eq!(part.initialization.id, fresh.part.initialization_id);
        assert_eq!(
            fresh.part.init_path,
            format!(
                "/live/{EVENT}/{}",
                sapflow_hls::playlist::epoch_init_name(&st.prefix, part.initialization.id)
            )
        );
        assert_eq!(
            fresh.part.part_path,
            format!(
                "/live/{EVENT}/{}",
                sapflow_hls::playlist::part_name(&st.prefix, part.id)
            )
        );
    }
    manager.revoke(SID, revoke(SID, 1, 2)).await.expect("end");
    assert!(
        manager
            .status(SID)
            .await
            .expect("ended")
            .fresh_media
            .is_none()
    );
    resumed.end();
}

#[tokio::test]
async fn same_codec_reconnect_requires_new_parts_and_old_lease_cannot_restore_freshness() {
    const VIDEO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expires = prepare(&manager).await;
    let (app, mut callbacks) = fresh_media_app(&manager);
    let mut first = fresh_publication(&app).await;
    feed_generated(&mut first, VIDEO);
    assert_eq!(fresh_events(&mut callbacks), ["publish"]);
    let old_lease = first.scoped_lease().expect("old scope");
    let first_media = manager
        .status(SID)
        .await
        .expect("first")
        .fresh_media
        .expect("first part");
    first.end();
    assert_eq!(fresh_events(&mut callbacks), ["unpublish"]);
    let mut resumed = fresh_publication(&app).await;
    let floor = next_part(&resumed);
    let without_media = manager.status(SID).await.expect("retained status");
    assert_eq!(without_media.state, "connected");
    assert!(without_media.fresh_media.is_none());
    feed_generated(&mut resumed, &VIDEO[..188]);
    assert!(
        manager
            .status(SID)
            .await
            .expect("headers only")
            .fresh_media
            .is_none()
    );
    assert!(fresh_events(&mut callbacks).is_empty());
    feed_generated(&mut resumed, &VIDEO[188..]);
    assert_eq!(fresh_events(&mut callbacks), ["publish"]);
    let ready = manager.status(SID).await.expect("fresh media");
    assert_eq!(ready.state, "live");
    let fresh = ready.fresh_media.expect("fresh same codec part");
    assert_eq!(
        fresh.part.initialization_id,
        first_media.part.initialization_id
    );
    assert!(fresh.part.part_id >= floor);
    assert!(matches!(old_lease.with_open(|| ()), Err(Error::Denied)));
    assert!(
        manager
            .status(SID)
            .await
            .expect("stale call cannot clear successor")
            .fresh_media
            .is_some()
    );
    // A higher prepared session generation cancels the old owner. Its retained
    // part identities never become a descriptor for the replacement grant.
    let replaced = manager
        .put(NEXT_SID, put(NEXT_SID, 2, 1, expires), MAX_BPS)
        .await
        .expect("replacement");
    assert!(replaced.fresh_media.is_none());
    assert_eq!(replaced.state, "prepared");
    assert!(matches!(
        resumed.push(|_, _| Ok(sapflow_hls::Update::default())),
        Err(crate::publish::PushError::Revoked)
    ));
    assert!(
        manager
            .status(NEXT_SID)
            .await
            .expect("replacement unchanged")
            .fresh_media
            .is_none()
    );
    resumed.end();
}

fn attempt_request(attempt: &str) -> PublishRequest {
    PublishRequest::parse_srt_stream_id(&format!(
        "#!::m=publish,r=live/{EVENT},u={SID},s={PASSWORD},forgesworn_attempt={attempt}"
    ))
    .expect("typed attempt request")
}

#[tokio::test]
async fn attempt_echo_binds_current_connection_and_fresh_media_in_one_receipt_snapshot() {
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const VIDEO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    let expiry = prepare(&manager).await;
    let (app, _callbacks) = fresh_media_app(&manager);
    let mut first = fresh_publication_request(&app, &attempt_request(A)).await;
    let lease = first.scoped_lease().expect("lease");
    let connected = manager.status(SID).await.expect("bound connected");
    assert_eq!(connected.state, "connected");
    assert_eq!(connected.connection_attempt_id.as_deref(), Some(A));
    assert!(connected.fresh_media.is_none());
    feed_generated(&mut first, VIDEO);
    let live = manager.status(SID).await.expect("first live");
    assert_eq!(live.connection_attempt_id.as_deref(), Some(A));
    assert_eq!(
        live.fresh_media
            .as_ref()
            .expect("first fresh")
            .connection_epoch,
        live.connection_epoch
    );
    // A grant refresh preserves this transport's identity rather than treating
    // request revision as an attempt ID or silently rotating active keys.
    let refreshed = manager
        .put(SID, put(SID, 1, 2, expiry + 1), MAX_BPS)
        .await
        .expect("refresh");
    assert_eq!(refreshed.connection_attempt_id.as_deref(), Some(A));
    first.end();
    lease.protocol_stopped();
    let held = manager.status(SID).await.expect("held");
    assert!(held.connection_attempt_id.is_none() && held.fresh_media.is_none());
    let mut second = fresh_publication_request(&app, &attempt_request(B)).await;
    let second_epoch = manager.status(SID).await.expect("second bound");
    assert!(second_epoch.connection_epoch > live.connection_epoch);
    assert_eq!(second_epoch.connection_attempt_id.as_deref(), Some(B));
    assert!(second_epoch.fresh_media.is_none());
    assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
    lease.protocol_stopped(); // Late old protocol cleanup cannot alter the echo.
    feed_generated(&mut second, VIDEO);
    let receipt = manager.status(SID).await.expect("second live");
    let json = serde_json::to_value(&receipt).expect("wire JSON");
    assert_eq!(json["connectionAttemptId"], B);
    assert_eq!(json["state"], "live");
    assert_eq!(
        json["freshMedia"]["connectionEpoch"],
        json["connectionEpoch"]
    );
    assert_eq!(
        json["freshMedia"]
            .as_object()
            .expect("unchanged fresh schema")
            .len(),
        7
    );
    // Explicit replacement clears the echo before any successor is bound.
    let replaced = manager
        .put(NEXT_SID, put(NEXT_SID, 2, 1, expiry + 2), MAX_BPS)
        .await
        .expect("replace session");
    assert!(replaced.connection_attempt_id.is_none() && replaced.fresh_media.is_none());
    assert!(matches!(
        second.push(|_, _| Ok(sapflow_hls::Update::default())),
        Err(crate::publish::PushError::Revoked)
    ));
    second.end();
    let terminal = manager
        .revoke(NEXT_SID, revoke(NEXT_SID, 2, 2))
        .await
        .expect("end");
    assert!(terminal.connection_attempt_id.is_none() && terminal.fresh_media.is_none());
}

#[tokio::test]
async fn attempt_metadata_needs_admitted_accepted_bound_connection_and_is_never_restored() {
    const ATTEMPT: &str = "abcdef0123456789abcdef0123456789";
    let scratch = Scratch::new();
    {
        let manager = scratch.open(false).await;
        prepare(&manager).await;
        let mut wrong = attempt_request(ATTEMPT);
        wrong.pass = Some("wrong".into());
        assert!(matches!(manager.admit(&wrong).await, Err(Error::Denied)));
        let refused = manager
            .status(SID)
            .await
            .expect("denied request did not attach");
        assert_eq!(refused.connection_epoch, 0);
        assert!(refused.connection_attempt_id.is_none());
        let lease = manager
            .admit(&attempt_request(ATTEMPT))
            .await
            .expect("admitted");
        assert!(
            manager
                .status(SID)
                .await
                .expect("unaccepted")
                .connection_attempt_id
                .is_none()
        );
        lease.accepted();
        assert!(
            manager
                .status(SID)
                .await
                .expect("unbound")
                .connection_attempt_id
                .is_none()
        );
        let registry = Arc::new(Registry::default());
        let (stream, generation, _) = registry
            .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
            .expect("claim");
        lease.bind(&registry, &stream, generation);
        assert_eq!(
            manager
                .status(SID)
                .await
                .expect("bound")
                .connection_attempt_id
                .as_deref(),
            Some(ATTEMPT)
        );
        // Revocation clears a live binding, independently of media completion.
        let revoked = manager
            .revoke(SID, revoke(SID, 1, 2))
            .await
            .expect("revoke");
        assert!(revoked.connection_attempt_id.is_none());
        assert!(matches!(lease.with_open(|| ()), Err(Error::Denied)));
        lease.released(false);
        lease.protocol_stopped();
        let stored = fs::read(scratch.config().store).expect("metadata");
        assert!(!String::from_utf8_lossy(&stored).contains(ATTEMPT));
    }
    let restored = scratch.open(false).await;
    let receipt = restored.status(SID).await.expect("restored terminal");
    assert!(receipt.connection_attempt_id.is_none() && receipt.fresh_media.is_none());
    assert_eq!(receipt.state, "revoked");
}

#[tokio::test]
async fn legacy_scoped_reconnect_never_inherits_previous_attempt_id() {
    const ATTEMPT: &str = "abcdef0123456789abcdef0123456789";
    const VIDEO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
    let scratch = Scratch::new();
    let manager = scratch.open(false).await;
    prepare(&manager).await;
    let (app, _callbacks) = fresh_media_app(&manager);
    let mut first = fresh_publication_request(&app, &attempt_request(ATTEMPT)).await;
    feed_generated(&mut first, VIDEO);
    first.end();
    let mut legacy = fresh_publication(&app).await;
    feed_generated(&mut legacy, VIDEO);
    let receipt = manager.status(SID).await.expect("compatible publisher");
    assert_eq!(receipt.state, "live");
    assert!(receipt.fresh_media.is_some());
    assert!(receipt.connection_attempt_id.is_none());
    manager.revoke(SID, revoke(SID, 1, 2)).await.expect("end");
    legacy.end();
}

#[tokio::test]
async fn active_attempt_echo_is_runtime_only_and_a_fresh_revision_cannot_restore_it() {
    const ATTEMPT: &str = "aabbccddeeff00112233445566778899";
    let scratch = Scratch::new();
    let expiry;
    {
        let manager = scratch.open(false).await;
        expiry = prepare(&manager).await;
        let lease = manager
            .admit(&attempt_request(ATTEMPT))
            .await
            .expect("admitted");
        lease.accepted();
        let registry = Arc::new(Registry::default());
        let (stream, generation, _) = registry
            .claim(&request(SID).path, MuxerConfig::default(), SourceKind::Ts)
            .expect("claim");
        lease.bind(&registry, &stream, generation);
        assert_eq!(
            manager
                .status(SID)
                .await
                .expect("active echo")
                .connection_attempt_id
                .as_deref(),
            Some(ATTEMPT)
        );
        let bytes = fs::read(scratch.config().store).expect("durable metadata");
        assert!(!String::from_utf8_lossy(&bytes).contains(ATTEMPT));
        lease.released(false);
        lease.protocol_stopped();
    }
    let restored = scratch.open(false).await;
    let inactive = restored.status(SID).await.expect("inactive restart");
    assert_eq!(inactive.state, "inactive");
    assert!(inactive.connection_attempt_id.is_none() && inactive.fresh_media.is_none());
    let refreshed = restored
        .put(SID, put(SID, 1, 2, expiry + 1), MAX_BPS)
        .await
        .expect("authoritative refresh");
    assert_eq!(refreshed.state, "prepared");
    assert!(refreshed.connection_attempt_id.is_none() && refreshed.fresh_media.is_none());
}
