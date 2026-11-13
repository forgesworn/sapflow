//! Our SRT listener with `Config::app_keys`, as a server that holds one
//! passphrase per stream drives it: datagrams from a few peers, time steps,
//! and for each caller a key check with the application's passphrase and a
//! strict accept. Records and client rewriting are those of
//! `own_srt_listener`; seeded with encrypting callers whose key material
//! matches `APP` (`seeds/own_srt_app_keys`, recorded by
//! `crates/sapflow-srt/tests/app_keys.rs`), so checks succeed and
//! encrypted data reaches open connections.
//!
//! Each record: control byte, time step (x 1 ms), length (2 bytes), datagram.
//! Control bits: 0x80 time passes only; 0x40 send the datagram as it is;
//! 0x20 refuse the next caller; 0x10 also check it with a wrong passphrase;
//! 0x08 answer with plain `accept` instead of `accept_with_key`; 0x04 check
//! with AES-128 as the application's key length; the low two bits pick the
//! peer. 0x1C closes the peer's connection.
//!
//! Mode: the listener has no global secret, or, when the first record's
//! peer bits are 3 (peer 0 either way, so the seeds keep their meaning), a
//! global secret with `allow_plaintext`, as the reference deployment runs
//! while it moves to app keys.
//!
//! Checks: `check_key` changes nothing and gives the same answer twice; a
//! wrong passphrase never passes; `accept_with_key` accepts exactly when the
//! check passed and otherwise refuses with the check's code; plain `accept`
//! admits exactly what the listener's own settings allow: plaintext, and in
//! the second mode key material wrapped under the global secret at its key
//! length, never key material without a global secret. The accessors a
//! replay guard reads (`pending_key_material`, `bound_key_material`,
//! `caller_socket`) agree with the check and with the caller's state.
#![no_main]
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use sapflow_srt::crypto::{CryptoConfig, KeySize, bound_key_material};
use sapflow_srt::packet::{Control, ControlPacket, Extension, HandshakeKind, Packet};
use sapflow_srt::{Config, ConnId, Event, KeyCheckError, Listener, RejectCode};

type Fixed = BuildHasherDefault<DefaultHasher>;

/// The application's passphrase; the seeds' callers use it too.
const APP: &[u8] = b"own_srt_app_keys fuzz passphrase";
const WRONG: &[u8] = b"own_srt_app_keys wrong passphrase";
/// The listener's own secret in the second mode.
const GLOBAL: &[u8] = b"own_srt_app_keys global passphrase";

/// Rewrites a client datagram the way a real client fills it in.
fn as_client(datagram: &[u8], cookie: Option<u32>, socket: Option<u32>) -> Vec<u8> {
    let mut d = datagram.to_vec();
    let control = d.first().is_some_and(|b| b & 0x80 != 0);
    let handshake = control && d.get(..2) == Some(&[0x80, 0x00]);
    if handshake {
        let kind = d.get(36..40).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        if kind == Some(HandshakeKind::Conclusion.to_u32())
            && let (Some(c), Some(field)) = (cookie, d.get_mut(44..48))
        {
            field.copy_from_slice(&c.to_be_bytes());
        }
    } else if let (Some(s), Some(field)) = (socket, d.get_mut(12..16)) {
        field.copy_from_slice(&s.to_be_bytes());
    }
    d
}

/// Sends what the listener queued, noting induction cookies; returns the
/// rejection codes among it.
fn drain(listener: &mut Listener<Fixed>, cookies: &mut HashMap<SocketAddr, u32>) -> Vec<u32> {
    let mut rejections = Vec::new();
    while let Some((to, out)) = listener.poll_transmit() {
        let p = Packet::parse(&out).expect("what we send parses");
        if let Packet::Control(ControlPacket {
            body: Control::Handshake(h),
            ..
        }) = p
        {
            match h.kind {
                HandshakeKind::Induction => {
                    cookies.insert(to, h.cookie);
                }
                HandshakeKind::Rejection(code) => rejections.push(code),
                HandshakeKind::Conclusion => {
                    // An accepted encrypted caller gets its key back.
                    let km = h.extensions.iter().any(|e| matches!(e, Extension::KmRsp(_)));
                    assert_eq!(km, h.encryption != 0, "{h:?}");
                }
                _ => {}
            }
        }
    }
    rejections
}

fuzz_target!(|data: &[u8]| {
    let start = Instant::now();
    let peers: [SocketAddr; 3] = [
        "192.0.2.1:5000".parse().expect("addr"),
        "192.0.2.2:5000".parse().expect("addr"),
        "[2001:db8::1]:5000".parse().expect("addr"),
    ];
    let global = CryptoConfig::new(GLOBAL.to_vec(), KeySize::Aes128).expect("passphrase");
    let optional = data.first().is_some_and(|c| c & 3 == 3);
    let config = Config {
        max_connections: 4,
        buffer_packets: 256,
        app_keys: true,
        crypto: optional.then(|| global.clone()),
        allow_plaintext: optional,
        ..Config::default()
    };
    let app256 = CryptoConfig::new(APP.to_vec(), KeySize::Aes256).expect("passphrase");
    let app128 = CryptoConfig::new(APP.to_vec(), KeySize::Aes128).expect("passphrase");
    let wrong = CryptoConfig::new(WRONG.to_vec(), KeySize::Aes256).expect("passphrase");
    let mut listener = Listener::with_key(config, start, Fixed::default());
    let mut cookies: HashMap<SocketAddr, u32> = HashMap::new();
    let mut sockets: HashMap<SocketAddr, u32> = HashMap::new();
    let mut now = start;
    let mut rest = data;
    while rest.len() >= 4 {
        let (ctl, step) = (rest[0], rest[1]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]])).min(rest.len() - 4);
        let datagram = &rest[4..4 + len];
        rest = &rest[4 + len..];
        now += Duration::from_millis(u64::from(step));
        let peer = peers[usize::from(ctl & 3) % 3];
        if ctl & 0x80 != 0 {
            listener.handle_timeout(now);
        } else if ctl & 0x40 != 0 {
            listener.handle_datagram(now, peer, datagram);
        } else {
            let d = as_client(datagram, cookies.get(&peer).copied(), sockets.get(&peer).copied());
            listener.handle_datagram(now, peer, &d);
        }
        if let Some(t) = listener.poll_timeout()
            && t <= now
        {
            listener.handle_timeout(now);
            assert!(listener.poll_timeout().is_none_or(|t| t > now), "timer in the past");
        }
        while let Some(ev) = listener.poll_event() {
            match ev {
                Event::Connect { id, peer, .. } => {
                    drain(&mut listener, &mut cookies);
                    let app = if ctl & 0x04 != 0 { &app128 } else { &app256 };
                    let open = listener.connections();
                    let check = listener.check_key(id, app);
                    assert_ne!(check, Err(KeyCheckError::NotPending));
                    assert_eq!(listener.check_key(id, app), check, "idempotent");
                    // What a replay guard reads agrees with the check.
                    let km = listener.pending_key_material(id);
                    assert_eq!(km.is_some(), check != Err(KeyCheckError::NoKeyMaterial));
                    assert_eq!(listener.sent_key_material(id), Some(km.is_some()));
                    if check.is_ok() {
                        assert!(km.and_then(bound_key_material).is_some(), "bound part");
                    }
                    assert!(listener.caller_socket(id).is_some(), "waiting");
                    if ctl & 0x10 != 0 {
                        let w = listener.check_key(id, &wrong);
                        assert!(w.is_err(), "a wrong passphrase passed");
                        if check.is_ok() {
                            assert_eq!(w, Err(KeyCheckError::BadSecret));
                        }
                    }
                    assert_eq!(listener.connections(), open, "a check changed nothing");
                    assert_eq!(listener.connection_stats(id), None, "still waiting");
                    assert!(drain(&mut listener, &mut cookies).is_empty(), "nothing answered");
                    if ctl & 0x20 != 0 {
                        listener.reject(id, RejectCode::http(401));
                        assert_eq!(listener.caller_socket(id), None, "refused");
                    } else if ctl & 0x08 != 0 {
                        // What the listener's own settings admit: plaintext,
                        // and key material only under the global secret at
                        // its own key length; never a downgrade.
                        let by_global = optional
                            && listener.check_key(id, &global) == Ok(global.key_size);
                        let accepted = listener.accept(id, now);
                        assert_eq!(
                            accepted,
                            check == Err(KeyCheckError::NoKeyMaterial) || by_global,
                            "{check:?}"
                        );
                        if accepted {
                            sockets.insert(peer, id.0);
                        } else {
                            let code = if optional {
                                RejectCode::BADSECRET
                            } else {
                                RejectCode::UNSECURE
                            };
                            let codes = drain(&mut listener, &mut cookies);
                            assert_eq!(codes, [code.0]);
                        }
                    } else {
                        let accepted = listener.accept_with_key(id, now, app);
                        assert_eq!(accepted, check.is_ok(), "{check:?}");
                        assert_eq!(listener.caller_socket(id).is_some(), accepted);
                        if accepted {
                            sockets.insert(peer, id.0);
                            assert_eq!(listener.check_key(id, app), Err(KeyCheckError::NotPending));
                        } else {
                            let code = check.err().and_then(KeyCheckError::reject_code);
                            let codes = drain(&mut listener, &mut cookies);
                            assert_eq!(codes, code.map(|c| c.0).into_iter().collect::<Vec<_>>());
                        }
                    }
                }
                Event::Data { .. } => {}
                Event::Closed { id, .. } => {
                    sockets.retain(|_, s| *s != id.0);
                }
            }
        }
        drain(&mut listener, &mut cookies);
        assert_eq!(
            listener.poll_timeout().is_some(),
            listener.connections() > 0
        );
        if ctl & 0x1C == 0x1C
            && let Some(&s) = sockets.get(&peer)
        {
            listener.close(ConnId(s), now);
            sockets.remove(&peer);
        }
    }
});
