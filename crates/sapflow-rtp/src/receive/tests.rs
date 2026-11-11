//! The receiver against FFmpeg's packets (A1.2 in `docs/whip-ingest.md`),
//! and its smaller rules.
//!
//! The fixtures (`tests/fixtures/README.md`) are H.264 and Opus that
//! FFmpeg sent as plain RTP, run as a black box, plus the same H.264
//! encode as Annex B. A simulated network replays them with packets
//! dropped, delayed, reordered and duplicated; a simulated sender answers
//! the receiver's NACKs after a 30 ms round trip, on the RTX stream or on
//! the original SSRC and sequence number, or not at all.

use super::*;
use crate::h264::Incomplete;
use crate::opus::FrameKind;
use crate::rtcp::{Compound, Packet};
use crate::rtp::{RtpBuilder, RtpPacket};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet};

const H264_REC: &[u8] = include_bytes!("../../tests/fixtures/ffmpeg-h264-fu.rec");
const H264_ANNEXB: &[u8] = include_bytes!("../../tests/fixtures/ffmpeg-h264-fu.h264");
const OPUS_REC: &[u8] = include_bytes!("../../tests/fixtures/ffmpeg-opus-stereo.rec");

const MS: u64 = 1000;
/// One-way delay of the simulated network; the RTT is twice this.
const ONE_WAY: u64 = 15 * MS;
const RTX_PT: u8 = 98;
const RTX_SSRC: u32 = 0x5254_5801;

fn records(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some((&channel, tail)) = rest.split_first() {
        let len = usize::from(u16::from_be_bytes([tail[0], tail[1]]));
        if channel == 0 {
            out.push(tail[2..2 + len].to_vec());
        }
        rest = &tail[2 + len..];
    }
    out
}

/// NAL units of an Annex B stream.
fn annexb_nals(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let start = |d: &[u8]| d.windows(3).position(|w| w == [0, 0, 1]);
    let Some(first) = start(data) else {
        return out;
    };
    data = &data[first + 3..];
    loop {
        match start(data) {
            Some(at) => {
                let mut end = at;
                if end > 0 && data[end - 1] == 0 {
                    end -= 1;
                }
                out.push(data[..end].to_vec());
                data = &data[at + 3..];
            }
            None => {
                out.push(data.to_vec());
                return out;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Pkt {
    stream: usize,
    seq: u16,
    ts: u32,
    marker: bool,
    payload: Vec<u8>,
    datagram: Vec<u8>,
    /// When the sender sends it, in microseconds.
    sent: u64,
}

struct Capture {
    video: Vec<Pkt>,
    audio: Vec<Pkt>,
    video_pt: u8,
    video_ssrc: u32,
    audio_pt: u8,
    audio_ssrc: u32,
}

fn capture() -> Capture {
    let parse = |stream: usize, recs: Vec<Vec<u8>>, clock: u64| -> Vec<Pkt> {
        let ts0 = RtpPacket::parse(&recs[0]).expect("rtp").timestamp();
        let mut out = Vec::new();
        let mut last_ts = None;
        let mut within = 0u64;
        for d in recs {
            let p = RtpPacket::parse(&d).expect("rtp");
            if last_ts == Some(p.timestamp()) {
                within += 1;
            } else {
                within = 0;
            }
            last_ts = Some(p.timestamp());
            // Packets of one frame go out 100 us apart.
            let sent =
                u64::from(p.timestamp().wrapping_sub(ts0)) * 1_000_000 / clock + within * 100;
            out.push(Pkt {
                stream,
                seq: p.sequence(),
                ts: p.timestamp(),
                marker: p.marker(),
                payload: p.payload().to_vec(),
                datagram: d.clone(),
                sent,
            });
        }
        out
    };
    let video = parse(0, records(H264_REC), 90_000);
    let audio = parse(1, records(OPUS_REC), 48_000);
    let v = RtpPacket::parse(&video[0].datagram).expect("rtp");
    let a = RtpPacket::parse(&audio[0].datagram).expect("rtp");
    Capture {
        video_pt: v.payload_type(),
        video_ssrc: v.ssrc(),
        audio_pt: a.payload_type(),
        audio_ssrc: a.ssrc(),
        video,
        audio,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resend {
    Rtx,
    Original,
    Never,
}

#[derive(Default)]
struct Impairments {
    /// Originals the network drops, by (stream, sequence number).
    drop: HashSet<(usize, u16)>,
    /// Originals delayed by this much.
    delay: BTreeMap<(usize, u16), u64>,
    /// Originals delivered twice.
    duplicate: HashSet<(usize, u16)>,
    /// Packets whose first retransmission is also dropped.
    lose_first_resend: HashSet<(usize, u16)>,
}

struct Outcome {
    events: Vec<Event>,
    nacked: HashSet<(usize, u16)>,
    feedback: Vec<Feedback>,
    receiver: Receiver,
}

fn receiver(c: &Capture, rtx: bool) -> Receiver {
    let mut r = Receiver::new(ReceiverConfig::default(), Instant::now());
    let mut video = StreamConfig::new(Codec::H264, c.video_pt);
    video.ssrc = Some(c.video_ssrc);
    video.nack = true;
    video.pli = true;
    if rtx {
        video.rtx = Some(RtxConfig {
            payload_type: RTX_PT,
            ssrc: Some(RTX_SSRC),
        });
    }
    let mut audio = StreamConfig::new(Codec::Opus, c.audio_pt);
    audio.ssrc = Some(c.audio_ssrc);
    audio.nack = true;
    assert_eq!(r.add_stream(video).expect("video").index(), 0);
    assert_eq!(r.add_stream(audio).expect("audio").index(), 1);
    r
}

/// Plays the capture through `imp` to a receiver, answering NACKs by
/// `resend`.
fn simulate(c: &Capture, imp: &Impairments, resend: Resend) -> Outcome {
    let mut r = receiver(c, resend == Resend::Rtx);
    let t0 = r.epoch;
    let at = |us: u64| t0 + Duration::from_micros(us);
    let mut queue: BinaryHeap<Reverse<(u64, u64, Vec<u8>)>> = BinaryHeap::new();
    let mut order = 0u64;
    let mut push = |q: &mut BinaryHeap<_>, t: u64, d: Vec<u8>| {
        order += 1;
        q.push(Reverse((t, order, d)));
    };
    let mut by_key: BTreeMap<(usize, u16), Pkt> = BTreeMap::new();
    for p in c.video.iter().chain(&c.audio) {
        let key = (p.stream, p.seq);
        by_key.insert(key, p.clone());
        if imp.drop.contains(&key) {
            continue;
        }
        let t = p.sent + ONE_WAY + imp.delay.get(&key).copied().unwrap_or(0);
        push(&mut queue, t, p.datagram.clone());
        if imp.duplicate.contains(&key) {
            push(&mut queue, t + 50, p.datagram.clone());
        }
    }
    let last = queue.iter().map(|Reverse((t, ..))| *t).max().unwrap_or(0);
    let mut events = Vec::new();
    let mut nacked = HashSet::new();
    let mut feedback = Vec::new();
    let mut resent: HashSet<(usize, u16)> = HashSet::new();
    let mut rtx_seq = 1000u16;
    let mut now = 0u64;
    loop {
        let net = queue.peek().map(|Reverse((t, ..))| *t);
        let timer = r
            .poll_timeout()
            .map(|i| micros(i.saturating_duration_since(t0)));
        let next = match (net, timer) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        if net.is_none() && next > last + 2_000 * MS {
            break;
        }
        assert!(next >= now, "time runs forward");
        now = next;
        if net.is_some_and(|t| t <= now) {
            let Reverse((_, _, d)) = queue.pop().expect("queued");
            let p = RtpPacket::parse(&d).expect("rtp");
            r.handle_rtp(at(now), &p).expect("taken");
        } else {
            r.handle_timeout(at(now));
        }
        while let Some(f) = r.poll_feedback() {
            // Every NACK reads back through the RTCP parser.
            let mut wire = Vec::new();
            f.write(&mut wire, 1).expect("built");
            let compound = Compound::parse(&wire).expect("parses");
            let parsed = compound.packets().next().expect("one").expect("ok");
            if let Feedback::Nack { media_ssrc, lost } = &f {
                let Packet::Nack(n) = parsed else {
                    panic!("a NACK");
                };
                assert_eq!(n.media_ssrc(), *media_ssrc);
                assert_eq!(&n.lost().collect::<Vec<_>>(), lost);
                let stream = usize::from(*media_ssrc == c.audio_ssrc);
                for &seq in lost {
                    let key = (stream, seq);
                    nacked.insert(key);
                    if resend == Resend::Never {
                        continue;
                    }
                    if imp.lose_first_resend.contains(&key) && resent.insert(key) {
                        continue;
                    }
                    let orig = &by_key[&key];
                    let d = if resend == Resend::Rtx && stream == 0 {
                        rtx_seq = rtx_seq.wrapping_add(1);
                        let mut payload = seq.to_be_bytes().to_vec();
                        payload.extend_from_slice(&orig.payload);
                        RtpBuilder {
                            marker: orig.marker,
                            payload_type: RTX_PT,
                            sequence: rtx_seq,
                            timestamp: orig.ts,
                            ssrc: RTX_SSRC,
                            payload,
                            ..RtpBuilder::default()
                        }
                        .build()
                    } else {
                        orig.datagram.clone()
                    };
                    push(&mut queue, now + 2 * ONE_WAY, d);
                }
            }
            feedback.push(f);
        }
        events.extend(std::iter::from_fn(|| r.poll_event()));
    }
    Outcome {
        events,
        nacked,
        feedback,
        receiver: r,
    }
}

fn access_units(events: &[Event]) -> Vec<AccessUnit> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::AccessUnit { au, .. } => Some(au.clone()),
            _ => None,
        })
        .collect()
}

fn audio_frames(events: &[Event]) -> Vec<OpusFrame> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Audio { frame, .. } => Some(frame.clone()),
            _ => None,
        })
        .collect()
}

fn lost_set(events: &[Event], stream: usize) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    for e in events {
        if let Event::Lost {
            stream: s,
            first,
            count,
        } = e
            && s.index() == stream
        {
            for n in 0..*count {
                out.insert(first.wrapping_add(n as u16));
            }
        }
    }
    out
}

/// No event that says anything went wrong.
fn assert_clean(events: &[Event]) {
    for e in events {
        assert!(
            matches!(e, Event::AccessUnit { .. } | Event::Audio { .. }),
            "unexpected {e:?}"
        );
        if let Event::Audio { frame, .. } = e {
            assert_eq!(frame.kind, FrameKind::Media);
        }
    }
}

/// Video frames of the capture: timestamp and the sequence numbers in it.
fn frames(c: &Capture) -> Vec<(u32, Vec<u16>)> {
    let mut out: Vec<(u32, Vec<u16>)> = Vec::new();
    for p in &c.video {
        match out.last_mut() {
            Some((ts, seqs)) if *ts == p.ts => seqs.push(p.seq),
            _ => out.push((p.ts, vec![p.seq])),
        }
    }
    out
}

fn lossless(c: &Capture) -> Outcome {
    simulate(c, &Impairments::default(), Resend::Never)
}

#[test]
fn fixtures_cover_what_a12_needs() {
    let c = capture();
    let fu_a = c.video.iter().filter(|p| p.payload[0] & 0x1f == 28).count();
    let stap_a = c.video.iter().filter(|p| p.payload[0] & 0x1f == 24).count();
    assert!(fu_a > 100, "FU-A in the capture: {fu_a}");
    assert!(stap_a >= 2, "STAP-A in the capture: {stap_a}");
    for stream in [&c.video, &c.audio] {
        let seqs: Vec<u16> = stream.iter().map(|p| p.seq).collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
            "contiguous"
        );
        assert!(
            seqs.contains(&65535) && seqs.contains(&0),
            "crosses the wrap"
        );
    }
    assert_ne!(c.video_pt, c.audio_pt);
    assert!(c.audio.iter().all(|p| p.payload[0] & 0x04 != 0), "stereo");
}

#[test]
fn lossless_capture_matches_the_annexb_reference() {
    let c = capture();
    let out = lossless(&c);
    assert_clean(&out.events);
    let aus = access_units(&out.events);
    let frames = frames(&c);
    assert_eq!(aus.len(), frames.len());
    assert_eq!(aus.len(), 30, "2 s at 15 fps");
    assert!(aus[0].idr && aus[15].idr);
    assert_eq!(aus.iter().filter(|a| a.idr).count(), 2);
    // Parameter sets, then the rest, in order, are exactly the NAL units
    // FFmpeg wrote as Annex B from the same encode.
    let mut ours: Vec<Vec<u8>> = Vec::new();
    for (au, (ts, _)) in aus.iter().zip(&frames) {
        assert_eq!(au.timestamp, *ts);
        assert_eq!(au.idr, !au.sps.is_empty(), "IDRs carry their SPS");
        ours.extend(au.sps.iter().cloned());
        ours.extend(au.pps.iter().cloned());
        let units: Vec<&[u8]> = au.nal_units().collect();
        let total: usize = units.iter().map(|u| 4 + u.len()).sum();
        assert_eq!(total, au.data.len(), "length prefixes add up");
        ours.extend(units.iter().map(|u| u.to_vec()));
    }
    assert_eq!(ours, annexb_nals(H264_ANNEXB));
    // Opus: every packet, in order, unchanged.
    let audio = audio_frames(&out.events);
    assert_eq!(audio.len(), c.audio.len());
    for (f, p) in audio.iter().zip(&c.audio) {
        assert_eq!((f.timestamp, &f.data, f.duration), (p.ts, &p.payload, 960));
    }
    // One PLI at the first packet, and the periodic ones; no NACK.
    assert!(out.nacked.is_empty());
    assert!(matches!(out.feedback[0], Feedback::Pli { media_ssrc } if media_ssrc == c.video_ssrc));
    let stats = out.receiver.stats(StreamId(0)).expect("stats");
    assert_eq!(
        (stats.frames, stats.lost, stats.missing_markers),
        (30, 0, 0)
    );
}

/// The packets A1.2 drops: a marker packet, a frame's first packet, an
/// FU-A start, middle and end, a whole frame, both sides of the wrap, and
/// Opus packets alone, in a burst and across the wrap.
fn drops(c: &Capture) -> (HashSet<(usize, u16)>, Vec<&'static str>) {
    let frames = frames(c);
    let info = |seq: u16| c.video.iter().find(|p| p.seq == seq).expect("packet");
    let fu = |seq: u16| {
        let p = info(seq);
        (p.payload[0] & 0x1f == 28).then(|| (p.payload[1] & 0x80 != 0, p.payload[1] & 0x40 != 0))
    };
    let mut set = HashSet::new();
    let mut what = Vec::new();
    let mut used_frames = HashSet::new();
    let mut take = |set: &mut HashSet<_>, frame: usize, seqs: &[u16], label: &'static str| {
        assert!(used_frames.insert(frame), "one impairment per frame");
        for &s in seqs {
            set.insert((0usize, s));
        }
        what.push(label);
    };
    // Frames with many packets, away from the IDRs and the wrap.
    let wrap_frame = frames
        .iter()
        .position(|(_, s)| s.contains(&65535) || s.contains(&0))
        .expect("wrap");
    let mut candidates = (2..frames.len())
        .filter(|&f| f % 15 > 1 && f.abs_diff(wrap_frame) > 1 && frames[f].1.len() >= 4);
    let f = candidates.next().expect("frame");
    take(
        &mut set,
        f,
        &[*frames[f].1.last().expect("marker")],
        "marker packet",
    );
    let f = candidates.next().expect("frame");
    take(&mut set, f, &[frames[f].1[0]], "first packet");
    // An FU-A start that is not the frame's first packet (a later
    // slice), a middle and an end without the marker.
    let mut pick = |set: &mut HashSet<_>, want: (bool, bool), label| {
        for f in candidates.by_ref() {
            if let Some(&s) = frames[f].1[1..frames[f].1.len() - 1]
                .iter()
                .find(|&&s| fu(s) == Some(want))
            {
                take(set, f, &[s], label);
                return;
            }
        }
        panic!("no {label}");
    };
    pick(&mut set, (true, false), "FU-A start");
    pick(&mut set, (false, false), "FU-A middle");
    pick(&mut set, (false, true), "FU-A end");
    let f = candidates.next().expect("frame");
    let whole = frames[f].1.clone();
    take(&mut set, f, &whole, "whole frame");
    take(&mut set, wrap_frame, &[65535, 0], "both sides of the wrap");
    for s in [65510u16, 65530, 65531, 65532, 65535, 0, 40] {
        set.insert((1, s));
    }
    (set, what)
}

/// A packet: (stream, sequence number).
type Key = (usize, u16);

fn delays(c: &Capture, dropped: &HashSet<Key>) -> (BTreeMap<Key, u64>, HashSet<Key>) {
    // Some packets 2 ms late (reordered inside the 10 ms reorder delay,
    // never to be NACKed) and some 25 ms late (NACKed, then the original
    // and the retransmission both arrive).
    let mut delay = BTreeMap::new();
    let mut short = HashSet::new();
    let keys: Vec<(usize, u16)> = c
        .video
        .iter()
        .chain(&c.audio)
        .map(|p| (p.stream, p.seq))
        .filter(|k| !dropped.contains(k))
        .collect();
    for (n, k) in keys.iter().enumerate() {
        // Only video packets with a next packet in the same frame can be
        // overtaken within 2 ms.
        if n % 23 == 5 && k.0 == 0 {
            let p = c.video.iter().position(|p| p.seq == k.1).expect("p");
            if c.video
                .get(p + 1)
                .is_some_and(|q| q.ts == c.video[p].ts && !dropped.contains(&(0, q.seq)))
            {
                delay.insert(*k, 2 * MS);
                short.insert(*k);
            }
        } else if n % 41 == 7 && k.0 == 0 {
            delay.insert(*k, 25 * MS);
        }
    }
    (delay, short)
}

fn duplicates(c: &Capture, dropped: &HashSet<(usize, u16)>) -> HashSet<(usize, u16)> {
    c.video
        .iter()
        .chain(&c.audio)
        .map(|p| (p.stream, p.seq))
        .enumerate()
        .filter(|(n, k)| n % 17 == 3 && !dropped.contains(k))
        .map(|(_, k)| k)
        .collect()
}

fn recovered(resend: Resend) {
    let c = capture();
    let base = lossless(&c);
    let (drop, what) = drops(&c);
    assert_eq!(what.len(), 7);
    let (delay, short) = delays(&c, &drop);
    assert!(!short.is_empty() && delay.len() > short.len());
    let duplicate = duplicates(&c, &drop);
    // Some retransmissions are lost too, so NACKs repeat.
    let lose_first_resend: HashSet<_> = drop.iter().copied().filter(|k| k.1 % 3 == 0).collect();
    assert!(!lose_first_resend.is_empty());
    let imp = Impairments {
        drop: drop.clone(),
        delay,
        duplicate,
        lose_first_resend,
    };
    let out = simulate(&c, &imp, resend);
    assert_clean(&out.events);
    // Exactly the lossless output.
    assert_eq!(access_units(&out.events), access_units(&base.events));
    assert_eq!(audio_frames(&out.events), audio_frames(&base.events));
    // Every dropped packet was NACKed; nothing merely reordered inside
    // the reorder delay was.
    for k in &drop {
        assert!(out.nacked.contains(k), "{k:?} NACKed");
    }
    for k in &short {
        assert!(!out.nacked.contains(k), "{k:?} reordered, not NACKed");
    }
    // RTT measured from NACK to retransmission: 30 ms here. On the
    // original SSRC, late originals (15 ms after their NACK) are timed
    // too, so the estimate can only fall short.
    assert!(out.receiver.rtt_measured());
    let rtt = out.receiver.rtt();
    let low = if resend == Resend::Rtx { 29 } else { 10 };
    assert!(
        rtt >= Duration::from_millis(low) && rtt <= Duration::from_millis(31),
        "{rtt:?}"
    );
    let v = out.receiver.stats(StreamId(0)).expect("stats");
    assert!(v.recovered as usize >= drop.iter().filter(|k| k.0 == 0).count());
    assert!(v.duplicates > 0);
    assert_eq!((v.lost, v.incomplete, v.skipped), (0, 0, 0));
    assert_eq!(out.receiver.buffered(StreamId(0)), Some((0, 0)));
}

#[test]
fn a12_every_retransmission_arrives_on_rtx() {
    recovered(Resend::Rtx);
}

#[test]
fn a12_every_retransmission_arrives_on_the_original_ssrc() {
    recovered(Resend::Original);
}

#[test]
fn a12_withheld_retransmissions_report_exactly_their_frames() {
    let c = capture();
    let base = lossless(&c);
    let (drop, _) = drops(&c);
    let (delay, _) = delays(&c, &drop);
    let imp = Impairments {
        drop: drop.clone(),
        delay,
        duplicate: duplicates(&c, &drop),
        ..Impairments::default()
    };
    let out = simulate(&c, &imp, Resend::Never);
    // Loss is reported for exactly the withheld packets.
    for stream in [0, 1] {
        let want: BTreeSet<u16> = drop.iter().filter(|k| k.0 == stream).map(|k| k.1).collect();
        assert_eq!(lost_set(&out.events, stream), want, "stream {stream}");
    }
    // Video: incomplete exactly for the frames that lost some but not all
    // of their packets (a frame lost whole has no timestamp to report; its
    // loss is in the Lost events). Every other frame is delivered exactly
    // as in the lossless run, or skipped until the next IDR.
    let base_aus: BTreeMap<u32, AccessUnit> = access_units(&base.events)
        .into_iter()
        .map(|a| (a.timestamp, a))
        .collect();
    let mut want_incomplete = BTreeSet::new();
    let mut want_delivered = BTreeSet::new();
    let mut want_skipped = BTreeSet::new();
    let mut awaiting = false;
    for (ts, seqs) in frames(&c) {
        let hit = seqs.iter().filter(|s| drop.contains(&(0, **s))).count();
        if hit > 0 {
            if hit < seqs.len() {
                want_incomplete.insert(ts);
            }
            awaiting = true;
        } else if awaiting && !base_aus[&ts].idr {
            want_skipped.insert(ts);
        } else {
            awaiting = false;
            want_delivered.insert(ts);
        }
    }
    let mut incomplete = BTreeSet::new();
    let mut skipped = BTreeSet::new();
    for e in &out.events {
        match e {
            Event::IncompleteFrame {
                timestamp, reason, ..
            } => {
                assert!(
                    matches!(reason, Incomplete::Gap | Incomplete::StartLost),
                    "{reason:?}"
                );
                incomplete.insert(*timestamp);
            }
            Event::FrameSkipped {
                timestamp, reason, ..
            } => {
                assert_eq!(*reason, SkipReason::AfterLoss);
                skipped.insert(*timestamp);
            }
            Event::Discontinuity { .. } | Event::Refused { .. } => panic!("{e:?}"),
            _ => {}
        }
    }
    assert_eq!(incomplete, want_incomplete);
    assert_eq!(skipped, want_skipped);
    let delivered = access_units(&out.events);
    assert_eq!(
        delivered
            .iter()
            .map(|a| a.timestamp)
            .collect::<BTreeSet<_>>(),
        want_delivered
    );
    for au in &delivered {
        assert_eq!(au, &base_aus[&au.timestamp]);
    }
    // A keyframe was asked for at once after each loss.
    assert!(
        out.feedback
            .iter()
            .filter(|f| matches!(f, Feedback::Pli { .. }))
            .count()
            >= 3
    );
    // Audio: silence marked as loss fills exactly the withheld packets'
    // time, and every delivered packet comes through unchanged.
    let lost_ranges: BTreeSet<u32> = c
        .audio
        .iter()
        .filter(|p| drop.contains(&(1, p.seq)))
        .map(|p| p.ts)
        .collect();
    let audio = audio_frames(&out.events);
    let filled: BTreeSet<u32> = audio
        .iter()
        .filter(|f| f.kind == FrameKind::Loss)
        .map(|f| f.timestamp)
        .collect();
    assert_eq!(filled, lost_ranges);
    assert!(audio.iter().all(|f| f.kind != FrameKind::Dtx));
    for f in audio.iter().filter(|f| f.kind == FrameKind::Loss) {
        assert_eq!(
            (f.duration, f.data.as_slice()),
            (960, &[0xfc, 0xff, 0xfe][..])
        );
    }
    let media: Vec<_> = audio
        .iter()
        .filter(|f| f.kind == FrameKind::Media)
        .cloned()
        .collect();
    let want: Vec<_> = audio_frames(&base.events)
        .into_iter()
        .filter(|f| !lost_ranges.contains(&f.timestamp))
        .collect();
    assert_eq!(media, want);
    // The timeline has no holes.
    for w in audio.windows(2) {
        assert_eq!(w[0].timestamp.wrapping_add(w[0].duration), w[1].timestamp);
    }
}

#[test]
fn opus_dtx_is_filled_and_not_counted_as_loss() {
    // The sender suppresses silence: packets left out with no gap in the
    // sequence numbers.
    let mut c = capture();
    let removed: Vec<u32> = c
        .audio
        .iter()
        .skip(30)
        .take(10)
        .chain(c.audio.iter().skip(60).take(3))
        .map(|p| p.ts)
        .collect();
    let kept: Vec<Pkt> = c
        .audio
        .iter()
        .filter(|p| !removed.contains(&p.ts))
        .cloned()
        .collect();
    let first = kept[0].seq;
    c.audio = kept
        .into_iter()
        .enumerate()
        .map(|(n, mut p)| {
            p.seq = first.wrapping_add(n as u16);
            p.datagram[2..4].copy_from_slice(&p.seq.to_be_bytes());
            p
        })
        .collect();
    let out = lossless(&c);
    assert_clean_but_dtx(&out.events);
    let audio = audio_frames(&out.events);
    let dtx: Vec<u32> = audio
        .iter()
        .filter(|f| f.kind == FrameKind::Dtx)
        .map(|f| f.timestamp)
        .collect();
    assert_eq!(dtx, removed);
    for w in audio.windows(2) {
        assert_eq!(w[0].timestamp.wrapping_add(w[0].duration), w[1].timestamp);
    }
    let a = out.receiver.stats(StreamId(1)).expect("stats");
    assert_eq!((a.dtx_frames, a.loss_frames, a.lost), (13, 0, 0));
}

fn assert_clean_but_dtx(events: &[Event]) {
    for e in events {
        assert!(
            matches!(e, Event::AccessUnit { .. } | Event::Audio { .. }),
            "{e:?}"
        );
        if let Event::Audio { frame, .. } = e {
            assert_ne!(frame.kind, FrameKind::Loss);
        }
    }
}

// The smaller rules, on built packets.

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
