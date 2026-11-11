use super::*;
use crate::ice::{Credentials, Transport};
use crate::testing::{fixture, synthetic_offer};
use std::net::SocketAddr;

const OURS: [u8; 32] = [0xab; 32];

fn creds() -> Credentials {
    Credentials::generate_seeded([9; 32]).expect("valid")
}

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            transport: Transport::Udp,
            address: "192.0.2.10:47210".parse::<SocketAddr>().expect("valid"),
        },
        Candidate {
            transport: Transport::Tcp,
            address: "192.0.2.10:47211".parse::<SocketAddr>().expect("valid"),
        },
    ]
}

fn negotiate(text: &str) -> Result<(String, Negotiated), Refusal> {
    let offer = Offer::parse(text.as_bytes())?;
    let c = creds();
    let cands = candidates();
    let config = AnswerConfig {
        credentials: &c,
        fingerprint: Fingerprint::from_bytes(OURS),
        candidates: &cands,
        opus_max_bitrate: 128_000,
        origin: 42,
    };
    Ok(answer(&offer, &config)?)
}

fn fp(s: &str) -> Fingerprint {
    Fingerprint::parse_sdp(s).expect("valid")
}

/// What one fixture's answer must accept.
struct Expect {
    group: &'static str,
    ufrag: &'static str,
    fingerprint: &'static str,
    setup: Setup,
    video: (u8, Option<u8>, H264Profile, [bool; 4]),
    video_ssrcs: (Option<u32>, Option<u32>),
    audio: (u8, bool),
    audio_ssrc: Option<u32>,
    mid: Option<u8>,
    twcc: Option<u8>,
}

/// Checks the answer text line by line, as a browser would read it.
fn check_answer(sdp: &str, n: &Negotiated, e: &Expect) {
    let c = creds();
    let back = Offer::parse(sdp.as_bytes()).expect("our answer parses");
    assert!(back.ice_lite, "a=ice-lite at session level");
    assert_eq!(
        back.bundles,
        [e.group.split(' ').map(String::from).collect::<Vec<_>>()]
    );
    assert!(sdp.ends_with("\r\n") && !sdp.replace("\r\n", "").contains('\n'));
    // Nothing from the offer's candidates or addresses comes back.
    for leak in [
        "203.0.113",
        "2001:db8",
        "mdns",
        "network-id",
        "tcptype active",
    ] {
        assert!(!sdp.contains(leak), "{leak}");
    }
    assert_eq!(sdp.matches("a=candidate:").count(), 2);
    assert!(sdp.contains("a=candidate:1 1 udp 2130706431 192.0.2.10 47210 typ host\r\n"));
    assert!(sdp.contains(" 1 tcp ") && sdp.contains("47211 typ host tcptype passive\r\n"));
    assert_eq!(sdp.matches("a=end-of-candidates").count(), 1);
    let ours = Fingerprint::from_bytes(OURS).to_sdp();
    for s in &back.sections {
        assert_ne!(s.port, 0);
        assert_eq!(back.direction(s), Direction::RecvOnly);
        assert_eq!(back.setup(s), Some(Setup::Passive));
        assert_eq!(back.ice_ufrag(s), Some(c.ufrag()));
        assert!(sdp.contains(&format!("a=ice-pwd:{}\r\n", c.pwd())));
        assert_eq!(back.fingerprints(s).0, [Fingerprint::from_bytes(OURS)]);
        assert!(s.rtcp_mux);
        for x in &s.extmaps {
            assert!(x.uri == MID_URI || x.uri == TRANSPORT_CC_URI, "{}", x.uri);
        }
        assert_eq!(s.extmap(MID_URI).map(|x| x.id), e.mid);
        match s.kind {
            MediaKind::Video => {
                let (pt, rtx, _, fb) = e.video;
                let formats: Vec<String> = [Some(pt), rtx]
                    .iter()
                    .flatten()
                    .map(u8::to_string)
                    .collect();
                assert_eq!(s.formats, formats);
                for (on, name) in fb
                    .iter()
                    .zip(["nack", "nack pli", "ccm fir", "transport-cc"])
                {
                    assert_eq!(s.has_feedback(pt, name), *on, "{name}");
                }
                assert_eq!(s.rtcp_fbs.len(), fb.iter().filter(|x| **x).count());
                assert_eq!(
                    fmtp_param(s.fmtp(pt).expect("valid"), "packetization-mode"),
                    Some("1")
                );
                if let Some(r) = rtx {
                    assert_eq!(
                        fmtp_param(s.fmtp(r).expect("valid"), "apt"),
                        Some(&*pt.to_string())
                    );
                }
                assert_eq!(
                    s.extmap(TRANSPORT_CC_URI).map(|x| x.id),
                    e.twcc.filter(|_| fb[3])
                );
            }
            MediaKind::Audio => {
                let (pt, twcc) = e.audio;
                assert_eq!(s.formats, [pt.to_string()]);
                assert_eq!(
                    s.fmtp(pt),
                    Some("stereo=1;maxaveragebitrate=128000;usedtx=0")
                );
                assert_eq!(s.rtpmap(pt).map(|r| r.channels), Some(Some(2)));
                assert_eq!(s.has_feedback(pt, "transport-cc"), twcc);
                assert_eq!(
                    s.extmap(TRANSPORT_CC_URI).map(|x| x.id),
                    e.twcc.filter(|_| twcc)
                );
            }
            k => panic!("unexpected {k:?}"),
        }
    }
    assert!(sdp.contains(&format!("a=fingerprint:sha-256 {ours}\r\n")));
    // What the session is given.
    assert_eq!(n.remote_ufrag, e.ufrag);
    assert_eq!(n.fingerprints, [fp(e.fingerprint)]);
    assert_eq!(n.setup, e.setup);
    let v = n.video.as_ref().expect("video");
    let (pt, rtx, profile, fb) = e.video;
    assert_eq!(
        (v.payload_type, v.rtx_payload_type, v.profile),
        (pt, rtx, Some(profile))
    );
    assert_eq!([v.nack, v.pli, v.fir, v.transport_cc], fb);
    assert_eq!((v.ssrc, v.rtx_ssrc), e.video_ssrcs);
    let a = n.audio.as_ref().expect("audio");
    assert_eq!((a.payload_type, a.transport_cc), e.audio);
    assert_eq!(a.ssrc, e.audio_ssrc);
    assert!(!a.nack && !a.pli && !a.fir && a.rtx_payload_type.is_none());
    assert_eq!((n.mid_extension, n.transport_cc_extension), (e.mid, e.twcc));
    assert!(n.rejected_mids.is_empty() && n.rejected_ssrcs.is_empty());
}

const ALL_FB: [bool; 4] = [true; 4];

#[test]
fn chrome_157() {
    let chrome = Expect {
        group: "0 1",
        ufrag: "c1gA",
        fingerprint: "4F:10:D9:17:BB:B1:B2:85:6D:81:98:90:44:3D:D8:1B:79:1D:AD:E3:17:C2:0C:13:D2:5D:34:B2:3D:92:8A:6D",
        setup: Setup::ActPass,
        video: (102, Some(103), H264Profile::Baseline, ALL_FB),
        video_ssrcs: (Some(2_985_303_976), Some(1_662_177_943)),
        audio: (111, true),
        audio_ssrc: Some(2_516_437_313),
        mid: Some(4),
        twcc: Some(3),
    };
    let (sdp, n) = negotiate(&fixture("chrome-157-desktop-macos/offer.sdp")).expect("valid");
    check_answer(&sdp, &n, &chrome);
    // The TCP run's offer has the same shape and other credentials.
    let (sdp, n) =
        negotiate(&fixture("chrome-157-desktop-macos/offer-tcp-run.sdp")).expect("valid");
    let v = n.video.as_ref().expect("valid");
    assert_eq!((v.payload_type, v.rtx_payload_type), (102, Some(103)));
    assert!(sdp.contains("a=group:BUNDLE 0 1\r\n"));
}

#[test]
fn firefox_160() {
    // Session-level fingerprint and a=sendrecv with sendonly sections,
    // extmap direction suffixes, fmtp keys in another order, mDNS
    // candidates and component 2 candidates.
    let firefox = Expect {
        group: "0 1",
        ufrag: "fc040ade",
        fingerprint: "0B:8A:A7:97:7C:0B:2C:2D:7E:DE:69:40:84:DA:1B:17:48:32:99:41:C5:34:B3:AD:B4:F2:6D:09:6C:F5:C6:57",
        setup: Setup::ActPass,
        video: (126, Some(127), H264Profile::ConstrainedBaseline, ALL_FB),
        video_ssrcs: (Some(2_287_091_134), Some(931_427_898)),
        audio: (109, true),
        audio_ssrc: Some(2_581_646_213),
        mid: Some(3),
        twcc: Some(7),
    };
    let (sdp, n) = negotiate(&fixture("firefox-160-desktop-macos/offer.sdp")).expect("valid");
    check_answer(&sdp, &n, &firefox);
}

#[test]
fn safari_macos_and_ios() {
    let mut safari = Expect {
        group: "0 1",
        ufrag: "oMBT",
        fingerprint: "10:96:09:59:8F:B1:24:25:8B:62:45:62:A8:C6:C7:97:3C:3A:D5:C7:64:A4:77:3D:F3:76:4B:61:B5:28:14:FC",
        setup: Setup::ActPass,
        // 640c1f (Constrained High) first, video before audio.
        video: (96, Some(97), H264Profile::High, ALL_FB),
        video_ssrcs: (Some(2_795_376_074), Some(2_183_124_677)),
        audio: (111, true),
        audio_ssrc: Some(1_967_218_180),
        mid: Some(9),
        twcc: Some(4),
    };
    let (sdp, n) = negotiate(&fixture("safari-26.6.2-macos/offer.sdp")).expect("valid");
    check_answer(&sdp, &n, &safari);
    let (_, n) = negotiate(&fixture("safari-26.6.2-macos/offer-tcp-run.sdp")).expect("valid");
    assert_eq!(n.video.expect("valid").payload_type, 96);
    safari.ufrag = "s229";
    safari.fingerprint = "18:B8:D8:5F:BA:21:73:B2:56:52:71:A2:CF:02:E2:93:4E:1B:10:AF:64:83:5C:0A:74:04:1D:9A:5E:89:9A:F6";
    safari.video_ssrcs = (Some(1_368_388_419), Some(2_015_727_447));
    safari.audio_ssrc = Some(1_499_826_845);
    let (sdp, n) = negotiate(&fixture("safari-27-ios/offer.sdp")).expect("valid");
    check_answer(&sdp, &n, &safari);
}

#[test]
fn obs_32() {
    // Session-level transport, a=group:LS, no RTX, FIR, transport-cc or
    // extmaps, and no candidates.
    let obs = Expect {
        group: "0 1",
        ufrag: "Ogbk",
        fingerprint: "E1:6A:FE:61:01:CD:76:A9:79:15:88:EE:E0:B0:BC:1E:AC:A1:07:33:24:F7:07:08:44:22:2C:E3:FA:A2:79:1E",
        setup: Setup::ActPass,
        video: (
            96,
            None,
            H264Profile::ConstrainedBaseline,
            [true, true, false, false],
        ),
        video_ssrcs: (Some(3_993_480_101), None),
        audio: (111, false),
        audio_ssrc: Some(3_993_480_100),
        mid: None,
        twcc: None,
    };
    let (sdp, n) = negotiate(&fixture("obs-32.0.4-macos/offer.sdp")).expect("valid");
    check_answer(&sdp, &n, &obs);
    let (_, n) = negotiate(&fixture("obs-32.0.4-macos/offer-tcp-run.sdp")).expect("valid");
    assert_eq!(n.video.expect("valid").payload_type, 96);
}

#[test]
fn ffmpeg_8_1() {
    let ffmpeg = Expect {
        group: "0 1",
        ufrag: "e1cfc3fe",
        fingerprint: "6F:CC:DD:9D:73:C4:E2:F0:1A:2B:33:D7:85:AB:CF:12:2D:B8:76:DF:81:57:CF:99:18:EE:E5:C8:F1:CE:A5:11",
        setup: Setup::Active,
        video: (
            106,
            Some(105),
            H264Profile::Baseline,
            [true, false, false, false],
        ),
        video_ssrcs: (Some(3_543_159_207), Some(3_543_159_208)),
        audio: (111, false),
        audio_ssrc: Some(3_543_159_206),
        mid: None,
        twcc: None,
    };
    let (sdp, n) =
        negotiate(&fixture("ffmpeg-8.1-reference/offer-dtls-active.sdp")).expect("valid");
    check_answer(&sdp, &n, &ffmpeg);
    // FFmpeg's default offers only the passive role: it parses, and is
    // outside the profile.
    let text = fixture("ffmpeg-8.1-reference/offer-default.sdp");
    assert!(Offer::parse(text.as_bytes()).is_ok());
    let refusal = negotiate(&text).expect_err("refused");
    assert_eq!(refusal, Refusal::Profile(ProfileError::Setup));
    assert_eq!(refusal.status(), 422);
}

#[test]
fn vanadium_157() {
    let vanadium = Expect {
        group: "0 1",
        ufrag: "DguX",
        fingerprint: "B6:03:05:76:09:B1:60:30:05:FC:C3:E4:32:DB:B1:5F:F6:DE:5E:47:23:71:22:06:EA:D6:E2:C2:E8:2D:EF:BC",
        setup: Setup::ActPass,
        video: (100, Some(101), H264Profile::Baseline, ALL_FB),
        video_ssrcs: (Some(1_200_957_387), Some(1_903_710_147)),
        audio: (111, true),
        audio_ssrc: Some(2_837_612_319),
        mid: Some(4),
        twcc: Some(3),
    };
    let (sdp, n) = negotiate(&fixture(
        "vanadium-157-android-grapheneos/offer-tcp-run.sdp",
    ))
    .expect("valid");
    check_answer(&sdp, &n, &vanadium);
    let (_, n) = negotiate(&fixture(
        "vanadium-157-android-grapheneos/offer-udp-run.sdp",
    ))
    .expect("valid");
    let v = n.video.expect("valid");
    assert_eq!((v.payload_type, v.rtx_payload_type), (100, Some(101)));
    assert!(v.ssrc.is_some() && n.audio.expect("valid").ssrc.is_some());
}

#[test]
fn every_w0_offer_parses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/whip/w0");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).expect("valid") {
        for f in std::fs::read_dir(entry.expect("valid").path()).expect("valid") {
            let p = f.expect("valid").path();
            if p.extension().is_some_and(|e| e == "sdp") {
                let text = std::fs::read(&p).expect("valid");
                let offer = Offer::parse(&text).unwrap_or_else(|e| panic!("{p:?}: {e:?}"));
                // The parsed offer holds no address.
                let dbg = format!("{offer:?}");
                for leak in ["203.0.113", "2001:db8", "mdns", "0.0.0.0"] {
                    assert!(!dbg.contains(leak), "{p:?} keeps {leak}");
                }
                n += 1;
            }
        }
    }
    assert_eq!(n, 12, "every W0 offer");
}

fn base() -> String {
    synthetic_offer(&Fingerprint::from_bytes([0x11; 32]), "abcd")
}

fn refused(text: &str) -> Refusal {
    negotiate(text).expect_err("refused")
}

#[test]
fn synthetic_offer_is_accepted() {
    let (sdp, n) = negotiate(&base()).expect("valid");
    assert!(sdp.contains("a=group:BUNDLE 0 1\r\n"));
    assert_eq!(n.video.as_ref().expect("valid").payload_type, 96);
    assert_eq!(n.video.as_ref().expect("valid").ssrc, Some(0x2001));
}

#[test]
fn profile_refusals() {
    use ProfileError as P;
    let b = base();
    let cases: Vec<(String, P)> = vec![
        (b.replace("a=group:BUNDLE 0 1\r\n", ""), P::NoBundle),
        (
            b.replace(
                "a=group:BUNDLE 0 1\r\n",
                "a=group:BUNDLE 0\r\na=group:BUNDLE 1\r\n",
            ),
            P::SeveralBundles,
        ),
        (
            b.replace("t=0 0\r\n", "t=0 0\r\na=ice-lite\r\n"),
            P::PeerIceLite,
        ),
        (b.replace("H264/90000", "VP8/90000"), P::NoH264),
        (
            b.replace("packetization-mode=1", "packetization-mode=0"),
            P::NoH264,
        ),
        (
            b.replace("profile-level-id=42e01f", "profile-level-id=6e001f"),
            P::NoH264,
        ),
        (b.replace("opus/48000/2", "PCMU/8000"), P::NoOpus),
        (b.replace("opus/48000/2", "opus/48000"), P::NoOpus),
        (b.replace("a=sendonly", "a=recvonly"), P::NoMedia),
        (
            b.replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 7"),
            P::NoMedia,
        ),
        (strip(&b, "a=ice-ufrag:"), P::NoIceCredentials),
        (strip(&b, "a=ice-pwd:"), P::NoIceCredentials),
        (strip(&b, "a=fingerprint:"), P::NoFingerprint),
        (
            b.replace("a=fingerprint:sha-256", "a=fingerprint:sha-1"),
            P::NoFingerprint,
        ),
        (b.replace("a=setup:actpass", "a=setup:passive"), P::Setup),
        (b.replace("a=setup:actpass", "a=setup:holdconn"), P::Setup),
        (strip(&b, "a=setup:"), P::NoSetup),
        (strip(&b, "a=rtcp-mux"), P::NoRtcpMux),
        (
            b.replacen("a=ice-ufrag:abcd", "a=ice-ufrag:wxyz", 1),
            P::InconsistentTransport,
        ),
        (
            b.replacen("a=setup:actpass", "a=setup:active", 1),
            P::InconsistentTransport,
        ),
        (
            b.replacen("a=extmap:4 urn", "a=extmap:5 urn", 1),
            P::InconsistentExtensions,
        ),
        (
            b.replacen(
                &format!("a=extmap:3 {TRANSPORT_CC_URI}"),
                "a=extmap:3 urn:ietf:params:rtp-hdrext:ssrc-audio-level",
                1,
            ),
            P::InconsistentExtensions,
        ),
        (
            b.replace(
                &format!("a=extmap:3 {TRANSPORT_CC_URI}"),
                &format!("a=extmap:4 {TRANSPORT_CC_URI}"),
            )
            .replace(
                &format!("a=extmap:4 {MID_URI}"),
                &format!("a=extmap:5 {MID_URI}"),
            )
            .replacen("a=extmap:5 urn", "a=extmap:4 urn", 1),
            P::InconsistentExtensions,
        ),
        (
            b.replace(
                "a=rtcp-mux\r\na=rtpmap:96",
                "a=rtcp-mux\r\na=simulcast:send 1;2\r\na=rtpmap:96",
            ),
            P::Simulcast,
        ),
        (
            b.replace("a=ssrc-group:FID", "a=ssrc-group:SIM"),
            P::Simulcast,
        ),
        (
            b.replace("96 97\r\n", "111 97\r\n")
                .replace(":96 ", ":111 ")
                .replace("apt=96", "apt=111"),
            P::PayloadTypeClash,
        ),
        (b.replace("a=ssrc:4097 ", "a=ssrc:8193 "), P::SsrcClash),
    ];
    for (i, (text, want)) in cases.into_iter().enumerate() {
        assert_eq!(refused(&text), Refusal::Profile(want), "case {i}");
        assert_eq!(Refusal::Profile(want).status(), 422);
    }
}

fn strip(text: &str, prefix: &str) -> String {
    text.split_inclusive("\r\n")
        .filter(|l| !l.starts_with(prefix))
        .collect()
}

#[test]
fn fingerprint_counts() {
    let b = base();
    let line = |byte: u8| {
        format!(
            "a=fingerprint:sha-256 {}\r\n",
            Fingerprint::from_bytes([byte; 32]).to_sdp()
        )
    };
    let with = |n: u8| {
        let extra: String = (0..n).map(|i| line(0x20 + i)).collect();
        b.replace("t=0 0\r\n", &format!("t=0 0\r\n{extra}"))
    };
    // Section-level lines win over session-level ones, so move them all to
    // the session.
    let session_only = |text: String| strip(&text, "a=fingerprint:sha-256 11");
    let (_, n) = negotiate(&session_only(with(4))).expect("valid");
    assert_eq!(n.fingerprints.len(), 4);
    assert_eq!(
        refused(&session_only(with(5))),
        Refusal::Profile(ProfileError::TooManyFingerprints)
    );
    // A repeated line counts once; other hash functions are ignored.
    let dup = session_only(with(1)).replace("t=0 0\r\n", &format!("t=0 0\r\n{}", line(0x20)));
    let other = dup.replace("t=0 0\r\n", "t=0 0\r\na=fingerprint:sha-1 AA:BB\r\n");
    let (_, n) = negotiate(&other).expect("valid");
    assert_eq!(n.fingerprints, [Fingerprint::from_bytes([0x20; 32])]);
}

#[test]
fn parse_refusals() {
    use ParseError as P;
    let b = base();
    let huge = format!("{b}{}", "a=x\r\n".repeat(4000));
    let long = b.replace("s=-", &format!("s={}", "x".repeat(2000)));
    let lines = format!("{b}{}", "a=x\r\n".repeat(1100));
    let mut sections = b.clone();
    for _ in 0..16 {
        sections.push_str("m=video 9 UDP/TLS/RTP/SAVPF 96\r\n");
    }
    let formats = b.replace(
        "96 97\r\n",
        &format!(
            "96 97 {}\r\n",
            (0..70)
                .map(|i| (i % 60 + 30).to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ),
    );
    let cases: Vec<(Vec<u8>, P)> = vec![
        (huge.into_bytes(), P::TooLarge),
        (b"v=0\r\ns=\xff\r\n".to_vec(), P::NotUtf8),
        (lines.into_bytes(), P::TooManyLines),
        (long.into_bytes(), P::LineTooLong),
        (sections.into_bytes(), P::TooManySections),
        (formats.into_bytes(), P::TooManyFormats),
        (b.replace("v=0", "v=1").into_bytes(), P::Version),
        (b.replace("v=0\r\n", "").into_bytes(), P::Version),
        (Vec::new(), P::Version),
        (b.replace("s=-", "s").into_bytes(), P::Line),
        (b.replace("s=-", "s=a\rb").into_bytes(), P::ControlCharacter),
        (b.replace("s=-", "s=a\0b").into_bytes(), P::ControlCharacter),
        (
            b.replace("packetization-mode=1", "packetization-mode=1\x1b[2J")
                .into_bytes(),
            P::ControlCharacter,
        ),
        (b.replace("s=-", "S=-").into_bytes(), P::Line),
        (
            b.replace("m=video 9", "m=video x").into_bytes(),
            P::MediaLine,
        ),
        (
            b.replace("96 97\r\n", "96 abc\r\n").into_bytes(),
            P::MediaLine,
        ),
        (b.replace("96 97\r\n", "\r\n").into_bytes(), P::MediaLine),
        (
            b.replace("a=rtpmap:96 H264/90000", "a=rtpmap:96 H264")
                .into_bytes(),
            P::Rtpmap,
        ),
        (
            b.replace("a=rtpmap:96 ", "a=rtpmap:300 ").into_bytes(),
            P::Rtpmap,
        ),
        (
            b.replace("a=rtcp-fb:96 nack\r\n", "a=rtcp-fb:96\r\n")
                .into_bytes(),
            P::RtcpFb,
        ),
        (
            b.replace("a=extmap:4 ", "a=extmap:0 ").into_bytes(),
            P::Extmap,
        ),
        (
            b.replace("a=extmap:4 ", "a=extmap:256 ").into_bytes(),
            P::Extmap,
        ),
        (
            b.replace("a=extmap:4 ", "a=extmap:4/sideways ")
                .into_bytes(),
            P::Extmap,
        ),
        (b.replace("a=ssrc:4097 ", "a=ssrc:x ").into_bytes(), P::Ssrc),
        (
            b.replace("a=ssrc-group:FID 8193 8194", "a=ssrc-group:FID 8193")
                .into_bytes(),
            P::Ssrc,
        ),
        (
            b.replacen("a=fingerprint:sha-256 11", "a=fingerprint:sha-256 1", 1)
                .into_bytes(),
            P::Fingerprint,
        ),
        (
            b.replacen("a=ice-ufrag:abcd", "a=ice-ufrag:ab", 1)
                .into_bytes(),
            P::IceCredential,
        ),
        (
            b.replacen("a=ice-ufrag:abcd", "a=ice-ufrag:ab cd", 1)
                .into_bytes(),
            P::IceCredential,
        ),
        (
            b.replacen(
                &format!("a=ice-pwd:{}", crate::testing::OFFER_PWD),
                "a=ice-pwd:short",
                1,
            )
            .into_bytes(),
            P::IceCredential,
        ),
        (
            b.replace("a=setup:actpass", "a=setup:both").into_bytes(),
            P::Setup,
        ),
        (b.replace("a=mid:1", "a=mid:0").into_bytes(), P::Mid),
        (b.replace("a=mid:1", "a=mid:").into_bytes(), P::Mid),
        (
            b.replace("a=group:BUNDLE 0 1", "a=group:BUNDLE")
                .into_bytes(),
            P::Group,
        ),
    ];
    for (i, (text, want)) in cases.into_iter().enumerate() {
        assert_eq!(
            negotiate_bytes(&text),
            Err(Refusal::Malformed(want)),
            "case {i}"
        );
    }
    assert_eq!(Refusal::Malformed(P::Line).status(), 400);
    assert_eq!(Refusal::Malformed(P::ControlCharacter).status(), 400);
    // A tab is allowed (RFC 8866's byte-string), and a CR only before LF.
    assert!(negotiate(&b.replace("s=-", "s=a\tb")).is_ok());
    assert!(negotiate(&b.replace("\r\n", "\n")).is_ok());
}

fn negotiate_bytes(b: &[u8]) -> Result<(), Refusal> {
    match std::str::from_utf8(b) {
        Ok(s) => negotiate(s).map(|_| ()),
        Err(_) => Offer::parse(b).map(|_| ()).map_err(Refusal::from),
    }
}

#[test]
fn extra_sections_are_answered_with_port_zero() {
    let b = base().replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0 1 2 3 4");
    let extra = "m=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=mid:2\r\na=sendonly\r\n\
        a=rtcp-mux\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1\r\na=ssrc:777 cname:x\r\n\
        m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=mid:3\r\na=sctp-port:5000\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=mid:4\r\na=recvonly\r\na=rtpmap:111 opus/48000/2\r\n";
    let (sdp, n) = negotiate(&format!("{b}{extra}")).expect("valid");
    assert!(sdp.contains("a=group:BUNDLE 0 1\r\n"));
    assert!(sdp.contains("m=video 0 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=mid:2\r\n"));
    assert!(sdp.contains(
        "m=application 0 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=mid:3\r\n"
    ));
    assert!(sdp.contains("m=audio 0 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=mid:4\r\n"));
    assert_eq!(n.rejected_mids, ["2", "3", "4"]);
    assert_eq!(n.rejected_ssrcs, [777]);
    // Sections stay in offer order in the answer.
    let back = Offer::parse(sdp.as_bytes()).expect("valid");
    let mids: Vec<_> = back
        .sections
        .iter()
        .map(|s| s.mid.clone().expect("valid"))
        .collect();
    assert_eq!(mids, ["0", "1", "2", "3", "4"]);
}

#[test]
fn audio_only_and_video_only() {
    let b = base();
    let audio_only = b.replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 0");
    let (sdp, n) = negotiate(&audio_only).expect("valid");
    assert!(n.video.is_none() && n.audio.is_some());
    assert!(sdp.contains("m=video 0 "));
    let video_only = b.replace("a=group:BUNDLE 0 1", "a=group:BUNDLE 1");
    let (sdp, n) = negotiate(&video_only).expect("valid");
    assert!(n.audio.is_none() && n.video.is_some());
    // The tagged section is the first of the answer's group.
    assert!(sdp.contains("a=group:BUNDLE 1\r\n"));
    let video = sdp.split("m=video").nth(1).expect("valid");
    assert!(video.contains("a=end-of-candidates"));
}

#[test]
fn inheritance_and_extension_directions() {
    // Session-level values are inherited, a section's own win, and a
    // sendonly extension is answered recvonly.
    let b = strip(&base(), "a=ice-ufrag:")
        .replace("t=0 0\r\n", "t=0 0\r\na=ice-ufrag:sess\r\na=sendrecv\r\n")
        .replace("a=extmap:4 urn", "a=extmap:4/sendonly urn");
    let (sdp, n) = negotiate(&b).expect("valid");
    assert_eq!(n.remote_ufrag, "sess");
    assert!(sdp.contains(&format!("a=extmap:4/recvonly {MID_URI}\r\n")));
    let own = b
        .replace("a=mid:0\r\n", "a=mid:0\r\na=ice-ufrag:mine\r\n")
        .replace("a=mid:1\r\n", "a=mid:1\r\na=ice-ufrag:mine\r\n");
    assert_eq!(negotiate(&own).expect("valid").1.remote_ufrag, "mine");
    // A recvonly extension cannot be received, so it is not answered.
    let recv = base().replace("a=extmap:4 urn", "a=extmap:4/recvonly urn");
    let (sdp, n) = negotiate(&recv).expect("valid");
    assert_eq!(n.mid_extension, None);
    assert!(!sdp.contains(MID_URI));
}

#[test]
fn h264_choice_follows_offer_order_and_profiles() {
    use H264Profile as H;
    for (pli, want) in [
        ("42e01f", Some(H::ConstrainedBaseline)),
        ("42001f", Some(H::Baseline)),
        ("4d001f", Some(H::Main)),
        ("4d401f", Some(H::Main)),
        ("4d801f", Some(H::ConstrainedBaseline)),
        ("58c01f", Some(H::ConstrainedBaseline)),
        ("58801f", Some(H::Baseline)),
        ("58001f", None),
        ("64001f", Some(H::High)),
        ("640c1f", Some(H::High)),
        ("6e001f", None),
        ("f4001f", None),
        ("42e0", None),
        ("zz001f", None),
    ] {
        assert_eq!(H264Profile::classify(pli), want, "{pli}");
    }
    // The first acceptable payload wins; an unacceptable earlier one is
    // passed over, and its RTX is not taken.
    let b = base().replace("96 97\r\n", "98 99 96 97\r\n").replace(
        "a=rtpmap:96 H264/90000",
        "a=rtpmap:98 H264/90000\r\na=fmtp:98 packetization-mode=0;profile-level-id=42e01f\r\n\
             a=rtpmap:99 rtx/90000\r\na=fmtp:99 apt=98\r\na=rtpmap:96 H264/90000",
    );
    let (_, n) = negotiate(&b).expect("valid");
    let v = n.video.expect("valid");
    assert_eq!((v.payload_type, v.rtx_payload_type), (96, Some(97)));
    // No profile-level-id means Baseline.
    let none = base().replace(";profile-level-id=42e01f", "");
    assert_eq!(
        negotiate(&none)
            .expect("valid")
            .1
            .video
            .expect("valid")
            .profile,
        Some(H::Baseline)
    );
}

#[test]
fn payload_types_that_read_as_rtcp_are_passed_over() {
    // H.264 at 72 with RTX at 73 comes first, then the usual 96 and 97.
    let b = base().replace("96 97\r\n", "72 73 96 97\r\n").replace(
        "a=rtpmap:96 H264/90000",
        "a=rtpmap:72 H264/90000\r\na=fmtp:72 packetization-mode=1;profile-level-id=42e01f\r\n\
         a=rtpmap:73 rtx/90000\r\na=fmtp:73 apt=72\r\na=rtpmap:96 H264/90000",
    );
    let (sdp, n) = negotiate(&b).expect("valid");
    let v = n.video.expect("video");
    assert_eq!((v.payload_type, v.rtx_payload_type), (96, Some(97)));
    assert!(!sdp.contains("a=rtpmap:72") && !sdp.contains("a=rtpmap:73"));
    // Only 72 and 73: no H.264 that can be received.
    let only = base()
        .replace("96 97\r\n", "72 73\r\n")
        .replace(":96 ", ":72 ")
        .replace(":97 ", ":73 ")
        .replace("apt=96", "apt=72");
    assert_eq!(refused(&only), Refusal::Profile(ProfileError::NoH264));
    // Opus at 80 alone: no Opus.
    let opus = base()
        .replace(" 111 0\r\n", " 80 0\r\n")
        .replace(":111 ", ":80 ");
    assert_eq!(refused(&opus), Refusal::Profile(ProfileError::NoOpus));
    // The refusal is the payload type's, not the codec's: 63 is fine.
    let ok = base()
        .replace(" 111 0\r\n", " 63 0\r\n")
        .replace(":111 ", ":63 ");
    let audio = negotiate(&ok).expect("valid").1.audio.expect("audio");
    assert_eq!(audio.payload_type, 63);
}

#[test]
fn ssrcs_without_fid_bind_later() {
    let b = base().replace("a=ssrc-group:FID 8193 8194\r\n", "");
    let (_, n) = negotiate(&b).expect("valid");
    let v = n.video.expect("valid");
    assert_eq!((v.ssrc, v.rtx_ssrc), (None, None), "two SSRCs, no FID");
    let one = strip(&base(), "a=ssrc").replace(
        "a=rtcp-mux\r\na=rtpmap:111",
        "a=rtcp-mux\r\na=ssrc:5 cname:x\r\na=rtpmap:111",
    );
    let (_, n) = negotiate(&one).expect("valid");
    assert_eq!(n.audio.expect("valid").ssrc, Some(5));
    assert_eq!(n.video.expect("valid").ssrc, None);
}

#[test]
fn opus_bitrate_is_held_to_the_rfc_range() {
    let offer = Offer::parse(base().as_bytes()).expect("valid");
    let c = creds();
    for (asked, got) in [(1, 6000), (96_000, 96_000), (900_000, 510_000)] {
        let config = AnswerConfig {
            credentials: &c,
            fingerprint: Fingerprint::from_bytes(OURS),
            candidates: &[],
            opus_max_bitrate: asked,
            origin: 1,
        };
        let (sdp, _) = answer(&offer, &config).expect("valid");
        assert!(sdp.contains(&format!("maxaveragebitrate={got};")));
    }
}

#[test]
fn every_prefix_of_the_fixtures_is_safe() {
    let text = fixture("chrome-157-desktop-macos/offer.sdp");
    let b = text.as_bytes();
    for n in (0..b.len()).step_by(7) {
        if let Ok(offer) = Offer::parse(&b[..n]) {
            let c = creds();
            let config = AnswerConfig {
                credentials: &c,
                fingerprint: Fingerprint::from_bytes(OURS),
                candidates: &candidates(),
                opus_max_bitrate: 64_000,
                origin: 1,
            };
            let _ = answer(&offer, &config);
        }
    }
}
