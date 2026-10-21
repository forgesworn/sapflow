//! Bounded wire/store parsing and stateful authority transitions. No I/O.
#![no_main]

#[cfg(not(feature = "fuzzing"))]
compile_error!("publish_session requires the fuzzing feature");

use libfuzzer_sys::fuzz_target;
use sapflow_core::StreamPath;
use sapflow_core::publish_session::{
    Authority, Command, ConnectionLease, EpochMs, Fingerprint, GrantMetadata, JsPositive,
    MAX_EPOCH_MS, MAX_JS_INTEGER, MAX_RECONNECT_GRACE_MS, Provision, Revoke, SessionId, SessionKey,
    SessionStatus, Snapshot,
};
use sha2::{Digest, Sha256};

// Compile the production parser modules, rather than a second fuzz-only parser.
// Their filesystem API is unused; only the feature-gated byte seam is called.
#[allow(dead_code)]
#[path = "../../crates/sapflow-server/src/publish_session_store.rs"]
mod publish_session_store;
#[allow(dead_code)]
#[path = "../../crates/sapflow-server/src/publish_session_wire.rs"]
mod publish_session_wire;

const CAP: usize = 8;
fn byte(input: &[u8], index: usize) -> u8 {
    input.get(index).copied().unwrap_or(0)
}
fn number(input: &[u8]) -> u64 {
    let mut value = [0; 8];
    for (destination, source) in value.iter_mut().zip(input) {
        *destination = *source;
    }
    u64::from_le_bytes(value)
}
fn identity(slot: u8, generation: u64, session: u64) -> Option<SessionKey> {
    Some(SessionKey {
        path: StreamPath::parse(&format!("live/event-fuzz-{}", slot % 16))?,
        event_generation: JsPositive::new(generation).ok()?,
        session_id: SessionId::parse(&format!("{session:032x}")).ok()?,
    })
}
fn verify_restore(snapshot: Snapshot) {
    if let Ok(restored) = Authority::restore(snapshot.clone(), CAP) {
        assert_eq!(restored.snapshot(), snapshot);
        for row in &snapshot.rows {
            assert!(!restored.admission_enabled(&row.key()));
        }
    }
}
fn verify_monotonic(before: &Snapshot, after: &Snapshot) {
    assert!(after.rows.len() <= CAP);
    assert!(after.clock_floor_ms >= before.clock_floor_ms);
    for old in &before.rows {
        let current = after.rows.iter().find(|row| row.path == old.path);
        assert!(current.is_some(), "authority must retain tombstones");
        if let Some(current) = current {
            assert!(current.event_generation >= old.event_generation);
            assert!(current.connection_epoch >= old.connection_epoch);
            if current.event_generation == old.event_generation {
                assert_eq!(current.session_id, old.session_id);
                assert!(current.revision >= old.revision);
                if old.status != SessionStatus::Prepared {
                    assert_ne!(current.status, SessionStatus::Prepared);
                }
            }
        }
    }
    let restored = Authority::restore(after.clone(), CAP);
    assert!(
        restored.is_ok(),
        "reachable authority snapshot must restore"
    );
    verify_restore(after.clone());
}

fn parser_paths(input: &[u8], authority: &mut Authority) {
    let _ = publish_session_store::validate_snapshot_bytes(input);
    if let Ok(value) = publish_session_wire::PutSession::parse(input) {
        let _ = value.validated(&value.session_id, 1000, 40_000_000);
        let _ = value.validated("invalid-route", 1000, 40_000_000);
    }
    if let Ok(value) = publish_session_wire::RevokeSession::parse(input) {
        let _ = value.validated(&value.session_id);
        let _ = value.validated("invalid-route");
    }

    // Valid JSON shaping reaches semantic validation without requiring the
    // mutator to first discover all field names or a store SHA-256 preimage.
    let session = format!("{:032x}", 1 + number(input) % 16);
    let value = serde_json::json!({
        "contractVersion": if byte(input, 0) & 1 == 0 {1} else {2},
        "eventId": "event-fuzz-0", "eventGeneration": 1 + u64::from(byte(input, 1) % 4),
        "sessionId": session.as_str(), "revision": 1 + u64::from(byte(input, 2)),
        "idempotencyKey": "cccccccccccccccccccccccccccccccc",
        "expiresAtUnixMs": 1001 + number(input) % 1_000_000,
        "reconnectGraceMs": number(input) % 4_000_001,
        "transport": if byte(input, 3) & 1 == 0 {"srt"} else {"rtmp"},
        "streamPath": "live/event-fuzz-0", "publishUser": session.as_str(),
        "publishPassword": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "srt": {"cipher": "aes256-ctr", "passphrase": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        "limits": {"maxPublishBps": 1 + number(input) % 50_000_000, "maxQueueBytes": 1500 + number(input) % 5_000_000}
    });
    if let Ok(bytes) = serde_json::to_vec(&value)
        && let Ok(value) = publish_session_wire::PutSession::parse(&bytes)
        && let Ok(provision) = value.validated(&session, 1000, 40_000_000)
    {
        let before = authority.snapshot();
        let _ = authority.apply(Command::Provision(provision));
        verify_monotonic(&before, &authority.snapshot());
    }
    let revoke = serde_json::json!({
        "contractVersion": 1, "eventId": "event-fuzz-0", "eventGeneration": 1 + u64::from(byte(input, 1) % 4),
        "sessionId": session.as_str(), "revision": 1 + u64::from(byte(input, 2)),
        "idempotencyKey": "dddddddddddddddddddddddddddddddd"
    });
    if let Ok(bytes) = serde_json::to_vec(&revoke)
        && let Ok(value) = publish_session_wire::RevokeSession::parse(&bytes)
        && let Ok(revoke) = value.validated(&session)
    {
        let before = authority.snapshot();
        let _ = authority.apply(Command::Revoke(revoke));
        verify_monotonic(&before, &authority.snapshot());
    }
}

fn mutated_restore(mut snapshot: Snapshot, input: &[u8]) {
    match byte(input, 1) % 10 {
        0 => snapshot.version = u32::from(byte(input, 2)),
        1 => snapshot.clock_floor_ms = 0,
        2 => snapshot.clock_floor_ms = MAX_EPOCH_MS + 1,
        8 | 9 => {
            if let Some(row) = snapshot.rows.first().cloned() {
                snapshot.rows.push(row);
            }
        }
        _ => {
            if let Some(row) = snapshot.rows.first_mut() {
                match byte(input, 1) % 10 {
                    3 => row.connection_epoch = MAX_JS_INTEGER + 1,
                    4 => row.status = SessionStatus::Expired,
                    5 => row.grant = None,
                    6 => {
                        if let Some(grant) = row.grant.as_mut() {
                            grant.reconnect_grace_ms = MAX_RECONNECT_GRACE_MS + 1;
                        }
                    }
                    7 => row.request_fingerprint = Fingerprint::new(Sha256::digest(input).into()),
                    _ => {}
                }
            }
        }
    }
    verify_restore(snapshot);
}

fuzz_target!(|input: &[u8]| {
    let Ok(mut authority) = Authority::new(CAP) else {
        return;
    };
    let Ok(initial_time) = EpochMs::new(1000) else {
        return;
    };
    authority.advance_clock(initial_time);
    // Four bounded initial rows make refresh, expiry, claims and tombstones
    // reachable immediately; arbitrary JSON is not needed for these paths.
    for slot in 0..4 {
        let Some(key) = identity(slot, 1, u64::from(slot) + 1) else {
            return;
        };
        let Ok(revision) = JsPositive::new(1) else {
            return;
        };
        let Ok(expiry) = EpochMs::new(100_000) else {
            return;
        };
        let _ = authority.apply(Command::Provision(Provision {
            key,
            revision,
            grant: GrantMetadata {
                expires_at_ms: expiry,
                reconnect_grace_ms: 90_000,
                immutable_fingerprint: Fingerprint::new([slot; 32]),
            },
            request_fingerprint: Fingerprint::new([slot + 1; 32]),
        }));
    }
    parser_paths(input, &mut authority);
    let mut leases: Vec<ConnectionLease> = Vec::with_capacity(CAP);
    for part in input.chunks(16).take(64) {
        let before = authority.snapshot();
        let slot = byte(part, 1) % 16;
        let Some(proposed) = identity(slot, 1 + u64::from(byte(part, 2) % 6), number(part) % 16)
        else {
            continue;
        };
        let old = authority.row(&proposed.path).cloned();
        let key = old
            .as_ref()
            .filter(|_| byte(part, 3) & 1 == 0)
            .map_or(proposed, |row| row.key());
        let revision_value =
            old.as_ref().map_or(1, |row| row.revision.get()) + u64::from(byte(part, 4) % 3);
        let Ok(revision) = JsPositive::new(revision_value) else {
            continue;
        };
        let Ok(expiry) = EpochMs::new(1 + number(part) % 300_000) else {
            continue;
        };
        let mut grant = old
            .as_ref()
            .and_then(|row| row.grant.clone())
            .unwrap_or(GrantMetadata {
                expires_at_ms: expiry,
                reconnect_grace_ms: 0,
                immutable_fingerprint: Fingerprint::new(Sha256::digest(part).into()),
            });
        if byte(part, 5) & 1 != 0 {
            grant.expires_at_ms = expiry;
        }
        if byte(part, 5) & 2 != 0 {
            grant.reconnect_grace_ms = u32::from(byte(part, 6)) * 20_000;
        }
        let fingerprint = if byte(part, 7) & 1 == 0 {
            old.as_ref()
                .map_or(Fingerprint::new([0; 32]), |row| row.request_fingerprint)
        } else {
            Fingerprint::new(Sha256::digest(part).into())
        };
        match byte(part, 0) % 8 {
            0 | 1 => {
                let provision = Provision {
                    key,
                    revision,
                    grant,
                    request_fingerprint: fingerprint,
                };
                let command = if byte(part, 0) % 8 == 0 {
                    Command::Provision(provision)
                } else {
                    Command::Refresh(provision)
                };
                let _ = authority.apply(command);
            }
            2 => {
                let _ = authority.apply(Command::Revoke(Revoke {
                    key,
                    revision,
                    request_fingerprint: fingerprint,
                }));
            }
            3 => {
                if let Ok(lease) = authority.reserve_claim(&key) {
                    if leases.len() == CAP {
                        leases.remove(0);
                    }
                    leases.push(lease);
                }
            }
            4 | 5 => {
                if let Some(lease) = leases.get(usize::from(byte(part, 2)) % leases.len().max(1)) {
                    if byte(part, 0) % 8 == 4 {
                        let _ = authority.commit_claim(lease);
                    } else {
                        let _ = authority.release_claim(lease);
                    }
                }
            }
            6 => {
                mutated_restore(authority.snapshot(), part);
                if let Ok(restored) = Authority::restore(authority.snapshot(), CAP) {
                    authority = restored;
                }
            }
            _ => {
                if let Ok(now) = EpochMs::new(1 + number(part) % 300_000) {
                    authority.advance_clock(now);
                }
            }
        }
        verify_monotonic(&before, &authority.snapshot());
    }
});
