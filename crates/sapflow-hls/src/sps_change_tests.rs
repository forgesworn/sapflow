//! A mid-stream change of H.264 parameter sets, as an encoder makes when it
//! changes resolution under load, through each front end: TS (SRT), FLV
//! (RTMP, a new AVC sequence header) and elementary streams (WHIP). Each
//! must start one new codec epoch at the first changed IDR, with the new
//! SPS in a new init segment after a discontinuity, the targets unchanged,
//! and every sample after the change under that init.
//!
//! `resolution-change.ts` is two generated test-pattern encodes, 160x90
//! then 128x72, joined by stream copy (see `tests/fixtures/README.md`).

use super::tests_support::{Es, elementary};
use super::*;
use crate::flv::FlvIngest;
use crate::muxer::{Entry, Muxer};
use crate::playlist;
use std::time::Duration;

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/resolution-change.ts");
const NOW: SystemTime = SystemTime::UNIX_EPOCH;
const PREFIX: &str = "fixed";

/// The fixture's elementary events, in order, video as access units.
enum Event {
    Video(i64, i64, codec::AccessUnit),
    Audio(i64, AacConfig, Vec<u8>),
}

fn events() -> Vec<Event> {
    let mut out = Vec::new();
    elementary(FIXTURE, |e| match e {
        Es::Video { dts, pts, annexb } => {
            out.push(Event::Video(dts, pts, codec::access_unit(&annexb)))
        }
        Es::Audio {
            pts,
            config,
            payload,
        } => out.push(Event::Audio(pts, config, payload)),
    });
    out
}

/// The two SPS the fixture carries, in order.
fn both_sps() -> (Vec<u8>, Vec<u8>) {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for e in events() {
        if let Event::Video(_, _, au) = e
            && let Some(sps) = au.sps
            && !seen.contains(&sps)
        {
            seen.push(sps);
        }
    }
    assert_eq!(seen.len(), 2, "the fixture changes SPS once");
    let mut it = seen.into_iter();
    (it.next().expect("first"), it.next().expect("second"))
}

/// The fixture's AAC configuration, which encoders announce before the
/// first frame (RTMP's sequence header; WHIP's answer for Opus).
fn aac() -> AacConfig {
    events()
        .into_iter()
        .find_map(|e| match e {
            Event::Audio(_, config, _) => Some(config),
            Event::Video(..) => None,
        })
        .expect("the fixture has audio")
}

/// An ingest for elementary streams with its audio announced.
fn es_ingest() -> EsIngest {
    let mut es = EsIngest::new(MuxerConfig::default());
    es.audio_config(aac());
    es
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// What a run saw: the updates that said `restarted`, and the targets and
/// playlist rules checked after every step.
#[derive(Default)]
struct Run {
    restarts: usize,
    /// (target duration, part target) once content existed, before the change.
    targets: Option<(u64, Duration)>,
    check: playlist::RuleCheck,
}

impl Run {
    fn step(&mut self, update: Update, muxer: Option<&Muxer>) {
        if update.restarted {
            self.restarts += 1;
        }
        let Some(m) = muxer.filter(|m| m.has_content()) else {
            return;
        };
        self.check
            .check(&playlist::media(m, PREFIX))
            .expect("every playlist keeps the HLS rules");
        if m.current_initialization_id() == 0 {
            self.targets = Some((m.target_duration(), m.part_target()));
        }
    }
}

/// The epoch rule, checked on the muxer a whole run left.
fn assert_one_epoch_at_the_change(run: &Run, m: &Muxer) {
    let (old_sps, new_sps) = both_sps();
    assert_eq!(run.restarts, 1, "one new epoch, at the first changed IDR");
    assert_eq!(m.current_initialization_id(), 1);
    let video = m.video().expect("video");
    assert_eq!((video.width, video.height), (128, 72));
    let old = m.initialization(0).expect("the first init is retained");
    let new = m.initialization(1).expect("the new init");
    assert!(contains(old, &old_sps) && !contains(old, &new_sps));
    assert!(
        contains(new, &new_sps) && !contains(new, &old_sps),
        "new avcC"
    );
    assert_eq!(
        Some((m.target_duration(), m.part_target())),
        run.targets,
        "TARGETDURATION and PART-TARGET unchanged"
    );
    let text = playlist::media(m, PREFIX);
    let map = format!(
        "#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"{}\"",
        playlist::epoch_init_name(PREFIX, 1)
    );
    assert!(text.contains(&map), "{text}");
    assert_eq!(text.matches("#EXT-X-DISCONTINUITY\n").count(), 1);
    // Every segment and part names its own init, epoch 0 then epoch 1 and
    // never back; epoch 1 opens with a discontinuity and an independent
    // part, so its samples decode under the new avcC alone.
    let segments: Vec<_> = m
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Segment(s) => Some(s.clone()),
            Entry::Gap { .. } => None,
        })
        .collect();
    let ids: Vec<u64> = segments.iter().map(|s| s.initialization.id).collect();
    assert!(ids.windows(2).all(|w| w[0] <= w[1]), "{ids:?}");
    assert!(ids.contains(&0) && ids.contains(&1), "{ids:?}");
    for s in &segments {
        assert!(
            s.parts
                .iter()
                .all(|p| p.initialization.id == s.initialization.id),
            "a segment's parts share its init"
        );
    }
    let first = segments
        .iter()
        .find(|s| s.initialization.id == 1)
        .expect("an epoch 1 segment");
    assert!(first.discontinuity);
    assert!(first.parts.first().is_some_and(|p| p.independent));
    assert_eq!(
        first.parts.first().map(|p| p.id),
        Some(m.epoch_first_part_id())
    );
    assert!(m.open_parts().iter().all(|p| p.initialization.id == 1));
}

#[test]
fn an_elementary_stream_sps_change_starts_a_new_epoch() {
    let mut es = es_ingest();
    let mut run = Run::default();
    for e in events() {
        let up = match e {
            Event::Video(dts, pts, au) => es.video(dts, pts, au, NOW),
            Event::Audio(pts, config, payload) => es.audio_at(pts, config, &payload, NOW),
        }
        .expect("generated fixture");
        run.step(up, es.muxer());
    }
    assert_one_epoch_at_the_change(&run, es.muxer().expect("muxer"));
}

#[test]
fn a_transport_stream_sps_change_starts_a_new_epoch() {
    let mut ts = TsIngest::new(MuxerConfig::default());
    let mut run = Run::default();
    for chunk in FIXTURE.chunks(1316) {
        let up = ts.push(chunk, NOW).expect("generated fixture");
        run.step(up, ts.muxer());
    }
    assert_one_epoch_at_the_change(&run, ts.muxer().expect("muxer"));
}

/// An AVC sequence header (FLV packet type 0) for these parameter sets.
fn sequence_header(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut tag = vec![0x17, 0, 0, 0, 0, 1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    tag.extend((sps.len() as u16).to_be_bytes());
    tag.extend(sps);
    tag.push(1);
    tag.extend((pps.len() as u16).to_be_bytes());
    tag.extend(pps);
    tag
}

#[test]
fn an_rtmp_sequence_header_change_starts_a_new_epoch() {
    // As an RTMP encoder sends it: an AVC sequence header before the first
    // frame and again when the parameter sets change, and frames without
    // parameter sets of their own.
    let mut flv = FlvIngest::new(MuxerConfig::default());
    let mut run = Run::default();
    let mut sent: Option<(Vec<u8>, Vec<u8>)> = None;
    let mut header = vec![0xaf, 0];
    header.extend(aac().audio_specific_config());
    flv.audio(0, &header).expect("AAC header");
    for e in events() {
        match e {
            Event::Video(dts, pts, au) => {
                let ms = (dts / 90) as u32;
                if let (Some(sps), Some(pps)) = (&au.sps, &au.pps)
                    && sent.as_ref() != Some(&(sps.clone(), pps.clone()))
                {
                    let up = flv
                        .video(ms, &sequence_header(sps, pps), NOW)
                        .expect("sequence header");
                    run.step(up, flv.muxer());
                    sent = Some((sps.clone(), pps.clone()));
                }
                let cts = ((pts - dts) / 90) as u32;
                let mut tag = vec![
                    if au.idr { 0x17 } else { 0x27 },
                    1,
                    (cts >> 16) as u8,
                    (cts >> 8) as u8,
                    cts as u8,
                ];
                tag.extend(au.data);
                let up = flv.video(ms, &tag, NOW).expect("frame");
                run.step(up, flv.muxer());
            }
            Event::Audio(pts, config, payload) => {
                let ms = (pts / 90) as u32;
                assert_eq!(config, aac());
                let mut tag = vec![0xaf, 1];
                tag.extend(payload);
                flv.audio(ms, &tag).expect("AAC frame");
            }
        }
    }
    assert_one_epoch_at_the_change(&run, flv.muxer().expect("muxer"));
}

/// Feeds the first half of the fixture (160x90) as elementary streams, with
/// `second_idr` rewriting the parameter sets of the second IDR.
fn first_half_with(second_idr: impl Fn(&mut codec::AccessUnit)) -> (EsIngest, usize) {
    let (_, new_sps) = both_sps();
    let mut es = es_ingest();
    let mut restarts = 0;
    let mut idrs = 0;
    for e in events() {
        let up = match e {
            Event::Video(dts, pts, mut au) => {
                if au.sps.as_ref() == Some(&new_sps) {
                    break;
                }
                if au.idr {
                    idrs += 1;
                    if idrs == 2 {
                        second_idr(&mut au);
                    }
                }
                es.video(dts, pts, au, NOW)
            }
            Event::Audio(pts, config, payload) => es.audio_at(pts, config, &payload, NOW),
        }
        .expect("generated fixture");
        restarts += usize::from(up.restarted);
    }
    assert_eq!(idrs, 2, "the first half has two IDRs");
    (es, restarts)
}

#[test]
fn repeated_sets_and_trailing_zero_bytes_start_nothing() {
    // The fixture repeats its SPS and PPS at every IDR; trailing zero bytes
    // belong to the byte stream, not the NAL unit.
    let (es, restarts) = first_half_with(|au| {
        for set in [&mut au.sps, &mut au.pps].into_iter().flatten() {
            set.extend([0, 0]);
        }
    });
    assert_eq!(restarts, 0);
    assert_eq!(es.muxer().expect("muxer").current_initialization_id(), 0);
}

#[test]
fn a_pps_change_alone_starts_a_new_epoch() {
    // The PPS lives in the init segment, not the samples.
    let (es, restarts) = first_half_with(|au| {
        if let Some(pps) = &mut au.pps {
            pps.push(0x80);
        }
    });
    assert_eq!(restarts, 1);
    let m = es.muxer().expect("muxer");
    assert_eq!(m.current_initialization_id(), 1);
    let video = m.video().expect("video");
    assert_eq!((video.width, video.height), (160, 90));
    assert!(video.pps.ends_with(&[0x80]));
    // The change came before the first segment closed, so nothing was
    // served under the first init: the stream simply starts at epoch 1.
    let text = playlist::media(m, PREFIX);
    assert!(
        !text.contains(&format!("\"{}\"", playlist::init_name(PREFIX))),
        "{text}"
    );
    playlist::RuleCheck::default()
        .check(&text)
        .expect("the HLS rules");
}

#[test]
fn a_changed_sps_that_does_not_parse_keeps_the_epoch() {
    let (es, restarts) = first_half_with(|au| au.sps = Some(vec![0x67, 0xff, 0xff, 0xff, 0xff]));
    assert_eq!(restarts, 0);
    let m = es.muxer().expect("muxer");
    assert_eq!(m.current_initialization_id(), 0);
    assert!(m.has_content(), "media carries on under the current epoch");
}

/// Each epoch's init followed by its segments decodes with no error.
#[test]
#[ignore = "requires installed ffmpeg; run explicitly for decode acceptance"]
fn each_epoch_decodes_under_its_own_init() {
    let mut es = es_ingest();
    for e in events() {
        match e {
            Event::Video(dts, pts, au) => es.video(dts, pts, au, NOW),
            Event::Audio(pts, config, payload) => es.audio_at(pts, config, &payload, NOW),
        }
        .expect("generated fixture");
    }
    let m = es.muxer().expect("muxer");
    // Scratch under the build directory, left for inspection.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("sps-epoch-decode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    for epoch in [0u64, 1] {
        let mut file = m.initialization(epoch).expect("init").to_vec();
        let mut segments = 0;
        for e in m.entries() {
            if let Entry::Segment(s) = e
                && s.initialization.id == epoch
            {
                for chunk in s.data.chunks() {
                    file.extend_from_slice(chunk);
                }
                segments += 1;
            }
        }
        assert!(segments > 0, "epoch {epoch} has segments");
        let path = dir.join(format!("epoch{epoch}.mp4"));
        std::fs::write(&path, &file).expect("write");
        let out = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-xerror", "-i"])
            .arg(&path)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "epoch {epoch}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v", "-show_entries"])
            .args(["stream=width,height", "-of", "csv=p=0"])
            .arg(&path)
            .output()
            .expect("ffprobe runs");
        let want = if epoch == 0 { "160,90" } else { "128,72" };
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), want);
    }
}

/// A reconnect whose parameter sets differ only by trailing zero bytes
/// carries the epoch on, as a mid-stream change does.
#[test]
fn a_reconnect_with_trailing_zero_bytes_keeps_the_epoch() {
    let (mut es, restarts) = first_half_with(|_| {});
    assert_eq!(restarts, 0);
    let mut held = es.take_muxer().expect("muxer");
    held.interrupt(NOW);
    let mut back = EsIngest::resume(MuxerConfig::default(), held);
    back.audio_config(aac());
    let mut restarted = false;
    for e in events().into_iter().take(40) {
        let up = match e {
            Event::Video(dts, pts, mut au) => {
                for set in [&mut au.sps, &mut au.pps].into_iter().flatten() {
                    set.extend([0, 0]);
                }
                back.video(dts, pts, au, NOW)
            }
            Event::Audio(pts, config, payload) => back.audio_at(pts, config, &payload, NOW),
        }
        .expect("generated fixture");
        restarted |= up.restarted;
    }
    assert!(!restarted, "the same sets, so the same epoch");
    assert_eq!(back.muxer().expect("muxer").current_initialization_id(), 0);
}
