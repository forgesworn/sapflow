//! Adapter regression tests use a real nonblocking UDP queue and virtual time.
use super::*;
use sapflow_srt::packet::{Control, ControlPacket};
use sapflow_srt::seq::Seq;
use sapflow_srt::{Caller, CallerConfig};
use std::time::Duration;

fn open(listener: &mut Listener, source: SocketAddr, socket_id: u32, now: Instant) -> ConnId {
    let server = SocketAddr::from(([127, 0, 0, 1], 9000));
    let mut caller = Caller::with_ids(
        CallerConfig::default(),
        server,
        socket_id,
        Seq::new(100),
        now,
    )
    .expect("caller");
    let mut accepted = None;
    for _ in 0..8 {
        while let Some((_, bytes)) = caller.poll_transmit(now) {
            listener.handle_datagram(now, source, &bytes);
        }
        while let Some(event) = listener.poll_event() {
            if let Event::Connect { id, .. } = event {
                assert!(listener.accept(id, now));
                accepted = Some(id);
            }
        }
        while let Some((_, bytes)) = listener.poll_transmit() {
            caller.handle_datagram(now, server, &bytes);
        }
        if caller.is_connected() {
            return accepted.expect("accepted");
        }
    }
    panic!("handshake did not finish");
}
fn keepalive(id: ConnId) -> Vec<u8> {
    let mut bytes = Vec::new();
    ControlPacket::new(0, id.0, Control::KeepAlive).encode(&mut bytes);
    bytes
}
fn closed(listener: &mut Listener) -> Vec<ConnId> {
    std::iter::from_fn(|| listener.poll_event())
        .filter_map(|event| match event {
            Event::Closed {
                id,
                reason: Some(CloseReason::Timeout),
                ..
            } => Some(id),
            _ => None,
        })
        .collect()
}
fn sockets() -> (StdUdpSocket, StdUdpSocket, StdUdpSocket) {
    let receiver = StdUdpSocket::bind(("127.0.0.1", 0)).expect("receiver");
    receiver.set_nonblocking(true).expect("nonblocking");
    let one = StdUdpSocket::bind(("127.0.0.1", 0)).expect("one");
    let two = StdUdpSocket::bind(("127.0.0.1", 0)).expect("two");
    (receiver, one, two)
}
#[test]
fn ready_packets_from_both_publishers_survive_overdue_timer() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (receiver, one, two) = sockets();
    let a = open(
        &mut listener,
        one.local_addr().expect("fixture socket or timer operation"),
        11,
        start,
    );
    let b = open(
        &mut listener,
        two.local_addr().expect("fixture socket or timer operation"),
        12,
        start,
    );
    one.send_to(
        &keepalive(a),
        receiver
            .local_addr()
            .expect("fixture socket or timer operation"),
    )
    .expect("fixture socket or timer operation");
    two.send_to(
        &keepalive(b),
        receiver
            .local_addr()
            .expect("fixture socket or timer operation"),
    )
    .expect("fixture socket or timer operation");
    // Await actual loopback delivery before moving virtual time. send_to alone
    // need not mean both packets are readable yet on every host under load.
    receiver
        .set_nonblocking(false)
        .expect("fixture socket or timer operation");
    receiver
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("fixture socket or timer operation");
    let mut ready = std::collections::VecDeque::new();
    for _ in 0..2 {
        let mut bytes = vec![0; 1500];
        let (n, peer) = receiver
            .recv_from(&mut bytes)
            .expect("fixture socket or timer operation");
        bytes.truncate(n);
        ready.push_back((bytes, peer));
    }
    service_ready_timers(
        &mut listener,
        &mut [0; 65536],
        || start + Duration::from_secs(6),
        |buf| {
            let Some((bytes, peer)) = ready.pop_front() else {
                return Err(std::io::ErrorKind::WouldBlock.into());
            };
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok((bytes.len(), peer))
        },
    );
    assert!(
        closed(&mut listener).is_empty(),
        "queued live peers must be read before expiry"
    );
    assert_eq!(listener.connections(), 2);
}
#[test]
fn active_peer_does_not_keep_a_silent_peer_alive() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (receiver, one, two) = sockets();
    let a = open(
        &mut listener,
        one.local_addr().expect("active peer address"),
        11,
        start,
    );
    let b = open(
        &mut listener,
        two.local_addr().expect("silent peer address"),
        12,
        start,
    );
    one.send_to(
        &keepalive(a),
        receiver.local_addr().expect("receiver address"),
    )
    .expect("queued keepalive");
    receiver.set_nonblocking(false).expect("blocking peek");
    receiver
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("bounded peek");
    receiver
        .peek_from(&mut [0; 1500])
        .expect("queued packet ready");
    receiver.set_nonblocking(true).expect("nonblocking reader");
    service_due_timers(&mut listener, &receiver, &mut [0; 65536], || {
        start + Duration::from_secs(6)
    });
    assert_eq!(closed(&mut listener), vec![b]);
    assert_eq!(listener.connections(), 1);
    // Expiry remains the configured five seconds, with no timeout extension.
    service_due_timers(&mut listener, &receiver, &mut [0; 65536], || {
        start + Duration::from_secs(11)
    });
    assert_eq!(closed(&mut listener), vec![a]);
}
#[test]
fn stray_peer_cannot_refresh_another_connection() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (receiver, one, two) = sockets();
    let a = open(
        &mut listener,
        one.local_addr().expect("fixture socket or timer operation"),
        11,
        start,
    );
    two.send_to(
        &keepalive(a),
        receiver
            .local_addr()
            .expect("fixture socket or timer operation"),
    )
    .expect("fixture socket or timer operation");
    receiver
        .set_nonblocking(false)
        .expect("fixture socket or timer operation");
    receiver
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("fixture socket or timer operation");
    receiver
        .peek_from(&mut [0; 1500])
        .expect("fixture socket or timer operation");
    receiver
        .set_nonblocking(true)
        .expect("fixture socket or timer operation");
    service_due_timers(&mut listener, &receiver, &mut [0; 65536], || {
        start + Duration::from_secs(6)
    });
    assert_eq!(closed(&mut listener), vec![a]);
}

#[test]
fn continuous_input_has_a_fixed_budget_and_still_expires_silent_peers() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (_, one, two) = sockets();
    let source = one.local_addr().expect("fixture socket or timer operation");
    let a = open(&mut listener, source, 11, start);
    let b = open(
        &mut listener,
        two.local_addr().expect("fixture socket or timer operation"),
        12,
        start,
    );
    let bytes = keepalive(a);
    let mut reads = 0;
    // Model a socket that never reaches WouldBlock. A biased read-first select
    // or an unbounded drain would never return to expiry/admission/cancellation.
    service_ready_timers(
        &mut listener,
        &mut [0; 65536],
        || start + Duration::from_secs(6),
        |buf| {
            reads += 1;
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok((bytes.len(), source))
        },
    );
    assert_eq!(reads, TIMER_RECV_BUDGET);
    assert_eq!(closed(&mut listener), vec![b]);
    assert_eq!(listener.connections(), 1);
}
#[test]
fn interrupted_reads_cannot_remove_the_work_bound_or_disable_expiry() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (_, one, _) = sockets();
    let a = open(
        &mut listener,
        one.local_addr().expect("fixture socket or timer operation"),
        11,
        start,
    );
    let mut reads = 0;
    service_ready_timers(
        &mut listener,
        &mut [0; 65536],
        || start + Duration::from_secs(6),
        |_| {
            reads += 1;
            Err(std::io::ErrorKind::Interrupted.into())
        },
    );
    assert_eq!(reads, TIMER_RECV_BUDGET);
    assert_eq!(closed(&mut listener), vec![a]);
}

#[test]
fn a_socket_receive_error_still_services_due_expiry() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (_, one, _) = sockets();
    let a = open(
        &mut listener,
        one.local_addr().expect("source address"),
        11,
        start,
    );
    // Preserve the adapter's nonfatal receive-error behaviour, and still run
    // due timers rather than abandoning the entire shared SRT listener.
    service_ready_timers(
        &mut listener,
        &mut [0; 65536],
        || start + Duration::from_secs(6),
        |_| Err(std::io::ErrorKind::ConnectionReset.into()),
    );
    assert_eq!(closed(&mut listener), vec![a]);
}

#[test]
fn a_batch_uses_the_current_clock_for_packets_and_final_expiry() {
    let start = Instant::now();
    let mut listener = Listener::new(Config::default(), start);
    let (_, one, _) = sockets();
    let source = one.local_addr().expect("source address");
    let a = open(&mut listener, source, 11, start);
    let mut packet = Some(keepalive(a));
    // The live packet arrives at6s, but processing is stalled again before
    // the batch ends at12s. It must expire five seconds after that arrival,
    // rather than using a single frozen timestamp for the whole batch.
    let mut clock = [
        start + Duration::from_secs(6),
        start + Duration::from_secs(12),
    ]
    .into_iter();
    service_ready_timers(
        &mut listener,
        &mut [0; 65536],
        || clock.next().expect("packet and final timer clock"),
        |buf| {
            let Some(bytes) = packet.take() else {
                return Err(std::io::ErrorKind::WouldBlock.into());
            };
            buf[..bytes.len()].copy_from_slice(&bytes);
            Ok((bytes.len(), source))
        },
    );
    assert_eq!(closed(&mut listener), vec![a]);
}

#[test]
fn queued_reordered_media_is_read_before_missing_packets_expire() {
    use sapflow_srt::packet::{DataPacket, Position};

    for retransmitted in [false, true] {
        let start = Instant::now();
        let mut listener = Listener::new(Config::default(), start);
        let source = SocketAddr::from(([127, 0, 0, 1], 9100));
        let id = open(&mut listener, source, 11, start);
        let mut ready = std::collections::VecDeque::new();
        for seq in [100, 102, 101] {
            let mut bytes = Vec::new();
            DataPacket {
                seq: Seq::new(seq),
                position: Position::Only,
                in_order: false,
                key: 0,
                retransmitted: retransmitted && seq == 101,
                message: seq - 99,
                timestamp: 0,
                dest: id.0,
                payload: &[seq as u8],
            }
            .encode(&mut bytes);
            ready.push_back(bytes);
        }
        // The adapter resumes after the120ms deadline with the whole batch
        // readable. Both reordered originals and queued retries must survive.
        service_ready_timers(
            &mut listener,
            &mut [0; 65536],
            || start + Duration::from_millis(300),
            |buf| {
                let Some(bytes) = ready.pop_front() else {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                };
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok((bytes.len(), source))
            },
        );
        let media: Vec<_> = std::iter::from_fn(|| listener.poll_event())
            .filter_map(|event| match event {
                Event::Data {
                    payload,
                    after_loss,
                    ..
                } => Some((payload, after_loss)),
                _ => None,
            })
            .collect();
        assert_eq!(
            media,
            vec![(vec![100], false), (vec![101], false), (vec![102], false)]
        );
    }
}
