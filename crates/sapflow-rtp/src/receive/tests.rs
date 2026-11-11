//! The receiver's rules, on built packets.

use super::*;
use crate::rtcp::{Compound, Packet};
use crate::rtp::{RtpBuilder, RtpPacket};

fn access_units(events: &[Event]) -> Vec<AccessUnit> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::AccessUnit { au, .. } => Some(au.clone()),
            _ => None,
        })
        .collect()
}

fn video_only(cfg: ReceiverConfig, rtx: bool) -> Receiver {
    let mut r = Receiver::new(cfg, Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.nack = true;
    v.pli = true;
    if rtx {
        v.rtx = Some(RtxConfig {
            payload_type: 97,
            ssrc: None,
        });
    }
    r.add_stream(v).expect("video");
    r
}

fn pkt(pt: u8, ssrc: u32, seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
    RtpBuilder {
        marker,
        payload_type: pt,
        sequence: seq,
        timestamp: ts,
        ssrc,
        payload: payload.to_vec(),
        ..RtpBuilder::default()
    }
    .build()
}

fn feed(r: &mut Receiver, at_ms: u64, d: &[u8]) -> Result<(StreamId, Received), ReceiveError> {
    let now = r.epoch + Duration::from_millis(at_ms);
    r.handle_rtp(now, &RtpPacket::parse(d).expect("rtp"))
}

#[test]
fn routing_binds_ssrcs_and_refuses_strangers() {
    let mut r = video_only(ReceiverConfig::default(), true);
    let idr = [0x65, 0x88, 1];
    assert_eq!(
        feed(&mut r, 0, &pkt(96, 7, 1, 0, true, &idr)),
        Ok((StreamId(0), Received::New))
    );
    assert_eq!(r.ssrc(StreamId(0)), Some(7));
    // Another SSRC with the media payload type is a stranger now.
    assert_eq!(
        feed(&mut r, 1, &pkt(96, 8, 2, 0, true, &idr)),
        Err(ReceiveError::UnknownStream)
    );
    // The right SSRC with a wrong payload type.
    assert_eq!(
        feed(&mut r, 1, &pkt(100, 7, 2, 0, true, &idr)),
        Err(ReceiveError::PayloadType {
            ssrc: 7,
            payload_type: 100
        })
    );
    // RTX binds its SSRC; padding-only probes and short payloads.
    assert_eq!(
        feed(&mut r, 2, &pkt(97, 9, 50, 0, false, &[])),
        Ok((StreamId(0), Received::RtxPadding))
    );
    assert_eq!(
        feed(&mut r, 2, &pkt(97, 9, 51, 0, false, &[1])),
        Err(ReceiveError::Rtx(RtxError::Truncated))
    );
    assert_eq!(
        feed(&mut r, 2, &pkt(97, 9, 52, 0, true, &[0, 1, 0x65, 0x88, 1])),
        Ok((StreamId(0), Received::Duplicate))
    );
    assert_eq!(r.stats(StreamId(0)).expect("stats").rtx_padding, 1);
    // Configuration refusals.
    let mut r = video_only(ReceiverConfig::default(), true);
    assert_eq!(
        r.add_stream(StreamConfig::new(Codec::Opus, 97)),
        Err(ConfigError::PayloadTypeTaken(97))
    );
    assert_eq!(
        r.add_stream(StreamConfig::new(Codec::Opus, 128)),
        Err(ConfigError::InvalidPayloadType(128))
    );
    let mut a = StreamConfig::new(Codec::Opus, 111);
    a.ssrc = Some(5);
    a.rtx = Some(RtxConfig {
        payload_type: 112,
        ssrc: Some(5),
    });
    assert_eq!(r.add_stream(a), Err(ConfigError::SsrcTaken(5)));
    for pt in 0..3 {
        r.add_stream(StreamConfig::new(Codec::Opus, 100 + pt))
            .expect("added");
    }
    assert_eq!(
        r.add_stream(StreamConfig::new(Codec::Opus, 120)),
        Err(ConfigError::TooManyStreams)
    );
}

#[test]
fn keyframe_requests_are_scheduled_and_rate_limited() {
    let mut r = video_only(ReceiverConfig::default(), false);
    let p_frame = [0x41, 0x9a, 1];
    feed(&mut r, 0, &pkt(96, 7, 1, 0, true, &p_frame)).expect("taken");
    // At once on the first packet; the P frame is skipped.
    assert_eq!(r.poll_feedback(), Some(Feedback::Pli { media_ssrc: 7 }));
    r.handle_timeout(r.epoch + Duration::from_millis(20));
    assert!(matches!(
        r.poll_event(),
        Some(Event::FrameSkipped {
            reason: SkipReason::NoKeyframeYet,
            ..
        })
    ));
    // Still waiting: asked again after the 1 s retry.
    r.handle_timeout(r.epoch + Duration::from_millis(999));
    assert_eq!(r.poll_feedback(), None);
    r.handle_timeout(r.epoch + Duration::from_millis(1000));
    assert_eq!(r.poll_feedback(), Some(Feedback::Pli { media_ssrc: 7 }));
    feed(&mut r, 1100, &pkt(96, 7, 2, 9000, true, &[0x65, 0x88, 1])).expect("idr");
    assert!(matches!(r.poll_event(), Some(Event::AccessUnit { .. })));
    // Then every 2 s.
    r.handle_timeout(r.epoch + Duration::from_millis(2999));
    assert_eq!(r.poll_feedback(), None);
    assert_eq!(
        r.poll_timeout(),
        Some(r.epoch + Duration::from_millis(3000))
    );
    r.handle_timeout(r.epoch + Duration::from_millis(3000));
    assert_eq!(r.poll_feedback(), Some(Feedback::Pli { media_ssrc: 7 }));
    // Loss asks at once, but not within 500 ms of the last request.
    feed(&mut r, 3100, &pkt(96, 7, 4, 18000, true, &p_frame)).expect("gap");
    r.handle_timeout(r.epoch + Duration::from_millis(3300));
    assert!(std::iter::from_fn(|| r.poll_event()).any(|e| matches!(
        e,
        Event::Lost {
            first: 3,
            count: 1,
            ..
        }
    )));
    let mut got = Vec::new();
    for ms in (3300..=3500).step_by(10) {
        r.handle_timeout(r.epoch + Duration::from_millis(ms));
        got.extend(std::iter::from_fn(|| r.poll_feedback()).map(|f| (ms, f)));
    }
    let plis: Vec<u64> = got
        .iter()
        .filter(|(_, f)| matches!(f, Feedback::Pli { .. }))
        .map(|(ms, _)| *ms)
        .collect();
    assert_eq!(plis, [3500]);
    // FIR where only FIR is negotiated, with a new sequence number each time.
    let mut r = Receiver::new(ReceiverConfig::default(), Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.fir = true;
    r.add_stream(v).expect("video");
    feed(&mut r, 0, &pkt(96, 7, 1, 0, true, &p_frame)).expect("taken");
    assert_eq!(
        r.poll_feedback(),
        Some(Feedback::Fir {
            media_ssrc: 7,
            sequence: 1
        })
    );
    r.handle_timeout(r.epoch + Duration::from_millis(1000));
    assert_eq!(
        r.poll_feedback(),
        Some(Feedback::Fir {
            media_ssrc: 7,
            sequence: 2
        })
    );
    let mut wire = Vec::new();
    Feedback::Fir {
        media_ssrc: 7,
        sequence: 2,
    }
    .write(&mut wire, 1)
    .expect("fir");
    assert!(matches!(
        Compound::parse(&wire).expect("parses").packets().next(),
        Some(Ok(Packet::Fir(_)))
    ));
}

#[test]
fn nacks_are_rate_limited_and_coalesced() {
    let cfg = ReceiverConfig {
        nack_bitrate: 8_000,
        nack_burst: 40,
        ..ReceiverConfig::default()
    };
    let mut r = video_only(cfg, false);
    let s = [0x41, 0x22, 1];
    // Ten missing, 18 apart, so each needs a pair of its own.
    let missing: Vec<u16> = (0..10).map(|k| k * 18 + 1).collect();
    feed(&mut r, 0, &pkt(96, 7, 0, 0, false, &s)).expect("taken");
    for seq in 1..=181u16 {
        if !missing.contains(&seq) {
            feed(&mut r, 1, &pkt(96, 7, seq, 0, false, &s)).expect("taken");
        }
    }
    r.handle_timeout(r.epoch + Duration::from_millis(11));
    let nacks: Vec<_> = std::iter::from_fn(|| r.poll_feedback())
        .filter(|f| matches!(f, Feedback::Nack { .. }))
        .collect();
    // The 40-byte burst holds 12 + 4 * 7 bytes: the oldest seven.
    assert_eq!(
        nacks,
        [Feedback::Nack {
            media_ssrc: 7,
            lost: missing[..7].to_vec()
        }]
    );
    let mut wire = Vec::new();
    nacks[0].write(&mut wire, 1).expect("built");
    assert_eq!(wire.len(), 40);
    // 1000 bytes a second: one more pair's 16 bytes after 16 ms, all three
    // (24 bytes) after 24.
    assert_eq!(r.poll_timeout(), Some(r.epoch + Duration::from_millis(27)));
    r.handle_timeout(r.epoch + Duration::from_millis(35));
    assert_eq!(
        r.poll_feedback(),
        Some(Feedback::Nack {
            media_ssrc: 7,
            lost: missing[7..].to_vec()
        })
    );
    assert_eq!(r.stats(StreamId(0)).expect("stats").nacked, 10);
    // Coalescing: a run of 17 takes one pair.
    let mut r = video_only(ReceiverConfig::default(), false);
    feed(&mut r, 0, &pkt(96, 7, 0, 0, false, &s)).expect("taken");
    feed(&mut r, 1, &pkt(96, 7, 18, 0, false, &s)).expect("taken");
    r.handle_timeout(r.epoch + Duration::from_millis(11));
    let Some(f @ Feedback::Nack { .. }) =
        std::iter::from_fn(|| r.poll_feedback()).find(|f| matches!(f, Feedback::Nack { .. }))
    else {
        panic!("a NACK");
    };
    let mut wire = Vec::new();
    f.write(&mut wire, 1).expect("built");
    assert_eq!(wire.len(), 16);
}

#[test]
fn sequence_restarts_and_backwards_clocks() {
    let mut r = video_only(ReceiverConfig::default(), false);
    let idr = [0x65, 0x88, 1];
    feed(&mut r, 0, &pkt(96, 7, 10, 0, true, &idr)).expect("taken");
    feed(&mut r, 20, &pkt(96, 7, 11, 3000, true, &idr)).expect("taken");
    // A jump of 30 000: set aside, then confirmed by its successor.
    assert_eq!(
        feed(&mut r, 30, &pkt(96, 7, 30_011, 6000, true, &idr)).map(|x| x.1),
        Ok(Received::Probe)
    );
    assert_eq!(
        feed(&mut r, 31, &pkt(96, 7, 30_012, 9000, true, &idr)).map(|x| x.1),
        Ok(Received::Restart)
    );
    let events: Vec<_> = std::iter::from_fn(|| r.poll_event()).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Discontinuity {
            cause: Discontinuity::Sequence,
            ..
        }
    )));
    assert_eq!(access_units(&events).len(), 4);
    // A clock that goes back is held at its latest reading.
    feed(&mut r, 10, &pkt(96, 7, 30_013, 12000, true, &idr)).expect("taken");
    assert!(r.poll_timeout().expect("timer") >= r.epoch + Duration::from_millis(31));
    assert_eq!(
        access_units(&std::iter::from_fn(|| r.poll_event()).collect::<Vec<_>>()).len(),
        1
    );
}
