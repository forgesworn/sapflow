use crate::{codec::AacConfig, fmp4, recording_timing::*};
use bytes::Bytes;
use mp4_atom::{Decode, Encode, Moof, Moov};

fn generation(id: u64) -> GenerationReceipt {
    GenerationReceipt {
        generation: id,
        input_frames: 6400,
        raw_frames: 6400,
        zero_frames: 0,
        input_rate: 48_000,
        encoded_frames: 9216,
        leading_frames: 2112,
        trailing_frames: 704,
        source_mapping: SourceMapping::Nearest,
    }
}
fn ledger() -> AudioLedger {
    let mut ledger = AudioLedger::default();
    ledger.begin(2112, 0).expect("actual first priming");
    ledger
}
fn presentation() -> RecordingPresentation {
    RecordingPresentation {
        audio_start: 0,
        audio_frames: 97 * 1024,
        leading_frames: 2112,
        trailing_frames: 860,
        video_start: None,
        video_end: None,
    }
}
fn audio() -> AacConfig {
    AacConfig::from_audio_specific_config(&[0x11, 0x88]).expect("canonical native AAC mono48")
}
fn movie(bytes: &Bytes) -> Moov {
    let mut input = std::io::Cursor::new(bytes.as_ref());
    mp4_atom::Ftyp::decode(&mut input).expect("own ftyp");
    Moov::decode(&mut input).expect("own movie metadata")
}

#[test]
fn measured_three_generation_slots_remove_only_file_head_and_tail() {
    let mut ledger = ledger();
    for n in 1..=3 {
        ledger
            .append(generation(n), n * 9)
            .expect("actual sequential EOF and count");
    }
    let extent = ledger.extent().expect("measured aggregate");
    assert_eq!(extent.source_frames_at_48k, 19_200);
    assert_eq!(extent.intentional_standby_frames, 5632);
    assert_eq!(extent.encoded_frames, 27_648);
    assert_eq!(extent.encoded_frames - 2112 - 704, 24_832);
    ledger.complete(extent, 27).expect("matching final receipt");
    assert_eq!(ledger.trailing(Completion::Complete, 27), Ok(704));
    assert_eq!(ledger.trailing(Completion::IncompletePrefix, 25), Ok(0));
}

#[test]
fn source_floor_and_nearest_remain_distinct() {
    let floor = source_extent(44117, 44100, SourceMapping::Floor).expect("SRC mapping");
    let nearest = source_extent(44117, 44100, SourceMapping::Nearest).expect("old mapping");
    assert_eq!(floor.frames, 48018);
    assert_eq!(nearest.frames, 48019);
    assert_eq!(floor.quantisation_numerator, -22200);
    assert_eq!(nearest.quantisation_numerator, 21900);
    assert_eq!(
        source_extent(u64::MAX, 44100, SourceMapping::Floor),
        Err(TimingError::Overflow)
    );
    assert_eq!(
        source_extent(10, 0, SourceMapping::Nearest),
        Err(TimingError::Unsupported)
    );
}

#[test]
fn actual_resampled_eof_is_checked_without_absorbing_five_frames() {
    let mut correct = generation(1);
    correct.input_frames = 14837;
    correct.raw_frames = 14837;
    correct.input_rate = 44100;
    correct.source_mapping = SourceMapping::Floor;
    correct.encoded_frames = 18432;
    correct.leading_frames = 2112;
    correct.trailing_frames = 18432 - 2112 - 16149;
    let mut state = ledger();
    state
        .append(correct, 18)
        .expect("measured normal resampler EOF");
    let mut bad = correct;
    bad.leading_frames = 2144;
    bad.trailing_frames = 134;
    let mut state = AudioLedger::default();
    state.begin(2144, 0).expect("original measured head");
    let before = state.clone();
    assert_eq!(state.append(bad, 18), Err(TimingError::Inconsistent));
    assert_eq!(state, before);
}

#[test]
fn contradictory_duplicate_and_stale_generation_are_atomic() {
    for mutate in 0..8 {
        let mut state = ledger();
        let before = state.clone();
        let mut value = generation(1);
        match mutate {
            0 => value.generation = 2,
            1 => value.raw_frames += 1,
            2 => value.encoded_frames += 1,
            3 => value.leading_frames = 2113,
            4 => value.trailing_frames = 1024,
            5 => value.input_rate = 0,
            6 => value.input_frames = u64::MAX,
            _ => value.generation = u64::MAX,
        }
        assert!(state.append(value, 9).is_err());
        assert_eq!(state, before);
    }
    let mut state = ledger();
    state.append(generation(1), 9).expect("first EOF");
    let before = state.clone();
    assert!(state.append(generation(1), 9).is_err());
    assert_eq!(state, before);
    assert!(state.append(generation(2), 17).is_err());
    assert_eq!(state, before);
}

#[test]
fn missing_eof_and_conflicting_final_receipt_never_complete() {
    let mut state = ledger();
    assert!(state.trailing(Completion::Complete, 9).is_err());
    assert_eq!(state.trailing(Completion::IncompletePrefix, 2), Ok(0));
    state.append(generation(1), 9).expect("generation EOF");
    let mut bad = state.extent().expect("aggregate");
    bad.source_frames_at_48k += 1;
    let before = state.clone();
    assert!(state.complete(bad, 9).is_err());
    assert_eq!(state, before);
    state
        .complete(before.extent().expect("actual aggregate"), 9)
        .expect("real final aggregate");
    assert_eq!(state.append(generation(2), 18), Err(TimingError::Closed));
    assert_eq!(
        state.complete(before.extent().expect("aggregate"), 9),
        Err(TimingError::Closed)
    );
}

#[test]
fn first_head_is_bounded_and_cannot_arrive_after_audio() {
    let mut state = AudioLedger::default();
    assert_eq!(state.begin(8192, 0), Err(TimingError::Unsupported));
    assert_eq!(state.begin(2112, 1), Err(TimingError::Inconsistent));
    assert_eq!(state.leading(), None);
    state.begin(2112, 0).expect("first head");
    assert_eq!(state.begin(2112, 0), Err(TimingError::Inconsistent));
}

#[test]
fn completed_first_generation_must_have_positive_measured_presentation() {
    let mut state = ledger();
    let before = state.clone();
    let mut value = generation(1);
    value.input_frames = 0;
    value.raw_frames = 0;
    value.encoded_frames = 3072;
    value.trailing_frames = 960;
    assert_eq!(state.append(value, 3), Err(TimingError::Inconsistent));
    assert_eq!(state, before);
    assert_eq!(
        state.trailing(Completion::IncompletePrefix, 3),
        Ok(0),
        "positive coded prefix stays recoverable without claiming EOF"
    );
}

#[test]
fn fragment_header_has_exact_non_millisecond_audio_presentation() {
    let facts = presentation();
    let bytes =
        fmp4::init_segment_for_native_recording(None, &audio(), &facts).expect("checked metadata");
    let m = movie(&bytes);
    assert_eq!(m.mvhd.timescale, MOVIE_TIMESCALE);
    assert_eq!(m.mvhd.duration, 96356 * 15);
    let t = &m.trak[0];
    assert_eq!(
        t.mdia.mdhd.duration, 0,
        "fragmented init has no media samples"
    );
    assert_eq!(t.tkhd.duration, 96356 * 15);
    let edit = t
        .edts
        .as_ref()
        .expect("recording edit")
        .elst
        .as_ref()
        .expect("exact edit");
    assert_eq!(edit.entries[0].media_time, Some(2112));
    assert_eq!(edit.entries[0].segment_duration, 96356 * 15);
    assert_eq!(t.mdia.minf.stbl.sgpd.len(), 1);
    let at = bytes.windows(4).position(|x| x == b"elst").expect("elst");
    assert_eq!(bytes[at + 4], 1, "fixed v1 even for short files");
    let mut stereo_source_extent = facts;
    stereo_source_extent.trailing_frames = 840;
    let m = movie(
        &fmp4::init_segment_for_native_recording(None, &audio(), &stereo_source_extent)
            .expect("real stereo-source EOF"),
    );
    assert_eq!(m.mvhd.duration, 96376 * 15);
}

#[test]
fn fixed_header_layout_survives_large_duration_and_unknown_eof() {
    let mut facts = presentation();
    let original =
        fmp4::init_segment_for_native_recording(None, &audio(), &facts).expect("initial");
    facts.audio_frames = 1024;
    facts.trailing_frames = 0;
    let short = fmp4::init_segment_for_native_recording(None, &audio(), &facts)
        .expect("positive coded short prefix");
    assert_eq!(short.len(), original.len());
    assert_eq!(movie(&short).trak[0].tkhd.duration, 0);
    facts.audio_frames = (u64::from(u32::MAX) + 1) * 1024;
    let long =
        fmp4::init_segment_for_native_recording(None, &audio(), &facts).expect("64-bit duration");
    assert_eq!(long.len(), original.len());
    facts.audio_frames = 97 * 1024;
    let prefix = movie(
        &fmp4::init_segment_for_native_recording(None, &audio(), &facts)
            .expect("unknown final tail"),
    );
    assert_eq!(prefix.mvhd.duration, 97216 * 15);
}

#[test]
fn actual_two_hour_header_keeps_all_duration_sections_version_one() {
    let video = crate::codec::H264Config::new(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("existing generated SPS/PPS");
    let mut facts = presentation();
    facts.video_start = Some(0);
    facts.video_end = Some(3000);
    facts.audio_frames = 1024;
    facts.trailing_frames = 0;
    let first = fmp4::init_segment_for_native_recording(Some(&video), &audio(), &facts)
        .expect("first short header");
    facts.audio_frames = (7210_u64 * 48000).div_ceil(1024) * 1024;
    facts.trailing_frames = 860;
    facts.video_end = Some(7210 * 90000);
    let last = fmp4::init_segment_for_native_recording(Some(&video), &audio(), &facts)
        .expect("more than two hours, no cap");
    assert_eq!(first.len(), last.len());
    let m = movie(&last);
    assert!(m.mvhd.duration > u64::from(u32::MAX));
    assert_eq!(
        m.mvhd.duration,
        facts.movie_duration().expect("exact rational duration")
    );
    for (kind, count) in [
        (b"mvhd", 1),
        (b"tkhd", 2),
        (b"mdhd", 2),
        (b"mehd", 1),
        (b"elst", 2),
    ] {
        for bytes in [&first, &last] {
            let offsets: Vec<_> = bytes
                .windows(4)
                .enumerate()
                .filter_map(|(at, name)| (name == kind).then_some(at))
                .collect();
            assert_eq!(offsets.len(), count);
            for at in offsets {
                assert_eq!(bytes[at + 4], 1, "fixed v1 for {kind:?}");
            }
        }
    }
}

#[test]
fn initial_offset_is_preserved_not_folded_into_trim() {
    let mut facts = presentation();
    facts.audio_start = 1440;
    let m = movie(
        &fmp4::init_segment_for_native_recording(None, &audio(), &facts)
            .expect("real delayed audio"),
    );
    let edits = &m.trak[0]
        .edts
        .as_ref()
        .expect("edit")
        .elst
        .as_ref()
        .expect("elst")
        .entries;
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].media_time, None);
    assert_eq!(edits[0].segment_duration, 1440 * 15);
    assert_eq!(edits[1].media_time, Some(1440 + 2112));
}

#[test]
fn av_header_keeps_video_clock_offset_and_chooses_actual_longer_track() {
    let video = crate::codec::H264Config::new(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("existing generated SPS/PPS");
    let mut facts = presentation();
    facts.video_start = Some(27000);
    facts.video_end = Some(270001);
    let initial = fmp4::init_segment_for_native_recording(Some(&video), &audio(), &facts)
        .expect("AV metadata");
    let m = movie(&initial);
    assert_eq!(m.mvhd.duration, 270001 * 8);
    assert_eq!(m.trak[0].mdia.mdhd.timescale, 90000);
    assert_eq!(m.trak[0].mdia.mdhd.duration, 0);
    let edits = &m.trak[0]
        .edts
        .as_ref()
        .expect("video edit")
        .elst
        .as_ref()
        .expect("video elst")
        .entries;
    assert_eq!(edits[0].segment_duration, 27000 * 8);
    assert_eq!(edits[1].media_time, Some(27000));
    assert_eq!(edits[1].segment_duration, (270001 - 27000) * 8);
    facts.audio_frames = 1024;
    facts.trailing_frames = 0;
    facts.video_end = Some(30000);
    let first = fmp4::init_segment_for_native_recording(Some(&video), &audio(), &facts)
        .expect("initial known offsets");
    assert_eq!(first.len(), initial.len());
}

#[test]
fn finite_hostile_fact_mutations_preserve_state_on_refusal() {
    let mut seed = 0x51c2a9e7_u64;
    for _ in 0..512 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut value = generation(1);
        match seed % 6 {
            0 => value.encoded_frames = seed,
            1 => value.input_frames = seed,
            2 => value.input_rate = seed as u32,
            3 => value.leading_frames = seed as u32,
            4 => value.trailing_frames = seed as u32,
            _ => value.generation = seed,
        }
        let mut state = ledger();
        let before = state.clone();
        if state.append(value, 9).is_err() {
            assert_eq!(state, before);
        } else {
            assert_eq!(
                state.extent().expect("accepted exact facts").encoded_frames,
                9 * 1024
            );
        }
    }
}

#[test]
fn unsupported_output_and_impossible_metadata_are_refused() {
    let stereo = AacConfig::from_audio_specific_config(&[0x11, 0x90]).expect("legacy stereo AAC");
    assert!(fmp4::init_segment_for_native_recording(None, &stereo, &presentation()).is_err());
    let mut value = presentation();
    value.audio_frames = u64::MAX;
    assert!(value.validate().is_err());
    value = presentation();
    value.audio_start = i64::MAX as u64;
    assert!(value.validate().is_err());
    value = presentation();
    value.video_start = Some(0);
    assert!(value.validate().is_err());
}

#[test]
fn fragment_mapping_retains_original_media_timing_and_bytes() {
    let samples = [fmp4::Sample {
        duration: 1024,
        cts: 0,
        sync: true,
        data: Bytes::from_static(&[1, 2, 3]),
    }];
    let runs = [fmp4::TrackRun {
        track_id: fmp4::AUDIO_TRACK,
        base_time: 15360,
        samples: &samples,
    }];
    let legacy = fmp4::encode_part(7, &runs);
    let checked = fmp4::encode_native_recording_part(7, &runs);
    let a = Moof::decode(&mut legacy.as_ref()).expect("legacy moof");
    let b = Moof::decode(&mut checked.as_ref()).expect("native moof");
    assert_eq!(a.traf[0].tfdt, b.traf[0].tfdt);
    assert_eq!(a.traf[0].trun[0].entries, b.traf[0].trun[0].entries);
    assert_eq!(&legacy[legacy.len() - 3..], &checked[checked.len() - 3..]);
    assert!(a.traf[0].sbgp.is_empty());
    assert_eq!(b.traf[0].sbgp[0].entries[0].sample_count, 1);
    assert_eq!(b.traf[0].sbgp[0].entries[0].group_description_index, 1);
}

#[test]
fn legacy_fragment_bytes_match_frozen_prechange_golden() {
    let samples = [fmp4::Sample {
        duration: 1024,
        cts: 0,
        sync: true,
        data: Bytes::from_static(&[1, 2, 3]),
    }];
    let runs = [fmp4::TrackRun {
        track_id: fmp4::AUDIO_TRACK,
        base_time: 0,
        samples: &samples,
    }];
    // Original common encoder layout, fixed one-AU vector. No new optional groups.
    let hex = "000000686d6f6f66000000106d66686400000000000000010000005074726166000000107466686400020000000000020000001474666474010000000000000000000000000000247472756e01000b050000000100000070020000000000040000000003000000000000000b6d646174010203";
    let expected: Vec<u8> = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|s| {
            let text = std::str::from_utf8(s).expect("ASCII golden");
            u8::from_str_radix(text, 16).expect("hex golden")
        })
        .collect();
    assert_eq!(
        fmp4::encode_part(1, &runs),
        before_recording_part(1, &runs),
        "exact old-source comparison"
    );
    assert_eq!(fmp4::encode_part(1, &runs).as_ref(), expected);
    let init = fmp4::init_segment_for_tracks(None, Some(&audio()), std::time::Duration::ZERO)
        .expect("legacy init");
    let m = movie(&init);
    assert_eq!(m.mvhd.timescale, 1000);
    assert!(m.trak[0].edts.is_none() && m.trak[0].mdia.minf.stbl.sgpd.is_empty());
}

// Exact prechange encoder, source-identity checked against retained baseline manifest.
fn before_recording_part(sequence_number: u32, runs: &[crate::fmp4::TrackRun<'_>]) -> bytes::Bytes {
    let runs: Vec<&crate::fmp4::TrackRun<'_>> =
        runs.iter().filter(|r| !r.samples.is_empty()).collect();

    let build = |offsets: &[i32]| mp4_atom::Moof {
        mfhd: mp4_atom::Mfhd { sequence_number },
        traf: runs
            .iter()
            .zip(offsets)
            .map(|(run, &offset)| mp4_atom::Traf {
                tfhd: mp4_atom::Tfhd {
                    track_id: run.track_id,
                    default_base_is_moof: true,
                    ..Default::default()
                },
                tfdt: Some(mp4_atom::Tfdt {
                    base_media_decode_time: run.base_time,
                }),
                trun: vec![mp4_atom::Trun {
                    data_offset: Some(offset),
                    entries: run
                        .samples
                        .iter()
                        .map(|s| mp4_atom::TrunEntry {
                            duration: Some(s.duration),
                            size: Some(s.data.len() as u32),
                            flags: Some(if s.sync { 0x0200_0000 } else { 0x0101_0000 }),
                            cts: Some(s.cts),
                        })
                        .collect(),
                }],
                ..Default::default()
            })
            .collect(),
    };

    // The data offsets depend on the moof's own size, which does not depend
    // on their values: encode once to measure, then again with real offsets.
    let mut probe = Vec::new();
    if build(&vec![0; runs.len()]).encode(&mut probe).is_err() {
        return bytes::Bytes::new();
    }
    let header = probe.len() + 8;
    let mut offsets = Vec::with_capacity(runs.len());
    let mut data = Vec::new();
    for run in &runs {
        offsets.push((header + data.len()) as i32);
        for s in run.samples {
            data.extend_from_slice(&s.data);
        }
    }

    let mut buf = Vec::with_capacity(header + data.len());
    if build(&offsets).encode(&mut buf).is_err()
        || (mp4_atom::Mdat { data }).encode(&mut buf).is_err()
    {
        return bytes::Bytes::new();
    }
    bytes::Bytes::from(buf)
}

#[test]
fn native_final_duration_changes_only_last_wire_entry_not_original_samples() {
    for trailing in [0, 1, 448, 1023] {
        let samples: Vec<_> = (0..3)
            .map(|_| fmp4::Sample {
                duration: 1024,
                cts: 0,
                sync: true,
                data: Bytes::from_static(&[1, 2, 3]),
            })
            .collect();
        let runs = [fmp4::TrackRun {
            track_id: fmp4::AUDIO_TRACK,
            base_time: 12288,
            samples: &samples,
        }];
        let original = fmp4::encode_native_recording_part(5, &runs);
        let final_bytes =
            fmp4::encode_native_recording_part_with_final_audio_duration(5, &runs, 1024 - trailing);
        let before = Moof::decode(&mut original.as_ref()).expect("original moof");
        let after = Moof::decode(&mut final_bytes.as_ref()).expect("final moof");
        assert_eq!(before.traf[0].tfdt, after.traf[0].tfdt);
        let a = &before.traf[0].trun[0];
        let b = &after.traf[0].trun[0];
        assert_eq!(a.data_offset, b.data_offset);
        assert_eq!(a.entries[..2], b.entries[..2]);
        let mut restored = b.entries[2].clone();
        restored.duration = Some(1024);
        assert_eq!(restored, a.entries[2]);
        assert_eq!(b.entries[2].duration, Some(1024 - trailing));
        assert_eq!(
            samples[2].duration, 1024,
            "shared publishing/sample metadata unchanged"
        );
        assert_eq!(original.len(), final_bytes.len());
        assert_eq!(
            &original[original.len() - 9..],
            &final_bytes[final_bytes.len() - 9..]
        );
        if trailing == 0 {
            assert_eq!(original, final_bytes);
        }
    }
    let empty = [fmp4::TrackRun {
        track_id: fmp4::AUDIO_TRACK,
        base_time: 0,
        samples: &[],
    }];
    assert!(
        fmp4::encode_native_recording_part_with_final_audio_duration(1, &empty, 1024).is_empty()
    );
    assert!(fmp4::encode_native_recording_part_with_final_audio_duration(1, &empty, 0).is_empty());
    assert!(
        fmp4::encode_native_recording_part_with_final_audio_duration(1, &empty, 1025).is_empty()
    );
}

#[test]
fn generation_coverage_retains_actual_previous_eof_while_current_last_au_is_held() {
    let mut value = ledger();
    assert_eq!(value.completed_generations_through(0), Ok(0));
    value.append(generation(1), 9).expect("first actual EOF");
    assert_eq!(value.completed_generations_through(8192), Ok(0));
    assert_eq!(value.completed_generations_through(9216), Ok(1));
    value.append(generation(2), 18).expect("second actual EOF");
    assert_eq!(value.completed_generations_through(17408), Ok(1));
    assert_eq!(value.completed_generations_through(18432), Ok(2));
    assert_eq!(
        value.completed_generations_through(16384),
        Err(TimingError::Inconsistent)
    );
    let before = value.clone();
    let mut bad = generation(3);
    bad.trailing_frames = 1024;
    assert!(value.append(bad, 27).is_err());
    assert_eq!(value, before);
    assert_eq!(value.completed_generations_through(17408), Ok(1));
}

#[test]
fn cold_movie_extent_matches_native_header_offsets_video_and_submilliseconds() {
    let video = crate::codec::H264Config::new(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("existing generated SPS/PPS");
    for (start, units, head, tail, first, end, ticks, milliseconds) in [
        (0, 9, 2112, 704, None, None, 96_000, 134),
        (1440, 97, 2112, 860, None, None, 1_466_940, 2038),
        (
            0,
            97,
            2112,
            860,
            Some(27_000),
            Some(270_001),
            2_160_008,
            3001,
        ),
        (0, 1, 1023, 0, None, None, 15, 1),
    ] {
        let value =
            RecordingPresentation::from_coded_audio_units(start, units, head, tail, first, end)
                .expect("bounded coded-unit construction");
        let extent = value.movie_extent().expect("checked cold extent");
        assert_eq!(extent.movie_timescale, 720_000);
        assert_eq!(extent.duration_ticks, ticks);
        assert_eq!(extent.duration_ms_ceil, milliseconds);
        assert_eq!(
            extent.duration_ticks,
            value.movie_duration().expect("same header duration")
        );
        let header =
            fmp4::init_segment_for_native_recording(first.map(|_| &video), &audio(), &value)
                .expect("same native header");
        let encoded = movie(&header);
        assert_eq!(encoded.mvhd.timescale, extent.movie_timescale);
        assert_eq!(encoded.mvhd.duration, extent.duration_ticks);
        assert_eq!(
            encoded
                .mvex
                .expect("native movie extends")
                .mehd
                .expect("native duration box")
                .fragment_duration,
            extent.duration_ticks
        );
    }
}

#[test]
fn cold_movie_extent_keeps_short_head_and_interior_standby_semantics() {
    let head = RecordingPresentation::from_coded_audio_units(0, 0, 2112, 0, None, None)
        .expect("initial empty coded head");
    assert_eq!(
        head.movie_extent()
            .expect("empty coded head extent")
            .duration_ticks,
        0
    );
    assert_eq!(
        head.movie_extent()
            .expect("empty coded head extent")
            .duration_ms_ceil,
        0
    );
    // Two 9216-frame coded generations retain their interior 704+2112 slot.
    let complete = RecordingPresentation::from_coded_audio_units(0, 18, 2112, 704, None, None)
        .expect("two-generation coded extent");
    let extent = complete
        .movie_extent()
        .expect("complete multi-generation extent");
    assert_eq!(extent.duration_ticks, 15_616 * 15);
    assert_eq!(extent.duration_ms_ceil, 326);
    assert_eq!(15_616, 2 * 6400 + 704 + 2112);
}

#[test]
fn cold_movie_extent_ceil_handles_maximum_ticks_without_rounding_add_overflow() {
    let value =
        RecordingPresentation::from_coded_audio_units(u64::MAX / 15 - 1024, 1, 0, 0, None, None)
            .expect("largest exact audio endpoint");
    let extent = value.movie_extent().expect("ceiling must not add719");
    assert_eq!(extent.duration_ticks, u64::MAX);
    assert_eq!(extent.duration_ms_ceil, u64::MAX / 720 + 1);
    let exact =
        RecordingPresentation::from_coded_audio_units(0, 3, 0, 0, None, None).expect("exact64ms");
    assert_eq!(
        exact
            .movie_extent()
            .expect("exact-millisecond extent")
            .duration_ms_ceil,
        64
    );
}

#[test]
fn cold_movie_extent_refuses_constructor_clock_and_scalar_overflow() {
    assert_eq!(
        RecordingPresentation::from_coded_audio_units(0, u64::MAX, 0, 0, None, None),
        Err(TimingError::Overflow)
    );
    for value in [
        RecordingPresentation::from_coded_audio_units(u64::MAX / 15, 1, 0, 0, None, None)
            .expect("constructor defers audio-clock overflow"),
        RecordingPresentation::from_coded_audio_units(0, 1, 0, 0, Some(0), Some(u64::MAX))
            .expect("constructor defers video-clock overflow"),
        RecordingPresentation::from_coded_audio_units(0, 1, 8192, 0, None, None)
            .expect("constructor defers unsupported head validation"),
        RecordingPresentation::from_coded_audio_units(0, 1, 0, 1024, None, None)
            .expect("constructor defers invalid tail validation"),
    ] {
        assert!(value.movie_extent().is_err());
    }
}
