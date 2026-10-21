//! Control-only binding to the common lifecycle/reconnect policy.
//! Native adapters still fence individual socket attempts and own their timers.
use crate::BindingError;
use sapflow_client::session::{
    Generation, ReconnectPolicy, Session, SessionAction, SessionError, SessionState,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, uniffi::Enum)]
pub enum NativeSessionAction {
    None,
    Connect,
    RetryAt { at_ns: u64 },
    AwaitGraceDeadline { at_ns: u64 },
    EndDestination,
    GraceExpired,
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
        }
    }
}

#[derive(Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NativeSessionState {
    Idle,
    Preparing,
    Connecting,
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
            SessionState::Live => Self::Live,
            SessionState::Reconnecting => Self::Reconnecting,
            SessionState::Interrupted => Self::Interrupted,
            SessionState::Stopping => Self::Stopping,
            SessionState::Stopped => Self::Stopped,
            SessionState::Failed => Self::Failed,
        }
    }
}

#[derive(uniffi::Record)]
pub struct NativeSessionSnapshot {
    pub generation: u64,
    pub state: NativeSessionState,
    pub transport_connected: bool,
    pub reconnect_deadline_ns: Option<u64>,
    pub transport_failures: u64,
    pub retry_attempts: u64,
    pub media_confirmations: u64,
    pub stale_callbacks: u64,
}
#[derive(uniffi::Record)]
pub struct NativeStopResult {
    pub generation: u64,
    pub action: NativeSessionAction,
}

struct Control {
    session: Session,
    stale_callbacks: u64,
}
#[derive(uniffi::Object)]
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
                SessionError::StaleGeneration => BindingError::StaleSession,
                _ => BindingError::InvalidSession,
            })
        })
    }
}
#[uniffi::export]
impl NativeSession {
    /// Consume the actual destination grace. Zero explicitly disables retry.
    #[uniffi::constructor]
    pub fn new(grace_ms: u64) -> Result<Arc<Self>, BindingError> {
        if grace_ms > 30 * 60 * 1000 {
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
    /// Only observed playable media may call this. A ready socket, publish
    /// acceptance or local preview is not a decoded-media observation.
    pub fn media_confirmed(&self, generation: u64, now_ns: u64) -> Result<(), BindingError> {
        self.current(generation, |session, current| {
            session.media_confirmed(current, now_ns)
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
                state: control.session.state().into(),
                transport_connected: control.session.transport_connected(),
                reconnect_deadline_ns: control.session.reconnect_deadline_ns(),
                transport_failures: stats.transport_failures,
                retry_attempts: stats.retry_attempts,
                media_confirmations: stats.media_confirmations,
                stale_callbacks: control.stale_callbacks,
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn stop_fences_all_late_callbacks_without_resurrecting_the_show() {
        let session = NativeSession::new(90_000).unwrap();
        let generation = session.start(0).unwrap();
        assert!(matches!(
            session.prepared(generation).unwrap(),
            NativeSessionAction::Connect
        ));
        session.connected(generation, 1).unwrap();
        session.transport_failed(generation, 2).unwrap();
        let stopped = session.stop(generation).unwrap();
        assert!(matches!(
            session.connected(generation, 3),
            Err(BindingError::StaleSession)
        ));
        session.finish_stop(stopped.generation).unwrap();
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.state, NativeSessionState::Stopped);
        assert_eq!(snapshot.stale_callbacks, 1);
        assert!(snapshot.reconnect_deadline_ns.is_none());
    }
    #[test]
    fn zero_destination_grace_is_not_replaced_by_a_default() {
        let session = NativeSession::new(0).unwrap();
        let generation = session.start(0).unwrap();
        session.prepared(generation).unwrap();
        assert!(matches!(
            session.transport_failed(generation, 1).unwrap(),
            NativeSessionAction::GraceExpired
        ));
    }
}
