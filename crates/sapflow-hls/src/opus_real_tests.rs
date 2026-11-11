//! Real Opus packets, from libopus fixtures (and, for the evidence run, a
//! Chrome recording), through ingest, the muxer and the playlists.
//!
//! The Ogg reader here is test scaffolding only (RFC 3533 pages, RFC 7845
//! identification header): sapflow takes Opus packets, not Ogg files.
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use mp4_atom::{Decode, Moof};

use crate::codec::{self, AudioConfig, OpusConfig, opus_packet};
use crate::fmp4::{self, AUDIO_TRACK, VIDEO_TRACK};
use crate::ingest::tests_support::{Es, elementary};
use crate::muxer::Part;
use crate::{EsIngest, Muxer, MuxerConfig, PartLookup, TrackSet, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

const STEREO_20: &[u8] = include_bytes!("../tests/fixtures/opus-48k-stereo-20ms.ogg");
const MONO_60: &[u8] = include_bytes!("../tests/fixtures/opus-48k-mono-60ms.ogg");
const STEREO_120: &[u8] = include_bytes!("../tests/fixtures/opus-48k-stereo-120ms.ogg");
const VIDEO: &[u8] = include_bytes!("../tests/fixtures/testsrc-3s.ts");

/// The packets of a single-stream Ogg file (RFC 3533 section 6): segments
/// of 255 bytes continue a packet, a shorter one ends it.
fn ogg_packets(mut data: &[u8]) -> Vec<Vec<u8>> {
    let (mut packets, mut current) = (Vec::new(), Vec::new());
    while data.len() >= 27 {
        assert_eq!(&data[..4], b"OggS", "Ogg capture pattern");
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
    packets
}

/// Channel count and pre-skip from an OpusHead packet (RFC 7845 section
/// 5.1), then the audio packets after OpusTags.
fn opus_stream(ogg: &[u8]) -> (u8, u16, Vec<Vec<u8>>) {
    let mut packets = ogg_packets(ogg);
    assert!(packets.len() > 2);
    let head = packets.remove(0);
    assert_eq!(&head[..8], b"OpusHead");
    assert_eq!(&packets.remove(0)[..8], b"OpusTags");
    (head[9], u16::from_le_bytes([head[10], head[11]]), packets)
}

fn samples(packets: &[Vec<u8>]) -> Vec<u32> {
    packets
        .iter()
        .map(|p| opus_packet(p).expect("valid real Opus packet").samples())
        .collect()
}

fn ticks(samples: u32) -> i64 {
    i64::from(samples) * 90_000 / 48_000
}

/// Every part a muxer closes, kept as it is produced, so a recording or a
/// whole-stream file can be built after the live window has moved on.
#[derive(Default)]
struct Collected {
    parts: Vec<Arc<Part>>,
    next: u64,
}

impl Collected {
    fn take(&mut self, m: &Muxer) {
        while self.next < m.next_part_id() {
            if let PartLookup::Found(part) = m.part(self.next) {
                self.parts.push(part);
            }
            self.next += 1;
        }
    }

    /// The audio sample durations and video frame count across the parts.
    fn tracks(&self) -> (Vec<u32>, usize) {
        let (mut audio, mut video) = (Vec::new(), 0);
        for part in &self.parts {
            let moof = Moof::decode(&mut Cursor::new(&part.data[..])).expect("moof");
            for traf in &moof.traf {
                let durations = traf
                    .trun
                    .iter()
                    .flat_map(|r| &r.entries)
                    .map(|e| e.duration.expect("duration"));
                match traf.tfhd.track_id {
                    AUDIO_TRACK => audio.extend(durations),
                    VIDEO_TRACK => video += durations.count(),
                    _ => panic!("unexpected track"),
                }
            }
        }
        (audio, video)
    }

    /// The whole stream as one fragmented MP4: init, then every part.
    fn stream_file(&self, init: &[u8]) -> Vec<u8> {
        let mut out = init.to_vec();
        for part in &self.parts {
            out.extend_from_slice(&part.data);
        }
        out
    }

    /// The server recorder's layout: a recording init carrying the total
    /// duration, then each part rebased to start at zero.
    fn recording_file(&self, m: &Muxer) -> Vec<u8> {
        let duration = self.parts.iter().map(|p| p.span).sum();
        let audio = m.audio_config().copied();
        let mut out = fmp4::init_segment_for_audio_tracks(m.video(), audio.as_ref(), duration)
            .expect("recording init")
            .to_vec();
        let mut origin = None;
        for part in &self.parts {
            out.extend_from_slice(
                &fmp4::rebase_part_for_file(
                    &part.data,
                    audio.map(|a| a.sample_rate()),
                    &mut origin,
                )
                .expect("own fragment"),
            );
        }
        out
    }
}

/// A live replay: each part and segment as the muxer closes it, and a
/// snapshot of the media playlist after every change, with the media time
/// it appeared at.
struct Live<'a> {
    dir: &'a Path,
    written_parts: u64,
    written_segments: std::collections::BTreeSet<u64>,
    inits: std::collections::BTreeSet<u64>,
    /// Media time in ms, next segment id, open parts, playlist file.
    snapshots: Vec<(i64, u64, usize, String)>,
    last: String,
}

impl<'a> Live<'a> {
    fn new(dir: &'a Path) -> Self {
        std::fs::create_dir_all(dir).expect("live dir");
        Self {
            dir,
            written_parts: 0,
            written_segments: Default::default(),
            inits: Default::default(),
            snapshots: Vec::new(),
            last: String::new(),
        }
    }

    fn update(&mut self, m: &Muxer, media_ticks: i64) {
        let write = |name: String, data: &[u8]| {
            std::fs::write(self.dir.join(name), data).expect("write live file");
        };
        for id in 0..=m.current_initialization_id() {
            if !self.inits.contains(&id)
                && let Some(init) = m.initialization(id)
            {
                write(playlist::epoch_init_name("live", id), init);
                self.inits.insert(id);
            }
        }
        while self.written_parts < m.next_part_id() {
            if let PartLookup::Found(part) = m.part(self.written_parts) {
                write(playlist::part_name("live", part.id), &part.data);
            }
            self.written_parts += 1;
        }
        for entry in m.entries() {
            if let crate::muxer::Entry::Segment(s) = entry
                && self.written_segments.insert(s.id)
            {
                let mut body = Vec::with_capacity(s.data.len());
                for chunk in s.data.chunks() {
                    body.extend_from_slice(chunk);
                }
                write(playlist::segment_name("live", s.id), &body);
            }
        }
        if !m.has_content() {
            return;
        }
        let text = playlist::media(m, "live");
        if text == self.last {
            return;
        }
        let name = format!("pl-{:05}.m3u8", self.snapshots.len());
        write(name.clone(), text.as_bytes());
        write("master.m3u8".into(), playlist::multivariant(m).as_bytes());
        let next = m.media_sequence() + m.entries().len() as u64;
        self.snapshots
            .push((media_ticks / 90, next, m.open_parts().len(), name));
        self.last = text;
    }

    fn finish(self) {
        let rows: Vec<String> = self
            .snapshots
            .iter()
            .map(|(ms, msn, parts, file)| {
                format!("{{\"ms\":{ms},\"msn\":{msn},\"parts\":{parts},\"file\":\"{file}\"}}")
            })
            .collect();
        std::fs::write(
            self.dir.join("snapshots.json"),
            format!("[\n{}\n]\n", rows.join(",\n")),
        )
        .expect("snapshots");
    }
}

/// Audio-only Opus: every packet in order at exact timestamps.
fn audio_only(packets: &[Vec<u8>], mut live: Option<&mut Live<'_>>) -> (Muxer, Collected) {
    let mut ingest = EsIngest::new(MuxerConfig::default());
    ingest
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    let mut rules = playlist::RuleCheck::default();
    let mut collected = Collected::default();
    let mut pts = 0;
    for p in packets {
        let up = ingest
            .opus_at(pts, p, NOW + Duration::from_micros(pts as u64 * 100 / 9))
            .expect("real Opus packet");
        assert!(!up.media_loss && !up.restarted);
        pts += ticks(opus_packet(p).expect("valid").samples());
        let m = ingest.muxer().expect("muxer");
        collected.take(m);
        if m.has_content() {
            rules
                .check(&playlist::media(m, "p"))
                .expect("Apple's rules");
        }
        if let Some(live) = live.as_deref_mut() {
            live.update(m, pts);
        }
    }
    let mut m = ingest.take_muxer().expect("muxer");
    m.interrupt(NOW);
    collected.take(&m);
    (m, collected)
}

/// Video from a TS file (its own audio ignored) and Opus packets from
/// time zero, interleaved in time order through one ingest.
fn audio_video(
    ts: &[u8],
    packets: &[Vec<u8>],
    mut live: Option<&mut Live<'_>>,
) -> (Muxer, Collected) {
    let mut video = Vec::new();
    elementary(ts, |es| {
        if let Es::Video { dts, pts, annexb } = es {
            video.push((dts, pts, annexb));
        }
    });
    let origin = video.first().expect("video").0;
    let mut ingest = EsIngest::new(MuxerConfig::default());
    let mut rules = playlist::RuleCheck::default();
    let mut collected = Collected::default();
    let (mut audio_pts, mut a, mut v) = (0i64, 0, 0);
    while v < video.len() {
        let video_dts = video[v].0 - origin;
        let now = |t: i64| NOW + Duration::from_micros(t as u64 * 100 / 9);
        let t = if a < packets.len() && audio_pts <= video_dts {
            ingest.opus(audio_pts, &packets[a]).expect("real Opus");
            let t = audio_pts;
            audio_pts += ticks(opus_packet(&packets[a]).expect("valid").samples());
            a += 1;
            t
        } else {
            let (dts, pts, annexb) = &video[v];
            let up = ingest
                .video(
                    dts - origin,
                    pts - origin,
                    codec::access_unit(annexb),
                    now(dts - origin),
                )
                .expect("real video");
            assert!(!up.media_loss && !up.restarted);
            v += 1;
            dts - origin
        };
        if let Some(m) = ingest.muxer() {
            collected.take(m);
            if m.has_content() {
                rules
                    .check(&playlist::media(m, "p"))
                    .expect("Apple's rules");
            }
            if let Some(live) = live.as_deref_mut() {
                live.update(m, t);
            }
        }
    }
    let mut m = ingest.take_muxer().expect("muxer");
    m.interrupt(NOW);
    collected.take(&m);
    (m, collected)
}

#[test]
fn real_libopus_packets_have_the_toc_durations_ffprobe_reports() {
    for (ogg, channels, frame) in [
        (STEREO_20, 2, 960),
        (MONO_60, 1, 2880),
        (STEREO_120, 2, 5760),
    ] {
        let (head_channels, pre_skip, packets) = opus_stream(ogg);
        assert_eq!((head_channels, pre_skip), (channels, 312), "libopus header");
        let durations = samples(&packets);
        assert!(durations.iter().all(|&d| d == frame), "{durations:?}");
        assert!(
            packets
                .iter()
                .all(|p| opus_packet(p).expect("valid").stereo == (channels == 2))
        );
        // 120 ms of libopus is several frames in one packet (code 1 or 3).
        if frame == 5760 {
            assert!(
                packets
                    .iter()
                    .all(|p| opus_packet(p).expect("valid").frames > 1)
            );
        }
    }
}

#[test]
fn real_audio_only_opus_keeps_every_packet_at_its_toc_duration() {
    for ogg in [STEREO_20, MONO_60, STEREO_120] {
        let (channels, _, packets) = opus_stream(ogg);
        let (m, collected) = audio_only(&packets, None);
        assert_eq!(
            m.audio_config(),
            Some(&AudioConfig::Opus(OpusConfig { channels }))
        );
        let (audio, video) = collected.tracks();
        assert_eq!(video, 0);
        assert_eq!(audio, samples(&packets), "every packet, in order");
        assert!(playlist::multivariant(&m).contains("CODECS=\"Opus\""));
    }
}

#[test]
fn real_opus_with_real_video_cuts_parts_on_video() {
    let (_, _, packets) = opus_stream(STEREO_20);
    let (m, collected) = audio_video(VIDEO, &packets, None);
    assert_eq!(m.tracks(), TrackSet::AudioVideo);
    let (audio, video) = collected.tracks();
    assert!(video > 60, "{video} frames");
    // Two seconds of 20 ms packets, the first before the keyframe opens the
    // muxer and the tail after the last video frame left out.
    assert!(
        audio.len() > 80 && audio.len() < packets.len(),
        "{}",
        audio.len()
    );
    assert!(audio.iter().all(|&d| d == 960));
    assert!(playlist::multivariant(&m).contains(",Opus\""));
}

#[test]
fn real_mono_then_stereo_packets_keep_the_first_configuration() {
    let (_, _, mono) = opus_stream(MONO_60);
    let (_, _, stereo) = opus_stream(STEREO_20);
    let (_, _, long) = opus_stream(STEREO_120);
    let mixed: Vec<Vec<u8>> = mono.into_iter().chain(stereo).chain(long).collect();
    let (m, collected) = audio_only(&mixed, None);
    assert_eq!(m.audio_config(), Some(&AudioConfig::Opus(OpusConfig::MONO)));
    assert_eq!(m.current_initialization_id(), 0);
    assert_eq!(collected.tracks().0, samples(&mixed));
}

fn json_summary(name: &str, m: &Muxer, collected: &Collected, packets: &[Vec<u8>]) -> String {
    let (audio, video) = collected.tracks();
    format!(
        "{{\"file\":\"{name}\",\"channels\":{},\"packets_in\":{},\"toc_samples_in\":{},\"audio_samples_out\":{},\"video_frames_out\":{video},\"seconds_out\":{:.6}}}",
        m.audio_config().map_or(0, |a| a.channels()),
        packets.len(),
        samples(packets).iter().map(|&s| u64::from(s)).sum::<u64>(),
        audio.iter().map(|&s| u64::from(s)).sum::<u64>(),
        collected
            .parts
            .iter()
            .map(|p| p.span)
            .sum::<Duration>()
            .as_secs_f64(),
    )
}

/// Writes sapflow's own output for the black-box checks: whole-stream and
/// recording files for FFmpeg, and LL-HLS replays for the browsers. Run it
/// by hand: `SAPFLOW_OPUS_EVIDENCE_OUT=<dir> [SAPFLOW_OPUS_EVIDENCE_SRC=<dir
/// with chrome-opus-20s.ogg and video-720p30-20s.ts>] cargo test -p
/// sapflow-hls write_opus_evidence -- --ignored`.
#[test]
#[ignore = "writes files for the FFmpeg and browser checks"]
fn write_opus_evidence() {
    let out = std::path::PathBuf::from(
        std::env::var("SAPFLOW_OPUS_EVIDENCE_OUT").expect("SAPFLOW_OPUS_EVIDENCE_OUT"),
    );
    std::fs::create_dir_all(&out).expect("out dir");
    let mut summary = Vec::new();
    let (_, _, mono) = opus_stream(MONO_60);
    let (_, _, stereo) = opus_stream(STEREO_20);
    let (_, _, long) = opus_stream(STEREO_120);
    let mixed: Vec<Vec<u8>> = mono.iter().chain(&stereo).chain(&long).cloned().collect();
    for (name, packets) in [
        ("audio-stereo-20ms.mp4", &stereo),
        ("audio-mono-60ms.mp4", &mono),
        ("audio-stereo-120ms.mp4", &long),
        ("audio-mixed-mono-then-stereo.mp4", &mixed),
    ] {
        let (m, collected) = audio_only(packets, None);
        std::fs::write(out.join(name), collected.stream_file(m.init())).expect("write");
        summary.push(json_summary(name, &m, &collected, packets));
        let rec = format!("recording-{name}");
        std::fs::write(out.join(&rec), collected.recording_file(&m)).expect("write");
        summary.push(json_summary(&rec, &m, &collected, packets));
    }
    let (m, collected) = audio_video(VIDEO, &stereo, None);
    std::fs::write(
        out.join("av-testsrc-stereo-20ms.mp4"),
        collected.stream_file(m.init()),
    )
    .expect("write");
    summary.push(json_summary(
        "av-testsrc-stereo-20ms.mp4",
        &m,
        &collected,
        &stereo,
    ));
    if let Ok(src) = std::env::var("SAPFLOW_OPUS_EVIDENCE_SRC") {
        let src = Path::new(&src);
        let chrome = std::fs::read(src.join("chrome-opus-20s.ogg")).expect("Chrome Opus");
        let video = std::fs::read(src.join("video-720p30-20s.ts")).expect("video");
        let (channels, pre_skip, packets) = opus_stream(&chrome);
        summary.push(format!(
            "{{\"source\":\"chrome-opus-20s.ogg\",\"head_channels\":{channels},\"head_pre_skip\":{pre_skip},\"toc_stereo_packets\":{},\"packets\":{}}}",
            packets.iter().filter(|p| opus_packet(p).is_some_and(|i| i.stereo)).count(),
            packets.len()
        ));
        let dir = out.join("live-av");
        let mut live = Live::new(&dir);
        let (m, collected) = audio_video(&video, &packets, Some(&mut live));
        live.finish();
        std::fs::write(out.join("av-chrome.mp4"), collected.stream_file(m.init())).expect("write");
        summary.push(json_summary("av-chrome.mp4", &m, &collected, &packets));
        std::fs::write(
            out.join("recording-av-chrome.mp4"),
            collected.recording_file(&m),
        )
        .expect("write");
        summary.push(json_summary(
            "recording-av-chrome.mp4",
            &m,
            &collected,
            &packets,
        ));
        let dir = out.join("live-audio");
        let mut live = Live::new(&dir);
        let (m, collected) = audio_only(&packets, Some(&mut live));
        live.finish();
        std::fs::write(
            out.join("audio-chrome.mp4"),
            collected.stream_file(m.init()),
        )
        .expect("write");
        summary.push(json_summary("audio-chrome.mp4", &m, &collected, &packets));
    }
    std::fs::write(
        out.join("summary.json"),
        format!("[\n{}\n]\n", summary.join(",\n")),
    )
    .expect("summary");
}
