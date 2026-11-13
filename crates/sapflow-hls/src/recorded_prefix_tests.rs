//! [`fmp4::recorded_prefix`] against recording files built as the recorder
//! builds them, cut where a killed writer or a lost disk tail would cut them.

use std::io::Cursor;
use std::time::{Duration, SystemTime};

use crate::fmp4::{self, RecordedPrefix, rebase_part_for_file_timed};
use crate::{MuxerConfig, PartLookup, TsIngest};

struct Recording {
    bytes: Vec<u8>,
    audio_rate: Option<u32>,
    /// Where each fragment ends in the file, with the writer's duration so far.
    ends: Vec<(u64, Duration)>,
}

fn recording() -> Recording {
    let mut ingest = TsIngest::new(MuxerConfig::default());
    for bytes in include_bytes!("../tests/fixtures/testsrc-3s.ts").chunks(1316) {
        ingest
            .push(bytes, SystemTime::UNIX_EPOCH)
            .expect("generated fixture");
    }
    let mut muxer = ingest.take_muxer().expect("muxer");
    muxer.interrupt(SystemTime::UNIX_EPOCH);
    let audio_rate = muxer.audio().map(|audio| audio.sample_rate);
    let mut bytes = fmp4::init_segment_for_tracks(muxer.video(), muxer.audio(), Duration::ZERO)
        .expect("init")
        .to_vec();
    let mut origin = None;
    let mut duration = Duration::ZERO;
    let mut ends = Vec::new();
    for id in 0..muxer.next_part_id() {
        let PartLookup::Found(part) = muxer.part(id) else {
            panic!("generated part retained");
        };
        let (output, end) =
            rebase_part_for_file_timed(&part.data, audio_rate, &mut origin).expect("rebase");
        bytes.extend_from_slice(&output);
        duration = duration.max(end);
        ends.push((bytes.len() as u64, duration));
    }
    assert!(ends.len() > 3, "several fragments");
    Recording {
        bytes,
        audio_rate,
        ends,
    }
}

fn prefix(bytes: &[u8], audio_rate: Option<u32>) -> RecordedPrefix {
    fmp4::recorded_prefix(&mut Cursor::new(bytes), bytes.len() as u64, audio_rate)
        .expect("in-memory read")
}

#[test]
fn a_whole_file_measures_as_the_writer_measured_it() {
    let recording = recording();
    let (end, duration) = *recording.ends.last().expect("fragments");
    assert_eq!(
        prefix(&recording.bytes, recording.audio_rate),
        RecordedPrefix {
            fragments: recording.ends.len() as u64,
            bytes: end,
            media_end: duration,
        }
    );
    assert!(duration > Duration::from_secs(2), "{duration:?}");
}

#[test]
fn a_cut_anywhere_keeps_exactly_the_whole_fragments_before_it() {
    let recording = recording();
    // Every byte around each fragment boundary, and a spread elsewhere.
    let mut cuts: Vec<usize> = (0..=recording.bytes.len()).step_by(997).collect();
    for &(end, _) in &recording.ends {
        let end = end as usize;
        cuts.extend(end.saturating_sub(40)..=(end + 40).min(recording.bytes.len()));
    }
    for cut in cuts {
        let got = prefix(&recording.bytes[..cut], recording.audio_rate);
        let whole = recording
            .ends
            .iter()
            .filter(|(end, _)| *end <= cut as u64)
            .count();
        let expected = recording.ends[..whole]
            .last()
            .map(|&(bytes, media_end)| RecordedPrefix {
                fragments: whole as u64,
                bytes,
                media_end,
            })
            .unwrap_or_default();
        assert_eq!(got, expected, "cut at {cut}");
    }
}

#[test]
fn a_zero_filled_or_damaged_tail_ends_the_prefix() {
    let recording = recording();
    let (keep, duration) = recording.ends[1];
    let mut bytes = recording.bytes[..keep as usize].to_vec();
    // A power cut can leave the file's length with no data written to it.
    bytes.resize(bytes.len() + 64 * 1024, 0);
    let got = prefix(&bytes, recording.audio_rate);
    assert_eq!(
        (got.fragments, got.bytes, got.media_end),
        (2, keep, duration)
    );
    // Damage inside the third fragment's moof stops there, too.
    let mut damaged = recording.bytes.clone();
    damaged[keep as usize + 9] ^= 0xff;
    assert_eq!(prefix(&damaged, recording.audio_rate).fragments, 2);
}

#[test]
fn what_is_not_one_of_our_recordings_has_no_prefix() {
    let recording = recording();
    for bytes in [
        Vec::new(),
        vec![0; 4096],
        b"not an mp4 at all".to_vec(),
        recording.bytes[8..].to_vec(),
    ] {
        assert_eq!(prefix(&bytes, recording.audio_rate).fragments, 0);
    }
}
