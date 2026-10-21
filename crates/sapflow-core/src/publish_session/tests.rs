use super::*;

fn positive(value: u64) -> JsPositive {
    JsPositive::new(value).expect("positive safe integer")
}
fn time(value: u64) -> EpochMs {
    EpochMs::new(value).expect("bounded time")
}
fn digest(value: u8) -> Fingerprint {
    Fingerprint::new([value; 32])
}
fn session_key(event: &str, generation: u64, session: u64) -> SessionKey {
    SessionKey {
        path: StreamPath::parse(&format!("live/event-{event}")).expect("path"),
        event_generation: positive(generation),
        session_id: SessionId::parse(&format!("{session:032x}")).expect("opaque id"),
    }
}
fn provision(key: &SessionKey, revision: u64, expires: u64, fingerprint: u8) -> Provision {
    Provision {
        key: key.clone(),
        revision: positive(revision),
        request_fingerprint: digest(fingerprint),
        grant: GrantMetadata {
            expires_at_ms: time(expires),
            reconnect_grace_ms: 90_000,
            immutable_fingerprint: digest(17),
        },
    }
}
fn revoke(key: &SessionKey, revision: u64, fingerprint: u8) -> Command {
    Command::Revoke(Revoke {
        key: key.clone(),
        revision: positive(revision),
        request_fingerprint: digest(fingerprint),
    })
}
fn authority(capacity: usize) -> Authority {
    let mut authority = Authority::new(capacity).expect("capacity");
    authority.advance_clock(time(1000));
    authority
}
fn prepare(authority: &mut Authority, key: &SessionKey) {
    assert_eq!(
        authority
            .apply(Command::Provision(provision(key, 1, 10_000, 1)))
            .expect("prepare")
            .disposition,
        Disposition::Applied
    );
}
fn claim(authority: &mut Authority, key: &SessionKey) -> ConnectionLease {
    let lease = authority.reserve_claim(key).expect("reserve");
    // The adapter must persist this snapshot before commit. This test models
    // the acknowledgement, not disk durability, which belongs to the server.
    assert_eq!(
        authority.snapshot().rows[0].connection_epoch,
        lease.connection_epoch
    );
    authority.commit_claim(&lease).expect("commit");
    lease
}

#[test]
fn opaque_ids_integers_and_time_are_strictly_bounded() {
    for bad in [
        "",
        "0123456789abcdef",
        "0123456789abcdef0123456789abcdeF",
        "0123456789abcdef0123456789abcdefg",
        "éééééééééééééééé",
    ] {
        assert_eq!(SessionId::parse(bad), Err(Error::InvalidSessionId));
    }
    let valid = "0123456789abcdef0123456789abcdef";
    assert_eq!(SessionId::parse(valid).expect("valid").as_str(), valid);
    for invalid in [0, MAX_JS_INTEGER + 1, u64::MAX] {
        assert_eq!(JsPositive::new(invalid), Err(Error::InvalidInteger));
    }
    assert_eq!(positive(MAX_JS_INTEGER).get(), MAX_JS_INTEGER);
    assert_eq!(EpochMs::new(0), Err(Error::InvalidTimestamp));
    assert_eq!(EpochMs::new(MAX_EPOCH_MS + 1), Err(Error::InvalidTimestamp));
    assert_eq!(time(MAX_EPOCH_MS).get(), MAX_EPOCH_MS);
    assert_eq!(digest(167).as_bytes(), &[167; 32]);
    assert_eq!(format!("{:?}", digest(167)), "Fingerprint([REDACTED])");
}

#[test]
fn no_command_can_activate_before_a_valid_clock_observation() {
    let mut authority = Authority::new(1).expect("capacity");
    assert_eq!(
        authority.apply(Command::Provision(provision(
            &session_key("a", 1, 1),
            1,
            10_000,
            1
        ))),
        Err(Error::InvalidTimestamp)
    );
    assert!(authority.snapshot().rows.is_empty());
}

#[test]
fn retained_session_ids_cannot_alias_another_event_control_route() {
    let mut authority = authority(2);
    let first = session_key("a", 1, 1);
    prepare(&mut authority, &first);
    let alias = session_key("b", 1, 1);
    let before = authority.snapshot();
    for command in [
        Command::Provision(provision(&alias, 1, 10_000, 2)),
        Command::Refresh(provision(&alias, 2, 20_000, 3)),
        revoke(&alias, 3, 4),
    ] {
        assert_eq!(authority.apply(command), Err(Error::SessionConflict));
        assert_eq!(authority.snapshot(), before);
    }
    // A terminal row also retains its route identity.
    authority
        .apply(revoke(&first, 2, 5))
        .expect("terminal fence");
    assert_eq!(
        authority.apply(Command::Provision(provision(&alias, 4, 20_000, 6))),
        Err(Error::SessionConflict)
    );
}

#[test]
fn restore_rejects_ambiguous_session_ids_on_distinct_event_rows() {
    let mut original = authority(2);
    prepare(&mut original, &session_key("a", 1, 1));
    prepare(&mut original, &session_key("b", 1, 2));
    let mut snapshot = original.snapshot();
    snapshot.rows[1].session_id = snapshot.rows[0].session_id.clone();
    assert!(matches!(
        Authority::restore(snapshot, 2),
        Err(Error::InvalidSnapshot)
    ));
}

#[test]
fn current_revision_is_idempotent_only_for_the_same_request_and_metadata() {
    let mut authority = authority(1);
    let key = session_key("a", 2, 2);
    let packet = provision(&key, 5, 10_000, 1);
    authority
        .apply(Command::Provision(packet.clone()))
        .expect("prepare");
    let before = authority.snapshot();
    assert_eq!(
        authority
            .apply(Command::Provision(packet.clone()))
            .expect("duplicate")
            .disposition,
        Disposition::Idempotent
    );
    assert_eq!(authority.snapshot(), before);
    let mut changed = packet.clone();
    changed.request_fingerprint = digest(2);
    assert_eq!(
        authority.apply(Command::Provision(changed)),
        Err(Error::RequestConflict)
    );
    let mut changed = packet;
    changed.grant.expires_at_ms = time(20_000);
    assert_eq!(
        authority.apply(Command::Provision(changed)),
        Err(Error::RequestConflict)
    );
    assert_eq!(authority.snapshot(), before);
    assert_eq!(
        authority.apply(Command::Provision(provision(&key, 4, 10_000, 3))),
        Err(Error::StaleRevision)
    );
    assert_eq!(
        authority.apply(Command::Provision(provision(
            &session_key("a", 1, 1),
            9,
            10_000,
            4
        ))),
        Err(Error::StaleGeneration)
    );
    assert_eq!(
        authority.apply(Command::Provision(provision(
            &session_key("a", 2, 3),
            9,
            10_000,
            5
        ))),
        Err(Error::SessionConflict)
    );
    assert_eq!(
        authority.apply(Command::Provision(provision(
            &session_key("a", 3, 2),
            1,
            10_000,
            6
        ))),
        Err(Error::SessionConflict)
    );
}

#[test]
fn refresh_changes_only_expiry_and_revision_and_keeps_the_owner() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    prepare(&mut authority, &key);
    let lease = claim(&mut authority, &key);
    let packet = provision(&key, 2, 20_000, 2);
    let result = authority
        .apply(Command::Refresh(packet.clone()))
        .expect("refresh");
    assert_eq!(result.ended, None);
    assert!(authority.lease_active(&lease));
    assert_eq!(
        authority
            .row(&key.path)
            .expect("row")
            .grant
            .as_ref()
            .expect("grant")
            .reconnect_grace_ms,
        90_000
    );
    let before = authority.snapshot();
    let mut changed = provision(&key, 3, 30_000, 3);
    changed.grant.immutable_fingerprint = digest(18);
    assert_eq!(
        authority.apply(Command::Refresh(changed)),
        Err(Error::ConfigurationChanged)
    );
    let mut changed = provision(&key, 3, 30_000, 4);
    changed.grant.reconnect_grace_ms = 0;
    assert_eq!(
        authority.apply(Command::Refresh(changed)),
        Err(Error::ConfigurationChanged)
    );
    assert_eq!(
        authority.apply(Command::Refresh(provision(&key, 3, 19_999, 5))),
        Err(Error::ExpiryReduced)
    );
    assert_eq!(authority.snapshot(), before);
    // A fresh authoritative revision can reaffirm the same future deadline.
    authority
        .apply(Command::Refresh(provision(&key, 3, 20_000, 6)))
        .expect("same future expiry");
}

#[test]
fn refresh_cannot_create_or_replace_an_unknown_identity() {
    let mut authority = authority(1);
    let first = session_key("a", 1, 1);
    assert_eq!(
        authority.apply(Command::Refresh(provision(&first, 1, 10_000, 1))),
        Err(Error::UnknownSession)
    );
    prepare(&mut authority, &first);
    assert_eq!(
        authority.apply(Command::Refresh(provision(
            &session_key("a", 2, 2),
            1,
            10_000,
            2
        ))),
        Err(Error::UnknownSession)
    );
    assert!(authority.admission_enabled(&first));
}

#[test]
fn revoke_before_provision_is_a_permanent_bounded_tombstone() {
    let mut authority = authority(1);
    let key = session_key("a", 3, 3);
    let command = revoke(&key, 9, 9);
    assert_eq!(
        authority
            .apply(command.clone())
            .expect("revoke before prepare")
            .disposition,
        Disposition::Applied
    );
    let row = authority.row(&key.path).expect("tombstone");
    assert_eq!(row.status, SessionStatus::Revoked);
    assert!(row.grant.is_none());
    assert_eq!(
        authority
            .apply(command)
            .expect("duplicate revoke")
            .disposition,
        Disposition::Idempotent
    );
    assert_eq!(
        authority.apply(Command::Provision(provision(&key, 10, 20_000, 10))),
        Err(Error::Terminal)
    );
    assert_eq!(
        authority.apply(Command::Refresh(provision(&key, 11, 20_000, 11))),
        Err(Error::Terminal)
    );
    assert_eq!(
        authority.apply(Command::Provision(provision(
            &session_key("a", 2, 2),
            99,
            20_000,
            12
        ))),
        Err(Error::StaleGeneration)
    );
    authority.advance_clock(time(MAX_EPOCH_MS));
    assert_eq!(authority.snapshot().rows.len(), 1);
    assert_eq!(
        authority.row(&key.path).expect("retained").status,
        SessionStatus::Revoked
    );
}

#[test]
fn revoke_wins_against_refresh_in_either_serial_order() {
    for refresh_first in [true, false] {
        let mut authority = authority(1);
        let key = session_key("a", 1, 1);
        prepare(&mut authority, &key);
        let lease = claim(&mut authority, &key);
        if refresh_first {
            authority
                .apply(Command::Refresh(provision(&key, 2, 20_000, 2)))
                .expect("refresh");
        }
        let result = authority.apply(revoke(&key, 3, 3)).expect("revoke");
        assert_eq!(result.ended, Some(lease.clone()));
        assert!(!result.admission_enabled);
        assert!(!authority.lease_active(&lease));
        assert_eq!(
            authority.apply(Command::Refresh(provision(&key, 4, 30_000, 4))),
            Err(Error::Terminal)
        );
        assert!(!authority.release_claim(&lease));
    }
}

#[test]
fn expiry_is_terminal_at_the_deadline_and_rollback_cannot_extend_it() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    let packet = provision(&key, 1, 10_000, 1);
    authority
        .apply(Command::Provision(packet.clone()))
        .expect("prepare");
    let lease = claim(&mut authority, &key);
    assert!(authority.advance_clock(time(9999)).ended.is_empty());
    assert!(authority.lease_active(&lease));
    assert_eq!(
        authority.advance_clock(time(10_000)).ended,
        vec![lease.clone()]
    );
    let rollback = authority.advance_clock(time(1001));
    assert!(rollback.clamped);
    assert_eq!(rollback.effective_now, time(10_000));
    assert!(rollback.ended.is_empty());
    assert_eq!(
        authority.apply(Command::Refresh(provision(&key, 2, 30_000, 2))),
        Err(Error::Terminal)
    );
    let duplicate = authority
        .apply(Command::Provision(packet))
        .expect("historical duplicate");
    assert_eq!(duplicate.disposition, Disposition::Idempotent);
    assert!(!duplicate.admission_enabled);
    assert_eq!(
        authority.row(&key.path).expect("expired").status,
        SessionStatus::Expired
    );
    assert_eq!(authority.reserve_claim(&key), Err(Error::AdmissionDisabled));
}

#[test]
fn refresh_before_expiry_moves_the_single_deadline_without_changing_grace() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    prepare(&mut authority, &key);
    let lease = claim(&mut authority, &key);
    authority.advance_clock(time(9999));
    authority
        .apply(Command::Refresh(provision(&key, 2, 20_000, 2)))
        .expect("refresh");
    assert!(authority.advance_clock(time(10_000)).ended.is_empty());
    assert!(authority.lease_active(&lease));
    assert_eq!(authority.advance_clock(time(20_000)).ended, vec![lease]);
}

#[test]
fn capacity_never_evicts_replay_fences_even_for_revoke_before_prepare() {
    assert!(matches!(Authority::new(0), Err(Error::InvalidCapacity)));
    assert!(matches!(
        Authority::new(MAX_EVENTS + 1),
        Err(Error::InvalidCapacity)
    ));
    let mut authority = authority(1);
    let first = session_key("a", 1, 1);
    let other = session_key("b", 1, 2);
    authority.apply(revoke(&first, 1, 1)).expect("tombstone");
    let before = authority.snapshot();
    assert_eq!(
        authority.apply(Command::Provision(provision(&other, 1, 20_000, 2))),
        Err(Error::Capacity)
    );
    assert_eq!(authority.apply(revoke(&other, 1, 2)), Err(Error::Capacity));
    assert_eq!(authority.snapshot(), before);
    // Replacing the same event consumes no additional row, but needs a new generation and ID.
    let replacement = session_key("a", 2, 2);
    prepare(&mut authority, &replacement);
    assert_eq!(authority.snapshot().rows.len(), 1);
}

#[test]
fn snapshots_restore_metadata_but_never_restore_admission_or_reservations() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    let packet = provision(&key, 1, 10_000, 1);
    authority
        .apply(Command::Provision(packet.clone()))
        .expect("prepare");
    let reserved = authority.reserve_claim(&key).expect("reserve");
    assert!(!authority.lease_active(&reserved));
    let snapshot = authority.snapshot();
    let mut restored = Authority::restore(snapshot.clone(), 1).expect("restore");
    assert_eq!(restored.snapshot(), snapshot);
    assert!(!restored.admission_enabled(&key));
    let replay = restored
        .apply(Command::Provision(packet))
        .expect("idempotent old request");
    assert_eq!(replay.disposition, Disposition::Idempotent);
    assert!(!replay.admission_enabled);
    assert_eq!(
        restored.commit_claim(&reserved),
        Err(Error::AdmissionDisabled)
    );
    assert_eq!(restored.reserve_claim(&key), Err(Error::AdmissionDisabled));
    restored
        .apply(Command::Refresh(provision(&key, 2, 10_000, 2)))
        .expect("fresh authoritative revision");
    assert_eq!(restored.commit_claim(&reserved), Err(Error::StaleLease));
    let next = claim(&mut restored, &key);
    assert_eq!(next.connection_epoch, reserved.connection_epoch + 1);
    assert!(!restored.release_claim(&reserved));
    assert!(restored.lease_active(&next));
}

#[test]
fn restart_keeps_both_revoked_and_expired_sessions_terminal() {
    for revoked in [true, false] {
        let mut authority = authority(1);
        let key = session_key("a", 1, 1);
        prepare(&mut authority, &key);
        if revoked {
            authority.apply(revoke(&key, 2, 2)).expect("revoke");
        } else {
            authority.advance_clock(time(10_000));
        }
        let mut restored = Authority::restore(authority.snapshot(), 1).expect("restore");
        assert_eq!(
            restored.apply(Command::Refresh(provision(&key, 3, 20_000, 3))),
            Err(Error::Terminal)
        );
        assert!(!restored.admission_enabled(&key));
    }
}

#[test]
fn snapshot_corruption_is_rejected_without_partial_restore() {
    let mut authority = authority(2);
    prepare(&mut authority, &session_key("a", 1, 1));
    let valid = authority.snapshot();
    let mut invalids = Vec::new();
    let mut s = valid.clone();
    s.version = SNAPSHOT_VERSION + 1;
    invalids.push(s);
    let mut s = valid.clone();
    s.clock_floor_ms = MAX_EPOCH_MS + 1;
    invalids.push(s);
    let mut s = valid.clone();
    s.clock_floor_ms = 0;
    invalids.push(s);
    let mut s = valid.clone();
    s.rows.push(s.rows[0].clone());
    invalids.push(s);
    let mut s = valid.clone();
    s.rows[0].grant = None;
    invalids.push(s);
    let mut s = valid.clone();
    s.rows[0].status = SessionStatus::Expired;
    invalids.push(s);
    let mut s = valid.clone();
    s.clock_floor_ms = 10_000;
    invalids.push(s);
    let mut s = valid.clone();
    s.rows[0].connection_epoch = MAX_JS_INTEGER + 1;
    invalids.push(s);
    let mut s = valid.clone();
    s.rows[0].grant.as_mut().expect("grant").reconnect_grace_ms = MAX_RECONNECT_GRACE_MS + 1;
    invalids.push(s);
    for snapshot in invalids {
        assert!(Authority::restore(snapshot, 2).is_err());
    }
    assert_eq!(
        Authority::restore(valid.clone(), 2)
            .expect("valid snapshot")
            .snapshot(),
        valid
    );
    prepare(&mut authority, &session_key("b", 1, 2));
    assert!(matches!(
        Authority::restore(authority.snapshot(), 1),
        Err(Error::InvalidSnapshot)
    ));
    let empty = Authority::new(1).expect("empty").snapshot();
    assert_eq!(
        Authority::restore(empty.clone(), 1)
            .expect("empty restore")
            .snapshot(),
        empty
    );
}

#[test]
fn only_one_reservation_exists_and_abandoned_epochs_are_never_reused() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    prepare(&mut authority, &key);
    let first = authority.reserve_claim(&key).expect("reserve");
    assert!(!authority.lease_active(&first));
    assert_eq!(authority.reserve_claim(&key), Err(Error::AlreadyClaimed));
    assert!(authority.release_claim(&first));
    let second = claim(&mut authority, &key);
    assert_eq!(second.connection_epoch, first.connection_epoch + 1);
    assert_eq!(authority.commit_claim(&first), Err(Error::StaleLease));
    assert_eq!(
        authority.commit_claim(&second),
        Err(Error::ClaimNotReserved)
    );
    assert!(!authority.release_claim(&first));
    assert!(authority.lease_active(&second));
}

#[test]
fn failed_unpublished_draft_lease_cannot_release_a_reused_epoch() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    prepare(&mut authority, &key);
    let mut failed_draft = authority.clone();
    let stale = failed_draft
        .reserve_claim(&key)
        .expect("unpublished reservation");
    drop(failed_draft); // The server did not save or publish this draft.
    let current = authority
        .reserve_claim(&key)
        .expect("new draft reservation");
    assert_eq!(stale.connection_epoch, current.connection_epoch);
    assert_ne!(stale, current);
    assert_eq!(authority.commit_claim(&stale), Err(Error::StaleLease));
    assert!(!authority.release_claim(&stale));
    authority
        .commit_claim(&current)
        .expect("acknowledged saved reservation");
    assert!(authority.lease_active(&current));
}

#[test]
fn replacement_keeps_epoch_high_water_and_old_release_cannot_touch_it() {
    let mut authority = authority(1);
    let first = session_key("a", 1, 1);
    prepare(&mut authority, &first);
    let old = claim(&mut authority, &first);
    let second = session_key("a", 2, 2);
    let result = authority
        .apply(Command::Provision(provision(&second, 1, 20_000, 2)))
        .expect("replacement");
    assert_eq!(result.ended, Some(old.clone()));
    let active = claim(&mut authority, &second);
    assert_eq!(active.connection_epoch, old.connection_epoch + 1);
    assert_eq!(
        authority.apply(revoke(&first, 99, 3)),
        Err(Error::StaleGeneration)
    );
    assert!(!authority.release_claim(&old));
    assert!(authority.lease_active(&active));
    assert_eq!(
        authority.row(&second.path).expect("row").revision,
        positive(1)
    );
}

#[test]
fn revoke_newer_generation_retires_the_old_owner_without_a_grant() {
    let mut authority = authority(1);
    let first = session_key("a", 1, 1);
    prepare(&mut authority, &first);
    let old = claim(&mut authority, &first);
    let second = session_key("a", 2, 2);
    assert_eq!(
        authority
            .apply(revoke(&second, 1, 2))
            .expect("new generation tombstone")
            .ended,
        Some(old.clone())
    );
    let row = authority.row(&second.path).expect("row");
    assert!(row.grant.is_none());
    assert_eq!(row.connection_epoch, old.connection_epoch);
    assert!(!authority.release_claim(&old));
    assert_eq!(
        authority.apply(Command::Provision(provision(&second, 2, 20_000, 3))),
        Err(Error::Terminal)
    );
}

#[test]
fn expiry_or_revoke_between_reserve_and_commit_prevents_admission() {
    for revoked in [true, false] {
        let mut authority = authority(1);
        let key = session_key("a", 1, 1);
        prepare(&mut authority, &key);
        let reserved = authority.reserve_claim(&key).expect("reserve");
        if revoked {
            assert_eq!(
                authority.apply(revoke(&key, 2, 2)).expect("revoke").ended,
                Some(reserved.clone())
            );
        } else {
            assert_eq!(
                authority.advance_clock(time(10_000)).ended,
                vec![reserved.clone()]
            );
        }
        assert_eq!(
            authority.commit_claim(&reserved),
            Err(Error::AdmissionDisabled)
        );
        assert!(!authority.lease_active(&reserved));
    }
}

#[test]
fn epoch_exhaustion_fails_closed_and_grace_boundaries_are_explicit() {
    let mut authority = authority(1);
    let key = session_key("a", 1, 1);
    let mut packet = provision(&key, 1, 10_000, 1);
    packet.grant.reconnect_grace_ms = MAX_RECONNECT_GRACE_MS + 1;
    assert_eq!(
        authority.apply(Command::Provision(packet)),
        Err(Error::InvalidGrace)
    );
    let mut packet = provision(&key, 1, 10_000, 1);
    packet.grant.reconnect_grace_ms = 0;
    authority
        .apply(Command::Provision(packet))
        .expect("zero grace");
    let mut snapshot = authority.snapshot();
    snapshot.rows[0].connection_epoch = MAX_JS_INTEGER;
    let mut restored = Authority::restore(snapshot, 1).expect("valid exhausted high-water");
    let mut packet = provision(&key, 2, 20_000, 2);
    packet.grant.reconnect_grace_ms = 0;
    restored
        .apply(Command::Refresh(packet))
        .expect("fresh revision");
    assert_eq!(restored.reserve_claim(&key), Err(Error::EpochExhausted));
    assert_eq!(
        restored.row(&key.path).expect("row").connection_epoch,
        MAX_JS_INTEGER
    );
    let replacement = session_key("a", 2, 2);
    let mut packet = provision(&replacement, 1, 20_000, 3);
    packet.grant.reconnect_grace_ms = MAX_RECONNECT_GRACE_MS;
    restored
        .apply(Command::Provision(packet))
        .expect("maximum grace");
    assert_eq!(
        restored.reserve_claim(&replacement),
        Err(Error::EpochExhausted)
    );
}
