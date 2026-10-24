//! A cold auth worker owns its runtime, relay client and shutdown. No foreign
//! callback or capture thread drives the runtime. Cancellation never queues.
use crate::{SignetCredential, SignetError, SignetEvent, contract};
use nostr::prelude::*;
use nostr_sdk::transport::websocket::{ConfiguredWebsocketTransport, WebSocketConfig};
use signet_nip46_client::{BunkerSession, ClientBuilder, RelayOptions, SessionOptions};
use std::num::NonZeroUsize;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, mpsc};
use zeroize::Zeroizing;

static OWNER: AtomicBool = AtomicBool::new(false);
type Response<T> = Result<T, SignetError>;
type Work<'a, T> = Pin<Box<dyn Future<Output = Response<T>> + Send + 'a>>;

trait Port: Send {
    fn connect(&mut self) -> Work<'_, (String, Zeroizing<Vec<u8>>)>;
    fn sign(&mut self, unsigned: UnsignedEvent) -> Work<'_, Event>;
    fn shutdown(&mut self) -> Work<'_, ()>;
}
struct RealPort {
    session: Option<BunkerSession>,
    expected: Option<String>,
}
impl Port for RealPort {
    fn connect(&mut self) -> Work<'_, (String, Zeroizing<Vec<u8>>)> {
        Box::pin(async {
            let session = self.session.as_ref().ok_or(SignetError::Closed)?;
            let persona = session
                .user_public_key()
                .await
                .map_err(|_| SignetError::Refused)?
                .to_hex();
            if self
                .expected
                .as_ref()
                .is_some_and(|expected| expected != &persona)
            {
                return Err(SignetError::InvalidProof);
            }
            let credential = session
                .persist()
                .await
                .map_err(|_| SignetError::Unavailable)?;
            Ok((
                persona.clone(),
                contract::encode_credential(credential, &persona)?,
            ))
        })
    }
    fn sign(&mut self, unsigned: UnsignedEvent) -> Work<'_, Event> {
        Box::pin(async {
            self.session
                .as_ref()
                .ok_or(SignetError::Closed)?
                .sign_event_async(unsigned)
                .await
                .map_err(|_| SignetError::Refused)
        })
    }
    fn shutdown(&mut self) -> Work<'_, ()> {
        Box::pin(async {
            if let Some(session) = self.session.take() {
                session.shutdown().await;
            }
            Ok(())
        })
    }
}

enum Config {
    Pair(Zeroizing<String>),
    Restore(Arc<SignetCredential>),
}
enum Command {
    Connect(u64),
    Sign(u64, contract::LoginTemplate),
}
struct State {
    last: u64,
    pending: Option<u64>,
    event: Option<SignetEvent>,
    persona: Option<String>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
    notify: Notify,
    done: AtomicBool,
}

/// At most one auth runtime per process. An owner whose native cleanup stalls
/// retains that permit; a replacement never bypasses it. Methods never perform
/// protocol work on the calling thread. Only wait_event/wait_closed may wait,
/// and must run on a dedicated cold Swift auth queue, never main/capture.
#[derive(uniffi::Object)]
pub struct NativeSignet {
    shared: Arc<Shared>,
    tx: mpsc::Sender<Command>,
    deadline: Instant,
}

fn limits() -> RelayOptions {
    let mut limits = nostr_sdk::prelude::RelayLimits::default();
    limits.messages.max_size = Some(16_384);
    limits.events.max_size = Some(16_384);
    limits.events.max_num_tags = Some(16);
    limits.events.max_size_per_kind.clear();
    limits.events.max_num_tags_per_kind.clear();
    RelayOptions::new()
        .limits(limits)
        .notification_channel_size(16)
        .reconnect(false)
}
fn client_builder() -> ClientBuilder {
    let config = WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(4096)
        .max_write_buffer_size(32_768)
        .max_message_size(Some(16_384))
        .max_frame_size(Some(16_384));
    ClientBuilder::default()
        .websocket_transport(ConfiguredWebsocketTransport::new(config))
        .max_relays(NonZeroUsize::new(4))
        .notification_channel_size(NonZeroUsize::new(16).expect("positive literal"))
}
fn real(config: Config) -> Response<Box<dyn Port>> {
    let options = SessionOptions {
        timeout: Duration::from_secs(60),
    };
    let (session, expected) = match config {
        Config::Pair(uri) => (
            BunkerSession::pair_with_client_builder(&uri, options, limits(), client_builder())
                .map_err(|_| SignetError::InvalidInput)?,
            None,
        ),
        Config::Restore(credential) => {
            let (persisted, persona) = contract::decode_credential(&credential.bytes)?;
            (
                BunkerSession::restore_with_client_builder(
                    persisted,
                    options,
                    limits(),
                    client_builder(),
                )
                .map_err(|_| SignetError::InvalidCredential)?,
                Some(persona),
            )
        }
    };
    Ok(Box::new(RealPort {
        session: Some(session),
        expected,
    }))
}

#[uniffi::export]
impl NativeSignet {
    /// Offline validation/construction. Only explicit connect starts pairing.
    #[uniffi::constructor]
    pub fn pair_bunker(uri: String, deadline_ms: u32) -> Response<Arc<Self>> {
        let uri = Zeroizing::new(uri);
        contract::bunker_uri(&uri, false)?;
        Self::spawn(Config::Pair(uri), deadline_ms)
    }
    /// Reuse exactly the stored client identity; never silently re-pair.
    #[uniffi::constructor]
    pub fn restore(credential: Arc<SignetCredential>, deadline_ms: u32) -> Response<Arc<Self>> {
        contract::decode_credential(&credential.bytes)?;
        Self::spawn(Config::Restore(credential), deadline_ms)
    }
    pub fn connect(&self, request_id: u64) -> Response<()> {
        self.submit(Command::Connect(request_id), request_id, false)
    }
    /// Only this exact kind21236 proof is exposed, not general event signing.
    pub fn sign_login(
        &self,
        request_id: u64,
        challenge: String,
        origin: String,
        app_name: String,
        created_at_seconds: u64,
    ) -> Response<()> {
        let persona = self
            .shared
            .state
            .try_lock()
            .map_err(|_| SignetError::Busy)?
            .persona
            .clone()
            .ok_or(SignetError::InvalidInput)?;
        let persona = PublicKey::from_hex(&persona).map_err(|_| SignetError::InvalidProof)?;
        let template = contract::LoginTemplate::new(
            persona,
            &challenge,
            &origin,
            &app_name,
            created_at_seconds,
        )?;
        template.fresh()?;
        self.submit(Command::Sign(request_id, template), request_id, true)
    }
    /// One bounded result slot, with no foreign callback retaining the worker.
    /// Blocking wait is capped at one second and wakes immediately on completion.
    pub fn wait_event(&self, timeout_ms: u32) -> Response<Option<SignetEvent>> {
        if timeout_ms > 1000 {
            return Err(SignetError::InvalidInput);
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| SignetError::Unavailable)?;
        let (mut state, _) = self
            .shared
            .wake
            .wait_timeout_while(state, Duration::from_millis(u64::from(timeout_ms)), |s| {
                s.event.is_none()
                    && !self.shared.done.load(Ordering::Acquire)
                    && !self.shared.stop.load(Ordering::Acquire)
            })
            .map_err(|_| SignetError::Unavailable)?;
        if self.shared.stop.load(Ordering::Acquire)
            && !matches!(state.event, Some(SignetEvent::Failed { .. }))
        {
            state.event = None;
            return Ok(None);
        }
        Ok(state.event.take())
    }
    /// Immediate atomic fence, independent of an in-flight operation or waiter.
    pub fn request_close(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.notify.notify_one();
        self.shared.wake.notify_all();
    }
    /// Actual runtime destruction, not a cancel request. On false retain owner,
    /// show cleanup failure and forbid replacement; a later actual close is safe.
    pub fn wait_closed(&self, timeout_ms: u32) -> Response<bool> {
        if timeout_ms > 2000 {
            return Err(SignetError::InvalidInput);
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| SignetError::Unavailable)?;
        let _guard = self
            .shared
            .wake
            .wait_timeout_while(state, Duration::from_millis(u64::from(timeout_ms)), |_| {
                !self.shared.done.load(Ordering::Acquire)
            })
            .map_err(|_| SignetError::Unavailable)?;
        Ok(self.shared.done.load(Ordering::Acquire))
    }
    pub fn is_closed(&self) -> bool {
        self.shared.done.load(Ordering::Acquire)
    }
}

impl NativeSignet {
    fn spawn(config: Config, deadline_ms: u32) -> Response<Arc<Self>> {
        Self::spawn_port(deadline_ms, move || real(config))
    }
    fn spawn_port<F>(deadline_ms: u32, make_port: F) -> Response<Arc<Self>>
    where
        F: FnOnce() -> Response<Box<dyn Port>> + Send + 'static,
    {
        if deadline_ms == 0 || deadline_ms > 120_000 {
            return Err(SignetError::InvalidInput);
        }
        let deadline = Instant::now() + Duration::from_millis(u64::from(deadline_ms));
        if OWNER
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(SignetError::Busy);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                last: 0,
                pending: None,
                event: None,
                persona: None,
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            notify: Notify::new(),
            done: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel(1);
        let worker = shared.clone();
        let spawn = std::thread::Builder::new()
            .name("sapflow-signet".into())
            .spawn(move || {
                // No global tracing feature/subscriber changes. Maintained NIP46
                // debug traces contain decrypted messages: suppress this worker.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tracing::dispatcher::with_default(&tracing::Dispatch::none(), || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .max_blocking_threads(1)
                            .build();
                        match runtime {
                            Ok(runtime) => {
                                runtime.block_on(run(worker.clone(), rx, deadline, make_port()));
                                // Default drop waits for any in-progress DNS/blocking
                                // work too. Never advertise Closed before it completes.
                                drop(runtime);
                            }
                            Err(_) => fail(&worker, 0, SignetError::Unavailable),
                        }
                    })
                }));
                if result.is_err() {
                    fail(&worker, 0, SignetError::Unavailable);
                }
                if let Ok(_state) = worker.state.lock() {
                    OWNER.store(false, Ordering::Release);
                    worker.done.store(true, Ordering::Release);
                    worker.wake.notify_all();
                }
            });
        if spawn.is_err() {
            OWNER.store(false, Ordering::Release);
            return Err(SignetError::Unavailable);
        }
        Ok(Arc::new(Self {
            shared,
            tx,
            deadline,
        }))
    }
    fn submit(&self, command: Command, id: u64, connected: bool) -> Response<()> {
        if id == 0 || id > 9_007_199_254_740_991 {
            return Err(SignetError::InvalidInput);
        }
        if self.shared.stop.load(Ordering::Acquire) || self.shared.done.load(Ordering::Acquire) {
            return Err(SignetError::Closed);
        }
        if Instant::now() >= self.deadline {
            self.request_close();
            return Err(SignetError::Deadline);
        }
        let mut state = self
            .shared
            .state
            .try_lock()
            .map_err(|_| SignetError::Busy)?;
        if state.pending.is_some() || state.event.is_some() {
            return Err(SignetError::Busy);
        }
        if id <= state.last || connected != state.persona.is_some() {
            return Err(SignetError::InvalidInput);
        }
        let previous = state.last;
        state.last = id;
        state.pending = Some(id);
        if self.tx.try_send(command).is_err() {
            state.pending = None;
            state.last = previous;
            return Err(SignetError::Busy);
        }
        Ok(())
    }
}

impl Drop for NativeSignet {
    fn drop(&mut self) {
        self.request_close();
    }
}

fn fail(shared: &Shared, id: u64, error: SignetError) {
    if let Ok(mut state) = shared.state.lock() {
        if !matches!(state.event, Some(SignetEvent::Failed { .. })) {
            state.event = Some(SignetEvent::Failed {
                request_id: id,
                error,
            });
        }
        state.pending = None;
        state.persona = None;
    }
    shared.stop.store(true, Ordering::Release);
    shared.wake.notify_all();
}
async fn run(
    shared: Arc<Shared>,
    mut commands: mpsc::Receiver<Command>,
    deadline: Instant,
    port: Response<Box<dyn Port>>,
) {
    let mut port = match port {
        Ok(port) => port,
        Err(error) => {
            fail(&shared, 0, error);
            return;
        }
    };
    loop {
        let command = tokio::select! {
            biased;
            _ = shared.notify.notified() => break,
            _ = tokio::time::sleep_until(deadline.into()) => { fail(&shared, 0, SignetError::Deadline); break; }
            command = commands.recv() => match command { Some(command) => command, None => break }
        };
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let id = match &command {
            Command::Connect(id) | Command::Sign(id, _) => *id,
        };
        let operation = async {
            match command {
                Command::Connect(_) => {
                    let (persona, bytes) = port.connect().await?;
                    Ok(SignetEvent::Ready {
                        request_id: id,
                        persona,
                        credential: Arc::new(SignetCredential { bytes }),
                    })
                }
                Command::Sign(_, template) => {
                    template.fresh()?;
                    let event = port.sign(template.unsigned.clone()).await?;
                    let json = template.verify(&event)?;
                    Ok(SignetEvent::Proof {
                        request_id: id,
                        json,
                    })
                }
            }
        };
        let result: Response<SignetEvent> = tokio::select! {
            biased;
            _ = shared.notify.notified() => Err(SignetError::Cancelled),
            _ = tokio::time::sleep_until(deadline.into()) => Err(SignetError::Deadline),
            result = operation => result,
        };
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        match result {
            Ok(event) => {
                if let Ok(mut state) = shared.state.lock()
                    && !shared.stop.load(Ordering::Acquire)
                    && state.pending == Some(id)
                {
                    if let SignetEvent::Ready { persona, .. } = &event {
                        state.persona = Some(persona.clone());
                    }
                    state.pending = None;
                    state.event = Some(event);
                    shared.wake.notify_all();
                }
            }
            Err(error) => {
                fail(&shared, id, error);
                break;
            }
        }
    }
    shared.stop.store(true, Ordering::Release);
    // Still retain the process permit until actual cleanup/runtime destruction.
    let _ = port.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    static SERIAL: Mutex<()> = Mutex::new(());
    struct Gates {
        connect: Notify,
        shutdown: Notify,
        entered: AtomicUsize,
        stopping: AtomicBool,
    }
    struct TestPort {
        gates: Arc<Gates>,
        blocked: bool,
        blocked_shutdown: bool,
        refuse: bool,
        keys: Keys,
    }
    impl Port for TestPort {
        fn connect(&mut self) -> Work<'_, (String, Zeroizing<Vec<u8>>)> {
            Box::pin(async {
                tracing::warn!("generated auth trace");
                self.gates.entered.fetch_add(1, Ordering::Release);
                if self.blocked {
                    self.gates.connect.notified().await;
                }
                let persona = self.keys.public_key().to_hex();
                let persisted = signet_nip46_client::PersistedSession {
                    app_secret_key: self.keys.secret_key().to_secret_hex(),
                    bunker_uri: format!("bunker://{persona}?relay=wss://relay.example.invalid"),
                };
                Ok((
                    persona.clone(),
                    contract::encode_credential(persisted, &persona)?,
                ))
            })
        }
        fn sign(&mut self, unsigned: UnsignedEvent) -> Work<'_, Event> {
            Box::pin(async {
                if self.refuse {
                    Err(SignetError::Refused)
                } else {
                    unsigned
                        .finalize(&self.keys)
                        .map_err(|_| SignetError::InvalidProof)
                }
            })
        }
        fn shutdown(&mut self) -> Work<'_, ()> {
            Box::pin(async {
                self.gates.stopping.store(true, Ordering::Release);
                if self.blocked_shutdown {
                    self.gates.shutdown.notified().await;
                }
                Ok(())
            })
        }
    }
    fn test_port(
        blocked: bool,
        blocked_shutdown: bool,
        refuse: bool,
    ) -> (Box<dyn Port>, Arc<Gates>) {
        let gates = Arc::new(Gates {
            connect: Notify::new(),
            shutdown: Notify::new(),
            entered: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
        });
        let keys = Keys::new(SecretKey::from_slice(&[1; 32]).expect("generated key"));
        (
            Box::new(TestPort {
                gates: gates.clone(),
                blocked,
                blocked_shutdown,
                refuse,
                keys,
            }),
            gates,
        )
    }
    fn ready(owner: &NativeSignet) -> Arc<SignetCredential> {
        owner.connect(1).expect("connect admitted");
        match owner.wait_event(1000).expect("completion") {
            Some(SignetEvent::Ready {
                request_id: 1,
                credential,
                ..
            }) => credential,
            _ => panic!("expected generated ready"),
        }
    }
    fn closed(owner: &NativeSignet) {
        owner.request_close();
        assert!(owner.wait_closed(2000).expect("actual cleanup"));
    }
    struct CountTrace(Arc<AtomicUsize>);
    impl tracing::Subscriber for CountTrace {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    #[test]
    fn native_worker_suppresses_only_its_own_traces_without_global_changes() {
        let _serial = SERIAL.lock().expect("serial");
        let count = Arc::new(AtomicUsize::new(0));
        tracing::dispatcher::with_default(
            &tracing::Dispatch::new(CountTrace(count.clone())),
            || {
                tracing::warn!("generated caller before");
                let (port, _) = test_port(false, false, false);
                let owner = NativeSignet::spawn_port(2000, move || Ok(port)).expect("owner");
                let _ = ready(&owner);
                closed(&owner);
                tracing::warn!("generated caller after");
            },
        );
        assert_eq!(count.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn panic_destroys_runtime_then_releases_permit_and_exports_fixed_error() {
        let _serial = SERIAL.lock().expect("serial");
        let owner = NativeSignet::spawn_port(2000, || panic!("generated fault")).expect("owner");
        assert!(matches!(
            owner.wait_event(1000).expect("event"),
            Some(SignetEvent::Failed {
                error: SignetError::Unavailable,
                ..
            })
        ));
        assert!(owner.wait_closed(2000).expect("runtime destruction"));
        let (port, _) = test_port(false, false, false);
        let successor =
            NativeSignet::spawn_port(2000, move || Ok(port)).expect("successor after destruction");
        closed(&successor);
    }
    #[test]
    fn invalid_bounds_and_request_ids_do_not_consume_capacity_or_protocol_work() {
        let _serial = SERIAL.lock().expect("serial");
        assert!(matches!(
            NativeSignet::spawn_port(0, || panic!("not admitted")),
            Err(SignetError::InvalidInput)
        ));
        assert!(matches!(
            NativeSignet::spawn_port(120001, || panic!("not admitted")),
            Err(SignetError::InvalidInput)
        ));
        let (port, _) = test_port(false, false, false);
        let owner = NativeSignet::spawn_port(2000, move || Ok(port)).expect("owner");
        assert!(matches!(owner.connect(0), Err(SignetError::InvalidInput)));
        assert!(matches!(
            owner.connect(9_007_199_254_740_992),
            Err(SignetError::InvalidInput)
        ));
        assert!(matches!(
            owner.wait_event(1001),
            Err(SignetError::InvalidInput)
        ));
        assert!(matches!(
            owner.wait_closed(2001),
            Err(SignetError::InvalidInput)
        ));
        let _ = ready(&owner);
        closed(&owner);
    }
    #[test]
    fn real_offline_owner_closes_and_releases_capacity_without_network() {
        let _serial = SERIAL.lock().expect("serial");
        let uri = format!(
            "bunker://{}?relay=wss://relay.example.invalid",
            Keys::generate().public_key().to_hex()
        );
        let owner = NativeSignet::pair_bunker(uri.clone(), 1000).expect("offline construction");
        assert!(matches!(
            NativeSignet::pair_bunker(uri.clone(), 1000),
            Err(SignetError::Busy)
        ));
        closed(&owner);
        let next = NativeSignet::pair_bunker(uri, 1000).expect("new actual owner after cleanup");
        closed(&next);
    }
    #[test]
    fn exact_native_worker_signs_fixed_proof_and_enforces_one_completion_slot() {
        let _serial = SERIAL.lock().expect("serial");
        let (port, _) = test_port(false, false, false);
        let owner = NativeSignet::spawn_port(2000, move || Ok(port)).expect("owner");
        let credential = ready(&owner);
        assert!(SignetCredential::restore(credential.encoded()).is_ok());
        assert!(matches!(owner.connect(1), Err(SignetError::InvalidInput)));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        owner
            .sign_login(
                2,
                "a".repeat(64),
                "https://api.example.invalid".into(),
                "Generated test".into(),
                now,
            )
            .expect("sign admitted");
        assert!(matches!(
            owner.sign_login(
                3,
                "a".repeat(64),
                "https://api.example.invalid".into(),
                "Generated test".into(),
                now
            ),
            Err(SignetError::Busy)
        ));
        match owner.wait_event(1000).expect("result") {
            Some(SignetEvent::Proof {
                request_id: 2,
                json,
            }) => {
                let event = Event::from_json(json).expect("event");
                assert_eq!(event.kind, Kind::Custom(21236));
                assert!(event.verify().is_ok());
            }
            _ => panic!("expected generated proof"),
        }
        closed(&owner);
    }
    #[test]
    fn cancellation_discards_late_connect_and_cleanup_must_finish_before_replacement() {
        let _serial = SERIAL.lock().expect("serial");
        let (port, gates) = test_port(true, true, false);
        let owner = NativeSignet::spawn_port(2000, move || Ok(port)).expect("owner");
        owner.connect(1).expect("admitted");
        let until = Instant::now() + Duration::from_secs(1);
        while gates.entered.load(Ordering::Acquire) == 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        assert_eq!(gates.entered.load(Ordering::Acquire), 1);
        owner.request_close();
        gates.connect.notify_one();
        assert!(!owner.wait_closed(10).expect("bounded wait"));
        assert!(matches!(owner.connect(2), Err(SignetError::Closed)));
        let (other, _) = test_port(false, false, false);
        assert!(matches!(
            NativeSignet::spawn_port(1000, move || Ok(other)),
            Err(SignetError::Busy)
        ));
        assert!(owner.wait_event(0).expect("no stale completion").is_none());
        gates.shutdown.notify_one();
        assert!(owner.wait_closed(2000).expect("actual close"));
        let (next, _) = test_port(false, false, false);
        let next =
            NativeSignet::spawn_port(1000, move || Ok(next)).expect("replacement now allowed");
        closed(&next);
    }
    #[test]
    fn original_deadline_expires_blocked_request_without_a_fresh_budget() {
        let _serial = SERIAL.lock().expect("serial");
        let (port, gates) = test_port(true, false, false);
        let owner = NativeSignet::spawn_port(40, move || Ok(port)).expect("owner");
        owner.connect(1).expect("admitted");
        assert!(matches!(
            owner.wait_event(1000).expect("result"),
            Some(SignetEvent::Failed {
                request_id: 1,
                error: SignetError::Deadline
            })
        ));
        gates.connect.notify_one();
        assert!(owner.wait_closed(2000).expect("cleanup"));
        assert!(owner.wait_event(0).expect("no replay").is_none());
        assert!(matches!(owner.connect(2), Err(SignetError::Closed)));
    }
    #[test]
    fn signer_refusal_is_terminal_and_not_a_secret_error_or_silent_repair() {
        let _serial = SERIAL.lock().expect("serial");
        let (port, _) = test_port(false, false, true);
        let owner = NativeSignet::spawn_port(2000, move || Ok(port)).expect("owner");
        ready(&owner);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        owner
            .sign_login(
                2,
                "a".repeat(64),
                "https://api.example.invalid".into(),
                "Test".into(),
                now,
            )
            .expect("admitted");
        assert!(matches!(
            owner.wait_event(1000).expect("refusal"),
            Some(SignetEvent::Failed {
                request_id: 2,
                error: SignetError::Refused
            })
        ));
        assert!(owner.wait_closed(2000).expect("cleanup"));
        assert!(matches!(owner.connect(3), Err(SignetError::Closed)));
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
