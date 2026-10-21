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
