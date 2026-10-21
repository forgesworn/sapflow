use super::*;
const SOURCE: TransportRecoverySource = TransportRecoverySource::SrtPeerAcknowledgedWatermark;
fn session() -> (Session, Generation) {
    let mut s =
        Session::new(ReconnectPolicy::new(1000, 5, 20).expect("valid recovery fixture operation"));
    let g = s.start(0).expect("valid recovery fixture operation");
    assert_eq!(s.prepared(g), Ok(SessionAction::Connect));
    assert_eq!(s.attempt_epoch(), 1);
    (s, g)
}
fn retry_sending() -> (Session, Generation, u64) {
    let (mut s, g) = session();
    assert_eq!(s.transport_failed(g, 10), Ok(SessionAction::RetryAt(15)));
    assert_eq!(s.tick(g, 15), Ok(SessionAction::Connect));
    let attempt = s.attempt_epoch();
    s.connected(g, 16)
        .expect("valid recovery fixture operation");
    assert_eq!(s.sending(g, attempt, 17), Ok(SessionAction::None));
    (s, g, attempt)
}
#[test]
fn peer_media_progress_recovers_network_only_and_each_later_outage_gets_fresh_window() {
    let (mut s, g, attempt) = retry_sending();
    assert_eq!(s.outage_deadline_ns(), Some(1010));
    assert_eq!(
        s.transport_recovered(g, attempt, 18, SOURCE),
        Ok(SessionAction::None)
    );
    assert_eq!(s.state(), SessionState::Sending);
    assert_eq!(s.media_readiness(), MediaReadiness::Unknown);
    assert_eq!(s.retry_budget(), RetryBudgetState::Available);
    assert_eq!(s.next_deadline_ns(), None);
    assert_eq!(s.stats().network_recoveries, 1);
    s.transport_recovered(g, attempt, 19, SOURCE)
        .expect("valid recovery fixture operation");
    assert_eq!(s.stats().network_recoveries, 1); // duplicate proof is idempotent
    s.transport_failed(g, 2000)
        .expect("valid recovery fixture operation");
    assert_eq!(s.outage_deadline_ns(), Some(3000));
    assert_eq!(s.tick(g, 2005), Ok(SessionAction::Connect));
    let next = s.attempt_epoch();
    assert!(next > attempt);
    s.connected(g, 2006)
        .expect("valid recovery fixture operation");
    s.sending(g, next, 2007)
        .expect("valid recovery fixture operation");
    assert_eq!(
        s.transport_recovered(g, attempt, 2008, SOURCE),
        Err(SessionError::StaleAttempt)
    );
    assert_eq!(s.outage_deadline_ns(), Some(3000));
    s.transport_recovered(g, next, 2009, SOURCE)
        .expect("valid recovery fixture operation");
    assert_eq!(s.stats().network_recoveries, 2);
    assert_eq!(s.stats().media_confirmations, 0);
    s.media_confirmed(g, 2010)
        .expect("valid recovery fixture operation");
    assert_eq!(s.state(), SessionState::Live);
    assert_eq!(s.media_readiness(), MediaReadiness::Confirmed);
}
#[test]
fn optional_ack_sender_survives_original_deadline_but_never_rearms_from_late_proof() {
    let (mut s, g, attempt) = retry_sending();
    assert_eq!(s.tick(g, 1010), Ok(SessionAction::RetryBudgetExhausted));
    assert!(s.transport_connected());
    assert_eq!(s.state(), SessionState::Sending);
    assert_eq!(s.retry_budget(), RetryBudgetState::Exhausted);
    assert_eq!(s.outage_deadline_ns(), Some(1010));
    assert_eq!(s.next_deadline_ns(), None);
    assert_eq!(s.reconnect_deadline_ns(), None);
    for now in [1011, 1_000_000] {
        assert_eq!(s.tick(g, now), Ok(SessionAction::None));
    }
    assert_eq!(
        s.transport_recovered(g, attempt, 1_000_001, SOURCE),
        Err(SessionError::InvalidTransition)
    );
    // A genuine late decoder can report playback, but cannot restore retry authority.
    s.media_confirmed(g, 1_000_002)
        .expect("valid recovery fixture operation");
    assert_eq!(s.state(), SessionState::Live);
    assert_eq!(s.retry_budget(), RetryBudgetState::Exhausted);
    assert_eq!(
        s.transport_failed(g, 1_000_003),
        Ok(SessionAction::GraceExpired)
    );
    assert_eq!(s.state(), SessionState::Failed);
    assert_eq!(s.outage_deadline_ns(), Some(1010));
}
#[test]
fn exact_deadline_proof_exhausts_sending_without_renewal_and_replace_is_terminal() {
    let (mut s, g, attempt) = retry_sending();
    assert_eq!(
        s.transport_recovered(g, attempt, 1010, SOURCE),
        Ok(SessionAction::RetryBudgetExhausted)
    );
    assert_eq!(s.stats().network_recoveries, 0);
    assert_eq!(
        s.replace_transport(g, 1011),
        Ok(SessionAction::GraceExpired)
    );
    assert!(!s.transport_connected());
}
#[test]
fn handshake_or_clock_rollback_cannot_establish_recovery() {
    let (mut s, g) = session();
    s.transport_failed(g, 10)
        .expect("valid recovery fixture operation");
    s.tick(g, 15).expect("valid recovery fixture operation");
    let attempt = s.attempt_epoch();
    s.connected(g, 16)
        .expect("valid recovery fixture operation");
    assert_eq!(
        s.transport_recovered(g, attempt, 17, SOURCE),
        Err(SessionError::InvalidTransition)
    );
    assert_eq!(
        s.sending(g, attempt, 15),
        Err(SessionError::ClockWentBackwards)
    );
    assert_eq!(s.tick(g, 1010), Ok(SessionAction::GraceExpired));
    assert_eq!(s.state(), SessionState::Failed);
    assert_eq!(s.retry_budget(), RetryBudgetState::Exhausted);
    assert_eq!(s.next_deadline_ns(), None);
    assert_eq!(s.outage_deadline_ns(), Some(1010));
}
#[test]
fn healthy_and_outage_rotation_fence_attempts_without_starting_or_extending_window() {
    let (mut s, g) = session();
    s.connected(g, 1).expect("valid recovery fixture operation");
    s.sending(g, 1, 2)
        .expect("valid recovery fixture operation");
    assert_eq!(s.replace_transport(g, 100_000), Ok(SessionAction::Connect));
    assert_eq!(s.outage_deadline_ns(), None);
    assert_eq!(s.sending(g, 1, 100_001), Err(SessionError::StaleAttempt));
    s.transport_failed(g, 100_002)
        .expect("valid recovery fixture operation");
    let deadline = s.outage_deadline_ns();
    s.replace_transport(g, 100_003)
        .expect("valid recovery fixture operation");
    assert_eq!(s.outage_deadline_ns(), deadline);
    let current = s.attempt_epoch();
    s.connected(g, 100_004)
        .expect("valid recovery fixture operation");
    s.sending(g, current, 100_005)
        .expect("valid recovery fixture operation");
    let stopped = s.cancel(g).expect("valid recovery fixture operation");
    assert_eq!(
        s.transport_recovered(g, current, 100_006, SOURCE),
        Err(SessionError::StaleGeneration)
    );
    assert_eq!(
        s.sending(stopped, current, 100_006),
        Err(SessionError::InvalidTransition)
    );
    assert_eq!(s.state(), SessionState::Stopped);
}
#[test]
fn zero_grace_never_opens_a_retry_window_even_after_submitted_media() {
    let mut s =
        Session::new(ReconnectPolicy::new(0, 1, 2).expect("valid recovery fixture operation"));
    let g = s.start(0).expect("valid recovery fixture operation");
    s.prepared(g).expect("valid recovery fixture operation");
    s.connected(g, 1).expect("valid recovery fixture operation");
    s.sending(g, s.attempt_epoch(), 2)
        .expect("valid recovery fixture operation");
    assert_eq!(s.transport_failed(g, 3), Ok(SessionAction::GraceExpired));
    assert_eq!(s.tick(g, 4), Ok(SessionAction::None));
}
