use super::*;
use crate::MediaCodec;

// Synthetic compressed bytes and supplied source/end scalars exercise the SDK
// container path only. No AAC decode, native source consumption or EOF is proved.
fn programme_mux() -> Arc<NativeRecordingMuxer> {
    NativeRecordingMuxer::with_programme_aac_timing(0, 4096, 100).expect("programme muxer")
}
fn start() -> NativeProgrammeWindowStart {
    NativeProgrammeWindowStart {
        generation: 7,
        coded_origin: 48_000,
        preparation_frames: 8192,
        measured_displacement: 2048,
        discard_prefix_frames: 8192,
        movie_delay_ticks: 0,
    }
}
fn end() -> NativeProgrammeGenerationEnd {
    NativeProgrammeGenerationEnd {
        generation: 7,
        coded_frames: 15_360,
        queued: NativeProgrammeQueuedFrames {
            preparation: 8192,
            programme_raw: 1517,
            programme_zero: 1248,
            block_padding: 307,
            codec_drain: 4096,
        },
    }
}
fn codec() -> Arc<MediaCodec> {
    MediaCodec::aac(&[0x11, 0x88]).expect("canonical AAC descriptor")
}
fn sample(codec: &MediaCodec, n: u8) -> Arc<MediaSample> {
    let tick = 56_192 + i64::from(n) * 1024;
    codec
        .sample(&[0x21, n], tick, tick, 1024, 48_000, false)
        .expect("validated synthetic coded handle")
}
fn drain(mux: &NativeRecordingMuxer) -> Vec<NativeProgrammeChunk> {
    let mut result = Vec::new();
    while let Some(chunk) = mux.take_programme_output().expect("programme output") {
        result.push(chunk);
    }
    result
}
fn submitted() -> (Arc<NativeRecordingMuxer>, Vec<NativeProgrammeChunk>) {
    let mux = programme_mux();
    let codec = codec();
    mux.begin_programme_window(start()).expect("frozen mapping");
    let mut chunks = Vec::new();
    for n in 0..5 {
        mux.push(sample(&codec, n)).expect("selected original AU");
        chunks.extend(drain(&mux));
    }
    (mux, chunks)
}
fn token(chunks: &[NativeProgrammeChunk]) -> Arc<NativeProgrammeCheckpoint> {
    chunks
        .iter()
        .find_map(|x| x.checkpoint.clone())
        .expect("actual sealed programme token")
}
// These are assertions over our own finite generated boxes, not a production parser.
fn body<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    let at = bytes
        .windows(4)
        .position(|v| v == kind)
        .expect("generated box kind");
    &bytes[at + 4..]
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().expect("generated u32 field"))
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().expect("generated u64 field"))
}
fn tfdt(bytes: &[u8]) -> u64 {
    let value = body(bytes, b"tfdt");
    if value[0] == 1 {
        u64_at(value, 4)
    } else {
        u64::from(u32_at(value, 4))
    }
}
fn durations(bytes: &[u8]) -> Vec<u32> {
    let value = body(bytes, b"trun");
    let flags = u32_at(value, 0) & 0xff_ffff;
    let mut at = 8;
    if flags & 1 != 0 {
        at += 4;
    }
    if flags & 4 != 0 {
        at += 4;
    }
    let mut result = Vec::new();
    for _ in 0..u32_at(value, 4) {
        assert_ne!(flags & 0x100, 0);
        result.push(u32_at(value, at));
        at += 4;
        for mask in [0x200, 0x400, 0x800] {
            if flags & mask != 0 {
                if mask == 0x800 {
                    assert_eq!(u32_at(value, at), 0, "original AAC CTS");
                }
                at += 4;
            }
        }
    }
    result
}

#[test]
fn programme_actual_chunks_keep_original_clock_payload_and_only_final_container_tail() {
    let (mux, mut chunks) = submitted();
    let first = token(&chunks);
    assert_eq!(first.facts().audio_units, 4);
    assert_eq!(first.facts().audio_end, 60_288);
    assert!(!first.facts().window_geometry_complete);
    assert_eq!(
        mux.stats().expect("accepted prefix stats").buffered_samples,
        1
    );
    assert_eq!(
        mux.stats()
            .expect("unknown-tail presentation stats")
            .duration_ms,
        64
    );
    mux.close_programme_window(end())
        .expect("actual accepted-span geometry");
    mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
        .expect("submitted window container finish");
    mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
        .expect("idempotent container finish");
    chunks.extend(drain(&mux));
    assert!(drain(&mux).is_empty());
    let initial = chunks.first().expect("initial output");
    let final_header = chunks.last().expect("final header");
    assert_eq!(initial.kind, NativeRecordingChunkKind::Initial);
    assert_eq!(final_header.kind, NativeRecordingChunkKind::FinalHeader);
    assert_eq!(initial.bytes.len(), final_header.bytes.len());
    assert_eq!(u64_at(body(&final_header.bytes, b"mvhd"), 24), 41_475);
    let edit = body(&final_header.bytes, b"elst");
    assert_eq!(u32_at(edit, 4), 1);
    assert_eq!(u64_at(edit, 8), 41_475);
    assert_eq!(u64_at(edit, 16), 58_240);
    let fragments: Vec<_> = chunks
        .iter()
        .filter(|x| x.kind == NativeRecordingChunkKind::Fragment)
        .collect();
    assert_eq!(fragments.len(), 2);
    assert_eq!(tfdt(&fragments[0].bytes), 56_192);
    assert_eq!(tfdt(&fragments[1].bytes), 60_288);
    let all_durations: Vec<_> = fragments.iter().flat_map(|x| durations(&x.bytes)).collect();
    assert_eq!(all_durations, [1024, 1024, 1024, 1024, 717]);
    let payload: Vec<_> = fragments
        .iter()
        .flat_map(|x| body(&x.bytes, b"mdat").iter().copied())
        .collect();
    assert_eq!(payload, [0x21, 0, 0x21, 1, 0x21, 2, 0x21, 3, 0x21, 4]);
    let final_token = fragments
        .last()
        .expect("final fragment")
        .checkpoint
        .as_ref()
        .expect("final sealed token");
    assert_eq!(final_token.facts().audio_end, 61_312);
    assert_eq!(final_token.facts().audio_container_end, 61_005);
    assert_eq!(final_token.facts().trailing_frames, 307);
    assert!(final_token.facts().window_geometry_complete);
    assert_eq!(mux.stats().expect("programme duration").duration_ms, 58);
}

#[test]
fn programme_backpressure_wrong_mode_and_first_clock_do_not_advance_media() {
    let mux = programme_mux();
    let codec = codec();
    mux.begin_programme_window(start()).expect("mapping");
    mux.push(sample(&codec, 0))
        .expect("first AU creates actual INITIAL");
    let before = mux.stats().expect("pending output stats");
    assert_eq!(
        mux.close_programme_window(end()),
        Err(NativeRecordingError::Backpressure)
    );
    assert!(matches!(
        mux.take_native_output(),
        Err(NativeRecordingError::InvalidConfig)
    ));
    assert_eq!(
        mux.stats()
            .expect("same pending INITIAL")
            .queued_output_bytes,
        before.queued_output_bytes
    );
    drain(&mux);
    mux.push(sample(&codec, 1))
        .expect("backpressure did not taint run");
    let other = programme_mux();
    other
        .begin_programme_window(start())
        .expect("other mapping");
    assert_eq!(
        other.push(sample(&codec, 1)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        other.stats().expect("no media accepted").accepted_samples,
        0
    );
}

#[test]
fn programme_window_uses_actual_accepted_counter_and_deficit_stays_incomplete() {
    let (mux, chunks) = submitted();
    let old = token(&chunks);
    let mut wrong = end();
    wrong.coded_frames = 12_288;
    assert_eq!(
        mux.close_programme_window(wrong),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert!(
        mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
            .is_err()
    );
    mux.finish_programme(NativeProgrammeCompletion::IncompletePrefix)
        .expect("retained original accepted prefix");
    let tail = drain(&mux);
    let terminal = tail
        .iter()
        .find_map(|x| x.checkpoint.clone())
        .expect("incomplete final fragment token");
    assert!(!terminal.facts().window_geometry_complete);
    assert_eq!(terminal.facts().trailing_frames, 0);
    assert_eq!(terminal.facts().audio_container_end, 61_312);
    assert_eq!(old.facts().audio_units, 4);
    assert!(!old.facts().window_geometry_complete);
}

#[test]
fn programme_incomplete_finish_after_closed_geometry_does_not_apply_known_tail() {
    let (mux, _) = submitted();
    mux.close_programme_window(end())
        .expect("checked supplied end");
    mux.finish_programme(NativeProgrammeCompletion::IncompletePrefix)
        .expect("incomplete remains explicit");
    let chunks = drain(&mux);
    let checkpoint = token(&chunks);
    assert_eq!(checkpoint.facts().trailing_frames, 0);
    assert!(!checkpoint.facts().window_geometry_complete);
    assert_eq!(
        checkpoint
            .presentation_extent()
            .expect("unknown-tail metadata")
            .duration_ticks,
        46_080
    );
    assert!(
        mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
            .is_err()
    );
}

#[test]
fn programme_end_before_full_selection_and_late_native_output_refuse_clean_finish() {
    let mux = programme_mux();
    let codec = codec();
    mux.begin_programme_window(start()).expect("mapping");
    for n in 0..4 {
        mux.push(sample(&codec, n)).expect("partial selected input");
        drain(&mux);
    }
    assert!(mux.close_programme_window(end()).is_err());
    assert!(
        mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
            .is_err()
    );
    let full = programme_mux();
    full.begin_programme_window(start())
        .expect("same-codec late-output mapping");
    for n in 0..5 {
        full.push(sample(&codec, n))
            .expect("same-codec selected AU");
        drain(&full);
    }
    full.close_programme_window(end())
        .expect("checked complete geometry");
    assert_eq!(
        full.push(sample(&codec, 5)),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert!(
        full.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
            .is_err()
    );
    full.finish_programme(NativeProgrammeCompletion::IncompletePrefix)
        .expect("late output keeps accepted prefix only");
}

#[test]
fn programme_v2_exact_owner_frontiers_and_immutable_tokens_survive_finish_cancel() {
    let (mux, chunks) = submitted();
    let first = token(&chunks);
    let binding = vec![7; 32];
    let file = vec![9; 32];
    let region_hash = vec![13; 32];
    let old = mux
        .encode_programme_checkpoint(
            first.clone(),
            binding.clone(),
            file.clone(),
            region_hash.clone(),
        )
        .expect("encode actual sealed token");
    assert_eq!(old.info.facts.audio_units, 4);
    assert_eq!(
        old.info.committed_offset,
        old.info.fragment_region_bytes + u64::from(old.info.initial_header_len)
    );
    let decoded = decode_programme_checkpoint(old.bytes.clone(), binding.clone(), file.clone())
        .expect("V2 outside decode");
    assert_eq!(
        decoded.header(),
        mux.programme_checkpoint_header(first.clone())
            .expect("exact token header")
    );
    assert!(decode_recording_checkpoint(old.bytes.clone(), binding.clone(), file.clone()).is_err());
    let (other, _) = submitted();
    assert!(other.programme_checkpoint_header(first.clone()).is_err());
    assert!(
        other
            .encode_programme_checkpoint(
                first.clone(),
                binding.clone(),
                file.clone(),
                region_hash.clone()
            )
            .is_err()
    );
    mux.close_programme_window(end()).expect("end");
    mux.finish_programme(NativeProgrammeCompletion::CompleteSubmittedWindow)
        .expect("finish");
    let final_chunks = drain(&mux);
    let final_token = token(&final_chunks);
    let final_bytes = mux
        .encode_programme_checkpoint(final_token, binding.clone(), file.clone(), vec![17; 32])
        .expect("terminal V2 token");
    let final_decoded =
        decode_programme_checkpoint(final_bytes.bytes, binding.clone(), file.clone())
            .expect("terminal outside decode");
    validate_programme_checkpoint_successor(decoded.clone(), final_decoded.clone())
        .expect("exact next snapshot");
    assert!(validate_programme_checkpoint_successor(final_decoded, decoded.clone()).is_err());
    let old_extent = first.presentation_extent().expect("original extent");
    mux.cancel().expect("cancel does not rewrite old tokens");
    let after = mux
        .encode_programme_checkpoint(first.clone(), binding, file, region_hash)
        .expect("immutable old token after cancel");
    assert_eq!(after.bytes, old.bytes);
    assert_eq!(
        first.presentation_extent().expect("same old extent"),
        old_extent
    );
    assert!(!decoded.info().facts.window_geometry_complete);
}

#[test]
fn programme_converter_receipts_and_output_apis_remain_strictly_separate() {
    let programme = programme_mux();
    assert_eq!(
        programme.begin_native_audio(0),
        Err(NativeRecordingError::InvalidConfig)
    );
    let converter = NativeRecordingMuxer::with_native_aac_timing(false, true, 0, 4096, 100)
        .expect("unchanged converter constructor");
    assert_eq!(
        converter.begin_programme_window(start()),
        Err(NativeRecordingError::InvalidConfig)
    );
    assert!(matches!(
        converter.take_programme_output(),
        Err(NativeRecordingError::InvalidConfig)
    ));
    assert_eq!(
        converter.finish_programme(NativeProgrammeCompletion::IncompletePrefix),
        Err(NativeRecordingError::InvalidConfig)
    );
    assert_eq!(
        programme.finish_native(NativeRecordingCompletion::IncompletePrefix),
        Err(NativeRecordingError::InvalidConfig)
    );
}

#[test]
fn programme_views_keep_reported_generation_and_original_recording_clock_consistent() {
    let codec = codec();
    let original = sample(&codec, 0);
    let views = original
        .aac_timing_views(7, -21_333, Some(56_200))
        .expect("caller-reported timing views");
    let recording = programme_mux();
    recording
        .begin_programme_window(start())
        .expect("original recording mapping");
    recording
        .push(views.recording_sample())
        .expect("original recording view accepted");
    assert_eq!(
        recording
            .stats()
            .expect("recording view count")
            .accepted_samples,
        1
    );
    let publishing = programme_mux();
    let mut shifted = start();
    shifted.coded_origin += 8;
    publishing
        .begin_programme_window(shifted)
        .expect("first clock alone would match publishing view");
    assert_eq!(
        publishing.push(views.publishing_sample().expect("publishing view")),
        Err(NativeRecordingError::InvalidMedia)
    );
    let wrong = original
        .aac_timing_views(8, -21_333, None)
        .expect("different reported generation");
    let generation = programme_mux();
    generation
        .begin_programme_window(start())
        .expect("exact supplied generation");
    assert_eq!(
        generation.push(wrong.recording_sample()),
        Err(NativeRecordingError::InvalidMedia)
    );
    assert_eq!(
        generation
            .stats()
            .expect("contradiction rejected before admission")
            .accepted_samples,
        0
    );
}
