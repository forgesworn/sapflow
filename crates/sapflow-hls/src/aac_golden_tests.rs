//! Byte-for-byte AAC output, pinned before Opus was added.
//!
//! Each scenario hashes every byte sapflow serves or records for it: init
//! segments, parts, segments and playlists. The digests were taken from the
//! code at 945cb21 (crates 0.2.0-rc.1), before any Opus change, so a
//! difference here means AAC output changed. The one exception is `epochs`,
//! retaken when the live window began keeping three target durations. The
//! scenarios cover the unmetered and fragment-quota constructors, TS,
//! elementary video-led and audio-only input, codec epochs over reconnects,
//! and every init builder.
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::codec::{AacConfig, AccessUnit};
use crate::fmp4::{self, AUDIO_TRACK, Sample, TrackRun, VIDEO_TRACK};
use crate::muxer::{Entry, tests};
use crate::recording_timing::RecordingPresentation;
use crate::{EsIngest, Muxer, MuxerConfig, PartLookup, TrackSet, TsIngest, playlist};

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Every byte a viewer or recorder could fetch from this muxer, in a fixed
/// order: each init, each part in the window, each segment, both playlists.
fn served(m: &Muxer) -> String {
    let mut h = Sha256::new();
    for id in 0..=m.current_initialization_id() {
        if let Some(init) = m.initialization(id) {
            h.update(id.to_be_bytes());
            h.update(init);
        }
    }
    for id in 0..m.next_part_id() {
        if let PartLookup::Found(part) = m.part(id) {
            h.update(id.to_be_bytes());
            h.update(part.duration.as_nanos().to_be_bytes());
            h.update(part.span.as_nanos().to_be_bytes());
            h.update([u8::from(part.independent)]);
            h.update(&part.data);
        }
    }
    for entry in m.entries() {
        if let Entry::Segment(segment) = entry {
            h.update(segment.id.to_be_bytes());
            h.update(segment.duration.as_nanos().to_be_bytes());
            for chunk in segment.data.chunks() {
                h.update(chunk);
            }
        }
    }
    h.update(playlist::media(m, "golden"));
    h.update(playlist::multivariant(m));
    hex(h.finalize())
}

fn ts(bytes: &[u8]) -> String {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    for chunk in bytes.chunks(1316) {
        ingest.push(chunk, NOW).expect("golden fixture");
    }
    ingest.finish(NOW).expect("golden fixture");
    served(ingest.muxer().expect("golden fixture"))
}

fn quota() -> Arc<sapflow_core::fragment::FragmentQuota> {
    Arc::new(sapflow_core::fragment::FragmentQuota::new(
        256 * 1024 * 1024,
    ))
}

/// As the server ingests SRT: with a shared fragment budget.
fn ts_quota(bytes: &[u8]) -> String {
    let mut ingest = TsIngest::with_fragment_quota(MuxerConfig::default(), quota());
    for chunk in bytes.chunks(1316) {
        ingest.push(chunk, NOW).expect("golden fixture");
    }
    ingest.finish(NOW).expect("golden fixture");
    served(ingest.muxer().expect("golden fixture"))
}

/// Codec epochs: alternating resolutions over reconnects, each resumed on
/// the retained muxer, which reconfigures its tracks.
fn epochs() -> String {
    let portrait = include_bytes!("../tests/fixtures/epoch-portrait.ts");
    let landscape = include_bytes!("../tests/fixtures/epoch-landscape.ts");
    let cfg = MuxerConfig {
        segment_count: 8,
        ..MuxerConfig::default()
    };
    let budget = quota();
    let mut ingest = TsIngest::with_fragment_quota(cfg, budget.clone());
    let mut h = Sha256::new();
    for epoch in 0..4 {
        let bytes = if epoch % 2 == 0 {
            portrait.as_slice()
        } else {
            landscape.as_slice()
        };
        for chunk in bytes.chunks(1316) {
            ingest.push(chunk, NOW).expect("golden fixture");
        }
        let mut held = ingest.take_muxer().expect("golden fixture");
        h.update(held.interrupt(NOW).segment.to_string());
        h.update(served(&held));
        ingest = TsIngest::resume(cfg, held);
    }
    hex(h.finalize())
}

/// An audio-only epoch change: 48 kHz mono, then 44.1 kHz stereo.
fn audio_only_reconfigure() -> String {
    let mut m = Muxer::audio_only(MuxerConfig::default(), aac([0x11, 0x88])).expect("AAC");
    for frame in 0..200i64 {
        m.push_audio_at(frame * 1920, Bytes::from(vec![frame as u8; 3]), NOW)
            .expect("AAC");
    }
    m.reconfigure_tracks(None, Some(aac([0x12, 0x10])), NOW)
        .expect("reconfigure");
    for frame in 0..200i64 {
        m.push_audio_at(
            frame * 1024 * 90_000 / 44_100,
            Bytes::from(vec![frame as u8; 4]),
            NOW,
        )
        .expect("AAC");
    }
    m.interrupt(NOW);
    served(&m)
}

fn aac(asc: [u8; 2]) -> AacConfig {
    AacConfig::from_audio_specific_config(&asc).expect("golden AAC")
}

/// Video-led AAC through the elementary API that RTMP uses, with or
/// without a shared fragment budget.
fn elementary_av(budget: bool) -> String {
    let video = tests::video();
    let mut ingest = if budget {
        EsIngest::with_fragment_quota(MuxerConfig::default(), quota())
    } else {
        EsIngest::new(MuxerConfig::default())
    };
    ingest.parameter_sets(Some(video.sps.clone()), Some(video.pps.clone()));
    ingest.audio_config(aac([0x11, 0x90]));
    let mut audio = 0i64;
    for frame in 0..200i64 {
        let dts = frame * 3000;
        while audio <= dts {
            ingest
                .audio(audio, aac([0x11, 0x90]), &[0x21, 0x10, audio as u8])
                .expect("golden AAC");
            audio += 1920;
        }
        ingest
            .video(
                dts,
                dts,
                AccessUnit {
                    data: vec![
                        0,
                        0,
                        0,
                        2,
                        if frame % 30 == 0 { 0x65 } else { 0x41 },
                        frame as u8,
                    ],
                    idr: frame % 30 == 0,
                    ..AccessUnit::default()
                },
                NOW + Duration::from_millis(frame as u64 * 33),
            )
            .expect("golden video");
    }
    served(ingest.muxer().expect("muxer"))
}

/// Declared AAC-only through the updating elementary API, at both rates.
fn elementary_audio_only(asc: [u8; 2]) -> String {
    let config = aac(asc);
    let mut ingest = EsIngest::new(MuxerConfig::default());
    ingest
        .declare_tracks(TrackSet::AudioOnly, NOW)
        .expect("declaration");
    for frame in 0..400i64 {
        let pts = frame * 1024 * 90_000 / i64::from(config.sample_rate);
        ingest
            .audio_at(pts, config, &[0x21, frame as u8], NOW)
            .expect("golden AAC");
    }
    let m = ingest.muxer_mut().expect("muxer");
    m.interrupt(NOW);
    served(m)
}

fn headers() -> String {
    let video = tests::video();
    let stereo = aac([0x11, 0x90]);
    let mono = aac([0x11, 0x88]);
    let mut h = Sha256::new();
    h.update(fmp4::init_segment(&video, Some(&stereo)));
    h.update(fmp4::init_segment(&video, None));
    h.update(fmp4::init_segment_for_file(
        &video,
        Some(&stereo),
        Duration::from_secs(7),
    ));
    for (v, a) in [
        (Some(&video), Some(&stereo)),
        (None, Some(&mono)),
        (Some(&video), None),
    ] {
        h.update(
            fmp4::init_segment_for_tracks(v, a, Duration::from_millis(12_345)).expect("tracks"),
        );
    }
    let presentation = RecordingPresentation {
        audio_start: 0,
        audio_frames: 97 * 1024,
        leading_frames: 2112,
        trailing_frames: 860,
        video_start: None,
        video_end: None,
    };
    h.update(fmp4::init_segment_for_native_recording(None, &mono, &presentation).expect("native"));
    let with_video = RecordingPresentation {
        video_start: Some(0),
        video_end: Some(90_000 * 2),
        ..presentation
    };
    h.update(
        fmp4::init_segment_for_native_recording(Some(&video), &mono, &with_video)
            .expect("native A/V"),
    );
    let samples: Vec<Sample> = (0..5)
        .map(|i| Sample {
            duration: 1024,
            cts: 0,
            sync: true,
            data: Bytes::from(vec![i as u8; 3]),
        })
        .collect();
    let frames = [Sample {
        duration: 3000,
        cts: 0,
        sync: true,
        data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    }];
    let runs = [
        TrackRun {
            track_id: VIDEO_TRACK,
            base_time: 0,
            samples: &frames,
        },
        TrackRun {
            track_id: AUDIO_TRACK,
            base_time: 0,
            samples: &samples,
        },
    ];
    h.update(fmp4::encode_part(1, &runs));
    h.update(fmp4::encode_native_recording_part(1, &runs[1..]));
    h.update(fmp4::encode_native_recording_part_with_final_audio_duration(1, &runs[1..], 300));
    let part = fmp4::encode_part(9, &runs);
    let mut origin = None;
    h.update(fmp4::rebase_part_for_file(&part, Some(48_000), &mut origin).expect("rebase"));
    hex(h.finalize())
}

#[test]
fn aac_output_is_byte_identical_to_0_2_0_rc_1() {
    let actual = [
        (
            "testsrc-3s",
            ts(include_bytes!("../tests/fixtures/testsrc-3s.ts")),
        ),
        (
            "transport-loss-6s",
            ts(include_bytes!("../tests/fixtures/transport-loss-6s.ts")),
        ),
        (
            "audio-48k-mono",
            ts(include_bytes!("../tests/fixtures/audio-48k-mono.ts")),
        ),
        (
            "audio-44k1-stereo",
            ts(include_bytes!("../tests/fixtures/audio-44k1-stereo.ts")),
        ),
        ("elementary-av", elementary_av(false)),
        ("elementary-av-quota", elementary_av(true)),
        (
            "testsrc-3s-quota",
            ts_quota(include_bytes!("../tests/fixtures/testsrc-3s.ts")),
        ),
        (
            "audio-48k-mono-quota",
            ts_quota(include_bytes!("../tests/fixtures/audio-48k-mono.ts")),
        ),
        ("epochs", epochs()),
        ("audio-only-reconfigure", audio_only_reconfigure()),
        ("elementary-aac-48k", elementary_audio_only([0x11, 0x88])),
        ("elementary-aac-44k1", elementary_audio_only([0x12, 0x10])),
        ("headers", headers()),
    ];
    // Changed where an interrupt ends a video-led run, and only there: the
    // terminal interrupt of `finish` (testsrc-3s, transport-loss-6s and
    // testsrc-3s-quota) and each reconnect of `epochs` now write the held
    // video frame, with the frame before it's duration or until the end of
    // the audio received, and the held AAC frame, where they were dropped.
    // Audio-only runs, and runs never interrupted, are byte-identical.
    let expected = [
        (
            "testsrc-3s",
            "01e66b558f2eecdb6d17d21e7d6d899b5494b4786cea151620f98ed96e16ada7",
        ),
        (
            "transport-loss-6s",
            "d1373ac5ecc214bd18d4181f03d1d2cf74769c78b6ef631691fe0709c4b55ae1",
        ),
        (
            "audio-48k-mono",
            "aea38555ff14d01955b9ee0888a7b22bea69e2b626f200750d5ab532fae5617d",
        ),
        (
            "audio-44k1-stereo",
            "297c0a9851cf3401f1ca7c54ee9f5148d0f3326958ef9a579dd0ce5a196dcc8d",
        ),
        (
            "elementary-av",
            "55c7780d53fe43a12f8f36fe4b5da0dba50826d07b6ae4060c07a89c46cec992",
        ),
        (
            "elementary-av-quota",
            "55c7780d53fe43a12f8f36fe4b5da0dba50826d07b6ae4060c07a89c46cec992",
        ),
        (
            "testsrc-3s-quota",
            "01e66b558f2eecdb6d17d21e7d6d899b5494b4786cea151620f98ed96e16ada7",
        ),
        (
            "audio-48k-mono-quota",
            "aea38555ff14d01955b9ee0888a7b22bea69e2b626f200750d5ab532fae5617d",
        ),
        // Changed with the RFC 8216bis 6.2.2 live window: at eight entries
        // the old count-only eviction left 5.40 s listed after the third and
        // fourth epochs; one more 1 s segment is now kept (6.40 s). Every
        // part and segment served before is unchanged. Retention for 6.2.2's
        // part and segment rules needs a pool, which this, like an rc.1
        // caller, does not give.
        (
            "epochs",
            "7a8acae1bd6413f2b9bc0e2533c13ccee2ee85e1b6e164d69d5d1cd920ab7aa9",
        ),
        (
            "audio-only-reconfigure",
            "83799c6dcf91d61d18fc1f2b9f84f2322c99d5c509b95f7c2e2ffc622b4cb023",
        ),
        (
            "elementary-aac-48k",
            "a306e81c6e4fd721a110de3377e8d7f3a29585e4a7f34abbd4fc559ddb9db3e7",
        ),
        (
            "elementary-aac-44k1",
            "ed501a0ee3f504cf23aca043814a658cbe916291f457f411bdb39b80de92b58d",
        ),
        (
            "headers",
            "8dd02851d537d17b2dc80c99f0ba2264c92ad9f21d04e315da7a150672a3055a",
        ),
    ];
    for (name, got) in &actual {
        println!("{name}: {got}");
    }
    assert_eq!(actual.len(), expected.len());
    for ((name, got), (want_name, want)) in actual.iter().zip(expected) {
        assert_eq!(*name, want_name);
        assert_eq!(got, want, "{name}: AAC output changed");
    }
}
