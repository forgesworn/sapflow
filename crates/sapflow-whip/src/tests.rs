//! Session tests: gate 1 of W3a in-process, for both SRTP profiles, and
//! each refusal the session must make.

use crate::sdp::MediaKind;
use crate::session::{EndReason, Event, SessionConfig, State};
use crate::stun::{self, Message};
use crate::testing::*;
use sapflow_dtls::SrtpProfile;
use sapflow_rtp::Alignment;
use sapflow_rtp::opus::FrameKind;
use std::time::{Duration, Instant};

const GCM: u16 = 0x0007;
const CM80: u16 = 0x0001;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn setup(seed: u8, profiles: &[u16]) -> (Publisher, Accepted, Instant) {
    let t0 = Instant::now();
    let p = Publisher::new(seed, profiles);
    let a = accept(&p.offer(), SessionConfig::default(), t0, seed);
    let mut p = p;
    p.answered(&a);
    assert_eq!(a.session.state(), State::Checking);
    (p, a, t0)
}

/// What one publish run saw.
#[derive(Default)]
struct Seen {
    units: Vec<(Vec<u8>, bool, usize, usize)>,
    audio: Vec<(Vec<u8>, FrameKind)>,
    aligned: Vec<Alignment>,
    lost: u64,
    other: usize,
}

fn collect(s: &mut crate::Session, seen: &mut Seen) {
    for e in events(s) {
        match e {
            Event::Media(sapflow_rtp::Event::AccessUnit { au, .. }) => {
                seen.units
                    .push((au.data, au.idr, au.sps.len(), au.pps.len()));
            }
            Event::Media(sapflow_rtp::Event::Audio { frame, .. }) => {
                seen.audio.push((frame.data, frame.kind));
            }
            Event::Media(sapflow_rtp::Event::Aligned { alignment }) => seen.aligned.push(alignment),
            Event::Media(sapflow_rtp::Event::Lost { count, .. }) => seen.lost += count,
            _ => seen.other += 1,
        }
    }
}

/// Gate 1: STUN, the DTLS test client, SRTP protect; H.264 with STAP-A
/// and FU-A, and Opus; the expected access units and audio come out, and
/// the client unprotects the session's RTCP.
fn publish(profile: u16, seed: u8) {
    let (mut p, mut a, t0) = setup(seed, &[profile]);
    let s = &mut a.session;
    p.connect(s, t0);
    let want = if profile == GCM {
        SrtpProfile::AeadAes128Gcm
    } else {
        SrtpProfile::Aes128CmHmacSha1_80
    };
    assert_eq!(s.state(), State::Connected);
    assert_eq!(s.profile(), Some(want));
    let ev = events(s);
    assert!(matches!(ev.first(), Some(Event::Nominated)), "{ev:?}");
    assert!(
        matches!(ev.get(1), Some(Event::Connected(x)) if *x == want),
        "{ev:?}"
    );
    assert_eq!(p.misdirected, 0);
    // A PLI as soon as SRTP is up, which the client can unprotect.
    assert!(
        p.rtcp.iter().any(|c| rtcp_types(c).contains(&(206, 1))),
        "PLI at once: {:?}",
        p.rtcp.iter().map(|c| rtcp_types(c)).collect::<Vec<_>>()
    );

    let mut t = t0;
    let mut seen = Seen::default();
    for d in p.sender_reports(0xe8f0_0000_0000_0000) {
        s.handle_datagram(t, p.tuple, &d);
    }
    let mut want_units = Vec::new();
    let mut want_audio = Vec::new();
    for step in 0..50u64 {
        t = t0 + ms(20 * step);
        if step % 2 == 0 {
            let (packets, data) = if step % 20 == 0 {
                p.keyframe(4000)
            } else {
                p.delta(if step % 4 == 0 { 600 } else { 2500 })
            };
            want_units.push((data, step % 20 == 0));
            for d in packets {
                s.handle_datagram(t, p.tuple, &d);
            }
        }
        let fill = step as u8;
        want_audio.push(opus_packet(fill));
        s.handle_datagram(t, p.tuple, &p.audio(fill));
        run_until(s, t);
        collect(s, &mut seen);
        p.receive(s);
    }
    run_until(s, t + ms(600));
    collect(s, &mut seen);
    p.receive(s);

    assert_eq!(seen.aligned.len(), 1);
    assert!(
        matches!(seen.aligned[0], Alignment::SenderReports),
        "{:?}",
        seen.aligned
    );
    let got: Vec<(Vec<u8>, bool)> = seen.units.iter().map(|u| (u.0.clone(), u.1)).collect();
    assert_eq!(got.len(), want_units.len());
    assert_eq!(got, want_units, "access units");
    assert_eq!(
        (seen.units[0].2, seen.units[0].3),
        (1, 1),
        "SPS and PPS with the first IDR"
    );
    let sent: Vec<Vec<u8>> = seen
        .audio
        .iter()
        .filter(|(_, k)| *k == FrameKind::Media)
        .map(|(d, _)| d.clone())
        .collect();
    assert_eq!(sent, want_audio, "audio");
    assert_eq!(seen.lost, 0);

    // Feedback: receiver reports, and transport-cc, all through SRTCP.
    let types: Vec<(u8, u8)> = p.rtcp.iter().flat_map(|c| rtcp_types(c)).collect();
    assert!(types.contains(&(201, 0)), "receiver reports");
    assert!(types.contains(&(202, 1)), "SDES CNAME");
    assert!(types.contains(&(205, 15)), "transport-cc");
    let stats = s.stats();
    assert_eq!(stats.feedback_sent as usize, p.rtcp.len());
    assert!(stats.srtp_packets > 50 && stats.srtcp_packets == 2);
    assert_eq!(
        (stats.srtp_refused, stats.unmatched, stats.refused),
        (0, 0, 0)
    );
    assert_eq!(
        stats.budget_dropped, 0,
        "the limit never drops an honest send"
    );
    assert_eq!(s.last_media(), Some(t0 + ms(980)));
    assert_eq!(p.misdirected, 0);
}

#[test]
fn gate1_aead_aes_128_gcm() {
    publish(GCM, 1);
}

#[test]
fn gate1_aes_cm_hmac_sha1_80() {
    publish(CM80, 2);
}

#[test]
fn gcm_is_chosen_when_both_are_offered() {
    let (mut p, mut a, t0) = setup(3, &[CM80, GCM]);
    p.connect(&mut a.session, t0);
    assert_eq!(a.session.profile(), Some(SrtpProfile::AeadAes128Gcm));
}

#[test]
fn dtls_from_an_unvalidated_tuple_is_ignored() {
    let (mut p, mut a, t0) = setup(4, &[GCM]);
    let s = &mut a.session;
    let hello = p.client.start();
    for d in &hello {
        s.handle_datagram(t0, p.tuple, d);
    }
    assert!(
        s.poll_transmit().is_none(),
        "nothing to an unvalidated tuple"
    );
    assert_eq!(s.stats().dtls_dropped, hello.len() as u64);
    assert_eq!(
        s.stats().dtls.datagrams_dropped + s.stats().dtls.records_dropped,
        0
    );
    // After the publisher nominates, DTLS from another tuple, validated or
    // not, still goes nowhere.
    let c = p.check(true);
    s.handle_datagram(t0, p.tuple, &c);
    p.receive(s);
    let other = tuple(9);
    for d in &hello {
        s.handle_datagram(t0, other, d);
    }
    let c = p.check(false);
    s.handle_datagram(t0, other, &c);
    let answers: Vec<_> = std::iter::from_fn(|| s.poll_transmit()).collect();
    assert_eq!(answers.len(), 1, "only the STUN answer");
    for d in &hello {
        s.handle_datagram(t0, other, d);
    }
    assert!(s.poll_transmit().is_none());
    assert_eq!(s.stats().dtls_dropped, 3 * hello.len() as u64);
    assert_eq!(s.state(), State::Handshaking);
}

#[test]
fn unauthenticated_stun_and_everything_else_draws_zero_bytes() {
    let (mut p, mut a, t0) = setup(5, &[GCM]);
    let s = &mut a.session;
    let honest = p.check(true);
    let mut wrong_pwd = p.stun.clone().expect("valid");
    wrong_pwd.key = b"wrongwrongwrongwrongwrong".to_vec();
    let mut wrong_user = p.stun.clone().expect("valid");
    wrong_user.username = b"nobody:pub05".to_vec();
    let mut flipped = honest.clone();
    flipped[30] ^= 1;
    let inputs: Vec<Vec<u8>> = vec![
        wrong_pwd.check(true),
        wrong_user.check(true),
        flipped,
        honest[..honest.len() - 8].to_vec(),
        vec![0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42],
        vec![0x80; 200],
        vec![0x81, 200, 0, 1, 0, 0, 0, 0],
        vec![0x16, 0xfe, 0xfd, 0, 0],
        vec![0xff; 40],
        Vec::new(),
        p.client.start().remove(0),
    ];
    for (i, d) in inputs.iter().enumerate() {
        s.handle_datagram(t0, tuple(i as u16 + 100), d);
        assert!(s.poll_transmit().is_none(), "input {i}");
    }
    assert_eq!(s.state(), State::Checking);
    let st = s.stats();
    assert_eq!(
        (
            st.ice.bad_integrity,
            st.ice.wrong_username,
            st.ice.bad_fingerprint
        ),
        (1, 1, 1)
    );
}

#[test]
fn ice_controlled_gets_487() {
    let (mut p, mut a, t0) = setup(6, &[GCM]);
    let s = &mut a.session;
    let req = p.stun.as_mut().expect("valid").request(true, true);
    s.handle_datagram(t0, p.tuple, &req);
    let t = s.poll_transmit().expect("487");
    assert_eq!(t.tuple, p.tuple);
    assert!(t.bytes.len() <= req.len());
    let m = Message::parse(&t.bytes).expect("valid");
    assert_eq!(m.msg_type, stun::BINDING_ERROR);
    assert_eq!(m.error.map(|e| e.0), Some(487));
    assert!(p.stun.as_ref().expect("valid").verify(&t.bytes));
    assert_eq!(s.selected(), None);
    assert!(!s.accepts(&p.tuple));
    // The publisher switches to controlling and goes on.
    p.connect(s, t0);
    assert_eq!(s.state(), State::Connected);
}

#[test]
fn use_candidate_moves_the_selected_tuple() {
    // USE-CANDIDATE moves the selected tuple; once media flows, sends go
    // where the latest fresh SRTP came from, and SRTP is taken from every
    // validated tuple.
    let (mut p, mut a, t0) = setup(7, &[CM80]);
    let s = &mut a.session;
    p.connect(s, t0);
    let first = p.tuple;
    assert_eq!(s.selected(), Some(first));
    assert_eq!(s.send_tuple(), Some(first));
    let _ = events(s);
    // A plain check from a new tuple validates it and moves nothing.
    let moved = tuple(41_000);
    let c = p.check(false);
    s.handle_datagram(t0 + ms(10), moved, &c);
    assert_eq!(s.selected(), Some(first));
    assert!(s.accepts(&moved));
    // SRTP from it is taken before it is selected, and sends follow it.
    for d in p.sender_reports(1 << 63) {
        s.handle_datagram(t0 + ms(10), moved, &d);
    }
    let (kf, _) = p.keyframe(500);
    for d in &kf {
        s.handle_datagram(t0 + ms(10), moved, d);
    }
    assert_eq!(s.stats().srtp_packets, kf.len() as u64);
    assert_eq!(s.send_tuple(), Some(moved));
    // From a tuple that never passed a check, it is not taken.
    let stranger = tuple(41_001);
    s.handle_datagram(t0 + ms(10), stranger, &p.audio(1));
    assert_eq!(s.stats().media_unvalidated, 1);
    assert_eq!(s.stats().srtp_packets, kf.len() as u64);
    // USE-CANDIDATE from it moves the selection there too.
    let c = p.check(true);
    s.handle_datagram(t0 + ms(20), moved, &c);
    assert_eq!(s.selected(), Some(moved));
    assert!(matches!(events(s).as_slice(), [Event::Nominated]));
    // Media still arriving on the old pair is not lost, and sends follow
    // the latest of it.
    let (kf2, _) = p.keyframe(500);
    for d in &kf2 {
        s.handle_datagram(t0 + ms(30), first, d);
    }
    assert_eq!(s.send_tuple(), Some(first));
    s.handle_datagram(t0 + ms(30), moved, &p.audio(2));
    assert_eq!(s.send_tuple(), Some(moved));
    assert_eq!(s.stats().srtp_packets, (kf.len() + kf2.len() + 1) as u64);
    // Everything went to one of the publisher's tuples, within the limit.
    run_until(s, t0 + ms(1600));
    while let Some(t) = s.poll_transmit() {
        assert!(t.tuple == first || t.tuple == moved);
    }
    for t in [first, moved] {
        let (received, sent) = s.tuple_bytes(&t).expect("validated");
        assert!(sent <= 4 * received, "{sent} for {received}");
    }
    assert_eq!(s.stats().budget_dropped, 0);
}

#[test]
fn consent_expires_30_s_after_the_last_check() {
    let (mut p, mut a, t0) = setup(8, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    // Media every second keeps the media timeout away; a check at 20 s
    // renews consent to 50 s.
    for second in 1..=49u64 {
        let at = t0 + Duration::from_secs(second);
        s.handle_datagram(at, p.tuple, &p.audio(second as u8));
        if second == 20 {
            let c = p.check(false);
            s.handle_datagram(at, p.tuple, &c);
        }
        run_until(s, at);
        p.receive(s);
    }
    assert_eq!(s.state(), State::Connected);
    run_until(s, t0 + Duration::from_secs(50));
    assert_eq!(s.state(), State::Ended(EndReason::ConsentExpired));
    assert!(
        events(s)
            .iter()
            .any(|e| matches!(e, Event::Ended(EndReason::ConsentExpired)))
    );
    // After the end: nothing sent, input ignored.
    p.receive(s);
    s.handle_datagram(t0 + Duration::from_secs(51), p.tuple, &p.check(false));
    assert!(s.poll_transmit().is_none());
    assert_eq!(s.poll_timeout(), None);
}

#[test]
fn handshake_deadline() {
    // No check at all.
    let (_, mut a, t0) = setup(9, &[GCM]);
    let s = &mut a.session;
    assert_eq!(s.poll_timeout(), Some(t0 + Duration::from_secs(10)));
    run_until(s, t0 + Duration::from_secs(10));
    assert_eq!(s.state(), State::Ended(EndReason::HandshakeTimeout));
    // Nominated, but DTLS never completes: the hello is answered and the
    // client's flight never comes.
    let (mut p, mut a, t0) = setup(10, &[GCM]);
    let s = &mut a.session;
    let c = p.check(true);
    s.handle_datagram(t0, p.tuple, &c);
    for d in p.client.start() {
        s.handle_datagram(t0, p.tuple, &d);
    }
    assert_eq!(s.state(), State::Handshaking);
    run_until(s, t0 + Duration::from_millis(9_999));
    assert_eq!(s.state(), State::Handshaking);
    run_until(s, t0 + Duration::from_secs(10));
    assert_eq!(s.state(), State::Ended(EndReason::HandshakeTimeout));
    p.receive(s);
    assert_eq!(p.misdirected, 0);
}

#[test]
fn fingerprint_mismatch_ends_the_session() {
    let t0 = Instant::now();
    let mut p = Publisher::new(11, &[GCM]);
    // The offer names another certificate than the one the client sends.
    let offer = synthetic_offer(&identity(99).fingerprint(), &p.ufrag);
    let mut a = accept(&offer, SessionConfig::default(), t0, 11);
    p.answered(&a);
    let s = &mut a.session;
    let c = p.check(true);
    s.handle_datagram(t0, p.tuple, &c);
    let mut pending = p.client.start();
    for _ in 0..8 {
        for d in std::mem::take(&mut pending) {
            s.handle_datagram(t0, p.tuple, &d);
        }
        pending = p.receive(s);
    }
    assert_eq!(
        s.state(),
        State::Ended(EndReason::Dtls(sapflow_dtls::Error::FingerprintMismatch))
    );
    assert!(!p.has_keys());
    assert_eq!(p.misdirected, 0);
}

#[test]
fn early_dtls_waits_for_nomination() {
    let (mut p, mut a, t0) = setup(12, &[GCM]);
    let s = &mut a.session;
    let c = p.check(false);
    s.handle_datagram(t0, p.tuple, &c);
    p.receive(s);
    let hello = p.client.start();
    for d in &hello {
        s.handle_datagram(t0, p.tuple, d);
    }
    assert!(s.poll_transmit().is_none(), "held, not answered");
    assert_eq!(s.stats().dtls_held, hello.len() as u64);
    let c = p.check(true);
    s.handle_datagram(t0, p.tuple, &c);
    // The STUN answer, then our first flight, at once.
    let replies = p.receive(s);
    assert!(
        !replies.is_empty(),
        "the server's flight arrived and the client answered"
    );
    for d in replies {
        s.handle_datagram(t0, p.tuple, &d);
    }
    p.receive(s);
    assert!(p.has_keys());
    assert_eq!(s.state(), State::Connected);
}

#[test]
fn close_and_peer_close() {
    let (mut p, mut a, t0) = setup(13, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    s.close();
    assert_eq!(s.state(), State::Ended(EndReason::Closed));
    let mut alerts = 0;
    while let Some(t) = s.poll_transmit() {
        assert_eq!(t.tuple, p.tuple);
        alerts += usize::from(t.bytes.first() == Some(&21));
    }
    assert_eq!(alerts, 1, "close_notify");
    s.close();

    let (mut p, mut a, t0) = setup(14, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    let alert = p.client.seal(21, &[1, 0]);
    s.handle_datagram(t0, p.tuple, &alert);
    assert_eq!(s.state(), State::Ended(EndReason::PeerClosed));
}

#[test]
fn oversized_and_unknown_datagrams_are_dropped() {
    let (mut p, mut a, t0) = setup(15, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    p.receive(s);
    s.handle_datagram(t0, p.tuple, &[0x80; 1501]);
    s.handle_datagram(t0, p.tuple, &[0x40; 100]);
    let st = s.stats();
    assert_eq!((st.oversized, st.other), (1, 1));
    // An RTP packet that fails SRTP authentication.
    let mut d = p.audio(1);
    let last = d.len() - 1;
    d[last] ^= 1;
    s.handle_datagram(t0, p.tuple, &d);
    assert_eq!(s.stats().srtp_refused, 1);
    assert!(s.poll_transmit().is_none());
}

#[test]
fn mid_matching_for_unnamed_ssrcs() {
    let t0 = Instant::now();
    let mut p = Publisher::new(16, &[GCM]);
    // No SSRCs in the offer, and a second video section that is rejected.
    let offer: String = p
        .offer()
        .split_inclusive("\r\n")
        .filter(|l| !l.starts_with("a=ssrc"))
        .collect::<String>()
        .replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0 1 2")
        + &format!(
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=mid:2\r\na=sendonly\r\n\
             a=rtcp-mux\r\na=extmap:{MID_ID} {}\r\na=rtpmap:96 H264/90000\r\n\
             a=fmtp:96 packetization-mode=1\r\n",
            crate::sdp::MID_URI
        );
    let mut a = accept(&offer, SessionConfig::default(), t0, 16);
    assert_eq!(a.negotiated.rejected_mids, ["2"]);
    p.answered(&a);
    let s = &mut a.session;
    p.connect(s, t0);
    // The rejected section's video arrives first, with the same payload
    // type: by payload type alone it would take the video stream.
    p.video_mid = b"2".to_vec();
    let idr = idr_slice(300, 1);
    let nals: [&[u8]; 3] = [SPS, PPS, &idr];
    let stray = p.video(&nals, 0x0bad);
    for d in &stray {
        s.handle_datagram(t0, p.tuple, d);
    }
    assert_eq!(s.stats().unmatched, stray.len() as u64);
    // An unknown MID is dropped too.
    p.video_mid = b"9".to_vec();
    let unknown = p.video(&nals, 0x0bae);
    for d in &unknown {
        s.handle_datagram(t0, p.tuple, d);
    }
    assert_eq!(s.stats().unmatched, (stray.len() + unknown.len()) as u64);
    // The accepted section's MID binds its SSRC.
    p.video_mid = b"1".to_vec();
    let (kf, _) = p.keyframe(300);
    for d in &kf {
        s.handle_datagram(t0, p.tuple, d);
    }
    let rx = s.receiver().expect("valid");
    let video = rx.ssrc(s.stream(MediaKind::Video).expect("valid"));
    assert_eq!(video, Some(VIDEO_SSRC));
    assert_eq!(s.stats().refused, 0);
}

#[test]
fn declared_ssrcs_win_over_the_mid() {
    // The offer names the video SSRC; its packets carry the MID of a
    // section answered with port 0. By SSRC first, they are video.
    let t0 = Instant::now();
    let mut p = Publisher::new(18, &[GCM]);
    let offer = p
        .offer()
        .replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0 1 2")
        + &format!(
            "m=video 9 UDP/TLS/RTP/SAVPF 98\r\nc=IN IP4 0.0.0.0\r\na=mid:2\r\na=sendonly\r\n\
             a=rtcp-mux\r\na=extmap:{MID_ID} {}\r\na=rtpmap:98 H264/90000\r\n",
            crate::sdp::MID_URI
        );
    let mut a = accept(&offer, SessionConfig::default(), t0, 18);
    assert_eq!(a.negotiated.rejected_mids, ["2"]);
    p.answered(&a);
    let s = &mut a.session;
    p.connect(s, t0);
    p.video_mid = b"2".to_vec();
    let (kf, _) = p.keyframe(300);
    for d in &kf {
        s.handle_datagram(t0, p.tuple, d);
    }
    let st = s.stats();
    assert_eq!((st.unmatched, st.refused), (0, 0));
    let video = s.stream(MediaKind::Video).expect("video");
    let rx = s.receiver().expect("receiver");
    assert_eq!(rx.stats(video).expect("stats").packets, kf.len() as u64);
}

#[test]
fn rtx_repairs_a_lost_packet() {
    let (mut p, mut a, t0) = setup(17, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    for d in p.sender_reports(1 << 62) {
        s.handle_datagram(t0, p.tuple, &d);
    }
    let first_seq = p.video_seq;
    let ts = p.video_ts;
    let (kf, want) = p.keyframe(3000);
    assert!(kf.len() >= 3, "STAP-A and FU-A");
    // Drop the second packet (the first FU-A fragment).
    for (i, d) in kf.iter().enumerate() {
        if i != 1 {
            s.handle_datagram(t0, p.tuple, d);
        }
    }
    s.handle_datagram(t0, p.tuple, &p.audio(1));
    run_until(s, t0 + ms(60));
    p.receive(s);
    let nacked = p.rtcp.iter().any(|c| rtcp_types(c).contains(&(205, 1)));
    assert!(nacked, "NACK sent");
    // Retransmit it over RTX.
    let idr = idr_slice(3000, 0x11);
    let payloads = packetise(&[SPS, PPS, &idr], 1100);
    let rtx = p.rtx(first_seq.wrapping_add(1), &payloads[1], ts, false);
    s.handle_datagram(t0 + ms(70), p.tuple, &rtx);
    run_until(s, t0 + ms(1700));
    let mut seen = Seen::default();
    collect(s, &mut seen);
    assert_eq!(seen.units.first().map(|u| u.0.clone()), Some(want));
    assert_eq!(seen.lost, 0);
}

/// Replays a recorded real-browser run (the example harness's `--record`)
/// offline: the same seeded session, our test STUN client validating a
/// documentation-range tuple, then the browser's DTLS datagrams in order.
/// Every datagram our DTLS sends must match the live run byte for byte,
/// the handshake must complete with the recorded profile, the peer's
/// leaf certificate must match the offer's fingerprint, and the browser's
/// first SRTP and SRTCP packets must authenticate.
fn replay(dir: &str) {
    use crate::Credentials;
    use crate::ice::{Kind, classify};
    use crate::sdp::{self, AnswerConfig, Offer};
    use sapflow_dtls::Fingerprint;
    let read =
        |f: &str| std::fs::read_to_string(crate_fixture(&format!("{dir}/{f}"))).expect("fixture");
    let offer = read("offer.sdp");
    let leaf = unhex(read("leaf.der.hex").trim());
    let summary = read("summary.txt");
    let profile = summary
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("profile "))
        .expect("profile");
    for leak in ["a=candidate", "127.0.0.1", "192.168.", ".local"] {
        assert!(!offer.contains(leak), "{leak}");
    }
    let parsed = Offer::parse(offer.as_bytes()).expect("offer parses");
    let creds = Credentials::generate_seeded(HARNESS_SEED).expect("credentials");
    let (ufrag, pwd) = (creds.ufrag().to_owned(), creds.pwd().to_owned());
    let (_, negotiated) = sdp::answer(
        &parsed,
        &AnswerConfig {
            credentials: &creds,
            fingerprint: server_identity().fingerprint(),
            candidates: &[],
            opus_max_bitrate: 128_000,
            origin: 1,
        },
    )
    .expect("answer");
    assert!(
        negotiated
            .fingerprints
            .contains(&Fingerprint::of_certificate(&leaf))
    );
    let t0 = Instant::now();
    let mut s = crate::Session::new_seeded(
        SessionConfig::default(),
        server_identity(),
        creds,
        negotiated.clone(),
        t0,
        HARNESS_SEED,
    )
    .expect("session");
    let from = tuple(5000);
    let mut stun = StunClient::new(&ufrag, &pwd, &negotiated.remote_ufrag);
    s.handle_datagram(t0, from, &stun.check(true));
    let mut now = t0;
    let mut ours = std::collections::VecDeque::new();
    let mut matched = 0;
    for line in read("dtls.txt").lines() {
        let (tag, rest) = line.split_at(1);
        match tag {
            "C" => s.handle_datagram(now, from, &unhex(rest.trim())),
            "T" => {
                now = s.poll_timeout().expect("a timer");
                s.handle_timeout(now);
            }
            "S" => {
                while ours.is_empty() {
                    let t = s.poll_transmit().expect("our DTLS datagram");
                    if classify(&t.bytes) == Kind::Dtls {
                        ours.push_back(t.bytes);
                    }
                }
                assert_eq!(
                    ours.pop_front(),
                    Some(unhex(rest.trim())),
                    "datagram {matched}"
                );
                matched += 1;
                continue;
            }
            other => panic!("{other}"),
        }
        while let Some(t) = s.poll_transmit() {
            if classify(&t.bytes) == Kind::Dtls {
                ours.push_back(t.bytes);
            }
        }
    }
    assert!(matched >= 2, "our flights were compared");
    assert_eq!(s.state(), State::Connected);
    assert_eq!(
        s.profile().map(|p| format!("{p:?}")).as_deref(),
        Some(profile)
    );
    let media = read("media.txt");
    let (mut rtp, mut rtcp) = (0, 0);
    for line in media.lines() {
        let (kind, hexed) = line.split_once(' ').expect("kind and bytes");
        s.handle_datagram(now, from, &unhex(hexed));
        match kind {
            "srtp" => rtp += 1,
            _ => rtcp += 1,
        }
    }
    let st = s.stats();
    assert!(rtp > 0 && rtcp > 0);
    assert_eq!(
        (st.srtp_packets, st.srtcp_packets),
        (rtp, rtcp),
        "the browser's SRTP authenticates"
    );
    assert_eq!((st.srtp_refused, st.srtcp_refused, st.malformed), (0, 0, 0));
}

#[test]
fn chrome_157_handshake_and_srtp_replay() {
    replay("chrome-157");
}

/// Bytes the session queued for `to`, split into STUN and everything else.
fn sent_to(s: &mut crate::Session, to: crate::Tuple) -> (Vec<usize>, usize) {
    let (mut stun, mut other) = (Vec::new(), 0);
    let mut rest = Vec::new();
    while let Some(t) = s.poll_transmit() {
        if t.tuple == to {
            if crate::ice::classify(&t.bytes) == crate::ice::Kind::Stun {
                stun.push(t.bytes.len());
            } else {
                other += t.bytes.len();
            }
        } else {
            rest.push(t);
        }
    }
    (stun, other)
}

/// M1: an observer re-sends a captured USE-CANDIDATE check with a
/// victim's address as its source. Nothing but a response no larger than
/// the request may reach the victim, and DTLS and RTCP must not follow.
#[test]
fn replayed_check_from_another_tuple_after_connect() {
    let (mut p, mut a, t0) = setup(19, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    p.receive(s);
    let victim = tuple(50_000);
    // A fresh nominating check from the publisher, captured on the way.
    let captured = p.check(true);
    s.handle_datagram(t0 + ms(10), p.tuple, &captured);
    p.receive(s);
    s.handle_datagram(t0 + ms(20), victim, &captured);
    let mut to_victim = sent_to(s, victim);
    // Time passes: receiver reports, PLIs and transport-cc are due.
    for d in p.sender_reports(1 << 62) {
        s.handle_datagram(t0 + ms(30), p.tuple, &d);
    }
    for step in 1..=40u64 {
        s.handle_datagram(t0 + ms(30 + 50 * step), p.tuple, &p.audio(step as u8));
        run_until(s, t0 + ms(30 + 50 * step));
        let (stun, other) = sent_to(s, victim);
        to_victim.0.extend(stun);
        to_victim.1 += other;
    }
    assert!(to_victim.0.iter().all(|n| *n <= captured.len()));
    assert_eq!(to_victim.1, 0, "DTLS or RTCP followed the replayed check");
}

/// M1 before SRTP: the replay lands mid-handshake, and DTLS
/// retransmissions must not follow it to the victim.
#[test]
fn replayed_check_from_another_tuple_mid_handshake() {
    let (mut p, mut a, t0) = setup(20, &[GCM]);
    let s = &mut a.session;
    let captured = p.check(true);
    s.handle_datagram(t0, p.tuple, &captured);
    for d in p.client.start() {
        s.handle_datagram(t0, p.tuple, &d);
    }
    p.receive(s);
    let victim = tuple(50_001);
    s.handle_datagram(t0 + ms(5), victim, &captured);
    let mut other = 0;
    let mut stun = Vec::new();
    for step in 1..=9u64 {
        run_until(s, t0 + Duration::from_secs(step));
        let (st, o) = sent_to(s, victim);
        stun.extend(st);
        other += o;
    }
    assert!(stun.iter().all(|n| *n <= captured.len()));
    assert_eq!(other, 0, "DTLS retransmissions followed the replayed check");
}

/// Before media, a nomination moves sends; once media flows, only fresh
/// media does, so a publisher's own move is followed by its next packet.
#[test]
fn sends_follow_fresh_media_once_it_flows() {
    let (mut p, mut a, t0) = setup(21, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    let first = p.tuple;
    s.handle_datagram(t0, first, &p.audio(1));
    assert_eq!(s.send_tuple(), Some(first));
    let second = tuple(41_200);
    let c = p.check(true);
    s.handle_datagram(t0 + ms(10), second, &c);
    assert_eq!(s.selected(), Some(second));
    assert_eq!(s.send_tuple(), Some(first), "no media from it yet");
    let (_, other) = sent_to(s, second);
    assert_eq!(other, 0);
    s.handle_datagram(t0 + ms(20), second, &p.audio(2));
    assert_eq!(s.send_tuple(), Some(second), "its first packet");
}

/// The attacker's copy of a check arrives before the original, after
/// SRTP is up. The original is then the repeat. Sends stay with the
/// proven tuple, and the publisher's next check takes the selection back.
#[test]
fn a_copy_that_beats_its_original_after_srtp() {
    let (mut p, mut a, t0) = setup(22, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    let home = p.tuple;
    for d in p.sender_reports(1 << 62) {
        s.handle_datagram(t0, home, &d);
    }
    p.receive(s);
    let victim = tuple(50_002);
    let captured = p.check(true);
    s.handle_datagram(t0 + ms(5), victim, &captured);
    assert_eq!(s.selected(), Some(victim));
    s.handle_datagram(t0 + ms(6), home, &captured);
    assert_eq!(s.stats().ice.replays, 1, "the original is now the repeat");
    let (stun, _) = sent_to(s, victim);
    assert!(stun.iter().all(|n| *n <= captured.len()));
    let mut to_victim = 0;
    for step in 1..=40u64 {
        let at = t0 + ms(10 + 50 * step);
        s.handle_datagram(at, home, &p.audio(step as u8));
        run_until(s, at);
        to_victim += sent_to(s, victim).1;
    }
    assert_eq!(to_victim, 0, "nothing but the STUN response to the victim");
    assert_eq!(s.send_tuple(), Some(home));
    assert!(s.stats().feedback_sent > 0);
    // The publisher's next check, a new transaction, recovers the selection.
    let fresh = p.check(true);
    s.handle_datagram(t0 + ms(2100), home, &fresh);
    assert_eq!(s.selected(), Some(home));
    assert_eq!(s.state(), State::Connected);
}

/// The attacker's copies of the check and the ClientHello arrive first,
/// mid-handshake. The victim gets no more than four times what the
/// attacker sent it, and the publisher's next check recovers the session.
#[test]
fn a_copy_that_beats_its_original_mid_handshake() {
    let (mut p, mut a, t0) = setup(23, &[GCM]);
    let s = &mut a.session;
    let victim = tuple(50_003);
    let check = p.check(true);
    let hello = p.client.start();
    let mut sent_by_attacker = check.len();
    s.handle_datagram(t0, victim, &check);
    for d in &hello {
        s.handle_datagram(t0, victim, d);
        sent_by_attacker += d.len();
    }
    // The originals, second, go nowhere.
    s.handle_datagram(t0, p.tuple, &check);
    for d in &hello {
        s.handle_datagram(t0, p.tuple, d);
    }
    let mut to_victim = 0;
    for step in 1..=9u64 {
        run_until(s, t0 + Duration::from_secs(step) / 2);
        let (stun, other) = sent_to(s, victim);
        to_victim += stun.iter().sum::<usize>() + other;
    }
    assert!(
        to_victim <= 4 * sent_by_attacker,
        "{to_victim} bytes to the victim for {sent_by_attacker}"
    );
    assert!(
        s.stats().budget_dropped > 0,
        "the retransmissions were held back"
    );
    // The publisher checks again and starts over from its own tuple.
    let fresh = p.check(true);
    s.handle_datagram(t0 + Duration::from_secs(5), p.tuple, &fresh);
    assert_eq!(s.send_tuple(), Some(p.tuple));
    // The server already answered the copied hello; the publisher's own
    // retransmission of it, now from the selected tuple, draws the flight.
    let mut pending = hello.clone();
    let mut now = t0 + Duration::from_secs(5);
    for _ in 0..8 {
        for d in std::mem::take(&mut pending) {
            s.handle_datagram(now, p.tuple, &d);
        }
        while let Some(t) = s.poll_transmit() {
            if t.tuple == p.tuple
                && crate::ice::classify(&t.bytes) == crate::ice::Kind::Dtls
                && let Ok(r) = p.client.handle(&t.bytes)
            {
                pending.extend(r);
            }
        }
        now += ms(300);
    }
    assert!(p.client.connected(), "the publisher's handshake completes");
    assert_eq!(s.state(), State::Connected);
}

/// The publisher moves pairs mid-handshake: our first flight went to the
/// first tuple, and the rest of the handshake runs on the second.
#[test]
fn an_honest_move_mid_handshake_connects() {
    let (mut p, mut a, t0) = setup(24, &[GCM]);
    let s = &mut a.session;
    let c = p.check(true);
    s.handle_datagram(t0, p.tuple, &c);
    for d in p.client.start() {
        s.handle_datagram(t0, p.tuple, &d);
    }
    let flight5 = p.receive(s);
    assert!(
        !flight5.is_empty(),
        "our first flight reached the first tuple"
    );
    p.tuple = tuple(41_300);
    let c = p.check(true);
    s.handle_datagram(t0 + ms(30), p.tuple, &c);
    assert_eq!(s.send_tuple(), Some(p.tuple));
    for d in flight5 {
        s.handle_datagram(t0 + ms(31), p.tuple, &d);
    }
    p.receive(s);
    assert!(p.has_keys(), "our last flight reached the second tuple");
    assert_eq!(s.state(), State::Connected);
    assert_eq!(s.stats().budget_dropped, 0);
}

/// Every captured W0 ClientHello, after one check, draws our first flight
/// at once within the default limit; with a limit of 1, Safari's would
/// wait for retransmissions.
#[test]
fn the_limit_admits_every_w0_hello_after_one_check() {
    for &(dir, stem, _) in sapflow_dtls::testing::CAPTURES {
        let (mut p, mut a, t0) = setup(25, &[GCM]);
        let s = &mut a.session;
        s.handle_datagram(t0, p.tuple, &p.check(true));
        for d in sapflow_dtls::testing::capture(dir, stem) {
            s.handle_datagram(t0, p.tuple, &d);
        }
        let (_, flight) = sent_to(s, p.tuple);
        assert!(flight >= 600, "{dir}/{stem}: {flight}");
        assert_eq!(s.stats().budget_dropped, 0, "{dir}/{stem}");
    }
    let t0 = Instant::now();
    let p = Publisher::new(26, &[GCM]);
    let config = SessionConfig {
        amplification_limit: 1,
        ..SessionConfig::default()
    };
    let mut a = accept(&p.offer(), config, t0, 26);
    let mut p = p;
    p.answered(&a);
    let s = &mut a.session;
    s.handle_datagram(t0, p.tuple, &p.check(true));
    for d in sapflow_dtls::testing::capture("safari-26.6.2-macos", "clienthello") {
        s.handle_datagram(t0, p.tuple, &d);
    }
    assert_eq!(sent_to(s, p.tuple).1, 0);
    assert!(s.stats().budget_dropped > 0);
}

/// After the publisher has gone, replays of checks the agent has forgotten
/// count as new, from its old tuple or a victim's, and renew consent. What
/// still stops sending is the media timeout: a copy of SRTP is fresh only
/// if it beats its original or the original was lost, so once the
/// publisher stops, nothing new counts.
#[test]
fn forgotten_replays_cannot_outlive_the_media() {
    let t0 = Instant::now();
    let mut p = Publisher::new(27, &[GCM]);
    let mut config = SessionConfig::default();
    config.ice.max_transactions = 16;
    let mut a = accept(&p.offer(), config, t0, 27);
    p.answered(&a);
    let s = &mut a.session;
    p.connect(s, t0);
    let home = p.tuple;
    let victim = tuple(50_004);
    // Ten seconds of publishing, a check every half second, all captured.
    let mut captured = Vec::new();
    for step in 1..=20u64 {
        let at = t0 + ms(500 * step);
        s.handle_datagram(at, home, &p.audio(step as u8));
        let c = p.check(true);
        s.handle_datagram(at, home, &c);
        captured.push(c);
        run_until(s, at);
        p.receive(s);
    }
    let gone = t0 + ms(10_000);
    let successes = s.stats().ice.successes;
    // Every 5 s, an old check from the publisher's tuple or the victim's.
    let mut to_victim = (Vec::new(), 0);
    for (i, old) in captured.iter().take(6).enumerate() {
        let at = gone + Duration::from_secs(5 * (i as u64 + 1)) - ms(1);
        run_until(s, at);
        p.receive(s);
        let from = if i % 2 == 0 { home } else { victim };
        s.handle_datagram(at, from, old);
        let (stun, other) = sent_to(s, victim);
        to_victim.0.extend(stun);
        to_victim.1 += other;
        p.receive(s);
    }
    assert_eq!(
        s.stats().ice.successes,
        successes + 6,
        "forgotten IDs are new checks"
    );
    assert!(to_victim.0.iter().all(|n| *n <= captured[0].len()));
    assert_eq!(to_victim.1, 0, "nothing else to the victim");
    assert_eq!(s.send_tuple(), Some(home));
    run_until(s, gone + Duration::from_secs(30) - ms(1));
    assert_eq!(s.state(), State::Connected);
    run_until(s, gone + Duration::from_secs(30));
    assert_eq!(s.state(), State::Ended(EndReason::MediaTimeout));
    p.receive(s);
    // From then on, nothing, whatever arrives.
    s.handle_datagram(gone + Duration::from_secs(31), home, &captured[7]);
    run_until(s, gone + Duration::from_secs(60));
    assert!(s.poll_transmit().is_none());
}

/// Milestones always get through: many moves before anything is polled
/// leave one Nominated, and Connected still follows.
#[test]
fn milestones_are_never_dropped() {
    let (mut p, mut a, t0) = setup(28, &[GCM]);
    let s = &mut a.session;
    let home = p.tuple;
    for i in 0..40u16 {
        let c = p.check(true);
        s.handle_datagram(t0, tuple(42_000 + i % 2), &c);
    }
    p.connect(s, t0);
    assert_eq!(s.selected(), Some(home));
    let ev = events(s);
    assert!(
        matches!(ev.as_slice(), [Event::Nominated, Event::Connected(_)]),
        "{ev:?}"
    );
}

/// One won race, then a publisher that keeps sending but never nominates
/// again. Returns (bytes sent to B, bytes received from B, the time sends
/// went back to the publisher's tuple if they did, the session's state).
fn won_race(seed: u8) -> (usize, usize, Option<Duration>, State) {
    let (mut p, mut a, t0) = setup(seed, &[GCM]);
    let s = &mut a.session;
    p.connect(s, t0);
    let home = p.tuple;
    for d in p.sender_reports(1 << 62) {
        s.handle_datagram(t0, home, &d);
    }
    s.handle_datagram(t0, home, &p.audio(0));
    let b = tuple(50_010);
    let (mut from_b, mut to_b) = (0usize, 0usize);
    let drain = |s: &mut crate::Session, to_b: &mut usize| {
        let mut home_bytes = 0;
        while let Some(t) = s.poll_transmit() {
            if t.tuple == b {
                *to_b += t.bytes.len();
            } else if t.tuple == home {
                home_bytes += t.bytes.len();
            }
        }
        home_bytes
    };
    // The copy of a nominating check, then of an SRTP packet, each first.
    let at = t0 + ms(100);
    let check = p.check(true);
    s.handle_datagram(at, b, &check);
    from_b += check.len();
    s.handle_datagram(at, home, &check);
    let srtp = p.audio(1);
    s.handle_datagram(at, b, &srtp);
    from_b += srtp.len();
    s.handle_datagram(at, home, &srtp);
    drain(s, &mut to_b);
    // Sixty seconds: audio every 20 ms, sender reports every second and a
    // check without USE-CANDIDATE every 2 s, all from home.
    let mut back = None;
    let mut home_after = 0;
    for step in 1..=3000u64 {
        let at = t0 + ms(100 + 20 * step);
        s.handle_datagram(at, home, &p.audio(step as u8));
        if step % 50 == 0 {
            for d in p.sender_reports((1 << 62) + step) {
                s.handle_datagram(at, home, &d);
            }
        }
        if step % 100 == 0 {
            s.handle_datagram(at, home, &p.check(false));
        }
        run_until(s, at);
        home_after += drain(s, &mut to_b);
        if back.is_none() && s.send_tuple() == Some(home) {
            back = Some(at - (t0 + ms(100)));
        }
    }
    assert!(home_after > 0, "the publisher still gets our RTCP");
    (to_b, from_b, back, s.state())
}

/// (a) Over 60 s, B is sent at most four times what it sent.
#[test]
fn a_won_race_cannot_lift_the_budget() {
    let (to_b, from_b, _, _) = won_race(29);
    println!("won race: {to_b} bytes to B for {from_b} from it");
    assert!(to_b <= 4 * from_b, "{to_b} bytes to B for {from_b} from it");
}

/// (b) Sends go back to the publisher's tuple at its next fresh packet,
/// here 20 ms of audio, and the session stays up.
#[test]
fn sends_return_to_the_publisher_after_a_won_race() {
    let (_, _, back, state) = won_race(30);
    println!("won race: sends back with the publisher after {back:?}, state {state:?}");
    assert_eq!(state, State::Connected);
    let back = back.expect("sends went back to the publisher");
    assert!(back <= ms(20), "{back:?}");
}

/// An honest low-rate publisher on an audio-only offer, for 60 s: audio
/// every 20 ms, or muted from the start (sender reports every 5 s only),
/// with a check every 2.5 s. Returns the peak ratio of bytes sent to
/// bytes received for its tuple, the budget drops and the final state.
fn low_rate(muted: bool, seed: u8) -> (f64, u64, State) {
    let t0 = Instant::now();
    let mut p = Publisher::new(seed, &[GCM]);
    let offer = p.offer().replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0");
    let mut a = accept(&offer, SessionConfig::default(), t0, seed);
    p.answered(&a);
    assert!(a.negotiated.video.is_none());
    let s = &mut a.session;
    let home = p.tuple;
    let mut peak = 0f64;
    let mut track = |s: &crate::Session| {
        if let Some((received, sent)) = s.tuple_bytes(&home) {
            peak = peak.max(sent as f64 / received.max(1) as f64);
        }
    };
    // The handshake a datagram at a time, so the peak after our first
    // flight is seen.
    s.handle_datagram(t0, home, &p.check(true));
    track(s);
    let mut pending = p.client.start();
    for _ in 0..8 {
        for d in std::mem::take(&mut pending) {
            s.handle_datagram(t0, home, &d);
            track(s);
        }
        pending = p.receive(s);
    }
    assert!(p.has_keys());
    for step in 1..=3000u64 {
        let at = t0 + ms(20 * step);
        if !muted {
            s.handle_datagram(at, home, &p.audio(step as u8));
        }
        if step % 250 == 0 {
            for d in p.sender_reports((1 << 62) + step) {
                s.handle_datagram(at, home, &d);
            }
        }
        if step % 125 == 0 {
            s.handle_datagram(at, home, &p.check(false));
        }
        run_until(s, at);
        track(s);
        p.receive(s);
    }
    (peak, s.stats().budget_dropped, s.state())
}

#[test]
fn honest_low_rate_publishers_are_never_capped() {
    for muted in [false, true] {
        let (peak, dropped, state) = low_rate(muted, 31 + u8::from(muted));
        println!("low rate, muted {muted}: peak sent/received {peak:.2}, dropped {dropped}");
        assert!(peak <= 4.0, "{peak}");
        assert_eq!(dropped, 0);
        assert_eq!(state, State::Connected);
    }
}

/// Round 3: a session old enough that checks are forgotten. A forgotten
/// nominating copy selects B, one SRTP packet whose original was lost
/// arrives from B, and forgotten copies from new tuples fill the table.
/// Returns the publisher's packets dropped as from an unvalidated tuple,
/// and how long after the attack sends were back with the publisher.
fn eviction_attack(seed: u8) -> (u64, Option<Duration>) {
    let t0 = Instant::now();
    let mut p = Publisher::new(seed, &[GCM]);
    let mut config = SessionConfig::default();
    config.ice.max_transactions = 16;
    let mut a = accept(&p.offer(), config, t0, seed);
    p.answered(&a);
    let s = &mut a.session;
    p.connect(s, t0);
    let home = p.tuple;
    let mut at = t0;
    let mut captured = Vec::new();
    for step in 1..=24u64 {
        at = t0 + ms(20 * step);
        s.handle_datagram(at, home, &p.audio(step as u8));
        let c = p.check(true);
        s.handle_datagram(at, home, &c);
        captured.push(c);
        run_until(s, at);
        p.receive(s);
    }
    assert_eq!(s.send_tuple(), Some(home));
    // The attack, all at one instant.
    let b = tuple(50_020);
    s.handle_datagram(at, b, &captured[0]);
    assert_eq!(s.selected(), Some(b), "a forgotten ID needs no race");
    let lost = p.audio(100);
    s.handle_datagram(at, b, &lost);
    assert_eq!(s.send_tuple(), Some(b));
    for (i, c) in captured.iter().enumerate().skip(1).take(7) {
        s.handle_datagram(at, tuple(51_000 + i as u16), c);
    }
    let before = s.stats().media_unvalidated;
    // The publisher goes on sending audio, and its next check is 2 s away.
    let mut back = None;
    for step in 1..=100u64 {
        let now = at + ms(20 * step);
        s.handle_datagram(now, home, &p.audio(step as u8));
        if back.is_none() && s.send_tuple() == Some(home) {
            back = Some(now - at);
        }
        run_until(s, now);
        while s.poll_transmit().is_some() {}
    }
    (s.stats().media_unvalidated - before, back)
}

#[test]
fn a_publisher_delivering_media_is_never_evicted() {
    let (dropped, back) = eviction_attack(33);
    println!("eviction attack: {dropped} publisher packets dropped, sends back after {back:?}");
    assert_eq!(dropped, 0, "the publisher's media was dropped");
    assert!(back.is_some_and(|b| b <= ms(20)), "{back:?}");
}

/// A DTLS record as the publisher's address would send it.
fn dtls_record(content_type: u8, epoch: u16, seq: u64, body: &[u8]) -> Vec<u8> {
    let mut r = vec![content_type, 0xfe, 0xfd];
    r.extend_from_slice(&epoch.to_be_bytes());
    r.extend_from_slice(&seq.to_be_bytes()[2..]);
    r.extend_from_slice(&(body.len() as u16).to_be_bytes());
    r.extend_from_slice(body);
    r
}

/// Round 3, step 3: input that has not authenticated, from the selected
/// tuple after Connected, must not end the session (RFC 6347 section
/// 4.1.2.7: invalid records are discarded).
#[test]
fn unauthenticated_dtls_after_connect_is_discarded() {
    type Case = (&'static str, fn(&mut Publisher) -> Vec<u8>);
    let cases: [Case; 4] = [
        ("epoch 0 fatal alert", |_| dtls_record(21, 0, 40, &[2, 40])),
        ("epoch 1 record with a bad tag", |p| {
            let mut r = p.client.seal(21, &[2, 40]);
            let last = r.len() - 1;
            r[last] ^= 1;
            r
        }),
        ("junk handshake record", |_| {
            dtls_record(22, 0, 41, &[0xff; 24])
        }),
        ("fresh ClientHello", |_| {
            let mut other = sapflow_dtls::testing::Client::new(
                sapflow_dtls::testing::Options::default(),
                identity(77),
                77,
            );
            other.start().concat()
        }),
    ];
    for (i, (name, make)) in cases.into_iter().enumerate() {
        let (mut p, mut a, t0) = setup(40 + i as u8, &[GCM]);
        let s = &mut a.session;
        p.connect(s, t0);
        let _ = events(s);
        let d = make(&mut p);
        s.handle_datagram(t0 + ms(5), p.tuple, &d);
        let ev = events(s);
        println!(
            "{name}: state {:?}, events {ev:?}, dtls {:?}",
            s.state(),
            s.stats().dtls
        );
        assert_eq!(s.state(), State::Connected, "{name}");
        assert!(!ev.iter().any(|e| matches!(e, Event::Ended(_))), "{name}");
    }
}
