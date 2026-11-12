//! An interrupt loses no media the muxer already has: every input frame
//! reaches the HLS parts, counted by payload, across a codec epoch change,
//! a reconnect and a re-anchor, for video with AAC, video with Opus and
//! audio-only Opus. Each playlist keeps the HLS rules, every part's video
//! sample durations add up to its duration, and the targets do not move.

use super::tests_support::{Es, elementary};
use super::*;
use crate::fmp4::{AUDIO_TRACK, VIDEO_TRACK};
use crate::muxer::{Muxer, Part};
use crate::playlist;
use mp4_atom::{Decode, Moof};
use std::time::Duration;

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/resolution-change.ts");
const OPUS_20: &[u8] = include_bytes!("../tests/fixtures/opus-48k-stereo-20ms.ogg");
const NOW: SystemTime = SystemTime::UNIX_EPOCH;
/// The fixture's change: the 128x72 half's first IDR, 90 kHz.
const CHANGE: i64 = 309_840;

/// An access unit's fields, copyable.
#[derive(Clone)]
struct Au {
    data: Vec<u8>,
    idr: bool,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

impl Au {
    fn of(au: codec::AccessUnit) -> Self {
        Self {
            data: au.data,
            idr: au.idr,
            sps: au.sps,
            pps: au.pps,
        }
    }

    fn unit(self) -> codec::AccessUnit {
        codec::AccessUnit {
            data: self.data,
            idr: self.idr,
            sps: self.sps,
            pps: self.pps,
        }
    }
}

#[derive(Clone)]
enum Event {
    Video(i64, i64, Au),
    Aac(i64, AacConfig, Vec<u8>),
    Opus(i64, Vec<u8>),
}

impl Event {
    fn at(&self) -> i64 {
        match self {
            Self::Video(dts, ..) | Self::Aac(dts, ..) | Self::Opus(dts, ..) => *dts,
        }
    }
}

/// The fixture's events, in file order, video as access units.
fn events() -> Vec<Event> {
    let mut out = Vec::new();
    elementary(FIXTURE, |e| match e {
        Es::Video { dts, pts, annexb } => {
            out.push(Event::Video(dts, pts, Au::of(codec::access_unit(&annexb))));
        }
        Es::Audio {
            pts,
            config,
            payload,
        } => out.push(Event::Aac(pts, config, payload)),
    });
    out
}

fn aac() -> AacConfig {
    events()
        .into_iter()
        .find_map(|e| match e {
            Event::Aac(_, config, _) => Some(config),
            _ => None,
        })
        .expect("AAC")
}

/// Opus packets of the 20 ms Ogg fixture (RFC 7845: OpusHead and OpusTags,
/// then audio), each with its 90 kHz time from `start`, repeated until
/// `until`. Repeated packets have equal payloads, so counts are compared
/// in order, not as sets.
fn opus_events(start: i64, until: i64) -> Vec<Event> {
    let mut packets = Vec::new();
    let mut data = OPUS_20;
    let mut current = Vec::new();
    while data.len() >= 27 {
        let count = usize::from(data[26]);
        let table = &data[27..27 + count];
        let mut body = &data[27 + count..];
        let mut used = 0;
        for &lacing in table {
            let (segment, rest) = body.split_at(usize::from(lacing));
            current.extend_from_slice(segment);
            body = rest;
            used += usize::from(lacing);
            if lacing < 255 {
                packets.push(std::mem::take(&mut current));
            }
        }
        data = &data[27 + count + used..];
    }
    // Audio packets after OpusHead and OpusTags, repeated to cover the span.
    let audio: Vec<Vec<u8>> = packets.into_iter().skip(2).collect();
    let mut at = start;
    let mut out = Vec::new();
    for p in audio.iter().cycle().cloned() {
        if at >= until {
            break;
        }
        let samples = i64::from(codec::opus_packet(&p).expect("Opus").samples());
        out.push(Event::Opus(at, p));
        at += samples * 90_000 / 48_000;
    }
    out
}

/// Samples of `track` in every part the muxer has, in order.
fn track_samples(m: &Muxer, track: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for part in parts(m) {
        let moof = Moof::decode(&mut part.data.as_ref()).expect("moof");
        for traf in moof.traf.iter().filter(|t| t.tfhd.track_id == track) {
            for run in &traf.trun {
                let mut offset = run.data_offset.expect("offset") as usize;
                for entry in &run.entries {
                    let end = offset + entry.size.expect("size") as usize;
                    out.push(part.data[offset..end].to_vec());
                    offset = end;
                }
            }
        }
    }
    out
}

fn parts(m: &Muxer) -> Vec<Arc<Part>> {
    (0..m.next_part_id())
        .filter_map(|id| match m.part(id) {
            crate::PartLookup::Found(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// Every part's video sample durations add up to its duration (90 kHz).
fn assert_honest_parts(m: &Muxer) {
    for part in parts(m) {
        let moof = Moof::decode(&mut part.data.as_ref()).expect("moof");
        let Some(video) = moof.traf.iter().find(|t| t.tfhd.track_id == VIDEO_TRACK) else {
            continue;
        };
        let sum: u64 = video
            .trun
            .iter()
            .flat_map(|r| &r.entries)
            .map(|e| u64::from(e.duration.expect("duration")))
            .sum();
        let want = (part.duration.as_nanos() * 90_000 / 1_000_000_000) as u64;
        assert!(
            sum.abs_diff(want) <= 1,
            "part {}: samples {sum} ticks, duration {want}",
            part.id
        );
    }
}

/// The playlist rules after every step, and the targets once fixed.
#[derive(Default)]
struct Watch {
    check: playlist::RuleCheck,
    targets: Option<(u64, Duration)>,
}

impl Watch {
    fn step(&mut self, m: Option<&Muxer>) {
        let Some(m) = m.filter(|m| m.has_content()) else {
            return;
        };
        self.check
            .check(&playlist::media(m, "p"))
            .expect("every playlist keeps the HLS rules");
        let now = (m.target_duration(), m.part_target());
        assert_eq!(*self.targets.get_or_insert(now), now, "targets unchanged");
    }
}

fn config() -> MuxerConfig {
    // A window that keeps every part of the run.
    MuxerConfig {
        segment_count: 64,
        ..MuxerConfig::default()
    }
}

/// Feeds `events` to `es`, ending with the interrupt a disconnect makes.
fn feed(es: &mut EsIngest, events: &[Event], watch: &mut Watch) -> Update {
    let mut all = Update::default();
    for e in events {
        let up = match e.clone() {
            Event::Video(dts, pts, au) => es.video(dts, pts, au.unit(), NOW),
            Event::Aac(pts, config, payload) => es.audio_at(pts, config, &payload, NOW),
            Event::Opus(pts, packet) => es.opus_at(pts, &packet, NOW),
        }
        .expect("fixture");
        all.merge(up);
        watch.step(es.muxer());
    }
    all
}

fn finish(es: &mut EsIngest, watch: &mut Watch) -> Update {
    let up = es.muxer_mut().expect("muxer").interrupt(NOW);
    watch.step(es.muxer());
    up
}

/// What should come out: every video AU from the first IDR on, and every
/// audio frame from that IDR's time on. Left as it was at the start of a
/// stream (no interrupt is involved): audio before the first keyframe has
/// no run to join, and of the audio that arrives before the first part
/// opens, at the stream's second video frame, only the last frame is kept.
fn expected(events: &[Event]) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let first = events
        .iter()
        .find_map(|e| match e {
            Event::Video(dts, _, au) if au.idr => Some(*dts),
            _ => None,
        })
        .unwrap_or(0);
    let second = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Event::Video(..)))
        .nth(1)
        .map_or(0, |(i, _)| i);
    let early: Vec<usize> = (0..second)
        .filter(|&i| matches!(&events[i], Event::Aac(p, ..) | Event::Opus(p, _) if *p >= first))
        .collect();
    let dropped = &early[..early.len().saturating_sub(1)];
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for (i, e) in events.iter().enumerate() {
        match e {
            Event::Video(dts, _, au) if *dts >= first => video.push(au.data.clone()),
            Event::Aac(pts, _, p) | Event::Opus(pts, p)
                if *pts >= first && !dropped.contains(&i) =>
            {
                audio.push(p.clone());
            }
            _ => {}
        }
    }
    (video, audio)
}

fn assert_all_out(es: &EsIngest, events: &[Event], what: &str) {
    let m = es.muxer().expect("muxer");
    let (video, audio) = expected(events);
    let (got_video, got_audio) = (track_samples(m, VIDEO_TRACK), track_samples(m, AUDIO_TRACK));
    assert_eq!(
        (got_video.len(), got_audio.len()),
        (video.len(), audio.len()),
        "{what}: video and audio frames out of those in"
    );
    assert_eq!(got_video, video, "{what}: video payloads");
    assert_eq!(got_audio, audio, "{what}: audio payloads");
    assert_honest_parts(m);
}

fn aac_ingest() -> EsIngest {
    let mut es = EsIngest::new(config());
    es.audio_config(aac());
    es
}

/// The codec epoch change with the fixture in time order, then the end.
#[test]
fn an_epoch_change_keeps_every_video_and_aac_frame() {
    let mut events = events();
    let from = events
        .iter()
        .position(|e| e.at() >= CHANGE - 9_000)
        .expect("the change");
    events[from..].sort_by_key(Event::at);
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    let up = feed(&mut es, &events, &mut watch);
    assert!(up.restarted, "the change starts an epoch");
    assert!(!up.media_loss, "nothing lost");
    assert!(!finish(&mut es, &mut watch).media_loss);
    assert_all_out(&es, &events, "epoch change");
    assert_continuous_audio(es.muxer().expect("muxer"));
}

/// In the fixture's own order, its audio PES arrive up to 200 ms after the
/// video they go with (FFmpeg packs several AAC frames a PES). The frame
/// stamped just before the change (3.4213 s) then arrives after the new
/// run's first part has closed: no part it fits is open, so it is dropped
/// and reported. Everything else comes out.
#[test]
fn late_audio_after_the_new_runs_first_part_is_reported() {
    let events = events();
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    let up = feed(&mut es, &events, &mut watch);
    assert!(up.restarted && up.media_loss, "the late frame is reported");
    finish(&mut es, &mut watch);
    let late = events
        .iter()
        .find_map(|e| match e {
            Event::Aac(pts, _, p) if *pts == 307_920 => Some(p.clone()),
            _ => None,
        })
        .expect("the late frame");
    let kept: Vec<Event> = events
        .iter()
        .filter(|e| !matches!(e, Event::Aac(_, _, p) if *p == late))
        .cloned()
        .collect();
    assert_all_out(&es, &kept, "late audio");
    assert_continuous_audio_but_for(es.muxer().expect("muxer"), 1024);
}

/// A late frame dropped and then the stream's end: the end's interrupt
/// reports it, though no video came after it.
#[test]
fn late_audio_lost_just_before_the_end_is_reported_by_it() {
    let events = events();
    // Up to and including the late frame, and no video after it.
    let late = events
        .iter()
        .position(|e| matches!(e, Event::Aac(pts, ..) if *pts == 307_920))
        .expect("the late frame");
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    // The new run's first part must have closed before the late frame
    // comes; in the fixture's order it has, by the frame's arrival.
    let up = feed(&mut es, &events[..late], &mut watch);
    assert!(up.restarted && !up.media_loss);
    let up = feed(&mut es, &events[late..=late], &mut watch);
    assert!(!up.media_loss, "audio alone reports nothing yet");
    // The frame is held until the next audio places or drops it.
    let next_audio = events[late + 1..]
        .iter()
        .find(|e| matches!(e, Event::Aac(..)))
        .cloned()
        .expect("audio after it");
    feed(&mut es, &[next_audio], &mut watch);
    assert!(finish(&mut es, &mut watch).media_loss, "the end reports it");
}

/// Audio-only Opus through a codec change of its own (a new channel count
/// on reconnect, which reconfigures the audio track): every packet out.
#[test]
fn an_audio_only_opus_epoch_change_keeps_every_packet() {
    let first = opus_events(0, 90_000);
    let second = opus_events(90_000, 2 * 90_000);
    let mut es = EsIngest::new(config());
    es.opus_config(OpusConfig::STEREO).expect("stereo");
    es.declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("audio only");
    let mut watch = Watch::default();
    feed(&mut es, &first, &mut watch);
    let m = es.muxer_mut().expect("muxer");
    let up = m
        .reconfigure_with_audio(None, Some(AudioConfig::Opus(OpusConfig::MONO)), NOW)
        .expect("a new audio epoch");
    assert!(!up.media_loss);
    // The fixture's packets are stereo; the new epoch only has to keep
    // whatever comes, so count them through it.
    feed(&mut es, &second, &mut watch);
    finish(&mut es, &mut watch);
    let m = es.muxer().expect("muxer");
    assert_eq!(m.current_initialization_id(), 1);
    let audio: Vec<Vec<u8>> = first
        .iter()
        .chain(&second)
        .filter_map(|e| match e {
            Event::Opus(_, p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(track_samples(m, AUDIO_TRACK), audio);
}

/// As [`assert_continuous_audio`], but allowing one gap of up to
/// `samples`, where a frame was dropped.
fn assert_continuous_audio_but_for(m: &Muxer, samples: u64) {
    let mut end: Option<u64> = None;
    let mut gaps = 0;
    for part in parts(m) {
        let moof = Moof::decode(&mut part.data.as_ref()).expect("moof");
        let Some(audio) = moof.traf.iter().find(|t| t.tfhd.track_id == AUDIO_TRACK) else {
            continue;
        };
        let base = audio.tfdt.as_ref().expect("tfdt").base_media_decode_time;
        if let Some(previous) = end
            && base.abs_diff(previous) > 2
        {
            assert!(
                base > previous && base - previous <= samples + 2,
                "part {}: a gap of {} samples",
                part.id,
                base as i64 - previous as i64
            );
            gaps += 1;
        }
        end = Some(
            base + audio
                .trun
                .iter()
                .flat_map(|r| &r.entries)
                .map(|e| u64::from(e.duration.expect("duration")))
                .sum::<u64>(),
        );
    }
    assert!(gaps <= 1, "{gaps} gaps");
}

/// As above, but video pauses 300 ms before the new IDR while audio carries
/// on, as a browser restarting its encoder does.
#[test]
fn an_epoch_change_after_a_video_pause_keeps_every_frame() {
    let mut events: Vec<Event> = events()
        .into_iter()
        .map(|e| match e {
            Event::Video(dts, pts, au) if dts >= CHANGE => {
                Event::Video(dts + 27_000, pts + 27_000, au)
            }
            other => other,
        })
        .collect();
    // Arrival in time order from just before the change; the stream's
    // start keeps its file order.
    let from = events
        .iter()
        .position(|e| e.at() >= CHANGE - 9_000)
        .expect("the change");
    events[from..].sort_by_key(Event::at);
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    let up = feed(&mut es, &events, &mut watch);
    assert!(!up.media_loss, "nothing lost");
    finish(&mut es, &mut watch);
    assert_all_out(&es, &events, "epoch change after a pause");
}

/// A publisher leaves (the disconnect's interrupt) and another resumes the
/// stream with a new configuration.
#[test]
fn a_reconnect_keeps_every_frame_of_both_connections() {
    let events = events();
    let (first, second): (Vec<Event>, Vec<Event>) =
        events.iter().cloned().partition(|e| e.at() < CHANGE);
    let mut watch = Watch::default();
    let mut es = aac_ingest();
    feed(&mut es, &first, &mut watch);
    let mut held = es.take_muxer().expect("muxer");
    held.interrupt(NOW);
    let mut back = EsIngest::resume(config(), held);
    back.audio_config(aac());
    feed(&mut back, &second, &mut watch);
    finish(&mut back, &mut watch);
    let m = back.muxer().expect("muxer");
    let (mut video, mut audio) = expected(&first);
    let (v2, a2) = expected(&second);
    video.extend(v2);
    audio.extend(a2);
    assert_eq!(
        (
            track_samples(m, VIDEO_TRACK).len(),
            track_samples(m, AUDIO_TRACK).len()
        ),
        (video.len(), audio.len()),
        "reconnect: video and audio frames out of those in"
    );
    assert_eq!(track_samples(m, VIDEO_TRACK), video);
    assert_eq!(track_samples(m, AUDIO_TRACK), audio);
    assert_honest_parts(m);
}

/// Audio a little ahead of video at the change: audio stamped after the
/// new keyframe arrives before it. It is carried into the new run, and the
/// audio timeline runs on across the boundary with no hole or overlap.
#[test]
fn an_epoch_change_with_audio_ahead_carries_it_into_the_new_run() {
    let mut events = events();
    // Video from the change on arrives 250 ms later than its own time.
    let late: Vec<Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Video(dts, ..) if *dts >= CHANGE))
        .cloned()
        .collect();
    events.retain(|e| !matches!(e, Event::Video(dts, ..) if *dts >= CHANGE));
    for e in late {
        let arrives = e.at() + 22_500;
        let at = events
            .iter()
            .position(|x| x.at() > arrives)
            .unwrap_or(events.len());
        events.insert(at, e);
    }
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    let up = feed(&mut es, &events, &mut watch);
    assert!(up.restarted && !up.media_loss);
    finish(&mut es, &mut watch);
    assert_all_out(&es, &events, "audio ahead");
    assert_continuous_audio(es.muxer().expect("muxer"));
}

/// Each part's audio starts where the previous part's ended, at 48 kHz.
fn assert_continuous_audio(m: &Muxer) {
    let mut end: Option<u64> = None;
    for part in parts(m) {
        let moof = Moof::decode(&mut part.data.as_ref()).expect("moof");
        let Some(audio) = moof.traf.iter().find(|t| t.tfhd.track_id == AUDIO_TRACK) else {
            continue;
        };
        let base = audio.tfdt.as_ref().expect("tfdt").base_media_decode_time;
        let length: u64 = audio
            .trun
            .iter()
            .flat_map(|r| &r.entries)
            .map(|e| u64::from(e.duration.expect("duration")))
            .sum();
        if let Some(previous) = end {
            assert!(
                base.abs_diff(previous) <= 2,
                "part {}: audio at {base}, the last ended at {previous}",
                part.id
            );
        }
        end = Some(base + length);
    }
}

/// Two audio frames between the new keyframe and the frame after it: the
/// new run's part is open from its keyframe, so neither is lost.
#[test]
fn audio_between_the_new_keyframe_and_its_next_frame_is_kept() {
    let mut events = video_with_opus();
    let find_idr = |events: &[Event]| {
        events
            .iter()
            .position(|e| matches!(e, Event::Video(dts, _, au) if au.idr && *dts == CHANGE))
            .expect("the change")
    };
    // The packet stamped just before the keyframe arrives just after it,
    // so two packets fall between the keyframe and the next frame.
    let idr = find_idr(&events);
    let before = events[..idr]
        .iter()
        .rposition(|e| matches!(e, Event::Opus(..)))
        .expect("a packet before the change");
    let packet = events.remove(before);
    events.insert(idr, packet);
    let idr = find_idr(&events);
    let next = idr
        + events[idr + 1..]
            .iter()
            .position(|e| matches!(e, Event::Video(..)))
            .expect("a frame after it")
        + 1;
    let between = events[idr + 1..next]
        .iter()
        .filter(|e| matches!(e, Event::Opus(..)))
        .count();
    assert!(between >= 2, "{between} packets in the window");
    let mut es = EsIngest::new(config());
    es.opus_config(OpusConfig::STEREO).expect("stereo");
    let mut watch = Watch::default();
    feed(&mut es, &events, &mut watch);
    finish(&mut es, &mut watch);
    assert_all_out(&es, &events, "audio after the new keyframe");
}

/// A reconnect whose old connection ends with audio further past its last
/// video than the open part and one more part can hold: what does not fit
/// is dropped, and said so.
#[test]
fn a_reconnect_with_audio_past_the_part_room_reports_the_loss() {
    let first: Vec<Event> = events()
        .into_iter()
        .filter(|e| match e {
            // Video stops 800 ms before the change; audio carries on to it.
            Event::Video(dts, ..) => *dts < CHANGE - 72_000,
            other => other.at() < CHANGE,
        })
        .collect();
    let mut es = aac_ingest();
    let mut watch = Watch::default();
    feed(&mut es, &first, &mut watch);
    let mut held = es.take_muxer().expect("muxer");
    let up = held.interrupt(NOW);
    assert!(up.media_loss, "the audio past the part's room is reported");
}

fn video_with_opus() -> Vec<Event> {
    let mut events: Vec<Event> = events()
        .into_iter()
        .filter(|e| matches!(e, Event::Video(..)))
        .collect();
    let first = events.first().map_or(0, Event::at);
    let last = events.last().map_or(0, Event::at);
    events.extend(opus_events(first, last));
    events.sort_by_key(Event::at);
    events
}

/// WHIP's shape: video with Opus, through the codec epoch change.
#[test]
fn an_epoch_change_keeps_every_video_and_opus_frame() {
    let events = video_with_opus();
    let mut es = EsIngest::new(config());
    es.opus_config(OpusConfig::STEREO).expect("stereo");
    let mut watch = Watch::default();
    let up = feed(&mut es, &events, &mut watch);
    assert!(up.restarted);
    finish(&mut es, &mut watch);
    assert_all_out(&es, &events, "video and Opus");
}

/// Audio-only Opus, re-anchored as the WHIP server does on an arrival-time
/// jump: an interrupt, then packets 500 ms later on the sender's clock.
#[test]
fn an_audio_only_opus_reanchor_keeps_every_packet() {
    let mut events = opus_events(0, 90_000);
    events.extend(opus_events(90_000 + 45_000, 2 * 90_000 + 45_000));
    let mut es = EsIngest::new(config());
    es.opus_config(OpusConfig::STEREO).expect("stereo");
    es.declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("audio only");
    let mut watch = Watch::default();
    let half = events.len() / 2;
    feed(&mut es, &events[..half], &mut watch);
    finish(&mut es, &mut watch);
    feed(&mut es, &events[half..], &mut watch);
    finish(&mut es, &mut watch);
    let m = es.muxer().expect("muxer");
    let audio: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            Event::Opus(_, p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        track_samples(m, AUDIO_TRACK).len(),
        audio.len(),
        "Opus packets"
    );
    assert_eq!(track_samples(m, AUDIO_TRACK), audio);
}
