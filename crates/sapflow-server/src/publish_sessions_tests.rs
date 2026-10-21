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
    lease.media_ready();
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
        lease.media_ready();
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
