//! Caller-driven session lifecycle. Generation tokens fence asynchronous work;
//! monotonic time and explicit actions keep cancellation independent of a runtime.

pub const DEFAULT_RECONNECT_GRACE_NS: u64 = 90_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation(u64);
impl Generation {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    StaleGeneration,
    InvalidTransition,
    ClockWentBackwards,
    ClockOverflow,
    GenerationExhausted,
    InvalidPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    grace_ns: u64,
    first_delay_ns: u64,
    max_delay_ns: u64,
}
impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            grace_ns: DEFAULT_RECONNECT_GRACE_NS,
            first_delay_ns: 500_000_000,
            max_delay_ns: 8_000_000_000,
        }
    }
}
impl ReconnectPolicy {
    /// Consume the destination's actual grace, rather than assuming its default.
    pub fn new(
        grace_ns: u64,
        first_delay_ns: u64,
        max_delay_ns: u64,
    ) -> Result<Self, SessionError> {
        if first_delay_ns == 0
            || first_delay_ns > max_delay_ns
            || (grace_ns != 0 && max_delay_ns > grace_ns)
        {
            return Err(SessionError::InvalidPolicy);
        }
        Ok(Self {
            grace_ns,
            first_delay_ns,
            max_delay_ns,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    None,
    Connect,
    RetryAt(u64),
    /// Deadline timer only; no connection attempt is permitted at this time.
    AwaitGraceDeadline(u64),
    EndDestination,
    GraceExpired,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    pub transport_connections: u64,
    pub media_confirmations: u64,
    pub transport_failures: u64,
    pub retry_attempts: u64,
    pub stale_callbacks: u64,
}

#[derive(Debug)]
pub struct Session {
    generation: Generation,
    state: SessionState,
    policy: ReconnectPolicy,
    last_now: Option<u64>,
    deadline_ns: Option<u64>,
    retry_at_ns: Option<u64>,
    failures_in_window: u32,
    transport_connected: bool,
    stats: SessionStats,
}
impl Session {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            generation: Generation(0),
            state: SessionState::Idle,
            policy,
            last_now: None,
            deadline_ns: None,
            retry_at_ns: None,
            failures_in_window: 0,
            transport_connected: false,
            stats: SessionStats::default(),
        }
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn state(&self) -> SessionState {
        self.state
    }
    pub fn stats(&self) -> SessionStats {
        self.stats
    }
    pub fn reconnect_deadline_ns(&self) -> Option<u64> {
        self.deadline_ns
    }
    pub fn transport_connected(&self) -> bool {
        self.transport_connected
    }
    fn check(&mut self, generation: Generation) -> Result<(), SessionError> {
        if generation != self.generation {
            self.stats.stale_callbacks = self.stats.stale_callbacks.saturating_add(1);
            Err(SessionError::StaleGeneration)
        } else {
            Ok(())
        }
    }
    fn clock(&mut self, now_ns: u64) -> Result<(), SessionError> {
        if self.last_now.is_some_and(|last| now_ns < last) {
            return Err(SessionError::ClockWentBackwards);
        }
        self.last_now = Some(now_ns);
        Ok(())
    }
    fn advance_generation(&mut self) -> Result<Generation, SessionError> {
        self.generation = Generation(
            self.generation
                .0
                .checked_add(1)
                .ok_or(SessionError::GenerationExhausted)?,
        );
        Ok(self.generation)
    }
    pub fn start(&mut self, now_ns: u64) -> Result<Generation, SessionError> {
        if !matches!(
            self.state,
            SessionState::Idle | SessionState::Stopped | SessionState::Failed
        ) {
            return Err(SessionError::InvalidTransition);
        }
        self.clock(now_ns)?;
        let generation = self.advance_generation()?;
        self.deadline_ns = None;
        self.retry_at_ns = None;
        self.failures_in_window = 0;
        self.transport_connected = false;
        self.state = SessionState::Preparing;
        Ok(generation)
    }
    pub fn prepared(&mut self, generation: Generation) -> Result<SessionAction, SessionError> {
        self.check(generation)?;
        if self.state != SessionState::Preparing {
            return Err(SessionError::InvalidTransition);
        }
        self.state = SessionState::Connecting;
        Ok(SessionAction::Connect)
    }
    /// A socket handshake does not establish that a viewer can receive media.
    pub fn connected(&mut self, generation: Generation, now_ns: u64) -> Result<(), SessionError> {
        self.check(generation)?;
        if !matches!(
            self.state,
            SessionState::Connecting | SessionState::Reconnecting
        ) {
            return Err(SessionError::InvalidTransition);
        }
        self.clock(now_ns)?;
        if self.deadline_ns.is_some_and(|deadline| now_ns >= deadline) {
            self.expire();
            return Err(SessionError::InvalidTransition);
        }
        self.transport_connected = true;
        self.retry_at_ns = None;
        self.stats.transport_connections = self.stats.transport_connections.saturating_add(1);
        Ok(())
    }
    /// Destination adapter supplies observed playable-media confirmation.
    /// A confirmed recovery closes this outage; a later outage starts a new one.
    pub fn media_confirmed(
        &mut self,
        generation: Generation,
        now_ns: u64,
    ) -> Result<(), SessionError> {
        self.check(generation)?;
        if !self.transport_connected
            || !matches!(
                self.state,
                SessionState::Connecting | SessionState::Reconnecting | SessionState::Live
            )
        {
            return Err(SessionError::InvalidTransition);
        }
        self.clock(now_ns)?;
        if self.state != SessionState::Live
            && self.deadline_ns.is_some_and(|deadline| now_ns >= deadline)
        {
            self.expire();
            return Err(SessionError::InvalidTransition);
        }
        self.state = SessionState::Live;
        self.deadline_ns = None;
        self.failures_in_window = 0;
        self.stats.media_confirmations = self.stats.media_confirmations.saturating_add(1);
        Ok(())
    }
    /// Replace an intentional codec/socket epoch within this logical session.
    /// The adapter must fence and close the old attempt before acting on Connect.
    /// This is not a network failure: a healthy replacement starts no outage,
    /// while an existing outage keeps its original deadline and failure history.
    pub fn replace_transport(
        &mut self,
        generation: Generation,
        now_ns: u64,
    ) -> Result<SessionAction, SessionError> {
        self.check(generation)?;
        if !matches!(
            self.state,
            SessionState::Connecting | SessionState::Live | SessionState::Reconnecting
        ) {
            return Err(SessionError::InvalidTransition);
        }
        self.clock(now_ns)?;
        if self.deadline_ns.is_some_and(|deadline| now_ns >= deadline) {
            self.expire();
            return Ok(SessionAction::GraceExpired);
        }
        self.transport_connected = false;
        self.retry_at_ns = None;
        self.state = if self.deadline_ns.is_some() {
            SessionState::Reconnecting
        } else {
            SessionState::Connecting
        };
        Ok(SessionAction::Connect)
    }

    pub fn transport_failed(
        &mut self,
        generation: Generation,
        now_ns: u64,
    ) -> Result<SessionAction, SessionError> {
        self.check(generation)?;
        if !matches!(
            self.state,
            SessionState::Connecting | SessionState::Reconnecting | SessionState::Live
        ) {
            return Err(SessionError::InvalidTransition);
        }
        self.clock(now_ns)?;
        let deadline = match self.deadline_ns {
            Some(deadline) => deadline,
            None => now_ns
                .checked_add(self.policy.grace_ns)
                .ok_or(SessionError::ClockOverflow)?,
        };
        self.deadline_ns = Some(deadline);
        self.stats.transport_failures = self.stats.transport_failures.saturating_add(1);
        self.transport_connected = false;
        if now_ns >= deadline {
            self.expire();
            return Ok(SessionAction::GraceExpired);
        }
        let multiplier = 1u64
            .checked_shl(self.failures_in_window.min(63))
            .unwrap_or(u64::MAX);
        let delay = self
            .policy
            .first_delay_ns
            .saturating_mul(multiplier)
            .min(self.policy.max_delay_ns);
        let retry_at = now_ns.saturating_add(delay).min(deadline);
        self.failures_in_window = self.failures_in_window.saturating_add(1);
        self.state = SessionState::Reconnecting;
        if retry_at >= deadline {
            self.retry_at_ns = None;
            Ok(SessionAction::AwaitGraceDeadline(deadline))
        } else {
            self.retry_at_ns = Some(retry_at);
            Ok(SessionAction::RetryAt(retry_at))
        }
    }
    fn expire(&mut self) {
        self.state = SessionState::Failed;
        self.retry_at_ns = None;
        self.transport_connected = false;
    }
    /// Must be driven while disconnected; no hidden timers are created.
    pub fn tick(
        &mut self,
        generation: Generation,
        now_ns: u64,
    ) -> Result<SessionAction, SessionError> {
        self.check(generation)?;
        self.clock(now_ns)?;
        if self.deadline_ns.is_some_and(|deadline| now_ns >= deadline) {
            match self.state {
                SessionState::Live => {
                    self.deadline_ns = None;
                    self.failures_in_window = 0;
                }
                SessionState::Connecting | SessionState::Reconnecting => {
                    self.expire();
                    return Ok(SessionAction::GraceExpired);
                }
                _ => {}
            }
        }
        if self.state == SessionState::Reconnecting
            && self.retry_at_ns.is_some_and(|retry| now_ns >= retry)
        {
            self.retry_at_ns = None;
            self.stats.retry_attempts = self.stats.retry_attempts.saturating_add(1);
            return Ok(SessionAction::Connect);
        }
        Ok(SessionAction::None)
    }
    /// Permission/OS interruption fences callbacks. Recording lifecycle is
    /// separate, since interruption might also require native file finalisation.
    pub fn interrupt(&mut self, generation: Generation) -> Result<Generation, SessionError> {
        self.check(generation)?;
        if !matches!(
            self.state,
            SessionState::Preparing
                | SessionState::Connecting
                | SessionState::Live
                | SessionState::Reconnecting
        ) {
            return Err(SessionError::InvalidTransition);
        }
        let generation = self.advance_generation()?;
        self.state = SessionState::Interrupted;
        self.retry_at_ns = None;
        self.transport_connected = false;
        Ok(generation)
    }
    /// Resumption requires native capture/permission revalidation and a new
    /// explicit start after ending/cancelling the interrupted session.
    pub fn stop(
        &mut self,
        generation: Generation,
    ) -> Result<(Generation, SessionAction), SessionError> {
        self.check(generation)?;
        if matches!(
            self.state,
            SessionState::Idle | SessionState::Stopped | SessionState::Stopping
        ) {
            return Err(SessionError::InvalidTransition);
        }
        let generation = self.advance_generation()?;
        self.state = SessionState::Stopping;
        self.retry_at_ns = None;
        self.deadline_ns = None;
        self.transport_connected = false;
        Ok((generation, SessionAction::EndDestination))
    }
    pub fn finish_stop(&mut self, generation: Generation) -> Result<(), SessionError> {
        self.check(generation)?;
        if self.state != SessionState::Stopping {
            return Err(SessionError::InvalidTransition);
        }
        self.state = SessionState::Stopped;
        Ok(())
    }
    /// Immediate local cancellation. The caller releases queue/transport/file
    /// resources and separately records whether destination end was delivered.
    pub fn cancel(&mut self, generation: Generation) -> Result<Generation, SessionError> {
        self.check(generation)?;
        let generation = self.advance_generation()?;
        self.state = SessionState::Stopped;
        self.retry_at_ns = None;
        self.deadline_ns = None;
        self.transport_connected = false;
        Ok(generation)
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;

    fn connecting() -> (Session, Generation) {
        let mut session = Session::new(ReconnectPolicy::default());
        let generation = session.start(0).expect("start");
        session.prepared(generation).expect("prepared");
        (session, generation)
    }
    fn live() -> (Session, Generation) {
        let (mut session, generation) = connecting();
        session.connected(generation, 1).expect("connected");
        session
            .media_confirmed(generation, 2)
            .expect("observed media");
        (session, generation)
    }

    #[test]
    fn healthy_rotation_after_long_show_does_not_create_an_outage() {
        let (mut session, generation) = live();
        let stats = session.stats();
        let rotation = 600_000_000_000;
        assert_eq!(
            session.replace_transport(generation, rotation),
            Ok(SessionAction::Connect)
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(session.state(), SessionState::Connecting);
        assert!(!session.transport_connected());
        assert_eq!(session.reconnect_deadline_ns(), None);
        assert_eq!(session.stats(), stats);
        assert_eq!(
            session.media_confirmed(generation, rotation + 1),
            Err(SessionError::InvalidTransition)
        );
        session
            .connected(generation, rotation + 2)
            .expect("new socket");
        assert_eq!(session.state(), SessionState::Connecting);
        session
            .media_confirmed(generation, rotation + 3)
            .expect("new playable media");
        assert_eq!(session.state(), SessionState::Live);
        let later_outage = rotation + 300_000_000_000;
        session
            .transport_failed(generation, later_outage)
            .expect("real failure");
        assert_eq!(
            session.reconnect_deadline_ns(),
            Some(later_outage + DEFAULT_RECONNECT_GRACE_NS)
        );
        assert_eq!(session.stats().transport_failures, 1);
    }

    #[test]
    fn unconfirmed_connecting_replacement_never_starts_a_failure_window() {
        for has_socket in [false, true] {
            let (mut session, generation) = connecting();
            if has_socket {
                session.connected(generation, 1).expect("socket only");
            }
            for now in [200_000_000_000, 400_000_000_000] {
                assert_eq!(
                    session.replace_transport(generation, now),
                    Ok(SessionAction::Connect)
                );
                assert_eq!(session.state(), SessionState::Connecting);
                assert!(!session.transport_connected());
                assert_eq!(session.reconnect_deadline_ns(), None);
                assert_eq!(session.stats().transport_failures, 0);
                assert_eq!(session.stats().retry_attempts, 0);
            }
            session
                .transport_failed(generation, 500_000_000_000)
                .expect("first actual failure");
            assert_eq!(session.reconnect_deadline_ns(), Some(590_000_000_000));
        }
    }

    #[test]
    fn rotation_during_outage_preserves_deadline_and_backoff_but_consumes_old_retry() {
        let (mut session, generation) = live();
        assert_eq!(
            session.transport_failed(generation, 10),
            Ok(SessionAction::RetryAt(500_000_010))
        );
        let deadline = 10 + DEFAULT_RECONNECT_GRACE_NS;
        let stats = session.stats();
        assert_eq!(
            session.replace_transport(generation, 20),
            Ok(SessionAction::Connect)
        );
        assert_eq!(session.state(), SessionState::Reconnecting);
        assert_eq!(session.reconnect_deadline_ns(), Some(deadline));
        assert_eq!(session.stats(), stats);
        assert_eq!(
            session.tick(generation, 500_000_010),
            Ok(SessionAction::None)
        );
        session
            .connected(generation, 500_000_020)
            .expect("replacement handshake");
        assert_eq!(session.state(), SessionState::Reconnecting);
        assert_eq!(
            session.replace_transport(generation, 1_000_000_000),
            Ok(SessionAction::Connect)
        );
        assert!(!session.transport_connected());
        // The next actual failure retains the prior outage's backoff history.
        assert_eq!(
            session.transport_failed(generation, 2_000_000_000),
            Ok(SessionAction::RetryAt(3_000_000_000))
        );
        assert_eq!(session.reconnect_deadline_ns(), Some(deadline));
        assert_eq!(session.stats().transport_failures, 2);
        assert_eq!(session.stats().retry_attempts, 0);
    }

    #[test]
    fn replacement_at_or_after_outage_expiry_never_rearms_or_connects() {
        for offset in [0, 1] {
            let (mut session, generation) = live();
            session.transport_failed(generation, 10).expect("failure");
            let deadline = session.reconnect_deadline_ns().expect("deadline");
            assert_eq!(
                session.replace_transport(generation, deadline + offset),
                Ok(SessionAction::GraceExpired)
            );
            assert_eq!(session.state(), SessionState::Failed);
            assert_eq!(session.reconnect_deadline_ns(), Some(deadline));
            assert!(!session.transport_connected());
            assert_eq!(session.stats().transport_failures, 1);
            assert_eq!(
                session.tick(generation, deadline + offset),
                Ok(SessionAction::None)
            );
            assert_eq!(
                session.replace_transport(generation, deadline + offset),
                Err(SessionError::InvalidTransition)
            );
        }
    }

    #[test]
    fn replacement_is_fenced_by_stop_cancel_and_nonongoing_states() {
        for cancel in [false, true] {
            let (mut session, generation) = live();
            let stopped = if cancel {
                session.cancel(generation).expect("cancel")
            } else {
                let (stopping, _) = session.stop(generation).expect("stop");
                assert_eq!(
                    session.replace_transport(stopping, 3),
                    Err(SessionError::InvalidTransition)
                );
                session.finish_stop(stopping).expect("stopped");
                stopping
            };
            assert_eq!(
                session.replace_transport(generation, 3),
                Err(SessionError::StaleGeneration)
            );
            assert_eq!(
                session.replace_transport(stopped, 3),
                Err(SessionError::InvalidTransition)
            );
            assert_eq!(session.state(), SessionState::Stopped);
            assert_eq!(session.reconnect_deadline_ns(), None);
        }
        let (mut session, generation) = live();
        let interrupted = session.interrupt(generation).expect("interrupt");
        assert_eq!(
            session.replace_transport(interrupted, 3),
            Err(SessionError::InvalidTransition)
        );
        let mut session = Session::new(ReconnectPolicy::default());
        let idle = session.generation();
        assert_eq!(
            session.replace_transport(idle, 0),
            Err(SessionError::InvalidTransition)
        );
        let preparing = session.start(0).expect("start");
        assert_eq!(
            session.replace_transport(preparing, 1),
            Err(SessionError::InvalidTransition)
        );
    }

    #[test]
    fn replacement_rejects_clock_rollback_without_mutating_current_attempt() {
        let (mut session, generation) = live();
        session.transport_failed(generation, 100).expect("failure");
        session.connected(generation, 200).expect("socket");
        let stats = session.stats();
        let deadline = session.reconnect_deadline_ns();
        assert_eq!(
            session.replace_transport(generation, 199),
            Err(SessionError::ClockWentBackwards)
        );
        assert_eq!(session.state(), SessionState::Reconnecting);
        assert!(session.transport_connected());
        assert_eq!(session.reconnect_deadline_ns(), deadline);
        assert_eq!(session.stats(), stats);
        assert_eq!(
            session.replace_transport(generation, 201),
            Ok(SessionAction::Connect)
        );
        assert!(!session.transport_connected());
    }

    #[test]
    fn zero_grace_still_allows_intentional_replacement_but_no_failure_retry() {
        let mut session = Session::new(ReconnectPolicy::new(0, 1, 2).expect("zero grace"));
        let generation = session.start(0).expect("start");
        session.prepared(generation).expect("prepared");
        assert_eq!(
            session.replace_transport(generation, 100),
            Ok(SessionAction::Connect)
        );
        assert_eq!(session.reconnect_deadline_ns(), None);
        assert_eq!(
            session.transport_failed(generation, 101),
            Ok(SessionAction::GraceExpired)
        );
        assert_eq!(session.state(), SessionState::Failed);
    }
}
