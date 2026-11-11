//! Bounded receiver-library benchmark, with no sockets or external media.
//!
//! Build this same example against both receiver revisions and run binaries
//! sequentially in alternating order. It is not a server or network result.
use sapflow_srt::packet::{Control, DataPacket, Position};
use sapflow_srt::receiver::{Receiver, Settings};
use sapflow_srt::seq::Seq;
use std::hint::black_box;
use std::time::Instant;

const LATENCY: i64 = 200_000;
const GAP_QUERIES: usize = 100_000;
// Keep the constant-time control cases long enough to expose nominal costs
// rather than timing a sub-millisecond interval on a shared desktop.
const CLEAN_QUERIES: usize = 300_000_000;
const STREAM_PACKETS: u32 = 10_000_000;

fn receiver() -> Receiver {
    Receiver::new(
        Settings {
            latency: LATENCY,
            buffer: 32_768,
            peer_idle: 5_000_000,
            max_payload: 1456,
        },
        0,
        77,
        Seq::new(0),
        0,
    )
}

fn packet(seq: u32, timestamp: u32, payload: &[u8]) -> DataPacket<'_> {
    DataPacket {
        seq: Seq::new(seq),
        position: Position::Only,
        in_order: false,
        key: 0,
        retransmitted: false,
        message: 1,
        timestamp,
        dest: 77,
        payload,
    }
}

fn gap_queries(gap: u32, pending: bool) {
    let mut r = receiver();
    r.on_data(0, &packet(gap, 0, b"verified"));
    while r.take_send().is_some() {}
    let expected = r.next_timer();
    let queries = if gap == 0 { CLEAN_QUERIES } else { GAP_QUERIES };
    let started = Instant::now();
    for _ in 0..queries {
        if pending {
            // Time has not reached delivery or retransmission: model repeated
            // embedding polls while an earlier gap is still recoverable.
            black_box(&mut r).on_timer(black_box(0));
        }
        assert_eq!(black_box(&r).next_timer(), expected);
    }
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(r.stats().received, 1);
    assert_eq!(r.stats().delivered, 0);
    assert!(r.take_delivery().is_none());
    r.on_timer(LATENCY);
    assert_eq!(r.take_delivery(), Some((b"verified".to_vec(), gap != 0)));
    assert!(r.take_delivery().is_none());
    assert_eq!(r.stats().dropped, u64::from(gap));
    println!(
        "{{\"profile\":\"{}\",\"gap\":{gap},\"operations\":{queries},\"elapsed_ns\":{elapsed},\"verified_deliveries\":1}}",
        if pending {
            "pending-and-timer"
        } else {
            "timer"
        }
    );
}

fn drain(r: &mut Receiver, payload: &[u8], count: &mut u32) {
    while let Some((bytes, lost)) = r.take_delivery() {
        assert_eq!(bytes, payload);
        assert!(!lost);
        *count += 1;
    }
    while r.take_send().is_some() {}
}

fn clean_stream() {
    let mut r = receiver();
    let payload = [0x53; 1200];
    let mut delivered = 0;
    let started = Instant::now();
    for seq in 0..STREAM_PACKETS {
        let now = i64::from(seq) * 1000;
        r.on_data(now, &packet(seq, now as u32, &payload));
        r.on_timer(now);
        black_box(r.next_timer());
        drain(&mut r, &payload, &mut delivered);
    }
    r.on_control(i64::from(STREAM_PACKETS) * 1000, &Control::Shutdown);
    drain(&mut r, &payload, &mut delivered);
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(delivered, STREAM_PACKETS);
    assert_eq!(r.stats().dropped, 0);
    let receiver_bytes = std::mem::size_of::<Receiver>();
    println!(
        "{{\"profile\":\"clean-stream\",\"payload_bytes\":1200,\"operations\":{STREAM_PACKETS},\"elapsed_ns\":{elapsed},\"verified_deliveries\":{delivered},\"receiver_bytes\":{receiver_bytes}}}"
    );
}

fn bursty_stream() {
    const CYCLES: u32 = 256;
    const PACKETS: u32 = 128;
    const GAP: u32 = 4096;
    let payload = [0x47; 1200];
    let mut delivered = 0;
    let started = Instant::now();
    for _ in 0..CYCLES {
        let mut r = receiver();
        for seq in 0..PACKETS {
            let now = i64::from(seq) * 10;
            r.on_data(now, &packet(GAP + seq, now as u32, &payload));
            black_box(r.next_timer());
            while r.take_send().is_some() {}
        }
        assert!(r.take_delivery().is_none());
        r.on_control(i64::from(PACKETS) * 10, &Control::Shutdown);
        for seq in 0..PACKETS {
            let (bytes, lost) = r.take_delivery().expect("every received packet delivered");
            assert_eq!(bytes, payload);
            assert_eq!(lost, seq == 0);
            delivered += 1;
        }
        assert!(r.take_delivery().is_none());
        assert_eq!(r.stats().dropped, u64::from(GAP));
    }
    let elapsed = started.elapsed().as_nanos();
    let operations = CYCLES * PACKETS;
    assert_eq!(delivered, operations);
    println!(
        "{{\"profile\":\"bursty-stream\",\"gap\":{GAP},\"payload_bytes\":1200,\"operations\":{operations},\"elapsed_ns\":{elapsed},\"verified_deliveries\":{delivered}}}"
    );
}

fn main() {
    for gap in [0, 4096, 16384] {
        gap_queries(gap, false);
        gap_queries(gap, true);
    }
    clean_stream();
    bursty_stream();
}
