//! Control-only binding to the common lifecycle/reconnect policy.
//! Native adapters still fence individual socket attempts and own their timers.
use crate::BindingError;
use sapflow_client::session::{
    Generation, MediaReadiness, ReconnectPolicy, RetryBudgetState, Session, SessionAction,
    SessionError, SessionState, TransportRecoverySource,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq)]
pub enum NativeSessionAction {
    None,
    Connect,
    RetryAt { at_ns: u64 },
    AwaitGraceDeadline { at_ns: u64 },
    EndDestination,
    GraceExpired,
    RetryBudgetExhausted,
}
impl From<SessionAction> for NativeSessionAction {
    fn from(action: SessionAction) -> Self {
        match action {
            SessionAction::None => Self::None,
            SessionAction::Connect => Self::Connect,
            SessionAction::RetryAt(at_ns) => Self::RetryAt { at_ns },
            SessionAction::AwaitGraceDeadline(at_ns) => Self::AwaitGraceDeadline { at_ns },
            SessionAction::EndDestination => Self::EndDestination,
            SessionAction::GraceExpired => Self::GraceExpired,
            SessionAction::RetryBudgetExhausted => Self::RetryBudgetExhausted,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum NativeSessionState {
    Idle,
    Preparing,
    Connecting,
    Sending,
    Live,
    Reconnecting,
    Interrupted,
    Stopping,
    Stopped,
    Failed,
}
impl From<SessionState> for NativeSessionState {
    fn from(state: SessionState) -> Self {
        match state {
            SessionState::Idle => Self::Idle,
            SessionState::Preparing => Self::Preparing,
            SessionState::Connecting => Self::Connecting,
            SessionState::Sending => Self::Sending,
            SessionState::Live => Self::Live,
            SessionState::Reconnecting => Self::Reconnecting,
            SessionState::Interrupted => Self::Interrupted,
            SessionState::Stopping => Self::Stopping,
            SessionState::Stopped => Self::Stopped,
            SessionState::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeMediaReadiness {
    Unknown,
    Confirmed,
}
impl From<MediaReadiness> for NativeMediaReadiness {
    fn from(value: MediaReadiness) -> Self {
        match value {
            MediaReadiness::Unknown => Self::Unknown,
            MediaReadiness::Confirmed => Self::Confirmed,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRetryBudgetState {
    Available,
    WindowOpen,
    Exhausted,
}
impl From<RetryBudgetState> for NativeRetryBudgetState {
    fn from(value: RetryBudgetState) -> Self {
        match value {
            RetryBudgetState::Available => Self::Available,
            RetryBudgetState::WindowOpen => Self::WindowOpen,
            RetryBudgetState::Exhausted => Self::Exhausted,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransportRecoverySource {
    SrtPeerAcknowledgedWatermark,
    RtmpPeerAcknowledgedWatermark,
}
impl From<NativeTransportRecoverySource> for TransportRecoverySource {
    fn from(value: NativeTransportRecoverySource) -> Self {
        match value {
            NativeTransportRecoverySource::SrtPeerAcknowledgedWatermark => {
                Self::SrtPeerAcknowledgedWatermark
            }
            NativeTransportRecoverySource::RtmpPeerAcknowledgedWatermark => {
                Self::RtmpPeerAcknowledgedWatermark
            }
        }
    }
}
impl From<TransportRecoverySource> for NativeTransportRecoverySource {
    fn from(value: TransportRecoverySource) -> Self {
        match value {
            TransportRecoverySource::SrtPeerAcknowledgedWatermark => {
                Self::SrtPeerAcknowledgedWatermark
            }
            TransportRecoverySource::RtmpPeerAcknowledgedWatermark => {
                Self::RtmpPeerAcknowledgedWatermark
            }
        }
    }
}

pub struct NativeSessionSnapshot {
    pub generation: u64,
    pub attempt_epoch: u64,
    pub media_readiness: NativeMediaReadiness,
    pub retry_budget: NativeRetryBudgetState,
    pub transport_recovery_source: Option<NativeTransportRecoverySource>,
    pub network_recoveries: u64,
    /// Original outage boundary, retained when retries are exhausted.
    pub outage_deadline_ns: Option<u64>,
    /// Drive this deadline, not the historical outage boundary.
    pub next_deadline_ns: Option<u64>,
    pub state: NativeSessionState,
    pub transport_connected: bool,
    pub reconnect_deadline_ns: Option<u64>,
    pub transport_failures: u64,
    pub retry_attempts: u64,
    pub media_confirmations: u64,
    pub stale_callbacks: u64,
}
pub struct NativeStopResult {
    pub generation: u64,
    pub action: NativeSessionAction,
}

struct Control {
    session: Session,
    stale_callbacks: u64,
}
pub struct NativeSession {
    control: Mutex<Control>,
}
impl NativeSession {
    fn with<T>(
        &self,
        callback: impl FnOnce(&mut Control) -> Result<T, BindingError>,
    ) -> Result<T, BindingError> {
        let mut control = self.control.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => BindingError::Busy,
            std::sync::TryLockError::Poisoned(_) => BindingError::InvalidSession,
        })?;
        callback(&mut control)
    }
    fn current<T>(
        &self,
        generation: u64,
        callback: impl FnOnce(&mut Session, Generation) -> Result<T, SessionError>,
    ) -> Result<T, BindingError> {
        self.with(|control| {
            let current = control.session.generation();
            if current.value() != generation {
                control.stale_callbacks = control.stale_callbacks.saturating_add(1);
                return Err(BindingError::StaleSession);
            }
            callback(&mut control.session, current).map_err(|error| match error {
                SessionError::ClockWentBackwards => BindingError::ClockWentBackwards,
                SessionError::StaleGeneration | SessionError::StaleAttempt => {
                    BindingError::StaleSession
                }
                _ => BindingError::InvalidSession,
            })
        })
    }
}
impl NativeSession {
    /// Consume the actual destination grace, 0..=3_600_000 milliseconds.
    /// Zero explicitly disables retry; larger values are refused, never clamped.
    pub fn new(grace_ms: u64) -> Result<Arc<Self>, BindingError> {
        if grace_ms > 60 * 60 * 1000 {
            return Err(BindingError::InvalidSession);
        }
        let grace_ns = grace_ms * 1_000_000;
        let max_delay = if grace_ns == 0 {
            8_000_000_000
        } else {
            grace_ns.min(8_000_000_000)
        };
        let policy = ReconnectPolicy::new(grace_ns, max_delay.min(500_000_000), max_delay)
            .map_err(|_| BindingError::InvalidSession)?;
        Ok(Arc::new(Self {
            control: Mutex::new(Control {
                session: Session::new(policy),
                stale_callbacks: 0,
            }),
        }))
    }
    pub fn start(&self, now_ns: u64) -> Result<u64, BindingError> {
        self.with(|control| {
            control
                .session
                .start(now_ns)
                .map(Generation::value)
                .map_err(|_| BindingError::InvalidSession)
        })
    }
    pub fn prepared(&self, generation: u64) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session.prepared(current).map(Into::into)
        })
    }
    pub fn connected(&self, generation: u64, now_ns: u64) -> Result<(), BindingError> {
        self.current(generation, |session, current| {
            session.connected(current, now_ns)
        })
    }
    /// Complete startup media entered this exact attempt; this clears no grace.
    pub fn sending(
        &self,
        generation: u64,
        attempt_epoch: u64,
        now_ns: u64,
    ) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session
                .sending(current, attempt_epoch, now_ns)
                .map(Into::into)
        })
    }
    /// Only native validated current-attempt peer watermark progress may call
    /// this. It clears a network outage, never confirms decoded playback.
    pub fn transport_recovered(
        &self,
        generation: u64,
        attempt_epoch: u64,
        now_ns: u64,
        source: NativeTransportRecoverySource,
    ) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session
                .transport_recovered(current, attempt_epoch, now_ns, source.into())
                .map(Into::into)
        })
    }
    /// Legacy synchronous input for adapters that already fence each attempt.
    /// Async probes must capture attemptEpoch before starting and use
    /// mediaConfirmedForAttempt. Socket readiness is never a media observation.
    pub fn media_confirmed(&self, generation: u64, now_ns: u64) -> Result<(), BindingError> {
        self.current(generation, |session, current| {
            session.media_confirmed(current, now_ns)
        })
    }
    /// Only actual decoded progress from the captured attempt may call this.
    /// Generation and attempt checks share the session's existing control lock;
    /// a delayed old probe cannot confirm its successor or extend its deadline.
    pub fn media_confirmed_for_attempt(
        &self,
        generation: u64,
        attempt_epoch: u64,
        now_ns: u64,
    ) -> Result<(), BindingError> {
        self.current(generation, |session, current| {
            session.media_confirmed_for_attempt(current, attempt_epoch, now_ns)
        })
    }
    /// Intentional socket/codec replacement is not a transport failure.
    /// Native owns cancellation and per-attempt fencing before Connect.
    pub fn replace_transport(
        &self,
        generation: u64,
        now_ns: u64,
    ) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session.replace_transport(current, now_ns).map(Into::into)
        })
    }
    pub fn transport_failed(
        &self,
        generation: u64,
        now_ns: u64,
    ) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session.transport_failed(current, now_ns).map(Into::into)
        })
    }
    pub fn tick(&self, generation: u64, now_ns: u64) -> Result<NativeSessionAction, BindingError> {
        self.current(generation, |session, current| {
            session.tick(current, now_ns).map(Into::into)
        })
    }
    pub fn interrupt(&self, generation: u64) -> Result<u64, BindingError> {
        self.current(generation, |session, current| {
            session.interrupt(current).map(Generation::value)
        })
    }
    pub fn stop(&self, generation: u64) -> Result<NativeStopResult, BindingError> {
        self.current(generation, |session, current| {
            session
                .stop(current)
                .map(|(generation, action)| NativeStopResult {
                    generation: generation.value(),
                    action: action.into(),
                })
        })
    }
    pub fn finish_stop(&self, generation: u64) -> Result<(), BindingError> {
        self.current(generation, Session::finish_stop)
    }
    pub fn cancel(&self, generation: u64) -> Result<u64, BindingError> {
        self.current(generation, |session, current| {
            session.cancel(current).map(Generation::value)
        })
    }
    pub fn snapshot(&self) -> Result<NativeSessionSnapshot, BindingError> {
        self.with(|control| {
            let stats = control.session.stats();
            Ok(NativeSessionSnapshot {
                generation: control.session.generation().value(),
                attempt_epoch: control.session.attempt_epoch(),
                media_readiness: control.session.media_readiness().into(),
                retry_budget: control.session.retry_budget().into(),
                transport_recovery_source: control
                    .session
                    .transport_recovery_source()
                    .map(Into::into),
                network_recoveries: stats.network_recoveries,
                outage_deadline_ns: control.session.outage_deadline_ns(),
                next_deadline_ns: control.session.next_deadline_ns(),
                state: control.session.state().into(),
                transport_connected: control.session.transport_connected(),
                reconnect_deadline_ns: control.session.reconnect_deadline_ns(),
                transport_failures: stats.transport_failures,
                retry_attempts: stats.retry_attempts,
                media_confirmations: stats.media_confirmations,
                stale_callbacks: control
                    .stale_callbacks
                    .saturating_add(stats.stale_callbacks),
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn decoded_observation_export_fences_attempt_before_clock_and_readiness() {
        let session = NativeSession::new(90_000).expect("session");
        let generation = session.start(0).expect("start");
        session.prepared(generation).expect("prepare");
        let prior = session.snapshot().expect("first attempt").attempt_epoch;
        session.transport_failed(generation, 10).expect("failure");
        assert_eq!(
            session.tick(generation, 500_000_010).expect("retry"),
            NativeSessionAction::Connect
        );
        let current = session.snapshot().expect("current attempt").attempt_epoch;
        assert!(prior < current);
        session
            .connected(generation, 500_000_011)
            .expect("replacement accepted");
        for stale in [0, prior, current + 1] {
            assert!(matches!(
                session.media_confirmed_for_attempt(generation, stale, u64::MAX),
                Err(BindingError::StaleSession)
            ));
        }
        let snapshot = session.snapshot().expect("unconfirmed snapshot");
        assert_eq!(snapshot.media_readiness, NativeMediaReadiness::Unknown);
        assert_eq!(snapshot.media_confirmations, 0);
        assert_eq!(snapshot.stale_callbacks, 3);
        assert_eq!(snapshot.outage_deadline_ns, Some(90_000_000_010));
        session
            .media_confirmed_for_attempt(generation, current, 500_000_012)
            .expect("current decoder");
        let snapshot = session.snapshot().expect("confirmed snapshot");
        assert_eq!(snapshot.media_readiness, NativeMediaReadiness::Confirmed);
        assert_eq!(snapshot.state, NativeSessionState::Live);
        assert_eq!(snapshot.outage_deadline_ns, None);
    }

    #[test]
    fn decoded_observation_export_cannot_resurrect_stopped_session() {
        let session = NativeSession::new(90_000).expect("session");
        let generation = session.start(0).expect("start");
        session.prepared(generation).expect("prepare");
        session.connected(generation, 1).expect("accepted");
        let attempt = session.snapshot().expect("attempt").attempt_epoch;
        let stopped = session.cancel(generation).expect("cancel");
        assert!(matches!(
            session.media_confirmed_for_attempt(generation, attempt, 2),
            Err(BindingError::StaleSession)
        ));
        assert!(matches!(
            session.media_confirmed_for_attempt(stopped, attempt, 2),
            Err(BindingError::InvalidSession)
        ));
        assert_eq!(
            session.snapshot().expect("stopped snapshot").state,
            NativeSessionState::Stopped
        );
    }

    #[test]
    fn intentional_replacement_preserves_the_logical_session_and_observed_media_gate() {
        let session = NativeSession::new(90_000).expect("valid recovery fixture operation");
        let generation = session.start(0).expect("valid recovery fixture operation");
        session
            .prepared(generation)
            .expect("valid recovery fixture operation");
        session
            .connected(generation, 1)
            .expect("valid recovery fixture operation");
        session
            .media_confirmed(generation, 2)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session
                .replace_transport(generation, 200_000_000_000)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::Connect
        ));
        let replaced = session
            .snapshot()
            .expect("valid recovery fixture operation");
        assert_eq!(replaced.generation, generation);
        assert_eq!(replaced.state, NativeSessionState::Connecting);
        assert!(!replaced.transport_connected);
        assert_eq!(replaced.reconnect_deadline_ns, None);
        assert_eq!(replaced.transport_failures, 0);
        assert_eq!(replaced.media_confirmations, 1);
        assert!(matches!(
            session.media_confirmed(generation, 200_000_000_001),
            Err(BindingError::InvalidSession)
        ));
        session
            .connected(generation, 200_000_000_002)
            .expect("valid recovery fixture operation");
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .state,
            NativeSessionState::Connecting
        );
        session
            .media_confirmed(generation, 200_000_000_003)
            .expect("valid recovery fixture operation");
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .state,
            NativeSessionState::Live
        );
    }

    #[test]
    fn replacement_while_reconnecting_preserves_expiry_and_clears_prior_retry() {
        let session = NativeSession::new(90_000).expect("valid recovery fixture operation");
        let generation = session.start(0).expect("valid recovery fixture operation");
        session
            .prepared(generation)
            .expect("valid recovery fixture operation");
        session
            .transport_failed(generation, 10)
            .expect("valid recovery fixture operation");
        let deadline = session
            .snapshot()
            .expect("valid recovery fixture operation")
            .reconnect_deadline_ns
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session
                .replace_transport(generation, 20)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::Connect
        ));
        assert!(matches!(
            session
                .tick(generation, 500_000_010)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::None
        ));
        let replaced = session
            .snapshot()
            .expect("valid recovery fixture operation");
        assert_eq!(replaced.state, NativeSessionState::Reconnecting);
        assert_eq!(replaced.reconnect_deadline_ns, Some(deadline));
        assert_eq!(replaced.transport_failures, 1);
        assert_eq!(replaced.retry_attempts, 0);
        assert!(matches!(
            session
                .replace_transport(generation, deadline)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::GraceExpired
        ));
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .state,
            NativeSessionState::Failed
        );
    }

    #[test]
    fn replacement_binding_rejects_clock_rollback_and_stopped_or_stale_generations() {
        let session = NativeSession::new(90_000).expect("valid recovery fixture operation");
        let generation = session
            .start(100)
            .expect("valid recovery fixture operation");
        session
            .prepared(generation)
            .expect("valid recovery fixture operation");
        session
            .connected(generation, 200)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session.replace_transport(generation, 199),
            Err(BindingError::ClockWentBackwards)
        ));
        assert!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .transport_connected
        );
        let stopped = session
            .stop(generation)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session.replace_transport(generation, 201),
            Err(BindingError::StaleSession)
        ));
        assert!(matches!(
            session.replace_transport(stopped.generation, 201),
            Err(BindingError::InvalidSession)
        ));
        session
            .finish_stop(stopped.generation)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session.replace_transport(stopped.generation, 201),
            Err(BindingError::InvalidSession)
        ));
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .state,
            NativeSessionState::Stopped
        );
    }

    #[test]
    fn destination_grace_accepts_the_full_hour_without_clamping() {
        assert!(matches!(
            NativeSession::new(3_600_001),
            Err(BindingError::InvalidSession)
        ));
        let session = NativeSession::new(3_600_000).expect("valid recovery fixture operation");
        let generation = session
            .start(100)
            .expect("valid recovery fixture operation");
        session
            .prepared(generation)
            .expect("valid recovery fixture operation");
        session
            .transport_failed(generation, 250)
            .expect("valid recovery fixture operation");
        let expected_deadline = 250 + 3_600_000 * 1_000_000u64;
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .reconnect_deadline_ns,
            Some(expected_deadline)
        );
        // A second failure retains the actual destination window.
        session
            .transport_failed(generation, 2000)
            .expect("valid recovery fixture operation");
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .reconnect_deadline_ns,
            Some(expected_deadline)
        );
        assert!(matches!(
            session
                .tick(generation, expected_deadline)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::GraceExpired
        ));
        assert_eq!(
            session
                .snapshot()
                .expect("valid recovery fixture operation")
                .state,
            NativeSessionState::Failed
        );
    }
    #[test]
    fn stop_fences_all_late_callbacks_without_resurrecting_the_show() {
        let session = NativeSession::new(90_000).expect("valid recovery fixture operation");
        let generation = session.start(0).expect("valid recovery fixture operation");
        assert!(matches!(
            session
                .prepared(generation)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::Connect
        ));
        session
            .connected(generation, 1)
            .expect("valid recovery fixture operation");
        session
            .transport_failed(generation, 2)
            .expect("valid recovery fixture operation");
        let stopped = session
            .stop(generation)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session.connected(generation, 3),
            Err(BindingError::StaleSession)
        ));
        session
            .finish_stop(stopped.generation)
            .expect("valid recovery fixture operation");
        let snapshot = session
            .snapshot()
            .expect("valid recovery fixture operation");
        assert_eq!(snapshot.state, NativeSessionState::Stopped);
        assert_eq!(snapshot.stale_callbacks, 1);
        assert!(snapshot.reconnect_deadline_ns.is_none());
    }
    #[test]
    fn zero_destination_grace_is_not_replaced_by_a_default() {
        let session = NativeSession::new(0).expect("valid recovery fixture operation");
        let generation = session.start(0).expect("valid recovery fixture operation");
        session
            .prepared(generation)
            .expect("valid recovery fixture operation");
        assert!(matches!(
            session
                .transport_failed(generation, 1)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::GraceExpired
        ));
    }
}

#[cfg(test)]
mod recovery_binding_tests {
    use super::*;
    #[test]
    fn typed_recovery_fences_attempt_and_never_claims_decoded_media() {
        let s = NativeSession::new(90_000).expect("valid recovery fixture operation");
        let g = s.start(0).expect("valid recovery fixture operation");
        s.prepared(g).expect("valid recovery fixture operation");
        s.connected(g, 1).expect("valid recovery fixture operation");
        let attempt = s
            .snapshot()
            .expect("valid recovery fixture operation")
            .attempt_epoch;
        s.sending(g, attempt, 2)
            .expect("valid recovery fixture operation");
        s.transport_failed(g, 3)
            .expect("valid recovery fixture operation");
        assert_eq!(
            s.tick(g, 500_000_003)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::Connect
        );
        s.connected(g, 500_000_004)
            .expect("valid recovery fixture operation");
        let next = s
            .snapshot()
            .expect("valid recovery fixture operation")
            .attempt_epoch;
        assert!(matches!(
            s.sending(g, attempt, 500_000_005),
            Err(BindingError::StaleSession)
        ));
        s.sending(g, next, 500_000_006)
            .expect("valid recovery fixture operation");
        s.transport_recovered(
            g,
            next,
            500_000_007,
            NativeTransportRecoverySource::SrtPeerAcknowledgedWatermark,
        )
        .expect("valid recovery fixture operation");
        let snapshot = s.snapshot().expect("valid recovery fixture operation");
        assert_eq!(snapshot.state, NativeSessionState::Sending);
        assert_eq!(snapshot.media_readiness, NativeMediaReadiness::Unknown);
        assert_eq!(snapshot.retry_budget, NativeRetryBudgetState::Available);
        assert_eq!(snapshot.network_recoveries, 1);
        assert_eq!(snapshot.next_deadline_ns, None);
        s.media_confirmed(g, 500_000_008)
            .expect("valid recovery fixture operation");
        assert_eq!(
            s.snapshot()
                .expect("valid recovery fixture operation")
                .media_readiness,
            NativeMediaReadiness::Confirmed
        );
    }
    #[test]
    fn unknown_peer_ack_exhausts_permission_not_current_sending_socket() {
        let s = NativeSession::new(1).expect("valid recovery fixture operation");
        let g = s.start(0).expect("valid recovery fixture operation");
        s.prepared(g).expect("valid recovery fixture operation");
        s.transport_failed(g, 1)
            .expect("valid recovery fixture operation");
        s.replace_transport(g, 2)
            .expect("valid recovery fixture operation");
        let a = s
            .snapshot()
            .expect("valid recovery fixture operation")
            .attempt_epoch;
        s.connected(g, 3).expect("valid recovery fixture operation");
        s.sending(g, a, 4)
            .expect("valid recovery fixture operation");
        assert_eq!(
            s.tick(g, 1_000_001)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::RetryBudgetExhausted
        );
        let snapshot = s.snapshot().expect("valid recovery fixture operation");
        assert_eq!(snapshot.retry_budget, NativeRetryBudgetState::Exhausted);
        assert!(snapshot.transport_connected);
        assert_eq!(snapshot.next_deadline_ns, None);
        assert_eq!(snapshot.outage_deadline_ns, Some(1_000_001));
        assert!(matches!(
            s.transport_recovered(
                g,
                a,
                1_000_002,
                NativeTransportRecoverySource::RtmpPeerAcknowledgedWatermark
            ),
            Err(BindingError::InvalidSession)
        ));
        assert_eq!(
            s.transport_failed(g, 1_000_003)
                .expect("valid recovery fixture operation"),
            NativeSessionAction::GraceExpired
        );
    }
}
