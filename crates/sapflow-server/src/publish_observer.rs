//! Bounded aggregate receipts, captured by an immutable connection scope.
//! No credentials, media, per-show lists or network/file operations live here.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use sapflow_core::publish_session::{ConnectionLease, MAX_JS_INTEGER, SessionKey};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationCounts {
    pub callbacks_pending: u64,
    pub callbacks_delivered: u64,
    pub callbacks_failed: u64,
    pub health_dropped: u64,
    pub recordings_active: u64,
    pub recordings_finalising: u64,
    pub recordings_finalised: u64,
    pub recordings_failed: u64,
    pub recordings_incomplete: u64,
}

fn increment(value: &mut u64) {
    *value = value.saturating_add(1).min(MAX_JS_INTEGER);
}

pub struct SessionObservations {
    callbacks_enabled: bool,
    counts: Mutex<ObservationCounts>,
}

impl SessionObservations {
    pub fn new(callbacks_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            callbacks_enabled,
            counts: Mutex::new(ObservationCounts::default()),
        })
    }
    /// Metadata cannot establish whether a previous process completed its file
    /// or callback. Preserve that uncertainty as sticky failure receipts.
    pub fn restored(callbacks_enabled: bool, had_connection: bool) -> Arc<Self> {
        let observations = Self::new(callbacks_enabled);
        if had_connection {
            let mut counts = observations.lock();
            counts.recordings_incomplete = 1;
            if callbacks_enabled {
                counts.callbacks_failed = 1;
            }
        }
        observations
    }
    fn lock(&self) -> MutexGuard<'_, ObservationCounts> {
        self.counts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    pub fn counts(&self) -> ObservationCounts {
        *self.lock()
    }
    pub(crate) fn callbacks_enabled(&self) -> bool {
        self.callbacks_enabled
    }
    pub fn recording_state(&self) -> &'static str {
        let counts = self.lock();
        if counts.recordings_failed > 0 {
            "failed"
        } else if counts.recordings_incomplete > 0 {
            "incomplete"
        } else if counts.recordings_active > 0 {
            "active"
        } else if counts.recordings_finalising > 0 {
            "finalising"
        } else if counts.recordings_finalised > 0 {
            "finalised"
        } else {
            "never-started"
        }
    }
    pub fn callback_state(&self) -> &'static str {
        let counts = self.lock();
        if !self.callbacks_enabled {
            "disabled"
        } else if counts.callbacks_failed > 0 {
            "failed"
        } else if counts.callbacks_pending > 0 {
            "pending"
        } else if counts.callbacks_delivered > 0 {
            "delivered"
        } else {
            "none"
        }
    }
    pub(crate) fn callback_failed(&self) {
        increment(&mut self.lock().callbacks_failed);
    }
    pub(crate) fn health_dropped(&self) {
        increment(&mut self.lock().health_dropped);
    }
    pub(crate) fn recording_failed(&self) {
        increment(&mut self.lock().recordings_failed);
    }
    pub(crate) fn callback_ticket(self: &Arc<Self>) -> CallbackTicket {
        increment(&mut self.lock().callbacks_pending);
        CallbackTicket {
            observations: self.clone(),
            completed: false,
        }
    }
}

pub(crate) struct CallbackTicket {
    observations: Arc<SessionObservations>,
    completed: bool,
}

impl CallbackTicket {
    pub(crate) fn finish(mut self, delivered: bool) {
        self.complete(Some(delivered));
    }
    pub(crate) fn dropped_health(mut self) {
        self.observations.health_dropped();
        self.complete(None);
    }
    fn complete(&mut self, delivered: Option<bool>) {
        if self.completed {
            return;
        }
        self.completed = true;
        let mut counts = self.observations.lock();
        counts.callbacks_pending = counts.callbacks_pending.saturating_sub(1);
        match delivered {
            Some(true) => increment(&mut counts.callbacks_delivered),
            Some(false) => increment(&mut counts.callbacks_failed),
            None => {}
        }
    }
}
impl Drop for CallbackTicket {
    fn drop(&mut self) {
        self.complete(Some(false));
    }
}

pub struct CallbackScope {
    key: SessionKey,
    connection_epoch: u64,
    pub(crate) observations: Arc<SessionObservations>,
    sequence: AtomicU64,
    recording_counter: AtomicU64,
    order: Mutex<()>,
}

impl CallbackScope {
    pub fn new(lease: &ConnectionLease, observations: Arc<SessionObservations>) -> Arc<Self> {
        Arc::new(Self {
            key: lease.key.clone(),
            connection_epoch: lease.connection_epoch,
            observations,
            sequence: AtomicU64::new(0),
            recording_counter: AtomicU64::new(0),
            order: Mutex::new(()),
        })
    }
    pub(crate) fn key(&self) -> &SessionKey {
        &self.key
    }
    pub(crate) fn connection_epoch(&self) -> u64 {
        self.connection_epoch
    }
    pub(crate) fn event_lock(&self) -> MutexGuard<'_, ()> {
        self.order.lock().unwrap_or_else(|error| error.into_inner())
    }
    pub(crate) fn next_sequence(&self) -> Option<u64> {
        self.sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1).filter(|next| *next <= MAX_JS_INTEGER)
            })
            .ok()
            .map(|old| old + 1)
    }
    pub(crate) fn recording(self: &Arc<Self>) -> Option<RecordingObservation> {
        let Some(counter) = self
            .recording_counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1).filter(|next| *next <= MAX_JS_INTEGER)
            })
            .ok()
            .map(|old| old + 1)
        else {
            self.observations.recording_failed();
            return None;
        };
        let mut hash = Sha256::new();
        hash.update(b"sapflow-scoped-recording-v1\0");
        let path = self.key.path.as_path();
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update(self.key.session_id.as_str().as_bytes());
        hash.update(self.key.event_generation.get().to_be_bytes());
        hash.update(self.connection_epoch.to_be_bytes());
        hash.update(counter.to_be_bytes());
        use std::fmt::Write;
        let mut id = String::with_capacity(32);
        for byte in &hash.finalize()[..16] {
            let _ = write!(id, "{byte:02x}");
        }
        Some(RecordingObservation(Arc::new(RecordingProgress {
            id,
            scope: self.clone(),
            phase: Mutex::new(RecordingPhase::NeverStarted),
            synced: AtomicBool::new(false),
        })))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecordingPhase {
    NeverStarted,
    Active,
    Finalising,
    Finalised,
    Failed,
    Incomplete,
}

struct RecordingProgress {
    id: String,
    scope: Arc<CallbackScope>,
    phase: Mutex<RecordingPhase>,
    synced: AtomicBool,
}

impl RecordingProgress {
    fn transition(&self, next: RecordingPhase) {
        let mut phase = self.phase.lock().unwrap_or_else(|error| error.into_inner());
        if *phase == next
            || matches!(*phase, RecordingPhase::Finalised | RecordingPhase::Failed)
            || (*phase == RecordingPhase::Incomplete && next != RecordingPhase::Failed)
        {
            return;
        }
        if next == RecordingPhase::Finalising && *phase == RecordingPhase::NeverStarted {
            return;
        }
        if next == RecordingPhase::Finalised && *phase == RecordingPhase::NeverStarted {
            return;
        }
        let mut counts = self.scope.observations.lock();
        match *phase {
            RecordingPhase::Active => {
                counts.recordings_active = counts.recordings_active.saturating_sub(1)
            }
            RecordingPhase::Finalising => {
                counts.recordings_finalising = counts.recordings_finalising.saturating_sub(1)
            }
            RecordingPhase::Incomplete => {
                counts.recordings_incomplete = counts.recordings_incomplete.saturating_sub(1)
            }
            _ => {}
        }
        match next {
            RecordingPhase::Active => increment(&mut counts.recordings_active),
            RecordingPhase::Finalising => increment(&mut counts.recordings_finalising),
            RecordingPhase::Finalised => increment(&mut counts.recordings_finalised),
            RecordingPhase::Failed => increment(&mut counts.recordings_failed),
            RecordingPhase::Incomplete => increment(&mut counts.recordings_incomplete),
            RecordingPhase::NeverStarted => return,
        }
        *phase = next;
    }
}
impl Drop for RecordingProgress {
    fn drop(&mut self) {
        let started = *self.phase.lock().unwrap_or_else(|error| error.into_inner())
            != RecordingPhase::NeverStarted;
        if started {
            self.transition(RecordingPhase::Incomplete);
        }
    }
}

#[derive(Clone)]
pub(crate) struct RecordingObservation(Arc<RecordingProgress>);

impl RecordingObservation {
    pub(crate) fn id(&self) -> &str {
        &self.0.id
    }
    pub(crate) fn active(&self) {
        self.0.transition(RecordingPhase::Active);
    }
    pub(crate) fn finalising(&self) {
        self.0.transition(RecordingPhase::Finalising);
    }
    pub(crate) fn finalised(&self) {
        self.0.synced.store(true, Ordering::Release);
        self.0.transition(RecordingPhase::Finalised);
    }
    pub(crate) fn synced_incomplete(&self) {
        self.0.synced.store(true, Ordering::Release);
        self.0.transition(RecordingPhase::Incomplete);
    }
    pub(crate) fn failed(&self) {
        self.0.transition(RecordingPhase::Failed);
    }
    pub(crate) fn incomplete(&self) {
        self.0.transition(RecordingPhase::Incomplete);
    }
    pub(crate) fn completion_state(&self) -> Option<&'static str> {
        if !self.0.synced.load(Ordering::Acquire) {
            return None;
        }
        match *self
            .0
            .phase
            .lock()
            .unwrap_or_else(|error| error.into_inner())
        {
            RecordingPhase::Finalised => Some("finalised"),
            RecordingPhase::Incomplete => Some("incomplete"),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn fixture_scope(
    event: &str,
    generation: u64,
    session: u64,
    epoch: u64,
) -> Arc<CallbackScope> {
    use sapflow_core::{StreamPath, publish_session::*};
    let mut authority = Authority::new(1).expect("authority");
    authority.advance_clock(EpochMs::new(1).expect("time"));
    let key = SessionKey {
        path: StreamPath::parse(&format!("live/event-{event}")).expect("path"),
        event_generation: JsPositive::new(generation).expect("generation"),
        session_id: SessionId::parse(&format!("{session:032x}")).expect("session"),
    };
    authority
        .apply(Command::Provision(Provision {
            key: key.clone(),
            revision: JsPositive::new(1).expect("revision"),
            grant: GrantMetadata {
                expires_at_ms: EpochMs::new(10000).expect("expiry"),
                reconnect_grace_ms: 0,
                immutable_fingerprint: Fingerprint::new([1; 32]),
            },
            request_fingerprint: Fingerprint::new([2; 32]),
        }))
        .expect("provision");
    let mut snapshot = authority.snapshot();
    snapshot.rows[0].connection_epoch = epoch - 1;
    let mut authority = Authority::restore(snapshot, 1).expect("restore");
    authority
        .apply(Command::Refresh(Provision {
            key: key.clone(),
            revision: JsPositive::new(2).expect("revision"),
            grant: GrantMetadata {
                expires_at_ms: EpochMs::new(10000).expect("expiry"),
                reconnect_grace_ms: 0,
                immutable_fingerprint: Fingerprint::new([1; 32]),
            },
            request_fingerprint: Fingerprint::new([3; 32]),
        }))
        .expect("refresh");
    CallbackScope::new(
        &authority.reserve_claim(&key).expect("lease"),
        SessionObservations::new(true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_cannot_invent_completion_from_connection_metadata() {
        let observations = SessionObservations::restored(true, true);
        assert_eq!(observations.recording_state(), "incomplete");
        assert_eq!(observations.callback_state(), "failed");
        observations.callback_ticket().finish(true);
        assert_eq!(observations.callback_state(), "failed");
        assert_eq!(
            SessionObservations::restored(false, true).callback_state(),
            "disabled"
        );
        let untouched = SessionObservations::restored(true, false);
        assert_eq!(untouched.recording_state(), "never-started");
        assert_eq!(untouched.callback_state(), "none");
    }
    #[test]
    fn callback_receipts_are_pending_until_ack_and_failure_is_sticky() {
        let observations = SessionObservations::new(true);
        assert_eq!(observations.callback_state(), "none");
        let first = observations.callback_ticket();
        let second = observations.callback_ticket();
        assert_eq!(observations.callback_state(), "pending");
        first.finish(true);
        assert_eq!(observations.callback_state(), "pending");
        drop(second);
        assert_eq!(observations.callback_state(), "failed");
        observations.callback_ticket().finish(true);
        assert_eq!(observations.callback_state(), "failed");
        assert_eq!(observations.counts().callbacks_delivered, 2);
        assert_eq!(observations.counts().callbacks_failed, 1);
        assert_eq!(observations.counts().callbacks_pending, 0);
    }
    #[test]
    fn dropped_health_is_visible_without_inventing_delivery() {
        let observations = SessionObservations::new(true);
        observations.callback_ticket().dropped_health();
        assert_eq!(observations.counts().health_dropped, 1);
        assert_eq!(observations.counts().callbacks_pending, 0);
        assert_eq!(observations.callback_state(), "none");
        assert_eq!(SessionObservations::new(false).callback_state(), "disabled");
    }
    #[test]
    fn recording_aggregate_never_calls_an_active_or_abandoned_file_finalised() {
        let scope = fixture_scope("a", 1, 1, 1);
        assert_eq!(scope.observations.recording_state(), "never-started");
        let first = scope.recording().expect("recording");
        first.active();
        first.finalising();
        assert_eq!(scope.observations.recording_state(), "finalising");
        first.finalised();
        assert_eq!(scope.observations.recording_state(), "finalised");
        let second = scope.recording().expect("recording");
        second.active();
        assert_eq!(scope.observations.recording_state(), "active");
        drop(second);
        assert_eq!(scope.observations.recording_state(), "incomplete");
        let third = scope.recording().expect("recording");
        third.active();
        third.failed();
        assert_eq!(scope.observations.recording_state(), "failed");
        assert_eq!(scope.observations.counts().recordings_active, 0);
    }
    #[test]
    fn recording_ids_are_bounded_and_distinct_across_epochs_sessions_and_counters() {
        let a = fixture_scope("a", 1, 1, 1);
        let b = fixture_scope("a", 1, 1, 2);
        let c = fixture_scope("a", 2, 2, 1);
        let records = [
            a.recording().expect("a"),
            a.recording().expect("a2"),
            b.recording().expect("b"),
            c.recording().expect("c"),
        ];
        let ids: std::collections::BTreeSet<_> =
            records.iter().map(RecordingObservation::id).collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.iter().all(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }));
    }
    #[test]
    fn sequence_and_recording_counter_exhaustion_fail_closed() {
        let scope = fixture_scope("a", 1, 1, 1);
        scope.sequence.store(MAX_JS_INTEGER - 1, Ordering::Relaxed);
        assert_eq!(scope.next_sequence(), Some(MAX_JS_INTEGER));
        assert_eq!(scope.next_sequence(), None);
        scope
            .recording_counter
            .store(MAX_JS_INTEGER, Ordering::Relaxed);
        assert!(scope.recording().is_none());
        assert_eq!(scope.observations.recording_state(), "failed");
    }
}
