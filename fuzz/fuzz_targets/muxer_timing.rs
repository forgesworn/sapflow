//! Arbitrary frame timing into the LL-HLS muxer: decode-time steps, stalls,
//! steps backwards, keyframes anywhere and audio in between. After every
//! push the rendered playlist must keep the rules `playlist::RuleCheck`
//! holds it to: fixed `TARGETDURATION` and `PART-TARGET`, no `EXTINF` over
//! the target, the part rules, consistent discontinuities.
#![no_main]
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use sapflow_hls::codec::{AacConfig, H264Config};
use sapflow_hls::{Muxer, MuxerConfig, playlist};
use sapflow_hls::{fmp4, recording_timing as recording};

const SPS: &[u8] = &[
    0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08,
    0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
];
const PPS: &[u8] = &[0x68, 0xef, 0xbc, 0x80];

// Bounded typed native-recording metadata, not raw MP4 parsing or a fake codec.
// This crate has no FFI dependency: real opaque checkpoint owner/drain behaviour
// is covered by the direct sapflow-ffi tests, not claimed by this target.
fn native_recording(data: &[u8]) {
    if data.len() > 4096 {
        return;
    }
    let audio = AacConfig::from_audio_specific_config(&[0x11, 0x88])
        .expect("fixed native AAC output configuration");
    let video = H264Config::new(SPS, PPS).expect("fixed native video configuration");
    let mut ledger = recording::AudioLedger::default();
    ledger.begin(2112, 0).expect("bounded first head");
    let mut observed = 0_u64;
    for rec in data.chunks(32).take(64) {
        let op = rec[0] % 9;
        let integer = |at: usize| -> u64 {
            let mut value = [0_u8; 8];
            if let Some(bytes) = rec.get(at..) {
                let count = bytes.len().min(8);
                value[..count].copy_from_slice(&bytes[..count]);
            }
            u64::from_le_bytes(value)
        };
        match op {
            0 | 1 => {
                // Structured valid commands reach deep generation/file logic;
                // hostile mutations then exercise refusal from that same state.
                let head = ledger.leading().expect("head survives completed events");
                let input = 9216 - u64::from(head) - 704;
                let mut event = recording::GenerationReceipt {
                    generation: ledger.extent().map_or(1, |x| x.generation_count + 1),
                    input_frames: input,
                    raw_frames: input,
                    zero_frames: 0,
                    input_rate: 48000,
                    encoded_frames: 9216,
                    leading_frames: head,
                    trailing_frames: 704,
                    source_mapping: recording::SourceMapping::Nearest,
                };
                let mut count = observed + 9;
                if op == 1 {
                    match integer(1) % 8 {
                        0 => event.generation = integer(9),
                        1 => event.input_frames = integer(9),
                        2 => event.raw_frames = integer(9),
                        3 => event.zero_frames = integer(9),
                        4 => event.input_rate = integer(9) as u32,
                        5 => event.encoded_frames = integer(9),
                        6 => event.leading_frames = integer(9) as u32,
                        _ => {
                            event.trailing_frames = integer(9) as u32;
                            count = integer(17);
                        }
                    }
                    event.source_mapping = if integer(25) & 1 == 0 {
                        recording::SourceMapping::Nearest
                    } else {
                        recording::SourceMapping::Floor
                    };
                }
                let before = ledger.clone();
                if ledger.append(event, count).is_err() {
                    assert_eq!(ledger, before, "refusal cannot mutate timing");
                } else {
                    observed = count;
                    let extent = ledger.extent().expect("accepted generation aggregate");
                    assert_eq!(
                        extent.encoded_frames,
                        observed
                            .checked_mul(1024)
                            .expect("accepted bounded coded count")
                    );
                    assert_eq!(
                        extent.encoded_frames
                            - u64::from(extent.first_leading_frames)
                            - u64::from(extent.last_trailing_frames),
                        extent.source_frames_at_48k + extent.intentional_standby_frames
                    );
                }
            }
            2 | 3 => {
                if let Some(mut receipt) = ledger.extent() {
                    if op == 3 {
                        receipt.encoded_frames = integer(1);
                        receipt.generation_count = integer(9);
                    }
                    let before = ledger.clone();
                    if ledger.complete(receipt, observed).is_err() {
                        assert_eq!(ledger, before);
                    }
                }
            }
            4 => {
                let before = ledger.clone();
                if ledger.begin(integer(1) as u32, integer(9)).is_err() {
                    assert_eq!(ledger, before);
                }
            }
            5 => {
                ledger = recording::AudioLedger::default();
                let head = [0, 2112, 2144, 8191][usize::from(rec[0] >> 4) % 4];
                ledger.begin(head, 0).expect("bounded reset head");
                observed = 0;
            }
            6 => {
                let mapping = if integer(17) & 1 == 0 {
                    recording::SourceMapping::Floor
                } else {
                    recording::SourceMapping::Nearest
                };
                if let Ok(extent) = recording::source_extent(integer(1), integer(9) as u32, mapping)
                {
                    assert!(
                        extent.quantisation_numerator.unsigned_abs()
                            < u64::from(extent.quantisation_denominator)
                    );
                }
            }
            7 => {
                let facts = recording::RecordingPresentation {
                    audio_start: integer(1),
                    audio_frames: integer(9),
                    leading_frames: integer(17) as u32,
                    trailing_frames: integer(25) as u32,
                    video_start: Some(integer(9)),
                    video_end: Some(integer(17)),
                };
                if let Ok(header) =
                    fmp4::init_segment_for_native_recording(Some(&video), &audio, &facts)
                {
                    assert!(header.len() < 65536);
                    assert!(facts.validate().is_ok());
                }
            }
            _ => {
                let bytes = Bytes::copy_from_slice(rec);
                let samples = [fmp4::Sample {
                    duration: 1024,
                    cts: 0,
                    sync: true,
                    data: bytes.clone(),
                }];
                let runs = [fmp4::TrackRun {
                    track_id: fmp4::AUDIO_TRACK,
                    base_time: integer(1),
                    samples: &samples,
                }];
                let legacy = fmp4::encode_part(1, &runs);
                let native = fmp4::encode_native_recording_part(1, &runs);
                assert!(!legacy.is_empty() && !native.is_empty());
                assert_eq!(&legacy[legacy.len() - bytes.len()..], bytes.as_ref());
                assert_eq!(&native[native.len() - bytes.len()..], bytes.as_ref());
                assert!(
                    native.len() > legacy.len(),
                    "only new mode adds group metadata"
                );
                // Native final-duration serialization is container-only: the
                // input entry and exact payload stay intact on every refusal.
                let duration = integer(9) as u32;
                let final_bytes = fmp4::encode_native_recording_part_with_final_audio_duration(
                    1, &runs, duration,
                );
                if (1..=1024).contains(&duration) {
                    assert!(!final_bytes.is_empty());
                    assert_eq!(final_bytes.len(), native.len());
                    assert_eq!(
                        &final_bytes[final_bytes.len() - bytes.len()..],
                        bytes.as_ref()
                    );
                    if duration == 1024 {
                        assert_eq!(final_bytes, native);
                    }
                } else {
                    assert!(final_bytes.is_empty());
                }
                assert_eq!(samples[0].duration, 1024);
                let before = ledger.clone();
                if let Ok(count) = ledger.completed_generations_through(integer(17)) {
                    assert!(count <= ledger.extent().map_or(0, |x| x.generation_count));
                }
                assert_eq!(ledger, before);
            }
        }
        // Pure header snapshots model the arithmetic covered by a sealed
        // receipt. They are not an actual FFI checkpoint or a durability claim.
        if let Some(extent) = ledger.extent() {
            let facts = recording::RecordingPresentation {
                audio_start: 1440,
                audio_frames: extent.encoded_frames,
                leading_frames: extent.first_leading_frames,
                trailing_frames: ledger
                    .trailing(recording::Completion::Complete, observed)
                    .unwrap_or(0),
                video_start: Some(0),
                video_end: Some(3001),
            };
            let header = fmp4::init_segment_for_native_recording(Some(&video), &audio, &facts)
                .expect("valid typed generation header");
            let mut open = facts;
            open.audio_frames = 0;
            open.trailing_frames = 0;
            open.video_end = Some(0);
            let initial = fmp4::init_segment_for_native_recording(Some(&video), &audio, &open)
                .expect("same initial fixed layout");
            assert_eq!(header.len(), initial.len());
            assert_eq!(
                ledger.trailing(recording::Completion::IncompletePrefix, observed),
                Ok(0)
            );
        }
    }
}

fn audio_timing(data: &[u8]) {
    let Some((&head, records)) = data.split_first() else {
        return;
    };
    let config = |stereo: bool| AacConfig {
        object_type: 2,
        sample_rate_index: if stereo { 4 } else { 3 },
        sample_rate: if stereo { 44_100 } else { 48_000 },
        channels: if stereo { 2 } else { 1 },
    };
    let mut stereo = head & 1 != 0;
    let mut muxer = Muxer::audio_only(MuxerConfig::default(), config(stereo))
        .expect("fixed supported AAC profile");
    let mut rules = playlist::RuleCheck::default();
    let mut pts = 0i64;
    for rec in records.chunks_exact(3) {
        let flags = rec[2];
        let step = i64::from(u16::from_le_bytes([rec[0], rec[1]]));
        let step = if flags & 2 != 0 { step * 16 } else { step };
        pts = pts.saturating_add(if flags & 4 != 0 { -step } else { step });
        let result = muxer.push_audio_at(pts, Bytes::from_static(b"aac"), std::time::UNIX_EPOCH);
        if flags & 8 != 0 {
            muxer.interrupt(std::time::UNIX_EPOCH);
        }
        if flags & 16 != 0 {
            stereo = !stereo;
            let _ = muxer.reconfigure_tracks(None, Some(config(stereo)), std::time::UNIX_EPOCH);
            pts = 0;
        }
        if muxer.has_content() {
            let rendered = playlist::media(&muxer, "p");
            if let Err(error) = rules.check(&rendered) {
                panic!("{error}\n{rendered}");
            }
        }
        if result.is_err() {
            return;
        }
    }
}

// Bounded allocations, response aliases and connection recovery. Copies in the
// oracle are test metadata; this verifies logical charges, not process RSS.
fn fragment_budget(data: &[u8]) {
    use sapflow_core::fragment::FragmentQuota;
    use sapflow_hls::{EsIngest, PartLookup};
    use std::sync::Arc;
    if data.len() < 2 {
        return;
    }
    let maximum = usize::from(u16::from_le_bytes([data[0], data[1]]));
    let quota = Arc::new(FragmentQuota::new(maximum));
    let video = H264Config::new(SPS, PPS).expect("generated supported config");
    let cfg = MuxerConfig {
        segment_count: 8,
        ..MuxerConfig::default()
    };
    let Ok(mut muxer) = Muxer::with_fragment_quota(cfg, Some(video.clone()), None, quota.clone())
    else {
        assert_eq!(quota.used(), 0);
        return;
    };
    let mut held: Vec<(Bytes, Vec<u8>)> = Vec::new();
    let mut dts = 0;
    let mut rules = playlist::RuleCheck::default();
    for record in data[2..].chunks_exact(8).take(256) {
        match record[0] % 6 {
            0 => {
                let _ = muxer.push_video(
                    dts,
                    dts,
                    record[1] & 1 != 0,
                    Bytes::copy_from_slice(&record[2..]),
                    std::time::UNIX_EPOCH,
                );
                dts += 3000;
            }
            1 | 2 if held.len() < 32 => {
                let bytes = if record[0] % 6 == 1 {
                    Some(muxer.init().clone())
                } else {
                    muxer
                        .next_part_id()
                        .checked_sub(1)
                        .and_then(|id| match muxer.part(id) {
                            PartLookup::Found(part) => Some(part.data.clone()),
                            _ => None,
                        })
                };
                if let Some(bytes) = bytes {
                    let before = quota.used();
                    let offset = usize::from(record[1]) % bytes.len().max(1);
                    let alias = bytes.slice(offset..);
                    held.push((alias.clone(), alias.to_vec()));
                    assert_eq!(quota.used(), before, "clones and slices share one charge");
                }
            }
            3 => {
                held.pop();
            }
            4 => {
                muxer.interrupt(std::time::UNIX_EPOCH);
                let mut ingest = EsIngest::resume(cfg, muxer);
                muxer = ingest.take_muxer().expect("retained presentation");
                dts = 0;
            }
            5 => {
                let old_epoch = muxer.current_initialization_id();
                if matches!(
                    muxer.reconfigure_tracks(Some(video.clone()), None, std::time::UNIX_EPOCH),
                    Err(sapflow_hls::muxer::MuxError::FragmentLimit)
                ) {
                    assert_eq!(
                        muxer.current_initialization_id(),
                        old_epoch,
                        "refusal cannot install an unaccepted successor epoch"
                    );
                }
            }
            _ => {}
        }
        assert!(quota.used() <= maximum);
        for (bytes, expected) in &held {
            assert_eq!(bytes.as_ref(), expected.as_slice());
        }
        if muxer.has_content() {
            let text = playlist::media(&muxer, "p");
            rules
                .check(&text)
                .unwrap_or_else(|error| panic!("{error}\n{text}"));
        }
    }
    drop(muxer);
    for (bytes, expected) in &held {
        assert_eq!(bytes.as_ref(), expected.as_slice());
    }
    drop(held);
    assert_eq!(quota.used(), 0);
}

fuzz_target!(|data: &[u8]| {
    if let Some(records) = data.strip_prefix(b"FBUD") {
        fragment_budget(records);
        return;
    }
    if let Some(records) = data.strip_prefix(b"NREC") {
        native_recording(records);
        return;
    }
    // Preserve historical video-timing inputs. New prefixed seeds exercise
    // exact AAC frame clocks, interruptions and retained codec epochs.
    if let Some(records) = data.strip_prefix(b"AAC1") {
        audio_timing(records);
        return;
    }
    let Some(video) = H264Config::new(SPS, PPS) else {
        return;
    };
    let aac = AacConfig {
        object_type: 2,
        sample_rate_index: 3,
        sample_rate: 48000,
        channels: 2,
    };
    let mut m = Muxer::new(MuxerConfig::default(), video, Some(aac));
    let (mut dts, mut audio) = (0i64, 0i64);
    let mut rules = playlist::RuleCheck::default();
    // Each record: a step in ticks (2 bytes), then flags: bit 0 keyframe,
    // bit 1 the step is a stall (x 16), bit 2 the step goes backwards,
    // bits 3-5 AAC frames before the frame, bits 6-7 a composition offset.
    for rec in data.chunks_exact(3) {
        let step = i64::from(u16::from_le_bytes([rec[0], rec[1]]));
        let flags = rec[2];
        let step = if flags & 2 != 0 { step * 16 } else { step };
        dts += if flags & 4 != 0 { -step } else { step };
        for _ in 0..(flags >> 3) & 7 {
            if m.push_audio(audio, Bytes::from_static(b"aac")).is_err() {
                return;
            }
            audio += 1920;
        }
        let pts = dts + i64::from(flags >> 6) * 3000;
        if m.push_video(
            dts,
            pts,
            flags & 1 == 1,
            Bytes::from_static(&[0; 8]),
            std::time::UNIX_EPOCH,
        )
        .is_err()
        {
            return;
        }
        if !m.has_content() {
            continue;
        }
        let pl = playlist::media(&m, "p");
        if let Err(e) = rules.check(&pl) {
            panic!("{e}\n{pl}");
        }
    }
});
