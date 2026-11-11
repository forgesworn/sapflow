//! Receiver reports, transport-cc and the timeline through the receiver,
//! on built packets and on the FFmpeg fixtures (which carry no sender
//! reports, so these are synthesised from known clocks).

use super::*;
use crate::rtcp::PT_RTPFB;
use crate::timeline::{Alignment, Fallback};
use crate::twcc::parse::{Status, parse_fci};

const TWCC_ID: u8 = 5;
/// The sender's wall clock at its first video frame, in NTP seconds.
const WALL: u64 = 3_971_808_000;

fn ntp_at(us: u64) -> u64 {
    (WALL << 32) + (u128::from(us) * (1u128 << 32)).div_ceil(1_000_000) as u64
}

fn sr(ssrc: u32, ntp: u64, rtp: u32) -> Vec<u8> {
    let mut out = vec![0x80, 200, 0, 6];
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&ntp.to_be_bytes());
    out.extend_from_slice(&rtp.to_be_bytes());
    out.extend_from_slice(&[0; 8]);
    out
}

fn rtcp(r: &mut Receiver, at_ms: u64, d: &[u8]) -> usize {
    let now = r.epoch + Duration::from_millis(at_ms);
    r.handle_rtcp(now, &Compound::parse(d).expect("rtcp"))
}

/// Every datagram the receiver has to send, each checked to parse.
fn datagrams(r: &mut Receiver) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let mut d = Vec::new();
        if !r.poll_feedback(0xfeed, b"sapflow", &mut d) {
            return out;
        }
        Compound::parse(&d).expect("a datagram parses");
        out.push(d);
    }
}

fn packets(d: &[u8]) -> Vec<Packet<'_>> {
    Compound::parse(d)
        .expect("parses")
        .packets()
        .map(|p| p.expect("valid"))
        .collect()
}

fn blocks(p: &Packet<'_>) -> Vec<ReportBlock> {
    match p {
        Packet::ReceiverReport(rr) => rr.reports().collect(),
        other => panic!("not an RR: {other:?}"),
    }
}

/// Extended sequence numbers of the video capture (it crosses the wrap).
fn video_ext(c: &Capture, seq: u16) -> u32 {
    let first = c.video[0].seq;
    if seq >= first {
        u32::from(seq)
    } else {
        0x1_0000 + u32::from(seq)
    }
}

/// The media SSRC's block counts originals only: what RTX repaired still
/// counts as lost (the sender's loss before repair). Retransmissions on
/// the original SSRC cannot be told from late originals and count once.
#[test]
fn report_blocks_under_a12_impairments() {
    let c = capture();
    for resend in [Resend::Rtx, Resend::Original] {
        let (mut imp, _) = impairments(&c);
        imp.rtx_probes = resend == Resend::Rtx;
        let out = simulate(&c, &imp, resend);
        let delivered: Vec<u32> = c
            .video
            .iter()
            .filter(|p| !imp.drop.contains(&(0, p.seq)))
            .map(|p| video_ext(&c, p.seq))
            .collect();
        let (lo, hi) = (
            *delivered.iter().min().expect("some"),
            *delivered.iter().max().expect("some"),
        );
        let dropped_inside = imp
            .drop
            .iter()
            .filter(|k| k.0 == 0 && (lo..=hi).contains(&video_ext(&c, k.1)))
            .count() as i32;
        assert!(dropped_inside > 0);
        let v = out.receiver.report_block(StreamId(0)).expect("video block");
        assert_eq!(v.ssrc, c.video_ssrc);
        assert_eq!(v.highest_sequence, hi, "{resend:?}");
        let expected_lost = if resend == Resend::Rtx {
            dropped_inside
        } else {
            0
        };
        assert_eq!(v.cumulative_lost, expected_lost, "{resend:?}");
        // Audio has no RTX: resent on its SSRC, duplicates ignored.
        let a = out.receiver.report_block(StreamId(1)).expect("audio block");
        assert_eq!(a.cumulative_lost, 0, "{resend:?}");
        // Jitter is from first arrivals only, so it stays near a run with
        // the same delays and nothing to retransmit. (A packet 25 ms late
        // at the end of a frame arrives before its gap is seen, and is
        // real jitter.) Each retransmission sampled would add a sixteenth
        // of its 30 ms lateness, 169 ticks at 90 kHz.
        let delays_only = Impairments {
            delay: imp.delay.clone(),
            ..Impairments::default()
        };
        let clean = simulate(&c, &delays_only, resend);
        let (cv, ca) = (
            clean
                .receiver
                .report_block(StreamId(0))
                .expect("video")
                .jitter,
            clean
                .receiver
                .report_block(StreamId(1))
                .expect("audio")
                .jitter,
        );
        assert!(
            v.jitter <= cv + 40,
            "video jitter {} against {cv}",
            v.jitter
        );
        assert!(
            a.jitter <= ca + 40,
            "audio jitter {} against {ca}",
            a.jitter
        );
    }
}

fn av_config() -> ReceiverConfig {
    ReceiverConfig {
        transport_cc: Some(TransportCcConfig::new(TWCC_ID)),
        ..ReceiverConfig::default()
    }
}

fn with_twcc(d: &[u8], tseq: u16) -> Vec<u8> {
    let p = RtpPacket::parse(d).expect("rtp");
    RtpBuilder {
        marker: p.marker(),
        payload_type: p.payload_type(),
        sequence: p.sequence(),
        timestamp: p.timestamp(),
        ssrc: p.ssrc(),
        payload: p.payload().to_vec(),
        extension: Some(RtpBuilder::one_byte_elements(&[(
            TWCC_ID,
            &tseq.to_be_bytes(),
        )])),
        ..RtpBuilder::default()
    }
    .build()
}

#[test]
fn poll_feedback_writes_compound_rtcp() {
    let mut r = Receiver::new(av_config(), Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.ssrc = Some(7);
    v.nack = true;
    r.add_stream(v).expect("video");
    let slice = [0x41, 0x9a, 1];
    // Packets 0 to 9 at 1 ms apart, without 4 (transport-wide 0 to 9 too).
    for n in 0..10u16 {
        if n == 4 {
            continue;
        }
        let d = with_twcc(&pkt(96, 7, n, u32::from(n) * 90, false, &slice), n);
        feed(&mut r, u64::from(n), &d).expect("taken");
    }
    // No report is due yet, so no datagram until the NACK at 15 ms.
    assert!(datagrams(&mut r).is_empty());
    r.handle_timeout(r.epoch + Duration::from_millis(15));
    let out = datagrams(&mut r);
    assert_eq!(out.len(), 1);
    let p = packets(&out[0]);
    assert!(blocks(&p[0]).is_empty(), "an empty RR first");
    assert!(matches!(p[1], Packet::Other(o) if o.packet_type == crate::rtcp::PT_SDES));
    let Packet::Nack(n) = p[2] else {
        panic!("a NACK: {p:?}");
    };
    assert_eq!(n.lost().collect::<Vec<_>>(), [4]);
    // At 50 ms, transport-cc for everything recorded.
    r.handle_timeout(r.epoch + Duration::from_millis(50));
    let out = datagrams(&mut r);
    let p = packets(&out[0]);
    assert!(blocks(&p[0]).is_empty());
    let Packet::Other(o) = p[2] else {
        panic!("transport-cc: {p:?}");
    };
    assert_eq!((o.packet_type, o.count), (PT_RTPFB, 15));
    let fb = parse_fci(o.body.get(8..).expect("fci")).expect("reads back");
    assert_eq!(fb.base, 0);
    assert_eq!(fb.statuses.len(), 10);
    assert_eq!(fb.statuses[4], Status::NotReceived);
    assert_eq!(fb.statuses[5], Status::Small(8), "2 ms after 3");
    // The first report, half an interval after the first packet, has a
    // block: 10 expected, 9 received.
    r.handle_timeout(r.epoch + Duration::from_millis(500));
    let out = datagrams(&mut r);
    assert_eq!(out.len(), 1);
    let b = blocks(&packets(&out[0])[0]);
    assert_eq!(b.len(), 1);
    assert_eq!((b[0].ssrc, b[0].highest_sequence), (7, 9));
    assert_eq!((b[0].cumulative_lost, b[0].fraction_lost), (1, 25));
    // Every interval after that, fixed.
    assert_eq!(
        r.poll_timeout(),
        Some(r.epoch + Duration::from_millis(1500))
    );
    r.handle_timeout(r.epoch + Duration::from_millis(1500));
    let b = blocks(&packets(&datagrams(&mut r)[0])[0]);
    assert_eq!(
        (b[0].cumulative_lost, b[0].fraction_lost),
        (1, 0),
        "a new interval"
    );
}

#[test]
fn reduced_size_and_the_datagram_cap() {
    let cfg = ReceiverConfig {
        reports: ReportConfig {
            reduced_size: true,
            max_datagram: 100,
            ..ReportConfig::default()
        },
        // Feedback for every packet at once: 24 bytes each.
        transport_cc: Some(TransportCcConfig {
            max_pending: 1,
            ..TransportCcConfig::new(TWCC_ID)
        }),
        ..no_hold()
    };
    let mut r = Receiver::new(cfg, Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.ssrc = Some(7);
    r.add_stream(v).expect("video");
    for n in 0..10u16 {
        let d = with_twcc(&pkt(96, 7, n, 0, false, &[0x41, 0x9a, 1]), n);
        feed(&mut r, u64::from(n), &d).expect("taken");
    }
    let out = datagrams(&mut r);
    let counts: Vec<usize> = out.iter().map(|d| packets(d).len()).collect();
    assert_eq!(counts, [4, 4, 2], "96 bytes, 96, 48");
    let mut fb = 0u8;
    for d in &out {
        assert!(d.len() <= 100);
        for p in packets(d) {
            let Packet::Other(o) = p else {
                panic!("transport-cc alone: {p:?}");
            };
            let f = parse_fci(o.body.get(8..).expect("fci")).expect("reads");
            assert_eq!((f.fb_count, f.base), (fb, u16::from(fb)));
            fb += 1;
        }
    }
    // A report still leads with the RR.
    r.handle_timeout(r.epoch + Duration::from_millis(500));
    let out = datagrams(&mut r);
    assert!(matches!(packets(&out[0])[0], Packet::ReceiverReport(_)));
}

#[test]
fn sender_reports_give_lsr_dlsr_and_wait_for_their_ssrc() {
    let mut r = Receiver::new(no_hold(), Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.rtx = Some(RtxConfig {
        payload_type: 97,
        ssrc: Some(99),
    });
    r.add_stream(v).expect("video");
    // Before the media SSRC is known: kept. About RTX: ignored.
    assert_eq!(rtcp(&mut r, 0, &sr(7, ntp_at(0), 0)), 1);
    assert_eq!(rtcp(&mut r, 0, &sr(99, ntp_at(0), 0)), 0);
    feed(&mut r, 250, &pkt(96, 7, 1, 0, true, &[0x41, 0x9a, 1])).expect("taken");
    let b = r.report_block(StreamId(0)).expect("block");
    assert_eq!(b.last_sr, (ntp_at(0) >> 16) as u32);
    // 250 ms since it arrived, in 1/65536 s.
    assert_eq!(b.delay_since_last_sr, 16_384);
    // A later one replaces it.
    rtcp(&mut r, 1000, &sr(7, ntp_at(1_000_000), 90_000));
    let b = r.report_block(StreamId(0)).expect("block");
    assert_eq!(b.last_sr, (ntp_at(1_000_000) >> 16) as u32);
    assert_eq!(b.delay_since_last_sr, 0);
}

/// The fixtures on one sender clock: video's first frame at sender time 0,
/// audio's first packet 300 ms later. Both arrive 15 ms after they are
/// sent. With `reports`, each stream's sender report comes 50 ms into
/// it. Returns the events and when each was polled (ms).
fn av_run(reports: bool) -> (Receiver, Vec<(u64, Event)>) {
    let c = capture();
    let mut r = Receiver::new(av_config(), Instant::now());
    let mut video = StreamConfig::new(Codec::H264, c.video_pt);
    video.ssrc = Some(c.video_ssrc);
    let audio = StreamConfig {
        ssrc: Some(c.audio_ssrc),
        ..StreamConfig::new(Codec::Opus, c.audio_pt)
    };
    r.add_stream(video).expect("video");
    r.add_stream(audio).expect("audio");
    const AUDIO_START: u64 = 300 * MS;
    let mut items: Vec<(u64, Vec<u8>, bool)> = Vec::new();
    let mut tseq = 0u16;
    for p in &c.video {
        items.push((p.sent + ONE_WAY, p.datagram.clone(), false));
    }
    for p in &c.audio {
        items.push((AUDIO_START + p.sent + ONE_WAY, p.datagram.clone(), false));
    }
    if reports {
        let v_ts0 = c.video[0].ts;
        let a_ts0 = c.audio[0].ts;
        items.push((
            50 * MS + ONE_WAY,
            sr(c.video_ssrc, ntp_at(50 * MS), v_ts0.wrapping_add(4500)),
            true,
        ));
        items.push((
            AUDIO_START + 50 * MS + ONE_WAY,
            sr(
                c.audio_ssrc,
                ntp_at(AUDIO_START + 50 * MS),
                a_ts0.wrapping_add(2400),
            ),
            true,
        ));
    }
    items.sort_by_key(|i| i.0);
    let t0 = r.epoch;
    let at = |us: u64| t0 + Duration::from_micros(us);
    let mut events = Vec::new();
    let timers = |r: &mut Receiver, until: u64, events: &mut Vec<(u64, Event)>| {
        while let Some(t) = r.poll_timeout() {
            if t > at(until) {
                break;
            }
            r.handle_timeout(t);
            let ms = micros(t.saturating_duration_since(t0)) / MS;
            events.extend(std::iter::from_fn(|| r.poll_event()).map(|e| (ms, e)));
            let _ = datagrams(r);
        }
    };
    for (t, d, is_rtcp) in items {
        timers(&mut r, t, &mut events);
        if is_rtcp {
            r.handle_rtcp(at(t), &Compound::parse(&d).expect("rtcp"));
        } else {
            let d = with_twcc(&d, tseq);
            tseq = tseq.wrapping_add(1);
            r.handle_rtp(at(t), &RtpPacket::parse(&d).expect("rtp"))
                .expect("taken");
        }
        events.extend(std::iter::from_fn(|| r.poll_event()).map(|e| (t / MS, e)));
        for d in datagrams(&mut r) {
            assert!(matches!(packets(&d)[0], Packet::ReceiverReport(_)));
        }
    }
    timers(&mut r, 4_000 * MS, &mut events);
    (r, events)
}

fn first_pts(events: &[(u64, Event)], stream: usize) -> (u32, u64) {
    events
        .iter()
        .find_map(|(_, e)| match e {
            Event::AccessUnit { stream: s, au, pts } if s.index() == stream => {
                Some((au.timestamp, *pts))
            }
            Event::Audio {
                stream: s,
                frame,
                pts,
            } if s.index() == stream => Some((frame.timestamp, *pts)),
            _ => None,
        })
        .expect("a frame")
}

#[test]
fn av_alignment_over_the_fixtures_by_sender_reports() {
    let c = capture();
    let (r, events) = av_run(true);
    // Held until audio's report at 365 ms, then aligned by the reports.
    let (when, first) = &events[0];
    assert_eq!(
        *first,
        Event::Aligned {
            alignment: Alignment::SenderReports
        }
    );
    assert_eq!(*when, 365);
    assert_eq!(r.alignment(), Some(Alignment::SenderReports));
    // Every frame exactly where the sender's clock puts it: video at its
    // RTP offset, audio 300 ms (27 000 ticks) later plus its own.
    let (v_ts0, a_ts0) = (c.video[0].ts, c.audio[0].ts);
    let mut frames = (0, 0);
    for (_, e) in &events {
        match e {
            Event::AccessUnit { au, pts, .. } => {
                assert_eq!(*pts, u64::from(au.timestamp.wrapping_sub(v_ts0)));
                frames.0 += 1;
            }
            Event::Audio { frame, pts, .. } => {
                let samples = u64::from(frame.timestamp.wrapping_sub(a_ts0));
                assert_eq!(*pts, 27_000 + samples * 90_000 / 48_000);
                frames.1 += 1;
            }
            Event::Discontinuity { .. } => panic!("{e:?}"),
            _ => {}
        }
    }
    assert!(frames.0 >= 25 && frames.1 >= 100, "{frames:?}");
    // Every packet carried a transport-wide number and was reported.
    let twcc = r.transport_cc_stats().expect("negotiated");
    assert_eq!(twcc.packets, (c.video.len() + c.audio.len()) as u64);
    assert!(twcc.feedback > 30);
}

#[test]
fn av_alignment_falls_back_to_arrival_without_reports() {
    let (r, events) = av_run(false);
    let (when, first) = &events[0];
    assert_eq!(
        *first,
        Event::Aligned {
            alignment: Alignment::Arrival(Fallback::Timeout)
        }
    );
    // 1.5 s after the first media packet, at 15 ms.
    assert_eq!(*when, 1515);
    assert_eq!(r.alignment(), Some(Alignment::Arrival(Fallback::Timeout)));
    let (_, v) = first_pts(&events, 0);
    let (_, a) = first_pts(&events, 1);
    // Arrival puts audio 300 ms after video's first packet, less the few
    // milliseconds the first access unit took to arrive whole.
    assert!(
        a > v && (a - v).abs_diff(27_000) < 900,
        "video {v}, audio {a}"
    );
    assert!(
        events
            .iter()
            .all(|(_, e)| !matches!(e, Event::Discontinuity { .. })),
        "no discontinuities"
    );
}

#[test]
fn audio_only_session_aligns_on_its_report() {
    let c = capture();
    let mut r = Receiver::new(ReceiverConfig::default(), Instant::now());
    let audio = StreamConfig {
        ssrc: Some(c.audio_ssrc),
        ..StreamConfig::new(Codec::Opus, c.audio_pt)
    };
    r.add_stream(audio).expect("audio");
    let a = &c.audio;
    feed(&mut r, 0, &a[0].datagram).expect("taken");
    assert_eq!(r.poll_event(), None, "held");
    rtcp(&mut r, 5, &sr(c.audio_ssrc, ntp_at(0), a[0].ts));
    assert_eq!(
        r.poll_event(),
        Some(Event::Aligned {
            alignment: Alignment::SenderReports
        })
    );
    feed(&mut r, 20, &a[1].datagram).expect("taken");
    r.handle_timeout(r.epoch + Duration::from_millis(40));
    let pts: Vec<u64> = std::iter::from_fn(|| r.poll_event())
        .filter_map(|e| match e {
            Event::Audio { pts, .. } => Some(pts),
            _ => None,
        })
        .collect();
    assert_eq!(pts, [0, 1800]);
}

/// A sender report far behind where the stream's timestamps put it:
/// reported, and timestamps never go back.
#[test]
fn a_drifting_report_is_a_discontinuity() {
    let mut r = Receiver::new(no_hold(), Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.ssrc = Some(7);
    v.pli = true;
    r.add_stream(v).expect("video");
    rtcp(&mut r, 0, &sr(7, ntp_at(0), 0));
    let idr = [0x65, 0x88, 1];
    let mut last = 0;
    for n in 0..60u64 {
        feed(
            &mut r,
            n * 33,
            &pkt(96, 7, n as u16, (n * 3000) as u32, true, &idr),
        )
        .expect("taken");
        r.handle_timeout(r.epoch + Duration::from_millis(n * 33 + 20));
        if n == 30 {
            // The sender says frame 30 was 300 ms later than its RTP time.
            rtcp(&mut r, n * 33 + 21, &sr(7, ntp_at(1_300_000), 90_000));
        }
        for e in std::iter::from_fn(|| r.poll_event()) {
            match e {
                Event::AccessUnit { pts, .. } => {
                    assert!(pts >= last, "{pts} after {last}");
                    last = pts;
                }
                Event::Discontinuity {
                    cause: Discontinuity::SenderReport { drift },
                    ..
                } => assert_eq!(drift, -27_000),
                _ => {}
            }
        }
    }
    assert_eq!(
        r.stats(StreamId(0))
            .expect("stats")
            .timeline_discontinuities,
        1
    );
    assert_eq!(last, 59 * 3000 + 27_000, "moved forward by the drift");
}

/// Video that only ever arrives over RTX (every original lost) still
/// starts the hold, which still ends on time.
#[test]
fn media_only_over_rtx_still_ends_the_hold() {
    let mut r = Receiver::new(ReceiverConfig::default(), Instant::now());
    let mut v = StreamConfig::new(Codec::H264, 96);
    v.ssrc = Some(7);
    v.rtx = Some(RtxConfig {
        payload_type: 97,
        ssrc: Some(99),
    });
    v.nack = true;
    r.add_stream(v).expect("video");
    // A padding-only probe is not media: nothing starts.
    feed(&mut r, 0, &pkt(97, 99, 1, 0, false, &[])).expect("probe");
    assert_eq!(r.poll_timeout(), None);
    for n in 0..60u16 {
        let mut body = n.to_be_bytes().to_vec();
        body.extend_from_slice(&[0x65, 0x88, 1]);
        let at = 10 + u64::from(n) * 33;
        feed(
            &mut r,
            at,
            &pkt(97, 99, 2 + n, u32::from(n) * 3000, true, &body),
        )
        .expect("taken");
        r.handle_timeout(r.epoch + Duration::from_millis(at));
    }
    assert_eq!(r.alignment(), Some(Alignment::Arrival(Fallback::Timeout)));
    assert!(matches!(r.poll_event(), Some(Event::Aligned { .. })));
    assert!(std::iter::from_fn(|| r.poll_event()).any(|e| matches!(e, Event::AccessUnit { .. })));
}
