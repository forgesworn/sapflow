//! Deterministic caller tests. Literal wire vectors below are constructed
//! directly from draft-sharabayko-srt-01, not another implementation.
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use sapflow_srt::packet::{
    Control, ControlPacket, Extension, Handshake, HandshakeKind, Packet, SRT_MAGIC, SrtHandshake,
    ext_flags, srt_flags,
};
use sapflow_srt::seq::Seq;
use sapflow_srt::{
    Caller, CallerCloseReason, CallerConfig, CallerEvent, Config, Event, Listener, SendError,
};

fn peer() -> SocketAddr {
    "127.0.0.1:9000".parse().expect("address")
}
fn source() -> SocketAddr {
    "127.0.0.1:9001".parse().expect("address")
}
fn time(now: Instant, us: u64) -> Instant {
    now + Duration::from_micros(us)
}
fn caller(config: CallerConfig, now: Instant) -> Caller {
    Caller::with_ids(config, peer(), 11, Seq::new(100), now).expect("config")
}
fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .flat_map(|word| {
            (0..word.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&word[i..i + 2], 16).expect("hex"))
        })
        .collect()
}
fn reply(kind: HandshakeKind) -> Handshake {
    Handshake {
        version: 5,
        encryption: 0,
        extension: if kind == HandshakeKind::Induction {
            SRT_MAGIC
        } else {
            ext_flags::HSREQ
        },
        initial_seq: 100,
        mtu: 1500,
        flow_window: 8192,
        kind,
        socket_id: 22,
        cookie: 0x12345678,
        peer_ip: [0; 16],
        extensions: if kind == HandshakeKind::Induction {
            vec![]
        } else {
            vec![Extension::HsRsp(SrtHandshake {
                version: 0x010500,
                flags: srt_flags::TSBPDRCV | srt_flags::CRYPT | srt_flags::REXMITFLG,
                receiver_delay: 120,
                sender_delay: 0,
            })]
        },
    }
}
fn control(caller: &mut Caller, now: Instant, control: Control) {
    let mut bytes = vec![];
    ControlPacket::new(0, 11, control).encode(&mut bytes);
    caller.handle_datagram(now, peer(), &bytes);
}
fn open(config: CallerConfig, now: Instant) -> Caller {
    let mut c = caller(config, now);
    c.poll_transmit(now).expect("induction");
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Induction)),
    );
    c.poll_transmit(now).expect("conclusion");
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Conclusion)),
    );
    assert!(c.is_connected());
    assert!(matches!(
        c.poll_event(),
        Some(CallerEvent::Connected { .. })
    ));
    c
}
fn data(bytes: &[u8]) -> sapflow_srt::packet::DataPacket<'_> {
    match Packet::parse(bytes).expect("packet") {
        Packet::Data(data) => data,
        _ => panic!("data expected"),
    }
}
fn reason(c: &mut Caller) -> CallerCloseReason {
    match c.poll_event().expect("closed") {
        CallerEvent::Closed { reason } => reason,
        _ => panic!("closed expected"),
    }
}

#[test]
fn independent_induction_and_conclusion_vectors() {
    let now = Instant::now();
    let config = CallerConfig {
        stream_id: "publish:x".into(),
        ..CallerConfig::default()
    };
    let mut c = caller(config, now);
    let (_, bytes) = c.poll_transmit(now).expect("induction");
    assert_eq!(
        bytes,
        hex("80000000 00000000 00000000 00000000
        00000004 00000002 00000064 000005dc 00001000 00000001
        0000000b 00000000 0100007f 00000000 00000000 00000000")
    );
    // Independent listener response: no helper from our packet encoder.
    let bytes = hex("80000000 00000000 00000000 0000000b
        00000005 00004a17 00000064 000005dc 00002000 00000001
        00000016 12345678 0100007f 00000000 00000000 00000000");
    c.handle_datagram(now, peer(), &bytes);
    let (_, bytes) = c.poll_transmit(now).expect("conclusion");
    assert_eq!(
        bytes,
        hex("80000000 00000000 00000000 00000016
        00000005 00000005 00000064 000005dc 00001000 ffffffff
        0000000b 12345678 0100007f 00000000 00000000 00000000
        00010003 00010500 00000025 00000078
        00050003 6c627570 3a687369 00000078")
    );
}

#[test]
fn handshake_retries_both_phases_and_absolute_deadline() {
    let now = Instant::now();
    let mut c = caller(CallerConfig::default(), now);
    assert!(c.poll_transmit(now).is_some());
    assert!(c.poll_transmit(time(now, 249_999)).is_none());
    assert!(c.poll_transmit(time(now, 250_000)).is_some());
    control(
        &mut c,
        time(now, 500_000),
        Control::Handshake(reply(HandshakeKind::Induction)),
    );
    assert!(c.poll_transmit(time(now, 500_000)).is_some());
    assert!(c.poll_transmit(time(now, 750_000)).is_some());
    c.handle_timeout(time(now, 5_000_000));
    assert_eq!(reason(&mut c), CallerCloseReason::ConnectTimeout);
    assert_eq!(c.poll_timeout(), None);
    assert!(c.poll_transmit(time(now, 5_000_000)).is_none());
    assert_eq!(c.stats().handshake_packets, 4);
}

#[test]
fn handshake_packet_budget_and_stop_are_terminal() {
    let now = Instant::now();
    let config = CallerConfig {
        max_handshake_packets: 1,
        ..CallerConfig::default()
    };
    let mut c = caller(config, now);
    assert!(c.poll_transmit(now).is_some());
    assert!(c.poll_transmit(time(now, 250_000)).is_none());
    assert_eq!(reason(&mut c), CallerCloseReason::HandshakeLimit);
    c.stop(now);
    assert_eq!(c.poll_event(), None);
    assert_eq!(c.enqueue(now, vec![1]), Err(SendError::NotConnected));
}

#[test]
fn rejects_unsupported_security_mode_version_and_extensions() {
    let now = Instant::now();
    for case in 0..8 {
        let mut c = caller(CallerConfig::default(), now);
        c.poll_transmit(now);
        control(
            &mut c,
            now,
            Control::Handshake(reply(HandshakeKind::Induction)),
        );
        c.poll_transmit(now);
        let mut h = reply(HandshakeKind::Conclusion);
        match case {
            0 => h.encryption = 2,
            1 => h.extensions.push(Extension::KmRsp(vec![0; 4])),
            2 => h.extensions.push(Extension::Filter(b"fec".to_vec())),
            3 => h.version = 4,
            4 => h.flow_window = 0,
            5 => h.extensions.clear(),
            6 => {
                if let Some(Extension::HsRsp(s)) = h.extensions.first_mut() {
                    s.flags |= srt_flags::STREAM;
                }
            }
            _ => h
                .extensions
                .push(h.extensions.first().expect("response").clone()),
        }
        control(&mut c, now, Control::Handshake(h));
        assert_eq!(
            reason(&mut c),
            CallerCloseReason::UnsupportedPeer,
            "case {case}"
        );
    }
}

#[test]
fn rejection_retains_peer_code() {
    let now = Instant::now();
    let mut c = caller(CallerConfig::default(), now);
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Rejection(2403))),
    );
    assert_eq!(reason(&mut c), CallerCloseReason::Rejected(2403));
}

#[test]
fn hostile_and_stray_packets_cannot_open_or_keep_session_alive() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    for bytes in [
        vec![],
        vec![0x80; 15],
        hex("80020000 00000001 00000000 0000000b 00000064 ff"),
    ] {
        c.handle_datagram(time(now, 4_000_000), peer(), &bytes);
    }
    let mut bytes = vec![];
    ControlPacket::new(0, 11, Control::KeepAlive).encode(&mut bytes);
    c.handle_datagram(time(now, 4_000_000), source(), &bytes);
    bytes[15] = 12;
    c.handle_datagram(time(now, 4_000_000), peer(), &bytes);
    c.handle_timeout(time(now, 5_000_000));
    assert_eq!(reason(&mut c), CallerCloseReason::PeerIdle);
    assert_eq!((c.stats().malformed, c.stats().stray), (3, 2));
}

#[test]
fn independent_data_ack_nak_vectors_and_original_timestamp() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    c.enqueue(time(now, 100), vec![1, 2, 3]).expect("queue");
    let (_, bytes) = c.poll_transmit(time(now, 200)).expect("data");
    assert_eq!(bytes, hex("00000064 c0000001 00000064 00000016 010203"));
    c.handle_datagram(
        time(now, 300),
        peer(),
        &hex("80030000 00000000 00000000 0000000b 00000064"),
    );
    let (_, bytes) = c.poll_transmit(time(now, 300)).expect("retry");
    assert_eq!(bytes, hex("00000064 c4000001 00000064 00000016 010203"));
    // Full ACK, ACK=101, ACK number 7, RTT 1000us, variance 500us.
    c.handle_datagram(
        time(now, 400),
        peer(),
        &hex("80020000 00000007 00000000 0000000b 00000065 000003e8 000001f4 00002000"),
    );
    let (_, bytes) = c.poll_transmit(time(now, 400)).expect("ACKACK");
    assert_eq!(bytes, hex("80060000 00000007 00000190 00000016"));
    assert_eq!(c.stats().send.acknowledged_packets, 1);
    assert_eq!(c.stats().send.buffered_packets, 0);
    assert_eq!(c.stats().send.buffered_bytes, 0);
    assert_eq!(c.stats().send.retransmitted_packets, 1);
}

#[test]
fn pacing_covers_retransmission_and_does_not_catch_up_in_a_burst() {
    let now = Instant::now();
    let mut config = CallerConfig::default();
    config.send.bandwidth = 1000;
    let mut c = open(config, now);
    for _ in 0..3 {
        c.enqueue(now, vec![1; 84]).expect("queue");
    }
    c.poll_transmit(now).expect("first");
    assert!(c.poll_transmit(now).is_none());
    assert!(c.poll_transmit(time(now, 99_999)).is_none());
    assert!(c.poll_transmit(time(now, 100_000)).is_some());
    control(
        &mut c,
        time(now, 100_001),
        Control::Nak(vec![(Seq::new(100), Seq::new(100))]),
    );
    assert!(c.poll_transmit(time(now, 100_001)).is_none());
    let (_, bytes) = c.poll_transmit(time(now, 200_000)).expect("paced retry");
    assert!(data(&bytes).retransmitted);
    assert!(c.poll_transmit(time(now, 200_000)).is_none());
    c.poll_transmit(time(now, 310_000)).expect("late next");
    assert!(c.poll_transmit(time(now, 310_000)).is_none());
}

#[test]
fn full_packet_and_byte_budgets_reject_without_growing() {
    let now = Instant::now();
    for packet_limit in [true, false] {
        let mut config = CallerConfig::default();
        config.send.max_packets = if packet_limit { 2 } else { 100 };
        config.send.max_bytes = if packet_limit { 1000 } else { 8 };
        let mut c = open(config, now);
        for _ in 0..2 {
            c.enqueue(now, vec![0; 4]).expect("queue");
        }
        c.poll_transmit(now);
        for _ in 0..1000 {
            assert_eq!(c.enqueue(now, vec![0; 4]), Err(SendError::QueueFull));
        }
        assert_eq!(c.stats().send.buffered_packets, 2);
        assert_eq!(c.stats().send.buffered_bytes, 8);
        c.stop(now);
        assert_eq!(c.stats().send.buffered_bytes, 0);
        assert_eq!(c.stats().send.abandoned_packets, 2);
        assert_eq!(reason(&mut c), CallerCloseReason::Cancelled);
        assert!(c.poll_timeout().is_none());
        let (_, bytes) = c.poll_transmit(now).expect("shutdown");
        assert!(matches!(
            Packet::parse(&bytes),
            Ok(Packet::Control(ControlPacket {
                body: Control::Shutdown,
                ..
            }))
        ));
        assert!(c.poll_transmit(now).is_none());
        c.stop(now);
        assert!(c.poll_event().is_none());
    }
}

#[test]
fn invalid_ack_and_huge_nak_cannot_release_or_expand_the_window() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    c.enqueue(now, vec![1; 10]).expect("queue");
    c.poll_transmit(now);
    c.handle_datagram(
        now,
        peer(),
        &hex("80020000 00000000 00000000 0000000b 00000066"),
    );
    c.handle_datagram(
        now,
        peer(),
        &hex("80030000 00000000 00000000 0000000b 80000000 3fffffff"),
    );
    assert_eq!(c.stats().send.buffered_packets, 1);
    assert_eq!(c.stats().send.invalid_controls, 2);
    assert!(c.poll_transmit(time(now, 100)).is_none());
    // Repeated, overlapping valid NAKs produce one outstanding retry.
    for _ in 0..1000 {
        control(
            &mut c,
            now,
            Control::Nak(vec![(Seq::new(0), Seq::new(100))]),
        );
    }
    let (_, bytes) = c.poll_transmit(time(now, 1000)).expect("one retry");
    assert!(data(&bytes).retransmitted);
    assert!(c.poll_transmit(time(now, 2000)).is_none());
}

#[test]
fn lost_ack_has_rto_recovery_then_bounded_retry_failure() {
    let now = Instant::now();
    let mut config = CallerConfig::default();
    config.send.max_retransmits = 1;
    let mut c = open(config, now);
    c.enqueue(now, vec![1]).expect("queue");
    c.poll_transmit(now);
    let (_, bytes) = c.poll_transmit(time(now, 320_000)).expect("RTO retry");
    assert!(data(&bytes).retransmitted);
    assert!(c.poll_transmit(time(now, 960_000)).is_none());
    assert_eq!(reason(&mut c), CallerCloseReason::RetryLimit);
    assert_eq!(c.stats().send.timeout_retransmissions, 2);
    assert_eq!(c.stats().send.buffered_packets, 0);
}

#[test]
fn media_age_budget_applies_to_pending_and_sent_data() {
    let now = Instant::now();
    for sent in [false, true] {
        let mut config = CallerConfig::default();
        config.send.max_age_us = 50_000;
        let mut c = open(config, now);
        c.enqueue(now, vec![1]).expect("queue");
        if sent {
            c.poll_transmit(now);
        }
        c.handle_timeout(time(now, 50_000));
        assert_eq!(reason(&mut c), CallerCloseReason::MediaExpired);
        assert_eq!(c.stats().send.abandoned_bytes, 1);
        assert_eq!(c.stats().send.buffered_bytes, 0);
    }
}

#[test]
fn coalesces_ackack_and_obeys_peer_receive_window() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    c.enqueue(now, vec![1]).expect("queue");
    for number in 1..1000 {
        control(
            &mut c,
            now,
            Control::Ack(sapflow_srt::packet::Ack {
                number,
                ack: Seq::new(100),
                stats: Some(sapflow_srt::packet::AckStats {
                    rtt: 1000,
                    rtt_var: 500,
                    buffer: 0,
                    rates: None,
                }),
            }),
        );
    }
    let (_, bytes) = c.poll_transmit(now).expect("coalesced ackack");
    assert!(matches!(
        Packet::parse(&bytes),
        Ok(Packet::Control(ControlPacket {
            body: Control::AckAck { number: 999 },
            ..
        }))
    ));
    assert!(c.poll_transmit(now).is_none(), "zero receive window");
    assert!(
        c.poll_timeout().expect("timer") > now,
        "no spin while blocked"
    );
}

#[test]
fn keepalive_shutdown_and_empty_invalid_payloads() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    assert_eq!(c.enqueue(now, vec![]), Err(SendError::Empty));
    assert_eq!(
        c.enqueue(now, vec![1; 1317]),
        Err(SendError::PayloadTooLarge)
    );
    let (_, bytes) = c.poll_transmit(time(now, 1_000_000)).expect("keepalive");
    assert!(matches!(
        Packet::parse(&bytes),
        Ok(Packet::Control(ControlPacket {
            body: Control::KeepAlive,
            ..
        }))
    ));
    control(&mut c, time(now, 1_100_000), Control::Shutdown);
    assert_eq!(reason(&mut c), CallerCloseReason::PeerClosed);
    assert!(c.poll_transmit(time(now, 1_100_000)).is_none());
}

#[test]
fn own_listener_round_trip_recovers_loss_reorder_and_duplicate() {
    let now = Instant::now();
    let mut c = caller(
        CallerConfig {
            stream_id: "publish:live/test".into(),
            ..CallerConfig::default()
        },
        now,
    );
    let mut listener = Listener::new(Config::default(), now);
    let mut accepted = false;
    let mut enqueued = false;
    let mut dropped = false;
    let mut held = None;
    let mut delivered = Vec::new();
    for us in (0..700_000).step_by(1000) {
        let at = time(now, us);
        c.handle_timeout(at);
        listener.handle_timeout(at);
        while let Some((_, bytes)) = c.poll_transmit(at) {
            if let Ok(Packet::Data(d)) = Packet::parse(&bytes) {
                if d.seq == Seq::new(101) && !d.retransmitted && !dropped {
                    dropped = true;
                    continue;
                }
                if d.seq == Seq::new(102) && !d.retransmitted {
                    held = Some(bytes);
                    continue;
                }
                listener.handle_datagram(at, source(), &bytes);
                listener.handle_datagram(at, source(), &bytes);
                if let Some(bytes) = held.take() {
                    listener.handle_datagram(at, source(), &bytes);
                }
            } else {
                listener.handle_datagram(at, source(), &bytes);
            }
        }
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect { id, stream_id, .. } => {
                    assert_eq!(stream_id, b"publish:live/test");
                    assert!(listener.accept(id, at));
                    accepted = true;
                }
                Event::Data { payload, .. } => delivered.push(payload),
                Event::Closed { .. } => panic!("unexpected close"),
            }
        }
        while let Some((_, bytes)) = listener.poll_transmit() {
            c.handle_datagram(at, peer(), &bytes);
        }
        if c.is_connected() && !enqueued {
            for i in 0..10 {
                c.enqueue(at, vec![i; 188]).expect("TS-sized payload");
            }
            enqueued = true;
        }
    }
    assert!(accepted && dropped);
    assert_eq!(delivered, (0..10).map(|i| vec![i; 188]).collect::<Vec<_>>());
    assert_eq!(c.stats().send.acknowledged_packets, 10);
    assert!(c.stats().send.retransmitted_packets >= 1);
    assert_eq!(c.stats().send.buffered_bytes, 0);
}

#[test]
fn sequence_numbers_wrap_and_stale_ack_cannot_reverse_progress() {
    let now = Instant::now();
    let initial = Seq::new(Seq::MAX);
    let mut c =
        Caller::with_ids(CallerConfig::default(), peer(), 11, initial, now).expect("config");
    for kind in [HandshakeKind::Induction, HandshakeKind::Conclusion] {
        c.poll_transmit(now);
        let mut h = reply(kind);
        h.initial_seq = initial.get();
        control(&mut c, now, Control::Handshake(h));
    }
    for _ in 0..2 {
        c.enqueue(now, vec![1]).expect("queue");
    }
    let (_, bytes) = c.poll_transmit(now).expect("first");
    assert_eq!(data(&bytes).seq, initial);
    let (_, bytes) = c.poll_transmit(time(now, 100)).expect("second");
    assert_eq!(data(&bytes).seq, Seq::new(0));
    c.handle_datagram(
        time(now, 200),
        peer(),
        &hex("80020000 00000000 00000000 0000000b 00000001"),
    );
    c.handle_datagram(
        time(now, 300),
        peer(),
        &hex("80020000 00000000 00000000 0000000b 7fffffff"),
    );
    assert_eq!(c.stats().send.acknowledged_packets, 2);
    assert_eq!(c.stats().send.invalid_controls, 1);
}

#[test]
fn observed_libsrt_echoed_induction_socket_targets_listener_zero() {
    let now = Instant::now();
    let mut c = caller(CallerConfig::default(), now);
    c.poll_transmit(now);
    let mut induction = reply(HandshakeKind::Induction);
    induction.socket_id = 11; // libsrt 1.5.4 echoes our ID on the wire.
    control(&mut c, now, Control::Handshake(induction));
    let (_, bytes) = c.poll_transmit(now).expect("conclusion");
    assert!(matches!(
        Packet::parse(&bytes),
        Ok(Packet::Control(ControlPacket {
            dest: 0,
            body: Control::Handshake(_),
            ..
        }))
    ));
}

#[test]
fn byte_budget_counts_owned_capacity_and_rejects_hidden_overallocation() {
    let now = Instant::now();
    let mut config = CallerConfig::default();
    config.send.max_bytes = 8;
    let mut c = open(config, now);
    let mut payload = Vec::with_capacity(1024);
    payload.push(1);
    assert_eq!(c.enqueue(now, payload), Err(SendError::QueueFull));
    assert_eq!(c.stats().send.buffered_bytes, 0);
    let mut payload = Vec::with_capacity(8);
    payload.push(1);
    c.enqueue(now, payload).expect("bounded allocation");
    assert_eq!(c.stats().send.buffered_bytes, 8);
    c.stop(now);
    assert_eq!(c.stats().send.abandoned_bytes, 1, "logical payload bytes");
    assert_eq!(c.stats().send.buffered_bytes, 0);
}

#[test]
fn malformed_nak_endpoints_ack_high_bit_and_reserved_subtypes_are_rejected() {
    let now = Instant::now();
    let mut c = open(CallerConfig::default(), now);
    for bytes in [
        "80030000 00000000 00000000 0000000b 80000064 80000065",
        "80030000 00000000 00000000 0000000b 80000064",
        "80020000 00000000 00000000 0000000b 80000064",
        "80020001 00000000 00000000 0000000b 00000064",
    ] {
        c.handle_datagram(now, peer(), &hex(bytes));
    }
    assert_eq!(c.stats().malformed, 4);
    assert_eq!(c.stats().send.invalid_controls, 0);
}

#[test]
fn configuration_bounds_and_cancel_before_connect() {
    let now = Instant::now();
    for case in 0..8 {
        let mut config = CallerConfig::default();
        match case {
            0 => config.stream_id = "é".repeat(257),
            1 => config.stream_id = "publish:\0lab".into(),
            2 => config.send.max_packets = 0,
            3 => config.send.max_bytes = usize::MAX,
            4 => config.send.bandwidth = 0,
            5 => config.send.max_payload = 1457,
            6 => config.handshake_retry = Duration::ZERO,
            _ => config.send.max_age_us = u64::MAX,
        }
        assert!(Caller::new(config, peer(), now).is_err(), "case {case}");
    }
    let mut c = caller(CallerConfig::default(), now);
    c.stop(now);
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Induction)),
    );
    assert_eq!(reason(&mut c), CallerCloseReason::Cancelled);
    assert!(c.poll_transmit(now).is_none());
    assert!(c.poll_timeout().is_none());
}

#[test]
fn negotiated_latency_reports_the_rounded_wire_request() {
    let now = Instant::now();
    let config = CallerConfig {
        latency: Duration::from_micros(120_500),
        ..CallerConfig::default()
    };
    let mut c = caller(config, now);
    c.poll_transmit(now);
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Induction)),
    );
    let (_, bytes) = c.poll_transmit(now).expect("conclusion");
    let Packet::Control(ControlPacket {
        body: Control::Handshake(h),
        ..
    }) = Packet::parse(&bytes).expect("handshake")
    else {
        panic!("handshake");
    };
    assert_eq!(h.srt_request().expect("request").sender_delay, 121);
    control(
        &mut c,
        now,
        Control::Handshake(reply(HandshakeKind::Conclusion)),
    );
    assert!(
        matches!(c.poll_event(),Some(CallerEvent::Connected {latency,..}) if latency==Duration::from_millis(121))
    );
}

#[test]
fn encrypted_session_selection_rotation_loss_and_no_plaintext_downgrade() {
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    for size in [KeySize::Aes128, KeySize::Aes256] {
        let now = Instant::now();
        let mut crypto = CryptoConfig::new(b"test scoped secret".to_vec(), size)
            .expect("valid encrypted test fixture");
        crypto.refresh_packets = 20;
        crypto.preannounce_packets = 8;
        let mut config = CallerConfig {
            crypto: Some(crypto.clone()),
            ..CallerConfig::default()
        };
        config.send.bandwidth = 10_000;
        let mut c = caller(config, now);
        let mut listener = Listener::new(
            Config {
                require_encryption: true,
                ..Config::default()
            },
            now,
        );
        let mut enqueued = false;
        let mut delivered = Vec::new();
        let mut dropped = false;
        let mut key_flags = 0;
        let mut id = None;
        let mut cipher_packet = None;
        for ms in 0..3000 {
            let at = now + Duration::from_millis(ms);
            c.handle_timeout(at);
            listener.handle_timeout(at);
            while let Some((_, bytes)) = c.poll_transmit(at) {
                if let Ok(Packet::Data(p)) = Packet::parse(&bytes) {
                    key_flags |= p.key;
                    assert_ne!(p.payload, &vec![(p.seq.get() - 100) as u8; 188]);
                    if p.seq.get() == 103 && !dropped {
                        dropped = true;
                        continue;
                    }
                    cipher_packet = Some(bytes.clone());
                }
                listener.handle_datagram(at, source(), &bytes);
            }
            while let Some(event) = listener.poll_event() {
                match event {
                    Event::Connect { id: conn, .. } => {
                        id = Some(conn);
                        assert!(listener.accept_with_crypto(conn, at, Some(crypto.clone())));
                    }
                    Event::Data { payload, .. } => delivered.push(payload),
                    Event::Closed { .. } => panic!("closed"),
                }
            }
            while let Some((_, bytes)) = listener.poll_transmit() {
                c.handle_datagram(at, peer(), &bytes);
            }
            if c.is_connected() && !enqueued {
                for n in 0..65 {
                    c.enqueue(at, vec![n; 188])
                        .expect("valid encrypted test fixture");
                }
                enqueued = true;
            }
        }
        assert_eq!(key_flags, 3);
        assert!(dropped);
        assert_eq!(delivered, (0..65).map(|n| vec![n; 188]).collect::<Vec<_>>());
        // Stripping key flags cannot turn encrypted bytes into application data.
        let mut stripped = cipher_packet.expect("valid encrypted test fixture");
        stripped[4] &= 0xe7;
        listener.handle_datagram(now + Duration::from_secs(3), source(), &stripped);
        assert!(
            listener
                .connection_stats(id.expect("valid encrypted test fixture"))
                .expect("valid encrypted test fixture")
                .encrypted
                > 0
        );
        c.stop(now + Duration::from_secs(3));
        assert_eq!(c.stats().send.buffered_bytes, 0);
    }
}

#[test]
fn encrypted_packet_count_refresh_bursts_and_lost_or_stale_responses() {
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    for size in [KeySize::Aes128, KeySize::Aes256] {
        for (packet_gap_us, lose_first_response) in
            [(1000, false), (100_000, false), (100_000, true)]
        {
            let now = Instant::now();
            let mut crypto = CryptoConfig::new(b"scoped rotation test secret".to_vec(), size)
                .expect("crypto config");
            crypto.refresh_packets = 32;
            crypto.preannounce_packets = 8;
            let mut config = CallerConfig {
                crypto: Some(crypto.clone()),
                latency: Duration::from_millis(1),
                ..CallerConfig::default()
            };
            config.send.bandwidth = 1_000_000;
            let mut c = caller(config, now);
            let mut listener = Listener::new(
                Config {
                    crypto: Some(crypto),
                    require_encryption: true,
                    latency: Duration::from_millis(1),
                    ..Config::default()
                },
                now,
            );
            let mut enqueued = 0;
            let mut next_payload_us = 0;
            let mut delivered = Vec::new();
            let mut requests: Vec<(u64, Vec<u8>)> = Vec::new();
            let mut responses = Vec::new();
            let mut key_flags = 0;
            let mut id = None;
            let mut lost = false;
            let mut stale_response: Option<Vec<u8>> = None;
            // Service timers every millisecond even in the slower control.
            for us in (0..20_000_000).step_by(1000) {
                let at = time(now, us);
                c.handle_timeout(at);
                listener.handle_timeout(at);
                if c.is_connected() && enqueued < 128 && us >= next_payload_us {
                    c.enqueue(at, vec![enqueued as u8; 188]).expect("queue");
                    enqueued += 1;
                    next_payload_us = us + packet_gap_us;
                }
                for _ in 0..8 {
                    while let Some((_, bytes)) = c.poll_transmit(at) {
                        match Packet::parse(&bytes).expect("own packet") {
                            Packet::Control(ControlPacket {
                                body:
                                    Control::Other {
                                        control_type: 0x7fff,
                                        subtype: 3,
                                        info: 0,
                                        cif,
                                    },
                                ..
                            }) => {
                                if lose_first_response
                                    && requests.last().is_some_and(|(_, old)| *old != cif)
                                    && let Some(stale) = &stale_response
                                {
                                    c.handle_datagram(at, peer(), stale);
                                }
                                requests.push((us, cif));
                            }
                            Packet::Data(p) => {
                                assert_ne!(p.key, 0, "no plaintext fallback");
                                key_flags |= p.key;
                            }
                            _ => {}
                        }
                        listener.handle_datagram(at, source(), &bytes);
                    }
                    while let Some(event) = listener.poll_event() {
                        match event {
                            Event::Connect { id: conn, .. } => {
                                id = Some(conn);
                                assert!(listener.accept(conn, at));
                            }
                            Event::Data { payload, .. } => delivered.push(payload),
                            Event::Closed { reason, .. } => panic!("closed: {reason:?}"),
                        }
                    }
                    while let Some((_, bytes)) = listener.poll_transmit() {
                        if let Packet::Control(ControlPacket {
                            body:
                                Control::Other {
                                    control_type: 0x7fff,
                                    subtype: 4,
                                    info: 0,
                                    cif,
                                },
                            ..
                        }) = Packet::parse(&bytes).expect("own packet")
                        {
                            responses.push(cif);
                            stale_response = Some(bytes.clone());
                            if lose_first_response && !lost {
                                lost = true;
                                continue;
                            }
                        }
                        c.handle_datagram(at, peer(), &bytes);
                    }
                    while let Some(event) = c.poll_event() {
                        assert!(matches!(event, CallerEvent::Connected { .. }), "{event:?}");
                    }
                }
                if delivered.len() == 128 && c.stats().send.acknowledged_packets == 128 {
                    break;
                }
            }
            assert_eq!(enqueued, 128);
            assert_eq!(
                delivered,
                (0..128).map(|n| vec![n; 188]).collect::<Vec<_>>()
            );
            assert_eq!(key_flags, 3, "both key slots used");
            assert!(requests.len() >= 4);
            assert_eq!(requests.len(), responses.len());
            assert!(
                responses
                    .iter()
                    .all(|response| requests.iter().any(|(_, request)| request == response))
            );
            if packet_gap_us == 1000 {
                assert_ne!(requests[0].1, requests[1].1);
                assert!(requests[1].0 - requests[0].0 < 100_000);
            }
            if lose_first_response {
                assert!(lost);
                assert_eq!(requests[0].1, requests[1].1, "lost KM response retried");
                assert!(requests[1].0 - requests[0].0 >= 100_000);
                assert!(
                    c.stats().send.invalid_controls > 0,
                    "stale responses ignored"
                );
            }
            assert_eq!(c.stats().send.encrypted_packets, 128);
            assert_eq!(c.stats().send.acknowledged_packets, 128);
            assert_eq!(c.stats().send.retransmitted_packets, 0);
            assert!(c.stats().send.km_responses >= 4);
            assert_eq!(c.stats().send.buffered_bytes, 0);
            let stats = listener
                .connection_stats(id.expect("connection"))
                .expect("open");
            assert_eq!(stats.decrypted, 128);
            assert_eq!(stats.km_rejected, 0);
            assert_eq!(stats.dropped, 0);
        }
    }
}

#[test]
fn missing_and_wrong_secrets_never_connect_or_deliver() {
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    for case in 0..3 {
        let now = Instant::now();
        let good = CryptoConfig::new(b"test scoped secret".to_vec(), KeySize::Aes128)
            .expect("valid encrypted test fixture");
        let bad = CryptoConfig::new(b"wrong scoped secret".to_vec(), KeySize::Aes128)
            .expect("valid encrypted test fixture");
        let mut c = caller(
            CallerConfig {
                crypto: (case != 0).then_some(good.clone()),
                ..CallerConfig::default()
            },
            now,
        );
        let mut listener = Listener::new(
            Config {
                crypto: Some(if case == 1 { bad } else { good }),
                require_encryption: true,
                ..Config::default()
            },
            now,
        );
        for ms in 0..1000 {
            let at = now + Duration::from_millis(ms);
            while let Some((_, bytes)) = c.poll_transmit(at) {
                listener.handle_datagram(at, source(), &bytes);
            }
            while let Some(e) = listener.poll_event() {
                if let Event::Connect { id, .. } = e {
                    assert!(!if case == 2 {
                        listener.accept_with_crypto(id, at, None)
                    } else {
                        listener.accept(id, at)
                    });
                } else {
                    panic!("media delivered");
                }
            }
            while let Some((_, bytes)) = listener.poll_transmit() {
                c.handle_datagram(at, peer(), &bytes);
            }
        }
        assert!(!c.is_connected());
        assert!(c.is_closed());
        assert_eq!(listener.connections(), 0);
    }
}

/// Connects one caller to one listener and sends three packets: whether it
/// connected and what the listener delivered.
fn session(
    caller_crypto: Option<sapflow_srt::crypto::CryptoConfig>,
    config: Config,
) -> (bool, Vec<Vec<u8>>) {
    let now = Instant::now();
    let mut c = caller(
        CallerConfig {
            crypto: caller_crypto,
            ..CallerConfig::default()
        },
        now,
    );
    let mut listener = Listener::new(config, now);
    let mut delivered = Vec::new();
    let mut enqueued = false;
    for ms in 0..1000 {
        let at = now + Duration::from_millis(ms);
        c.handle_timeout(at);
        listener.handle_timeout(at);
        while let Some((_, bytes)) = c.poll_transmit(at) {
            listener.handle_datagram(at, source(), &bytes);
        }
        while let Some(e) = listener.poll_event() {
            match e {
                Event::Connect { id, .. } => {
                    listener.accept(id, at);
                }
                Event::Data { payload, .. } => delivered.push(payload),
                Event::Closed { .. } => {}
            }
        }
        while let Some((_, bytes)) = listener.poll_transmit() {
            c.handle_datagram(at, peer(), &bytes);
        }
        if c.is_connected() && !enqueued {
            for n in 0..3 {
                c.enqueue(at, vec![n; 188]).expect("enqueue");
            }
            enqueued = true;
        }
    }
    (c.is_connected(), delivered)
}

#[test]
fn optional_encryption_admits_plaintext_and_matching_callers_only() {
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    let good = CryptoConfig::new(b"test shared secret".to_vec(), KeySize::Aes128).expect("good");
    let bad = CryptoConfig::new(b"wrong shared secret".to_vec(), KeySize::Aes128).expect("bad");
    let sent: Vec<Vec<u8>> = (0..3).map(|n| vec![n; 188]).collect();
    let optional = Config {
        crypto: Some(good.clone()),
        allow_plaintext: true,
        ..Config::default()
    };
    // Plaintext and a matching secret both connect and deliver.
    assert_eq!(session(None, optional.clone()), (true, sent.clone()));
    assert_eq!(session(Some(good.clone()), optional.clone()), (true, sent));
    // An encrypting caller is never downgraded: a wrong secret is refused.
    assert_eq!(session(Some(bad), optional.clone()), (false, vec![]));
    // Without the option, or when encryption is required, plaintext is refused.
    let enforced = Config {
        allow_plaintext: false,
        ..optional.clone()
    };
    assert_eq!(session(None, enforced), (false, vec![]));
    let required = Config {
        require_encryption: true,
        ..optional
    };
    assert_eq!(session(None, required), (false, vec![]));
}

#[test]
fn encrypted_caller_refuses_missing_wrong_or_duplicate_key_confirmation() {
    use sapflow_srt::crypto::{CryptoConfig, KeySize};
    for case in 0..3 {
        let now = Instant::now();
        let config = CallerConfig {
            crypto: Some(
                CryptoConfig::new(b"test scoped secret".to_vec(), KeySize::Aes128).expect("secret"),
            ),
            ..CallerConfig::default()
        };
        let mut c = caller(config, now);
        c.poll_transmit(now);
        control(
            &mut c,
            now,
            Control::Handshake(reply(HandshakeKind::Induction)),
        );
        c.poll_transmit(now);
        let mut response = reply(HandshakeKind::Conclusion);
        if case > 0 {
            response.extension |= ext_flags::KMREQ;
            response.extensions.push(Extension::KmRsp(vec![0; 56]));
        }
        if case == 2 {
            response.extensions.push(Extension::KmRsp(vec![0; 56]));
        }
        control(&mut c, now, Control::Handshake(response));
        assert!(!c.is_connected());
        assert_eq!(reason(&mut c), CallerCloseReason::Crypto);
    }
}

#[test]
fn try_enqueue_returns_identical_allocation_on_backpressure_and_closed_session() {
    let now = Instant::now();
    let mut config = CallerConfig::default();
    config.send.max_packets = 1;
    let mut c = open(config, now);
    c.enqueue(now, vec![1; 188]).expect("first");
    let mut payload = Vec::with_capacity(1316);
    payload.extend_from_slice(&[2; 188]);
    let pointer = payload.as_ptr();
    let capacity = payload.capacity();
    let (error, payload) = c.try_enqueue(now, payload).expect_err("full");
    assert_eq!(error, SendError::QueueFull);
    assert_eq!(payload.as_ptr(), pointer);
    assert_eq!(payload.capacity(), capacity);
    assert_eq!(payload, vec![2; 188]);
    c.stop(now);
    let (error, payload) = c.try_enqueue(now, payload).expect_err("closed");
    assert_eq!(error, SendError::NotConnected);
    assert_eq!(payload.as_ptr(), pointer);
    assert_eq!(payload.capacity(), capacity);
}
