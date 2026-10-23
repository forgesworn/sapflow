//! Portable checkpoint outside-input parser/semantic and successor invariants.
//! No FFI owner, media decoder, filesystem or durability is simulated.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_hls::recording_checkpoint::*;

fn scalar(data: &[u8], at: usize) -> u64 {
    let mut out = [0; 8];
    for (slot, byte) in out.iter_mut().zip(data.iter().skip(at)) {
        *slot = *byte;
    }
    u64::from_le_bytes(out)
}
fn decode_invariant(bytes: &[u8], expected: &CheckpointBinding) {
    if let Ok(value) = decode(bytes, expected) {
        assert_eq!(value.info.context.binding, *expected);
        let canonical = encode(value.as_ref()).expect("decoded facts encode canonically");
        assert_eq!(canonical, bytes);
    }
}
fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_CHECKPOINT_BYTES * 2 {
        return;
    }
    let binding = CheckpointBinding {
        recording: [7; 32],
        source_file: [9; 32],
    };
    decode_invariant(data, &binding);
    // Independently supplied bindings normally come from the platform owner.
    // Matching arbitrary encoded identities here reaches deeper parser states;
    // this is explicitly not an authority or platform-binding proof.
    if data.len() >= 96 {
        let mut expected = binding;
        expected.recording.copy_from_slice(&data[32..64]);
        expected.source_file.copy_from_slice(&data[64..96]);
        decode_invariant(data, &expected);
    }
    let selector = scalar(data, 0);
    let units = selector % 16;
    let video = selector & 16 != 0 || units == 0;
    let header_len = match selector % 4 {
        0 => 1,
        1 => 16,
        2 => MAX_HEADER_BYTES,
        _ => (scalar(data, 8) % 4096 + 1) as usize,
    };
    let mut header = vec![0x5a; header_len];
    for (at, b) in data.iter().take(header_len).enumerate() {
        header[at] = *b;
    }
    let start = scalar(data, 16) % 48_000;
    let leading = [0, 2112, 8191][(selector as usize >> 5) % 3];
    let mut info = CheckpointInfo {
        context: CheckpointContext {
            binding,
            media_epoch_ns: scalar(data, 24),
            initial_header_len: header_len as u32,
            initial_header_sha256: metadata_sha256(&header),
        },
        facts: CheckpointFacts {
            sequence: 1,
            audio_units: units,
            video_units: u64::from(video),
            audio_start: start,
            audio_end: start + units * 1024,
            audio_container_end: start + units * 1024,
            final_trailing_frames: 0,
            complete_audio_eof: false,
            video_start: video.then_some(0),
            video_end: video.then_some(3000),
            first_leading_frames: leading,
            completed_generation_count: 0,
        },
        committed_offset: header_len as u64 + 100,
        fragment_region_bytes: 100,
        fragment_region_sha256: [13; 32],
    };
    let bytes = encode(CheckpointRef {
        info: &info,
        header: &header,
    })
    .expect("structured incomplete prefix");
    decode_invariant(&bytes, &binding);
    if units * 1024 > u64::from(leading) {
        let mut complete = info;
        let available = units * 1024 - u64::from(leading);
        let tail = (scalar(data, 32) % 1024).min(available - 1) as u32;
        complete.facts.complete_audio_eof = true;
        complete.facts.completed_generation_count = 1;
        complete.facts.final_trailing_frames = tail;
        complete.facts.audio_container_end -= u64::from(tail);
        let closed = encode(CheckpointRef {
            info: &complete,
            header: &header,
        })
        .expect("structured proven EOF");
        decode_invariant(&closed, &binding);
        assert!(
            validate_successor(&complete, &info).is_err(),
            "complete EOF is terminal"
        );
    }
    // Structured arms cover MAX66080 even though current CI raw max_len is65536.
    let mut changed = bytes.clone();
    for (n, record) in data.chunks(3).take(64).enumerate() {
        let at = (usize::from(record[0]) + n * 257) % changed.len();
        changed[at] ^= record.get(1).copied().unwrap_or(1);
    }
    decode_invariant(&changed, &binding);
    let end = changed.len() - 32;
    let hash = metadata_sha256(&changed[..end]);
    changed[end..].copy_from_slice(&hash);
    decode_invariant(&changed, &binding);
    let prior = info;
    info.facts.sequence = 2;
    info.facts.audio_units += 1;
    info.facts.audio_end += 1024;
    info.facts.audio_container_end += 1024;
    info.committed_offset += 123;
    info.fragment_region_bytes += 123;
    validate_successor(&prior, &info).expect("actual structured next prefix");
    match selector % 8 {
        0 => info.facts.sequence = scalar(data, 8) as u32,
        1 => info.committed_offset = scalar(data, 8),
        2 => info.facts.audio_units = scalar(data, 8),
        3 => info.facts.completed_generation_count = scalar(data, 8),
        4 => info.facts.final_trailing_frames = scalar(data, 8) as u32,
        5 => info.facts.complete_audio_eof = true,
        6 => info.context.media_epoch_ns ^= scalar(data, 8),
        _ => info.facts.video_end = Some(scalar(data, 8)),
    }
    if validate_successor(&prior, &info).is_ok() {
        let encoded = encode(CheckpointRef {
            info: &info,
            header: &header,
        })
        .expect("accepted next facts encode");
        decode_invariant(&encoded, &binding);
    }
});
