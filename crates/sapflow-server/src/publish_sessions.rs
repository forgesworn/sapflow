//! Durable admission and the fast media/revocation gate. Disk work never holds
//! the gate. Lock order: operation -> gate -> registry -> stream session -> media.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sapflow_core::PublishRequest;
use sapflow_core::publish_session::{
    self as policy, Authority, Command, ConnectionLease, Disposition, EpochMs, SessionKey,
    SessionStatus,
};
use sapflow_srt::crypto::{CryptoConfig, KeySize};
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::config::ControlConfig;
use crate::publish_observer::{CallbackScope, SessionObservations};
use crate::publish_session_store::PublishSessionStore;
use crate::publish_session_wire::{Limits, PutSession, RevokeSession, Secret};
use crate::stream::{LiveStream, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Conflict,
    Denied,
    Unknown,
    Busy,
    Unavailable,
}
impl From<policy::Error> for Error {
    fn from(value: policy::Error) -> Self {
        match value {
            policy::Error::UnknownSession => Self::Unknown,
            policy::Error::AlreadyClaimed | policy::Error::Capacity => Self::Busy,
            policy::Error::AdmissionDisabled | policy::Error::Terminal => Self::Denied,
            _ => Self::Conflict,
        }
    }
}

#[derive(Clone)]
pub struct PublishSessions(Arc<Inner>);
struct Inner {
    operation: AsyncMutex<()>,
    gate: Mutex<State>,
    store: Arc<Mutex<PublishSessionStore>>,
    clock: Clock,
    callbacks_enabled: bool,
    changed: tokio::sync::Notify,
    slots: Arc<tokio::sync::Semaphore>,
}
struct State {
    authority: Authority,
    grants: HashMap<String, Grant>,
    usable: bool,
}
struct Credentials {
    password: Secret,
    crypto: CryptoConfig,
}
struct Grant {
    key: SessionKey,
    credentials: Option<Credentials>,
    deadline: Instant,
    limits: Limits,
    grace: Duration,
    observations: Arc<SessionObservations>,
    owner: Option<Arc<Owner>>,
}
struct Binding {
    registry: Weak<Registry>,
    stream: Weak<LiveStream>,
    generation: u64,
}
struct Owner {
    lease: ConnectionLease,
    scope: Arc<CallbackScope>,
    cancel: watch::Sender<bool>,
    protocol: AtomicU8, // 0 never accepted, 1 active, 2 stopping, 3 stopped.
    released: AtomicBool,
    published: AtomicBool,
    held: AtomicBool,
    binding: Mutex<Option<Binding>>,
}

struct Clock {
    start: Instant,
    base: u64,
    #[cfg(test)]
    forced_now: std::sync::atomic::AtomicU64,
}
impl Clock {
    fn wall() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn new(floor: u64) -> Self {
        Self {
            start: Instant::now(),
            base: Self::wall().max(floor),
            #[cfg(test)]
            forced_now: std::sync::atomic::AtomicU64::new(0),
        }
    }
    fn now(&self) -> u64 {
        #[cfg(test)]
        let forced = self.forced_now.load(Ordering::Acquire);
        #[cfg(test)]
        if forced > 0 {
            return forced;
        }
        Self::wall().max(
            self.base.saturating_add(
                self.start
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            ),
        )
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub contract_version: u32,
    pub event_id: String,
    pub event_generation: u64,
    pub session_id: String,
    pub revision: u64,
    pub expires_at_unix_ms: u64,
    pub connection_epoch: u64,
    pub state: &'static str,
    pub admission: &'static str,
    pub protocol: &'static str,
    pub recording: &'static str,
    pub callbacks: &'static str,
    pub observations: Option<crate::publish_observer::ObservationCounts>,
}

/// A lease contains no raw credentials. All media and registry mutation must
/// occur inside with_open; cancellation also wakes an idle protocol owner.
#[derive(Clone)]
pub struct ScopedLease {
    manager: PublishSessions,
    owner: Arc<Owner>,
    _life: Arc<LeaseLife>,
}
struct LeaseLife {
    owner: Arc<Owner>,
}
impl Drop for LeaseLife {
    fn drop(&mut self) {
        // A caller can disappear while durable admission finishes. Releasing
        // an unconsumed result must not leave its reservation attached forever.
        if !self.owner.released.swap(true, Ordering::AcqRel) {
            self.owner.cancel.send_replace(true);
            if self.owner.protocol.load(Ordering::Acquire) == 0 {
                self.owner.protocol.store(3, Ordering::Release);
            }
        }
    }
}

impl PublishSessions {
    pub async fn open(config: &ControlConfig, callbacks_enabled: bool) -> Result<Self, Error> {
        let path = config.store.clone();
        let (store, authority) = tokio::task::spawn_blocking(move || {
            let (mut store, snapshot) =
                PublishSessionStore::open(&path).map_err(|_| Error::Unavailable)?;
            let authority = match snapshot {
                Some(s) => {
                    Authority::restore(s, policy::MAX_EVENTS).map_err(|_| Error::Unavailable)?
                }
                None => {
                    let authority =
                        Authority::new(policy::MAX_EVENTS).map_err(|_| Error::Unavailable)?;
                    store
                        .replace(&authority.snapshot())
                        .map_err(|_| Error::Unavailable)?;
                    authority
                }
            };
            Ok::<_, Error>((store, authority))
        })
        .await
        .map_err(|_| Error::Unavailable)??;
        let floor = authority.snapshot().clock_floor_ms;
        Ok(Self(Arc::new(Inner {
            operation: AsyncMutex::new(()),
            gate: Mutex::new(State {
                authority,
                grants: HashMap::new(),
                usable: true,
            }),
            store: Arc::new(Mutex::new(store)),
            clock: Clock::new(floor),
            callbacks_enabled,
            changed: tokio::sync::Notify::new(),
            slots: Arc::new(tokio::sync::Semaphore::new(32)),
        })))
    }
    fn gate(&self) -> MutexGuard<'_, State> {
        match self.0.gate.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                state.usable = false;
                for grant in state.grants.values_mut() {
                    Self::stop_grant(grant);
                }
                state
            }
        }
    }
    pub fn now_ms(&self) -> u64 {
        self.0.clock.now()
    }
    fn slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Error> {
        self.0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)
    }

    fn reconcile(state: &State, draft: &mut Authority) {
        for grant in state.grants.values() {
            if let Some(owner) = &grant.owner
                && owner.released.load(Ordering::Acquire)
            {
                draft.release_claim(&owner.lease);
            }
        }
    }
    fn draft(&self, state: &State) -> Result<Authority, Error> {
        if !state.usable {
            return Err(Error::Unavailable);
        }
        let mut draft = state.authority.clone();
        Self::reconcile(state, &mut draft);
        draft.advance_clock(EpochMs::new(self.now_ms()).map_err(|_| Error::Unavailable)?);
        Ok(draft)
    }
    fn stop_grant(grant: &mut Grant) {
        grant.credentials = None;
        if let Some(owner) = &grant.owner {
            let _ = owner
                .protocol
                .compare_exchange(0, 3, Ordering::AcqRel, Ordering::Acquire);
            let _ = owner
                .protocol
                .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
            owner.cancel.send_replace(true);
            if let Some(binding) = owner
                .binding
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                && let (Some(registry), Some(stream)) =
                    (binding.registry.upgrade(), binding.stream.upgrade())
            {
                registry.terminate(&stream, binding.generation);
            }
        }
    }
    fn fence_draft(state: &mut State, draft: &Authority) {
        for grant in state.grants.values_mut() {
            if !draft.admission_enabled(&grant.key) {
                Self::stop_grant(grant);
            }
        }
    }
    fn poison(&self) {
        let mut state = self.gate();
        state.usable = false;
        for grant in state.grants.values_mut() {
            Self::stop_grant(grant);
        }
    }
    async fn save(&self, draft: &Authority) -> Result<(), Error> {
        let snapshot = draft.snapshot();
        let store = self.0.store.clone();
        let result = tokio::task::spawn_blocking(move || {
            store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace(&snapshot)
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            self.poison();
            return Err(Error::Unavailable);
        }
        Ok(())
    }

    /// A command is staged under the gate, fsynced outside it, then published.
    /// Revocation/expiry fences runtime owners before waiting for durable I/O.
    pub async fn put(
        &self,
        route_id: &str,
        wire: PutSession,
        max_bps: u64,
    ) -> Result<Receipt, Error> {
        let slot = self.slot()?;
        let manager = self.clone();
        let route = route_id.to_owned();
        tokio::spawn(async move {
            let _slot = slot;
            manager.put_inner(&route, wire, max_bps).await
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn put_inner(
        &self,
        route_id: &str,
        wire: PutSession,
        max_bps: u64,
    ) -> Result<Receipt, Error> {
        let _operation = self.0.operation.lock().await;
        self.expire_locked().await?;
        let value = wire
            .validated(route_id, self.now_ms(), max_bps)
            .map_err(|_| Error::Invalid)?;
        let key = value.key.clone();
        let (mut draft, outcome, restoring_same_identity) = {
            let mut state = self.gate();
            let mut draft = self.draft(&state)?;
            let restoring_same_identity = state
                .authority
                .row(&key.path)
                .is_some_and(|row| row.key() == key)
                && !state.grants.contains_key(key.path.event_id());
            let unchanged_expiry = draft.row(&key.path).is_some_and(|row| {
                row.key() == key
                    && value.revision > row.revision
                    && row
                        .grant
                        .as_ref()
                        .is_some_and(|g| value.grant.expires_at_ms <= g.expires_at_ms)
            });
            let result = if unchanged_expiry {
                Err(policy::Error::ExpiryReduced)
            } else {
                draft.apply(Command::Provision(value))
            };
            Self::fence_draft(&mut state, &draft);
            (draft, result, restoring_same_identity)
        };
        // Persist clock/expiry changes even when the requested mutation fails.
        self.save(&draft).await?;
        let mut state = self.gate();
        if !state.usable {
            return Err(Error::Unavailable);
        }
        Self::reconcile(&state, &mut draft);
        state.authority = draft;
        let outcome = outcome.map_err(Error::from)?;
        if outcome.disposition == Disposition::Applied {
            let deadline = Instant::now()
                + Duration::from_millis(wire.expires_at_unix_ms.saturating_sub(self.now_ms()));
            if let Some(grant) = state.grants.get_mut(key.path.event_id())
                && grant.key == key
            {
                // A refresh never replaces an active SRT key or resets receipts.
                grant.deadline = deadline;
                // A refresh staged before expiry may finish durable I/O after
                // the old fast deadline. Its identical secret re-enables new
                // admission; the cancelled old protocol owner stays cancelled.
                if grant.credentials.is_none() {
                    grant.credentials = Some(Credentials {
                        password: wire.publish_password,
                        crypto: CryptoConfig::new(
                            wire.srt.passphrase.expose().as_bytes().to_vec(),
                            KeySize::Aes256,
                        )
                        .map_err(|_| Error::Invalid)?,
                    });
                }
            } else {
                if let Some(mut old) = state.grants.remove(key.path.event_id()) {
                    Self::stop_grant(&mut old);
                }
                let old_epoch = state
                    .authority
                    .row(&key.path)
                    .map_or(0, |r| r.connection_epoch);
                let observations = SessionObservations::restored(
                    self.0.callbacks_enabled,
                    restoring_same_identity && old_epoch > 0,
                );
                let crypto = CryptoConfig::new(
                    wire.srt.passphrase.expose().as_bytes().to_vec(),
                    KeySize::Aes256,
                )
                .map_err(|_| Error::Invalid)?;
                state.grants.insert(
                    key.path.event_id().to_owned(),
                    Grant {
                        key: key.clone(),
                        credentials: Some(Credentials {
                            password: wire.publish_password,
                            crypto,
                        }),
                        deadline,
                        limits: wire.limits,
                        grace: Duration::from_millis(wire.reconnect_grace_ms.into()),
                        observations,
                        owner: None,
                    },
                );
            }
        }
        self.0.changed.notify_one();
        self.receipt_locked(&state, route_id)
    }

    pub async fn revoke(&self, route_id: &str, wire: RevokeSession) -> Result<Receipt, Error> {
        let slot = self.slot()?;
        let manager = self.clone();
        let route = route_id.to_owned();
        tokio::spawn(async move {
            let _slot = slot;
            manager.revoke_inner(&route, wire).await
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn revoke_inner(&self, route_id: &str, wire: RevokeSession) -> Result<Receipt, Error> {
        let _operation = self.0.operation.lock().await;
        self.expire_locked().await?;
        let command = wire.validated(route_id).map_err(|_| Error::Invalid)?;
        let (mut draft, outcome) = {
            let mut state = self.gate();
            let mut draft = self.draft(&state)?;
            let result = draft.apply(Command::Revoke(command));
            Self::fence_draft(&mut state, &draft);
            (draft, result)
        };
        self.save(&draft).await?;
        let mut state = self.gate();
        Self::reconcile(&state, &mut draft);
        state.authority = draft;
        outcome.map_err(Error::from)?;
        self.0.changed.notify_one();
        self.receipt_locked(&state, route_id)
    }

    pub async fn status(&self, session_id: &str) -> Result<Receipt, Error> {
        let slot = self.slot()?;
        let manager = self.clone();
        let session_id = session_id.to_owned();
        tokio::spawn(async move {
            let _slot = slot;
            manager.status_inner(&session_id).await
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn status_inner(&self, session_id: &str) -> Result<Receipt, Error> {
        let _operation = self.0.operation.lock().await;
        self.expire_locked().await?;
        self.receipt_locked(&self.gate(), session_id)
    }
    fn receipt_locked(&self, state: &State, session_id: &str) -> Result<Receipt, Error> {
        if !state.usable {
            return Err(Error::Unavailable);
        }
        let snapshot = state.authority.snapshot();
        let row = snapshot
            .rows
            .iter()
            .find(|r| r.session_id.as_str() == session_id)
            .ok_or(Error::Unknown)?;
        let grant = state
            .grants
            .get(row.path.event_id())
            .filter(|g| g.key == row.key());
        let owner = grant.and_then(|g| g.owner.as_ref());
        let open = state.authority.admission_enabled(&row.key())
            && grant.is_some_and(|g| g.credentials.is_some() && Instant::now() < g.deadline);
        let protocol = match owner.map(|o| o.protocol.load(Ordering::Acquire)) {
            Some(1) => "active",
            Some(2) => "stopping",
            Some(3) => "stopped",
            _ if row.connection_epoch > 0 && grant.is_none() => "stopped",
            _ => "none",
        };
        let session_state = match row.status {
            SessionStatus::Revoked => "revoked",
            SessionStatus::Expired => "expired",
            SessionStatus::Prepared if !open => "inactive",
            SessionStatus::Prepared => {
                // A refresh may commit after the old fast deadline cancelled
                // its owner. Renewed authority permits a new connection, never
                // retroactively confirms that cancelled connection's media.
                let continuing = owner.filter(|o| !*o.cancel.borrow());
                let active = continuing
                    .filter(|o| protocol == "active" && !o.released.load(Ordering::Acquire));
                if continuing.is_some_and(|o| o.held.load(Ordering::Acquire)) {
                    "held"
                } else if active.is_some_and(|o| o.published.load(Ordering::Acquire)) {
                    "live"
                } else if active.is_some() {
                    "connected"
                } else {
                    "prepared"
                }
            }
        };
        Ok(Receipt {
            contract_version: 1,
            event_id: row.path.event_id().to_owned(),
            event_generation: row.event_generation.get(),
            session_id: session_id.to_owned(),
            revision: row.revision.get(),
            expires_at_unix_ms: row.grant.as_ref().map_or(0, |g| g.expires_at_ms.get()),
            connection_epoch: row.connection_epoch,
            state: session_state,
            admission: if open { "open" } else { "denied" },
            protocol,
            recording: grant.map_or(
                if row.connection_epoch > 0 {
                    "incomplete"
                } else {
                    "never-started"
                },
                |g| g.observations.recording_state(),
            ),
            callbacks: grant.map_or(
                if !self.0.callbacks_enabled {
                    "disabled"
                } else if row.connection_epoch > 0 {
                    "failed"
                } else {
                    "none"
                },
                |g| g.observations.callback_state(),
            ),
            observations: grant.map(|g| g.observations.counts()),
        })
    }

    pub async fn admit(&self, request: &PublishRequest) -> Result<ScopedLease, Error> {
        let slot = self.slot()?;
        let manager = self.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let _slot = slot;
            manager.admit_inner(&request).await
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn admit_inner(&self, request: &PublishRequest) -> Result<ScopedLease, Error> {
        let _operation = self.0.operation.lock().await;
        let (mut draft, reservation, must_save) = {
            let mut state = self.gate();
            let mut draft = self.draft(&state)?;
            Self::fence_draft(&mut state, &draft);
            let result = state
                .grants
                .get(request.path.event_id())
                .filter(|grant| Instant::now() < grant.deadline)
                .and_then(|grant| {
                    grant
                        .credentials
                        .as_ref()
                        .map(|credentials| (grant, credentials))
                })
                .filter(|(grant, credentials)| {
                    request.user.as_deref() == Some(grant.key.session_id.as_str())
                        && request
                            .pass
                            .as_deref()
                            .is_some_and(|p| credentials.password.matches(p))
                })
                .ok_or(Error::Denied)
                .and_then(|(grant, _)| draft.reserve_claim(&grant.key).map_err(Error::from));
            let must_save =
                result.is_ok() || draft.snapshot().rows != state.authority.snapshot().rows;
            (draft, result, must_save)
        };
        if must_save {
            self.save(&draft).await?;
        }
        let result = {
            let mut state = self.gate();
            Self::reconcile(&state, &mut draft);
            match reservation {
                Err(error) => {
                    state.authority = draft;
                    Err(error)
                }
                Ok(lease) => {
                    let allowed = state
                        .grants
                        .get(request.path.event_id())
                        .is_some_and(|grant| {
                            grant.key == lease.key
                                && grant.credentials.is_some()
                                && Instant::now() < grant.deadline
                        });
                    if !allowed {
                        draft.release_claim(&lease);
                        state.authority = draft;
                        Err(Error::Denied)
                    } else {
                        draft.commit_claim(&lease).map_err(Error::from)?;
                        state.authority = draft;
                        let grant = state
                            .grants
                            .get_mut(request.path.event_id())
                            .ok_or(Error::Denied)?;
                        let (cancel, _) = watch::channel(false);
                        let owner = Arc::new(Owner {
                            scope: CallbackScope::new(&lease, grant.observations.clone()),
                            lease,
                            cancel,
                            protocol: AtomicU8::new(0),
                            released: AtomicBool::new(false),
                            published: AtomicBool::new(false),
                            held: AtomicBool::new(false),
                            binding: Mutex::new(None),
                        });
                        grant.owner = Some(owner.clone());
                        Ok(ScopedLease {
                            manager: self.clone(),
                            _life: Arc::new(LeaseLife {
                                owner: owner.clone(),
                            }),
                            owner,
                        })
                    }
                }
            }
        };
        if matches!(result, Err(Error::Denied)) {
            self.expire_locked().await?;
        }
        result
    }

    async fn expire_locked(&self) -> Result<(), Error> {
        {
            let state = self.gate();
            if !state.usable {
                return Err(Error::Unavailable);
            }
            if state
                .authority
                .next_expiry()
                .is_none_or(|deadline| deadline.get() > self.now_ms())
            {
                return Ok(());
            }
        }
        let mut draft = {
            let mut state = self.gate();
            let draft = self.draft(&state)?;
            Self::fence_draft(&mut state, &draft);
            draft
        };
        // Clock floors are saved on admissions and mutations. A timer/status
        // write is needed only when an actual session becomes terminal.
        let changed = {
            let state = self.gate();
            draft.snapshot().rows != state.authority.snapshot().rows
        };
        if changed {
            self.save(&draft).await?;
            let mut state = self.gate();
            Self::reconcile(&state, &mut draft);
            state.authority = draft;
        }
        Ok(())
    }
    pub async fn expire(&self) -> Result<(), Error> {
        let manager = self.clone();
        tokio::spawn(async move {
            let _operation = manager.0.operation.lock().await;
            manager.expire_locked().await
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    pub async fn run_expiry(&self) -> Result<(), Error> {
        loop {
            let notified = self.0.changed.notified();
            let delay = {
                let state = self.gate();
                if !state.usable {
                    return Err(Error::Unavailable);
                }
                state.authority.next_expiry().map(|deadline| {
                    Duration::from_millis(deadline.get().saturating_sub(self.now_ms()).min(1000))
                })
            };
            match delay {
                Some(delay) => {
                    tokio::select! {()=tokio::time::sleep(delay)=>{},()=notified=>{}}
                }
                None => notified.await,
            }
            {
                let mut state = self.gate();
                for grant in state.grants.values_mut() {
                    if Instant::now() >= grant.deadline {
                        Self::stop_grant(grant);
                    }
                }
            }
            self.expire().await?;
        }
    }
}

impl ScopedLease {
    fn current(&self, state: &State) -> bool {
        state.usable
            && !self.owner.released.load(Ordering::Acquire)
            && state
                .grants
                .get(self.owner.lease.key.path.event_id())
                .is_some_and(|g| {
                    g.key == self.owner.lease.key
                        && g.credentials.is_some()
                        && Instant::now() < g.deadline
                        && g.owner
                            .as_ref()
                            .is_some_and(|o| Arc::ptr_eq(o, &self.owner))
                })
            && !*self.owner.cancel.borrow()
    }
    pub fn scope(&self) -> Arc<CallbackScope> {
        self.owner.scope.clone()
    }
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.owner.cancel.subscribe()
    }
    pub fn with_open<T>(&self, f: impl FnOnce() -> T) -> Result<T, Error> {
        let mut state = self.manager.gate();
        let current = self.current(&state);
        if !current {
            if let Some(grant) = state.grants.get_mut(self.owner.lease.key.path.event_id())
                && grant.key == self.owner.lease.key
                // Refusing a released lease must preserve a valid held stream
                // and credentials for its replacement connection. A previously
                // cancelled owner likewise cannot erase credentials restored
                // by a refresh that committed after the old fast deadline.
                && !self.owner.released.load(Ordering::Acquire)
                && !*self.owner.cancel.borrow()
                && grant
                    .owner
                    .as_ref()
                    .is_some_and(|o| Arc::ptr_eq(o, &self.owner))
            {
                PublishSessions::stop_grant(grant);
            }
            return Err(Error::Denied);
        }
        Ok(f())
    }
    pub fn bind(&self, registry: &Arc<Registry>, stream: &Arc<LiveStream>, generation: u64) {
        *self.owner.binding.lock().unwrap_or_else(|e| e.into_inner()) = Some(Binding {
            registry: Arc::downgrade(registry),
            stream: Arc::downgrade(stream),
            generation,
        });
    }
    pub fn crypto(&self) -> Option<CryptoConfig> {
        let state = self.manager.gate();
        if !self.current(&state) {
            return None;
        }
        state
            .grants
            .get(self.owner.lease.key.path.event_id())
            .and_then(|g| g.credentials.as_ref())
            .map(|c| c.crypto.clone())
    }
    pub fn limits(&self) -> Result<(Limits, Duration), Error> {
        let state = self.manager.gate();
        if !self.current(&state) {
            return Err(Error::Denied);
        }
        let g = state
            .grants
            .get(self.owner.lease.key.path.event_id())
            .ok_or(Error::Denied)?;
        Ok((g.limits, g.grace))
    }
    pub fn accepted(&self) {
        let _ = self
            .owner
            .protocol
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn media_ready(&self) {
        self.owner.published.store(true, Ordering::Release);
    }
    pub fn released(&self, held: bool) {
        self.owner.held.store(held, Ordering::Release);
        self.owner.released.store(true, Ordering::Release);
        let _ = self
            .owner
            .protocol
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn protocol_stopped(&self) {
        self.owner.protocol.store(3, Ordering::Release);
    }
}

impl std::fmt::Debug for ScopedLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedLease")
            .field("key", &self.owner.lease.key)
            .field("connection_epoch", &self.owner.lease.connection_epoch)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "publish_sessions_tests.rs"]
mod tests;
