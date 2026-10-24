use super::*;
use crate::MediaCodec;

fn native_mux(fragment_ms: u32) -> Arc<NativeRecordingMuxer> {
    NativeRecordingMuxer::with_native_aac_timing(false, true, 0, 4096, fragment_ms)
        .expect("native recording owner")
}
fn audio() -> Arc<MediaCodec> {
    MediaCodec::aac(&[0x11, 0x88]).expect("current native output AAC")
}
fn sample(codec: &MediaCodec, n: u64) -> Arc<MediaSample> {
    let pts = i64::try_from(n * 1024).expect("bounded test timestamp");
    codec
        .sample(&[1, 2, 3], pts, pts, 1024, 48000, false)
        .expect("validated sample handle")
}
fn drain(mux: &NativeRecordingMuxer) -> Vec<NativeRecordingChunk> {
    let mut chunks = Vec::new();
    while let Some(chunk) = mux.take_native_output().expect("actual native output") {
        chunks.push(chunk);
    }
    chunks
}
fn generation() -> NativeAudioGenerationReceipt {
    NativeAudioGenerationReceipt {
        generation: 1,
        input_frames: 6400,
        raw_frames: 6400,
        zero_frames: 0,
        input_rate: 48000,
        encoded_frames: 9216,
        leading_frames: 2112,
        trailing_frames: 704,
        source_mapping: NativeRecordingSourceMapping::Nearest,
    }
}
fn file() -> NativeAudioFileReceipt {
    NativeAudioFileReceipt {
        generation_count: 1,
        source_frames_at_48k: 6400,
        intentional_standby_frames: 0,
        encoded_frames: 9216,
        first_leading_frames: 2112,
        last_trailing_frames: 704,
    }
}

#[test]
fn complete_native_file_requires_exact_eof_and_keeps_fixed_header() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("measured first head");
    let mut chunks = Vec::new();
    for n in 0..9 {
        mux.push(sample(&codec, n)).expect("same validated AAC");
        chunks.extend(drain(&mux));
    }
    mux.end_native_audio_generation(generation())
        .expect("actual generation EOF");
    mux.complete_native_audio(file())
        .expect("actual aggregate EOF");
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("checked complete finish");
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("same finish repeat");
    chunks.extend(drain(&mux));
    let first = chunks.first().expect("initial header");
    let last = chunks.last().expect("final header");
    assert_eq!(first.kind, NativeRecordingChunkKind::Initial);
    assert_eq!(last.kind, NativeRecordingChunkKind::FinalHeader);
    assert_eq!(first.bytes.len(), last.bytes.len());
    assert_ne!(first.bytes, last.bytes);
    assert_eq!(mux.stats().expect("stats").accepted_samples, 9);
    assert!(
        mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
            .is_err()
    );
    assert_eq!(
        mux.push(sample(&codec, 9)).err(),
        Some(NativeRecordingError::Closed)
    );
}

#[test]
fn native_checkpoint_covers_only_sealed_samples_not_new_buffered_au() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("first head");
    let mut chunks = Vec::new();
    for n in 0..5 {
        mux.push(sample(&codec, n)).expect("accepted AAC");
        chunks.extend(drain(&mux));
    }
    let checkpoint = chunks
        .iter()
        .find_map(|x| x.checkpoint.clone())
        .expect("span actually sealed fragment");
    let facts = checkpoint.facts();
    assert_eq!(facts.sequence, 1);
    assert_eq!(facts.audio_units, 4, "new fifth AU is still buffered");
    assert_eq!(facts.audio_end, 4096);
    assert_eq!(facts.completed_generation_count, 0);
    assert_eq!(mux.stats().expect("stats").buffered_samples, 1);
    assert_eq!(mux.stats().expect("stats").accepted_samples, 5);
    let header = mux
        .checkpoint_header(checkpoint.clone())
        .expect("exact sealed prefix header");
    assert_eq!(header.len(), chunks[0].bytes.len());
    let other = native_mux(100);
    assert_eq!(
        other.checkpoint_header(checkpoint).err(),
        Some(NativeRecordingError::InvalidMedia)
    );
}

#[test]
fn missing_final_eof_is_sticky_but_retains_positive_incomplete_tail() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("first head");
    for n in 0..5 {
        mux.push(sample(&codec, n)).expect("accepted prefix");
        drain(&mux);
    }
    assert_eq!(
        mux.finish_native(NativeRecordingCompletion::Complete),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        mux.push(sample(&codec, 5)),
        Err(NativeRecordingError::InvalidMedia)
    );
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("preserve unknown EOF without inventing padding");
    let chunks = drain(&mux);
    assert_eq!(chunks[0].kind, NativeRecordingChunkKind::Fragment);
    assert_eq!(
        chunks[0]
            .checkpoint
            .as_ref()
            .expect("actual tail coverage")
            .facts()
            .audio_units,
        5
    );
    assert_eq!(
        chunks.last().expect("header").kind,
        NativeRecordingChunkKind::FinalHeader
    );
}

#[test]
fn all_short_coded_heads_are_retained_without_claiming_complete() {
    for count in 1..=3 {
        let mux = native_mux(100);
        let codec = audio();
        mux.begin_native_audio(2112).expect("first head");
        for n in 0..count {
            mux.push(sample(&codec, n)).expect("positive original AU");
            drain(&mux);
        }
        mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
            .expect("short retained coded prefix");
        let chunks = drain(&mux);
        let cp = chunks[0].checkpoint.as_ref().expect("actual coded tail");
        assert_eq!(cp.facts().audio_units, count);
        assert_eq!(cp.facts().completed_generation_count, 0);
    }
}

#[test]
fn timing_event_replay_or_future_count_cannot_relabel_current_file() {
    for mutate in 0..3 {
        let mux = native_mux(100);
        let codec = audio();
        mux.begin_native_audio(2112).expect("first head");
        for n in 0..9 {
            mux.push(sample(&codec, n)).expect("AAC");
            drain(&mux);
        }
        let mut event = generation();
        match mutate {
            0 => event.generation = 2,
            1 => event.encoded_frames += 1024,
            _ => event.raw_frames += 1,
        }
        assert_eq!(
            mux.end_native_audio_generation(event),
            Err(NativeRecordingError::InvalidMedia)
        );
        assert_eq!(
            mux.end_native_audio_generation(generation()),
            Err(NativeRecordingError::Closed)
        );
        mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
            .expect("failed facts preserve real prefix");
    }
}

#[test]
fn completed_audio_eof_rejects_late_au_before_it_changes_counters() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("head");
    for n in 0..9 {
        mux.push(sample(&codec, n)).expect("AAC");
        drain(&mux);
    }
    mux.end_native_audio_generation(generation())
        .expect("generation EOF");
    mux.complete_native_audio(file()).expect("file EOF");
    assert_eq!(
        mux.push(sample(&codec, 9)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(mux.stats().expect("stats").accepted_samples, 9);
    assert!(
        mux.finish_native(NativeRecordingCompletion::Complete)
            .is_err()
    );
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("retained incomplete prefix");
}

#[test]
fn native_mode_and_legacy_calls_cannot_silently_mix() {
    let mux = native_mux(100);
    assert_eq!(mux.finish(), Err(NativeRecordingError::InvalidConfig));
    assert_eq!(
        mux.take_output().err(),
        Some(NativeRecordingError::InvalidConfig)
    );
    let legacy = NativeRecordingMuxer::new(false, true, 0, 4096, 100).expect("legacy owner");
    assert_eq!(
        legacy.take_native_output().err(),
        Some(NativeRecordingError::InvalidConfig)
    );
    assert_eq!(
        legacy.begin_native_audio(2112),
        Err(NativeRecordingError::InvalidConfig)
    );
    assert!(NativeRecordingMuxer::with_native_aac_timing(true, false, 0, 4096, 100).is_err());
}

#[test]
fn native_wrong_output_format_refuses_before_ownership_transfer() {
    let mux = native_mux(100);
    mux.begin_native_audio(2112).expect("head");
    let codec = MediaCodec::aac(&[0x11, 0x90]).expect("legacy stereo output");
    assert_eq!(
        mux.push(sample(&codec, 0)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(mux.stats().expect("stats").accepted_samples, 0);
    assert_eq!(mux.stats().expect("stats").buffered_bytes, 0);
}

#[test]
fn native_audio_gap_cannot_be_hidden_by_a_contiguous_presentation_header() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("head");
    mux.push(sample(&codec, 0)).expect("first actual AAC");
    drain(&mux);
    assert_eq!(
        mux.push(sample(&codec, 2)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(mux.stats().expect("stats").accepted_samples, 1);
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("accepted prefix remains");
}

#[test]
fn actual_sample_handle_is_shared_until_fragment_released() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(2112).expect("head");
    let original = sample(&codec, 0);
    assert_eq!(Arc::strong_count(&original.sample), 1);
    mux.push(original.clone()).expect("same original sample");
    drain(&mux);
    assert_eq!(Arc::strong_count(&original.sample), 2);
    assert_eq!(
        mux.guard().expect("test owner").audio.samples[0]
            .data
            .as_ptr(),
        original.sample.payload().as_ptr()
    );
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("actual flush");
    assert_eq!(Arc::strong_count(&original.sample), 1);
}

fn own_boxes(bytes: &[u8]) -> Vec<&[u8]> {
    let mut at = 0;
    let mut result = Vec::new();
    while at < bytes.len() {
        assert!(at + 8 <= bytes.len());
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("box size")) as usize;
        assert!(size >= 8 && at + size <= bytes.len());
        result.push(&bytes[at..at + size]);
        at += size;
    }
    result
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("generated scalar"))
}
fn audio_wire_durations(chunks: &[NativeRecordingChunk]) -> Vec<u32> {
    let mut result = Vec::new();
    for chunk in chunks
        .iter()
        .filter(|x| x.kind == NativeRecordingChunkKind::Fragment)
    {
        let top = own_boxes(&chunk.bytes);
        let moof = top.iter().find(|x| &x[4..8] == b"moof").expect("moof");
        for traf in own_boxes(&moof[8..])
            .into_iter()
            .filter(|x| &x[4..8] == b"traf")
        {
            let children = own_boxes(&traf[8..]);
            let tfhd = children.iter().find(|x| &x[4..8] == b"tfhd").expect("tfhd");
            if u32_at(tfhd, 12) != fmp4::AUDIO_TRACK {
                continue;
            }
            for trun in children.iter().filter(|x| &x[4..8] == b"trun") {
                let flags = u32_at(trun, 8) & 0xffffff;
                assert_ne!(flags & 0x100, 0);
                let mut at = 16 + usize::from(flags & 1 != 0) * 4 + usize::from(flags & 4 != 0) * 4;
                for _ in 0..u32_at(trun, 12) {
                    result.push(u32_at(trun, at));
                    at += 4;
                    for bit in [0x200, 0x400, 0x800] {
                        if flags & bit != 0 {
                            at += 4;
                        }
                    }
                }
                assert_eq!(at, trun.len());
            }
        }
    }
    result
}
fn small_receipts(
    count: u64,
    head: u32,
    tail: u32,
) -> (NativeAudioGenerationReceipt, NativeAudioFileReceipt) {
    let encoded = count * 1024;
    let valid = encoded - u64::from(head) - u64::from(tail);
    (
        NativeAudioGenerationReceipt {
            generation: 1,
            input_frames: valid,
            raw_frames: valid,
            zero_frames: 0,
            input_rate: 48000,
            encoded_frames: encoded,
            leading_frames: head,
            trailing_frames: tail,
            source_mapping: NativeRecordingSourceMapping::Nearest,
        },
        NativeAudioFileReceipt {
            generation_count: 1,
            source_frames_at_48k: valid,
            intentional_standby_frames: 0,
            encoded_frames: encoded,
            first_leading_frames: head,
            last_trailing_frames: tail,
        },
    )
}
fn video_codec() -> Arc<MediaCodec> {
    MediaCodec::h264(
        &[
            0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0,
            3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
        ],
        &[0x68, 0xef, 0xbc, 0x80],
    )
    .expect("generated config")
}
fn video_sample(codec: &MediaCodec, n: u64, size: usize) -> Arc<MediaSample> {
    assert!(size >= 5);
    let mut data = vec![0; size];
    data[..4].copy_from_slice(&((size - 4) as u32).to_be_bytes());
    data[4] = 0x65;
    let pts = (n * 3000) as i64;
    codec
        .sample(&data, pts, pts, 3000, 90000, true)
        .expect("typed generated IDR")
}

#[test]
fn final_tail_is_serialized_before_first_write_and_shared_sample_is_unchanged() {
    for (count, head, tail) in [
        (1, 0, 0),
        (1, 0, 1),
        (1, 0, 1023),
        (3, 2112, 448),
        (9, 2112, 704),
    ] {
        let mux = native_mux(100);
        let codec = audio();
        mux.begin_native_audio(head).expect("head");
        let mut chunks = Vec::new();
        let mut original = None;
        for n in 0..count {
            let unit = sample(&codec, n);
            original = Some(unit.clone());
            mux.push(unit).expect("AU");
            chunks.extend(drain(&mux));
        }
        let (generation, file) = small_receipts(count, head, tail);
        mux.end_native_audio_generation(generation)
            .expect("measured generation");
        mux.complete_native_audio(file).expect("checked aggregate");
        mux.finish_native(NativeRecordingCompletion::Complete)
            .expect("complete");
        chunks.extend(drain(&mux));
        let wire = audio_wire_durations(&chunks);
        assert_eq!(wire.len(), count as usize);
        assert!(wire[..wire.len() - 1].iter().all(|x| *x == 1024));
        assert_eq!(wire.last(), Some(&(1024 - tail)));
        let last = chunks
            .iter()
            .rev()
            .find_map(|x| x.checkpoint.clone())
            .expect("terminal sealed receipt");
        let facts = last.facts();
        assert_eq!(facts.audio_units, count);
        assert_eq!(facts.audio_end, count * 1024);
        assert_eq!(facts.audio_container_end, count * 1024 - u64::from(tail));
        assert_eq!(facts.final_trailing_frames, tail);
        assert!(facts.complete_audio_eof);
        assert_eq!(facts.completed_generation_count, 1);
        let final_header = chunks
            .iter()
            .find(|x| x.kind == NativeRecordingChunkKind::FinalHeader)
            .expect("final header");
        assert_eq!(
            mux.checkpoint_header(last)
                .expect("exact predeath terminal header"),
            final_header.bytes
        );
        assert_eq!(
            original
                .expect("original retained")
                .sample
                .timing()
                .duration(),
            1024
        );
        assert_eq!(mux.stats().expect("released").buffered_samples, 0);
    }
}

#[test]
fn invalid_or_unaccepted_successor_cannot_release_a_possible_final_au() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(0).expect("head");
    for n in 0..4 {
        mux.push(sample(&codec, n)).expect("AU");
        drain(&mux);
    }
    let before = mux.stats().expect("before");
    assert_eq!(
        mux.push(sample(&codec, 5)),
        Err(NativeRecordingError::InvalidMedia)
    );
    let after = mux.stats().expect("after");
    assert_eq!(before.accepted_samples, after.accepted_samples);
    assert_eq!(before.buffered_samples, after.buffered_samples);
    assert_eq!(before.fragments, after.fragments);
    assert!(drain(&mux).is_empty());
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("retain all accepted");
    assert_eq!(audio_wire_durations(&drain(&mux)), vec![1024; 4]);
}

#[test]
fn blocked_output_retry_does_not_accept_or_relabel_the_same_successor() {
    let mux = native_mux(100);
    let codec = audio();
    mux.begin_native_audio(0).expect("head");
    mux.push(sample(&codec, 0)).expect("first");
    assert_eq!(
        mux.push(sample(&codec, 1)),
        Err(NativeRecordingError::Backpressure)
    );
    assert_eq!(mux.stats().expect("pending").accepted_samples, 1);
    drain(&mux);
    mux.push(sample(&codec, 1)).expect("same unit after drain");
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("retained tail");
    assert_eq!(audio_wire_durations(&drain(&mux)), vec![1024; 2]);
}

#[test]
fn tiny_budget_audio_successor_can_release_old_au_atomically_without_extra_input_slot() {
    let mux = NativeRecordingMuxer::with_native_aac_timing(false, true, 0, 1024, 100)
        .expect("small budget");
    let codec = audio();
    mux.begin_native_audio(0).expect("head");
    for n in 0..2 {
        let pts = n * 1024;
        let unit = codec
            .sample(&vec![1; 900], pts, pts, 1024, 48000, false)
            .expect("bounded sample");
        mux.push(unit).expect("successor validates before release");
        let state = mux.guard().expect("owner");
        assert!(state.bytes <= 1024);
        assert_eq!(state.samples, 1);
        drop(state);
        let chunks = drain(&mux);
        if n == 1 {
            assert_eq!(audio_wire_durations(&chunks), vec![1024]);
            assert_eq!(
                chunks[0]
                    .checkpoint
                    .as_ref()
                    .expect("old only")
                    .facts()
                    .audio_units,
                1
            );
        }
    }
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("retained last");
    assert_eq!(audio_wire_durations(&drain(&mux)), vec![1024]);
}

#[test]
fn held_audio_plus_video_over_budget_is_sticky_without_partial_flush_or_acceptance() {
    let mux = NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 1024, 100)
        .expect("small AV budget");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(0).expect("head");
    mux.push(video_sample(&v, 0, 5)).expect("initial IDR");
    drain(&mux);
    mux.push(
        a.sample(&vec![1; 900], 0, 0, 1024, 48000, false)
            .expect("AAC"),
    )
    .expect("held audio");
    drain(&mux);
    let before = mux.stats().expect("before");
    assert_eq!(before.buffered_bytes, 905);
    assert_eq!(
        mux.push(video_sample(&v, 1, 400)),
        Err(NativeRecordingError::InvalidMedia)
    );
    let after = mux.stats().expect("after");
    assert_eq!(after.accepted_samples, 2);
    assert_eq!(after.buffered_bytes, 905);
    assert_eq!(after.fragments, 0);
    assert!(drain(&mux).is_empty());
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("safe original prefix");
    let chunks = drain(&mux);
    assert_eq!(audio_wire_durations(&chunks), vec![1024]);
    assert!(
        !chunks[0]
            .checkpoint
            .as_ref()
            .expect("incomplete")
            .facts()
            .complete_audio_eof
    );
}

#[test]
fn video_driven_flush_retains_audio_and_checkpoint_never_counts_it() {
    let mux =
        NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 1024, 100).expect("AV owner");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(0).expect("head");
    mux.push(video_sample(&v, 0, 5)).expect("IDR");
    drain(&mux);
    mux.push(sample(&a, 0)).expect("held AAC");
    drain(&mux);
    mux.push(video_sample(&v, 1, 700)).expect("video");
    drain(&mux);
    mux.push(video_sample(&v, 2, 700)).expect("video boundary");
    let chunks = drain(&mux);
    let facts = chunks[0]
        .checkpoint
        .as_ref()
        .expect("actual video-only chunk")
        .facts();
    assert_eq!(facts.audio_units, 0);
    assert_eq!(facts.audio_end, 0);
    assert_eq!(facts.video_units, 2);
    assert!(audio_wire_durations(&chunks).is_empty());
    let state = mux.guard().expect("held owner");
    assert_eq!(state.audio.samples.len(), 1);
    assert_eq!(state.audio.samples[0].duration, 1024);
    drop(state);
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("tail");
    assert_eq!(audio_wire_durations(&drain(&mux)), vec![1024]);
}

#[test]
fn stalled_audio_cannot_gain_unbounded_media_age_from_video_only_arrivals() {
    let mux =
        NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 4096, 100).expect("AV owner");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(0).expect("head");
    mux.push(video_sample(&v, 0, 5)).expect("IDR");
    drain(&mux);
    mux.push(sample(&a, 0)).expect("AAC");
    drain(&mux);
    assert_eq!(
        mux.push(video_sample(&v, 3, 5)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        mux.stats()
            .expect("no successor acceptance")
            .accepted_samples,
        2
    );
    assert!(drain(&mux).is_empty());
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("actual accepted prefix");
}

#[test]
fn final_trailing_at_least_one_au_refuses_without_dropping_or_redistributing_payload() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0).expect("head");
    for n in 0..3 {
        mux.push(sample(&a, n)).expect("AU");
        drain(&mux);
    }
    let (mut generation, _) = small_receipts(3, 0, 0);
    generation.trailing_frames = 1024;
    generation.input_frames -= 1024;
    generation.raw_frames -= 1024;
    assert_eq!(
        mux.end_native_audio_generation(generation),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        mux.finish_native(NativeRecordingCompletion::Complete),
        Err(NativeRecordingError::InvalidMedia)
    );
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("no guessed multi-AU trim");
    assert_eq!(audio_wire_durations(&drain(&mux)), vec![1024; 3]);
}

#[test]
fn empty_native_owner_cannot_create_a_fake_positive_complete_file() {
    let mux = native_mux(100);
    mux.begin_native_audio(0).expect("head");
    assert_eq!(
        mux.finish_native(NativeRecordingCompletion::Complete),
        Err(NativeRecordingError::MissingTracks)
    );
    assert!(drain(&mux).is_empty());
    assert_eq!(mux.stats().expect("empty").accepted_samples, 0);
}

#[test]
fn multiple_generation_receipts_before_video_flush_count_only_fully_sealed_generations() {
    let mux =
        NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 1024, 100).expect("AV owner");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(0).expect("head");
    mux.push(video_sample(&v, 0, 5)).expect("IDR");
    drain(&mux);
    for n in 0..3 {
        mux.push(sample(&a, n)).expect("actual generation AU");
        drain(&mux);
        mux.end_native_audio_generation(NativeAudioGenerationReceipt {
            generation: n + 1,
            input_frames: 1024,
            raw_frames: 1024,
            zero_frames: 0,
            input_rate: 48000,
            encoded_frames: 1024,
            leading_frames: 0,
            trailing_frames: 0,
            source_mapping: NativeRecordingSourceMapping::Nearest,
        })
        .expect("actual each-generation EOF");
    }
    mux.push(video_sample(&v, 1, 700)).expect("video");
    drain(&mux);
    mux.push(video_sample(&v, 2, 700))
        .expect("video-driven boundary");
    let chunks = drain(&mux);
    let facts = chunks[0]
        .checkpoint
        .as_ref()
        .expect("sealed prefix")
        .facts();
    assert_eq!(facts.audio_units, 2);
    assert_eq!(facts.completed_generation_count, 2);
    assert!(!facts.complete_audio_eof);
    assert_eq!(audio_wire_durations(&chunks), vec![1024; 2]);
    mux.complete_native_audio(NativeAudioFileReceipt {
        generation_count: 3,
        source_frames_at_48k: 3072,
        intentional_standby_frames: 0,
        encoded_frames: 3072,
        first_leading_frames: 0,
        last_trailing_frames: 0,
    })
    .expect("actual aggregate");
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("complete");
    let chunks = drain(&mux);
    let facts = chunks[0]
        .checkpoint
        .as_ref()
        .expect("terminal prefix")
        .facts();
    assert_eq!(facts.audio_units, 3);
    assert_eq!(facts.completed_generation_count, 3);
    assert!(facts.complete_audio_eof);
}

#[test]
fn eof_at_output_boundary_keeps_last_unit_until_drain_and_checked_finish() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0).expect("head");
    let mut chunks = Vec::new();
    for n in 0..4 {
        mux.push(sample(&a, n)).expect("AU");
        chunks.extend(drain(&mux));
    }
    mux.push(sample(&a, 4))
        .expect("boundary successor admitted");
    assert_eq!(mux.stats().expect("held last").buffered_samples, 1);
    let (generation, file) = small_receipts(5, 0, 448);
    mux.end_native_audio_generation(generation)
        .expect("generation");
    mux.complete_native_audio(file).expect("aggregate");
    assert_eq!(
        mux.finish_native(NativeRecordingCompletion::Complete),
        Err(NativeRecordingError::Backpressure)
    );
    assert!(!mux.stats().expect("not committed early").finished);
    chunks.extend(drain(&mux));
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("after old output drain");
    chunks.extend(drain(&mux));
    assert_eq!(
        audio_wire_durations(&chunks),
        vec![1024, 1024, 1024, 1024, 576]
    );
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("same outcome idempotent");
    assert!(drain(&mux).is_empty());
    assert_eq!(
        mux.finish_native(NativeRecordingCompletion::IncompletePrefix),
        Err(NativeRecordingError::Closed)
    );
}

#[test]
fn unknown_eof_preserves_nominal_last_duration_even_after_receiving_complete_metadata() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0).expect("head");
    for n in 0..3 {
        mux.push(sample(&a, n)).expect("AU");
        drain(&mux);
    }
    let (generation, file) = small_receipts(3, 0, 448);
    mux.end_native_audio_generation(generation)
        .expect("generation");
    mux.complete_native_audio(file).expect("aggregate");
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("explicit incomplete policy");
    let chunks = drain(&mux);
    assert_eq!(audio_wire_durations(&chunks), vec![1024; 3]);
    let facts = chunks[0]
        .checkpoint
        .as_ref()
        .expect("unknown tail header")
        .facts();
    assert_eq!(facts.final_trailing_frames, 0);
    assert_eq!(facts.audio_container_end, facts.audio_end);
    assert!(!facts.complete_audio_eof);
}

fn encode_slot(
    mux: &NativeRecordingMuxer,
    token: Arc<NativeRecordingCheckpoint>,
    hash: [u8; 32],
) -> NativeRecordingCheckpointEncoding {
    mux.encode_checkpoint(token, vec![7; 32], vec![9; 32], hash.to_vec())
        .expect("exact issued snapshot encode")
}
#[test]
fn portable_checkpoint_frontier_equals_actual_serialized_initial_and_fragments() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(2112).expect("measured head");
    let mut initial = Vec::new();
    let mut region = Vec::new();
    let mut saved = None;
    let mut decoded_prior = None;
    for n in 0..17 {
        let payload = vec![1; (n as usize % 5 + 1) * 7];
        let pts = (n * 1024) as i64;
        let unit = a
            .sample(&payload, pts, pts, 1024, 48000, false)
            .expect("validated variable AAC payload");
        mux.push(unit).expect("bounded actual push");
        for chunk in drain(&mux) {
            match chunk.kind {
                NativeRecordingChunkKind::Initial => {
                    assert!(initial.is_empty());
                    initial = chunk.bytes;
                }
                NativeRecordingChunkKind::Fragment => {
                    region.extend_from_slice(&chunk.bytes);
                    let token = chunk.checkpoint.expect("fragment issuer");
                    let hash = checkpoint::metadata_sha256(&region);
                    let encoded = encode_slot(&mux, token.clone(), hash);
                    assert_eq!(encoded.info.initial_header_len as usize, initial.len());
                    assert_eq!(
                        encoded.info.initial_header_sha256,
                        checkpoint::metadata_sha256(&initial)
                    );
                    assert_eq!(encoded.info.fragment_region_bytes, region.len() as u64);
                    assert_eq!(
                        encoded.info.committed_offset,
                        (initial.len() + region.len()) as u64
                    );
                    let decoded = decode_recording_checkpoint(
                        encoded.bytes.clone(),
                        vec![7; 32],
                        vec![9; 32],
                    )
                    .expect("same portable codec");
                    assert_eq!(
                        decoded.header(),
                        mux.checkpoint_header(token.clone())
                            .expect("same issuer header")
                    );
                    assert_eq!(
                        decoded.info().committed_offset,
                        encoded.info.committed_offset
                    );
                    if let Some(prior) = decoded_prior.take() {
                        validate_recording_checkpoint_successor(prior, decoded.clone())
                            .expect("real exact next chunk");
                    }
                    decoded_prior = Some(decoded);
                    if saved.is_none() {
                        saved = Some((token, hash, encoded.bytes));
                    }
                }
                NativeRecordingChunkKind::FinalHeader => panic!("no early final header"),
            }
        }
    }
    let (old, hash, bytes) = saved.expect("several actual fragments");
    let original_end = old.sealed_file_end;
    assert!(mux.guard().expect("owner").native_sealed_fragment_bytes > old.fragment_region_bytes);
    assert_eq!(
        encode_slot(&mux, old.clone(), hash).bytes,
        bytes,
        "old token never uses new counters"
    );
    mux.finish_native(NativeRecordingCompletion::IncompletePrefix)
        .expect("unknown EOF prefix");
    let before = mux
        .guard()
        .expect("committed finish")
        .native_sealed_fragment_bytes;
    let chunks = drain(&mux);
    assert!(
        chunks
            .iter()
            .any(|c| c.kind == NativeRecordingChunkKind::FinalHeader)
    );
    assert_eq!(
        mux.guard()
            .expect("no dequeue counter")
            .native_sealed_fragment_bytes,
        before
    );
    assert_eq!(
        encode_slot(&mux, old.clone(), hash).info.committed_offset,
        original_end
    );
    mux.cancel().expect("already sealed cancellation");
    assert_eq!(
        encode_slot(&mux, old, hash).bytes,
        bytes,
        "old accepted prefix survives cancel"
    );
}
#[test]
fn portable_checkpoint_foreign_owner_and_digest_refusal_do_not_mutate_output_or_state() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0).expect("head");
    let mut token = None;
    for n in 0..5 {
        mux.push(sample(&a, n)).expect("AU");
        let chunks = drain(&mux);
        token = token.or_else(|| chunks.into_iter().find_map(|x| x.checkpoint));
    }
    let token = token.expect("real issued chunk");
    let other = native_mux(100);
    let before = mux.stats().expect("before");
    let frontier = mux.guard().expect("owner").native_sealed_fragment_bytes;
    assert_eq!(
        other
            .encode_checkpoint(token.clone(), vec![7; 32], vec![9; 32], vec![13; 32])
            .err(),
        Some(RecordingCheckpointError::ForeignCheckpoint)
    );
    for length in [0, 31, 33, 128] {
        assert_eq!(
            mux.encode_checkpoint(token.clone(), vec![0; length], vec![9; 32], vec![13; 32])
                .err(),
            Some(RecordingCheckpointError::DigestLength)
        );
        assert_eq!(
            mux.encode_checkpoint(token.clone(), vec![7; 32], vec![0; length], vec![13; 32])
                .err(),
            Some(RecordingCheckpointError::DigestLength)
        );
        assert_eq!(
            mux.encode_checkpoint(token.clone(), vec![7; 32], vec![9; 32], vec![0; length])
                .err(),
            Some(RecordingCheckpointError::DigestLength)
        );
    }
    let after = mux.stats().expect("after");
    assert_eq!(after.accepted_samples, before.accepted_samples);
    assert_eq!(after.queued_output_bytes, before.queued_output_bytes);
    assert_eq!(
        mux.guard()
            .expect("unchanged native frontier")
            .native_sealed_fragment_bytes,
        frontier
    );
    assert!(drain(&mux).is_empty());
}
#[test]
fn portable_checkpoint_held_and_rejected_input_cannot_advance_byte_frontier() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0).expect("head");
    mux.push(sample(&a, 0)).expect("first held AU");
    drain(&mux);
    {
        let state = mux.guard().expect("owner");
        assert_eq!(state.native_sealed_fragment_bytes, 0);
        let plan = state
            .plan_native_flush(NativeFlushMode::Live {
                release_audio_tail: false,
            })
            .expect("empty seal plan");
        assert!(plan.output.is_none());
        assert_eq!(plan.sealed_fragment_bytes, 0);
    }
    for n in 1..5 {
        mux.push(sample(&a, n)).expect("AAC");
        drain(&mux);
    }
    let before = mux
        .guard()
        .expect("before refused successor")
        .native_sealed_fragment_bytes;
    assert_eq!(
        mux.push(sample(&a, 4)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        mux.guard()
            .expect("after refused successor")
            .native_sealed_fragment_bytes,
        before
    );
    assert!(drain(&mux).is_empty());
}
#[test]
fn portable_checkpoint_zero_audio_video_only_uses_actual_optional_topology_and_frontier() {
    let mux =
        NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 1024, 100).expect("AV owner");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(2112).expect("actual leading");
    let mut initial_len = 0;
    let mut token = None;
    let mut region_bytes = 0;
    mux.push(video_sample(&v, 0, 5)).expect("IDR");
    drain(&mux);
    mux.push(sample(&a, 0)).expect("held AAC");
    for c in drain(&mux) {
        if c.kind == NativeRecordingChunkKind::Initial {
            initial_len = c.bytes.len();
        }
    }
    mux.push(video_sample(&v, 1, 700)).expect("video");
    drain(&mux);
    mux.push(video_sample(&v, 2, 700)).expect("video boundary");
    for c in drain(&mux) {
        region_bytes += c.bytes.len();
        token = c.checkpoint;
    }
    let encoded = encode_slot(&mux, token.expect("actual video-only receipt"), [1; 32]);
    assert_eq!(encoded.info.facts.audio_units, 0);
    assert_eq!(encoded.info.facts.audio_end, 0);
    assert_eq!(encoded.info.facts.completed_generation_count, 0);
    assert_eq!(encoded.info.facts.video_units, 2);
    assert_eq!(
        encoded.info.committed_offset,
        (initial_len + region_bytes) as u64
    );
    decode_recording_checkpoint(encoded.bytes, vec![7; 32], vec![9; 32])
        .expect("zero AAC is not false empty media");
}
#[test]
fn portable_checkpoint_complete_tail_and_final_rewrite_keep_coded_and_container_frontiers_distinct()
{
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(2112).expect("head");
    for n in 0..9 {
        mux.push(sample(&a, n)).expect("actual AU");
        drain(&mux);
    }
    mux.end_native_audio_generation(generation())
        .expect("native generation EOF");
    mux.complete_native_audio(file())
        .expect("checked aggregate");
    let prior = mux
        .guard()
        .expect("pre-EOF frontier")
        .native_sealed_fragment_bytes;
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("real EOF");
    let chunks = drain(&mux);
    let mut fragment_len = 0;
    let mut token = None;
    for c in chunks {
        if c.kind == NativeRecordingChunkKind::Fragment {
            fragment_len += c.bytes.len();
            token = c.checkpoint;
        }
    }
    let after = mux
        .guard()
        .expect("post-EOF frontier")
        .native_sealed_fragment_bytes;
    assert_eq!(
        after,
        prior + fragment_len as u64,
        "offset-zero final rewrite adds no bytes"
    );
    let encoded = encode_slot(&mux, token.expect("actual final fragment"), [3; 32]);
    assert_eq!(encoded.info.facts.audio_end, 9216);
    assert_eq!(encoded.info.facts.audio_container_end, 8512);
    assert!(encoded.info.facts.complete_audio_eof);
    assert_eq!(encoded.info.facts.completed_generation_count, 1);
    assert_eq!(encoded.info.facts.final_trailing_frames, 704);
    decode_recording_checkpoint(encoded.bytes, vec![7; 32], vec![9; 32])
        .expect("real complete EOF slot");
}
#[test]
fn portable_checkpoint_frontier_arithmetic_refuses_before_commit_without_large_allocations() {
    assert_eq!(sealed_byte_frontier(100, 0, 20), Ok((20, 120)));
    assert_eq!(
        sealed_byte_frontier(100, u64::MAX, 1),
        Err(NativeRecordingError::Encoding)
    );
    assert_eq!(
        sealed_byte_frontier(100, i64::MAX as u64 - 100, 1),
        Err(NativeRecordingError::Encoding)
    );
}
#[test]
fn portable_checkpoint_existing_multi_generation_coverage_is_not_inferred_from_latest_eof() {
    let mux =
        NativeRecordingMuxer::with_native_aac_timing(true, true, 0, 1024, 100).expect("AV owner");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(0).expect("head");
    mux.push(video_sample(&v, 0, 5)).expect("IDR");
    drain(&mux);
    for n in 0..3 {
        mux.push(sample(&a, n)).expect("actual generation AU");
        drain(&mux);
        mux.end_native_audio_generation(NativeAudioGenerationReceipt {
            generation: n + 1,
            input_frames: 1024,
            raw_frames: 1024,
            zero_frames: 0,
            input_rate: 48000,
            encoded_frames: 1024,
            leading_frames: 0,
            trailing_frames: 0,
            source_mapping: NativeRecordingSourceMapping::Nearest,
        })
        .expect("actual FIFO EOF");
    }
    mux.push(video_sample(&v, 1, 700)).expect("video");
    drain(&mux);
    mux.push(video_sample(&v, 2, 700)).expect("boundary");
    let token = drain(&mux)
        .into_iter()
        .find_map(|x| x.checkpoint)
        .expect("sealed old prefix");
    let encoded = encode_slot(&mux, token, [4; 32]);
    assert_eq!(encoded.info.facts.audio_units, 2);
    assert_eq!(encoded.info.facts.completed_generation_count, 2);
    assert!(!encoded.info.facts.complete_audio_eof);
}

#[test]
fn portable_checkpoint_decode_refuses_oversize_input_and_nonexact_expected_digests() {
    assert_eq!(
        decode_recording_checkpoint(
            vec![0; checkpoint::MAX_CHECKPOINT_BYTES + 1],
            vec![7; 32],
            vec![9; 32]
        )
        .err(),
        Some(RecordingCheckpointError::Envelope)
    );
    for length in [0, 31, 33] {
        assert_eq!(
            decode_recording_checkpoint(Vec::new(), vec![7; length], vec![9; 32]).err(),
            Some(RecordingCheckpointError::DigestLength)
        );
        assert_eq!(
            decode_recording_checkpoint(Vec::new(), vec![7; 32], vec![9; length]).err(),
            Some(RecordingCheckpointError::DigestLength)
        );
    }
}

#[test]
fn cold_presentation_query_old_token_is_immutable_without_muxer_lock() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(2112).expect("synthetic native head");
    let mut first = None;
    for n in 0..9 {
        mux.push(sample(&a, n)).expect("same validated handle");
        for chunk in drain(&mux) {
            if first.is_none() {
                first = chunk.checkpoint;
            }
        }
    }
    let first = first.expect("actual issued first fragment token");
    let before = first.presentation_extent().expect("cold metadata");
    assert_eq!(before.movie_timescale, 720_000);
    assert_eq!(before.duration_ticks, 29_760);
    assert_eq!(before.duration_ms_ceil, 42);
    assert!(!before.complete_audio_eof);
    {
        let _held = mux.guard().expect("hold actual muxer lock");
        assert_eq!(
            first.presentation_extent().expect("retained prefix extent"),
            before
        );
    }
    let stats_before = mux.stats().expect("actual muxer statistics");
    mux.end_native_audio_generation(generation())
        .expect("measured generation EOF");
    mux.complete_native_audio(file())
        .expect("measured file EOF");
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("complete submitted SDK inputs");
    let terminal = drain(&mux)
        .into_iter()
        .rev()
        .find_map(|chunk| chunk.checkpoint)
        .expect("actual complete fragment token");
    let complete = terminal
        .presentation_extent()
        .expect("retained complete extent");
    assert_eq!(complete.duration_ticks, 96_000);
    assert_eq!(complete.duration_ms_ceil, 134);
    assert!(complete.complete_audio_eof);
    assert_eq!(
        first.presentation_extent().expect("retained prefix extent"),
        before
    );
    assert_eq!(
        mux.stats().expect("actual muxer statistics").duration_ms,
        192,
        "untrimmed legacy stats"
    );
    assert_eq!(
        mux.stats()
            .expect("actual muxer statistics")
            .accepted_samples,
        stats_before.accepted_samples
    );
    mux.cancel()
        .expect("cancel does not invalidate issued snapshots");
    assert_eq!(
        first.presentation_extent().expect("retained prefix extent"),
        before
    );
    assert_eq!(
        terminal
            .presentation_extent()
            .expect("retained complete extent"),
        complete
    );
    assert_eq!(
        mux.stats()
            .expect("actual muxer statistics")
            .queued_output_bytes,
        0
    );
}

#[test]
fn cold_presentation_query_keeps_epoch_offset_and_actual_longer_video() {
    let mux = NativeRecordingMuxer::with_native_aac_timing(true, true, 1_000_000_000, 4096, 5000)
        .expect("fixed one-second media epoch");
    let a = audio();
    let v = video_codec();
    mux.begin_native_audio(2112)
        .expect("measured first leading frames");
    mux.push(video_sample(&v, 39, 5))
        .expect("delayed video sample");
    drain(&mux);
    for n in 1..=9 {
        let pts = 48_000 + n * 1024;
        let input = a
            .sample(&[1, 2, 3], pts, pts, 1024, 48_000, false)
            .expect("delayed validated audio sample");
        mux.push(input).expect("accepted delayed audio");
        drain(&mux);
    }
    mux.end_native_audio_generation(generation())
        .expect("measured generation EOF");
    mux.complete_native_audio(file())
        .expect("measured file EOF");
    mux.finish_native(NativeRecordingCompletion::Complete)
        .expect("complete submitted SDK inputs");
    let chunks = drain(&mux);
    let token = chunks
        .iter()
        .find_map(|c| c.checkpoint.clone())
        .expect("actual sealed A/V token");
    let facts = token.facts();
    assert_eq!(facts.audio_start, 1024);
    assert_eq!(facts.video_start, Some(27_000));
    assert_eq!(facts.video_end, Some(30_000));
    let extent = token.presentation_extent().expect("issued token extent");
    assert_eq!(extent.movie_timescale, 720_000);
    assert_eq!(extent.duration_ticks, 240_000);
    assert_eq!(extent.duration_ms_ceil, 334);
    assert!(extent.complete_audio_eof);
    let final_header = chunks
        .iter()
        .find(|c| c.kind == NativeRecordingChunkKind::FinalHeader)
        .expect("actual final header");
    assert_eq!(
        mux.checkpoint_header(token)
            .expect("same-owner reconstruction header"),
        final_header.bytes
    );
}

#[test]
fn cold_presentation_query_distinguishes_one_frame_eof_and_incomplete_finish() {
    for (count, head, tail, outcome, expected_ticks, expected_ms, complete) in [
        (1, 0, 1023, NativeRecordingCompletion::Complete, 15, 1, true),
        (
            9,
            2112,
            704,
            NativeRecordingCompletion::IncompletePrefix,
            106_560,
            148,
            false,
        ),
    ] {
        let mux = native_mux(100);
        let a = audio();
        mux.begin_native_audio(head).expect("known short head");
        for n in 0..count {
            mux.push(sample(&a, n))
                .expect("accepted validated audio sample");
            drain(&mux);
        }
        let (generation, file) = small_receipts(count, head, tail);
        mux.end_native_audio_generation(generation)
            .expect("known short generation EOF");
        mux.complete_native_audio(file)
            .expect("known short file EOF");
        mux.finish_native(outcome)
            .expect("requested SDK completion");
        let token = drain(&mux)
            .into_iter()
            .rev()
            .find_map(|c| c.checkpoint)
            .expect("actual final sealed token");
        let extent = token.presentation_extent().expect("issued token extent");
        assert_eq!(extent.duration_ticks, expected_ticks);
        assert_eq!(extent.duration_ms_ceil, expected_ms);
        assert_eq!(extent.complete_audio_eof, complete);
    }
}

#[test]
fn cold_presentation_query_defensive_scalar_boundary_returns_existing_errors() {
    let mux = native_mux(100);
    let a = audio();
    mux.begin_native_audio(0)
        .expect("synthetic zero leading frames");
    let mut token = None;
    for n in 0..5 {
        mux.push(sample(&a, n))
            .expect("accepted validated audio sample");
        for chunk in drain(&mux) {
            if token.is_none() {
                token = chunk.checkpoint;
            }
        }
    }
    let mut token = token.expect("actual unique issued token");
    // Private Rust test corruption only. No public token mutation/injection API.
    let original = token.facts();
    Arc::get_mut(&mut token)
        .expect("test owns the sole issued-token reference")
        .facts
        .audio_units = u64::MAX;
    assert_eq!(
        token.presentation_extent().err(),
        Some(NativeRecordingError::Encoding)
    );
    let unique = Arc::get_mut(&mut token).expect("test owns the sole issued-token reference");
    unique.facts = original;
    unique.facts.first_leading_frames = 8192;
    assert_eq!(
        token.presentation_extent().err(),
        Some(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        mux.stats()
            .expect("actual muxer statistics")
            .accepted_samples,
        5
    );
}
