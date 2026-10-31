//! A transport gap the TS continuity counters cannot see.
use super::*;
use crate::muxer::Entry;

const NOW: SystemTime = SystemTime::UNIX_EPOCH;

fn samples(ingest: &TsIngest) -> usize {
    use mp4_atom::Decode;
    ingest.muxer().map_or(0, |muxer| {
        muxer
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Segment(s) => Some(s),
                _ => None,
            })
            .flat_map(|s| &s.parts)
            .map(|part| {
                let moof = mp4_atom::Moof::decode(&mut std::io::Cursor::new(&part.data[..]))
                    .expect("actual MP4 moof");
                moof.traf
                    .iter()
                    .flat_map(|t| &t.trun)
                    .map(|r| r.entries.len())
                    .sum::<usize>()
            })
            .sum()
    })
}

/// The generated A/V fixture with 16 consecutive video-PID packets removed,
/// starting at a frame's first packet mid-GOP: the counters line up again.
/// Returns the packets before the gap and those after it.
fn wrapped_gap() -> (Vec<u8>, Vec<u8>) {
    let fixture = include_bytes!("../tests/fixtures/testsrc-3s.ts");
    let packets: Vec<&[u8]> = fixture.chunks_exact(TS_SIZE).collect();
    let pid = |p: &[u8]| (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
    let video: Vec<usize> = (0..packets.len())
        .filter(|&i| pid(packets[i]) == 256)
        .collect();
    // The second frame start after a third of the stream: mid-GOP.
    let start = (0..video.len())
        .filter(|&v| video[v] > packets.len() / 3 && packets[video[v]][1] & 0x40 != 0)
        .nth(1)
        .expect("mid-stream video frame start");
    let removed: Vec<usize> = video[start..start + 16].to_vec();
    let cut = removed[0];
    let keep = |range: std::ops::Range<usize>| -> Vec<u8> {
        range
            .filter(|i| !removed.contains(i))
            .flat_map(|i| packets[i].to_vec())
            .collect()
    };
    (keep(0..cut), keep(cut..packets.len()))
}

#[test]
fn a_gap_of_sixteen_packets_on_one_pid_is_invisible_without_the_transport_signal() {
    let (before, after) = wrapped_gap();
    let mut blind = TsIngest::new(MuxerConfig::default());
    blind.push(&before, NOW).expect("prefix");
    blind.push(&after, NOW).expect("suffix");
    blind.finish(NOW).expect("finish");
    assert_eq!(blind.dropped_packets(), 0, "continuity counters wrapped");

    let mut told = TsIngest::new(MuxerConfig::default());
    told.push(&before, NOW).expect("prefix");
    told.transport_gap(NOW);
    told.push(&after, NOW).expect("suffix");
    let update = told.finish(NOW).expect("finish");
    assert!(told.dropped_packets() > 0 && update.media_loss);
    // Video waits for the next keyframe instead of carrying a merged frame.
    assert!(
        samples(&told) < samples(&blind),
        "{} samples with the signal, {} without",
        samples(&told),
        samples(&blind)
    );
    assert!(
        samples(&told) > 0,
        "media before the gap and after the keyframe"
    );
}

#[test]
fn a_transport_gap_after_finish_or_a_failed_parser_changes_nothing() {
    let (before, _) = wrapped_gap();
    let mut ingest = TsIngest::new(MuxerConfig::default());
    ingest.push(&before, NOW).expect("prefix");
    ingest.finish(NOW).expect("finish");
    let dropped = ingest.dropped_packets();
    ingest.transport_gap(NOW);
    assert_eq!(ingest.dropped_packets(), dropped);
}

fn track_frames(ingest: &TsIngest, track_id: u32) -> usize {
    use mp4_atom::Decode;
    let muxer = ingest.muxer().expect("muxer");
    (0..muxer.next_part_id())
        .filter_map(|id| match muxer.part(id) {
            crate::PartLookup::Found(part) => Some(part),
            _ => None,
        })
        .map(|part| {
            let moof = mp4_atom::Moof::decode(&mut std::io::Cursor::new(&part.data[..]))
                .expect("actual MP4 moof");
            moof.traf
                .iter()
                .filter(|t| t.tfhd.track_id == track_id)
                .flat_map(|t| &t.trun)
                .map(|r| r.entries.len())
                .sum::<usize>()
        })
        .sum()
}

/// 16 video-PID packets lost from the start of the keyframe at 2 s, so
/// video waits for the keyframe at 3 s, a gap of just over a second. The
/// signal's keyframe wait holds the last good frame, as a continuity jump's
/// does: no break, and the audio under the gap is kept.
#[test]
fn a_signalled_gap_holds_the_last_frame_and_keeps_the_audio() {
    let fixture = include_bytes!("../tests/fixtures/transport-loss-6s.ts");
    let packets: Vec<&[u8]> = fixture.chunks_exact(TS_SIZE).collect();
    let video = |p: &[u8]| ((u16::from(p[1] & 0x1f) << 8) | u16::from(p[2])) == 256;
    let keyframe = |p: &[u8]| video(p) && p[1] & 0x40 != 0 && p[3] & 0x20 != 0 && p[5] & 0x40 != 0;
    let keyframes: Vec<usize> = (0..packets.len())
        .filter(|&i| keyframe(packets[i]))
        .collect();
    let removed: Vec<usize> = (keyframes[2]..packets.len())
        .filter(|&i| video(packets[i]))
        .take(16)
        .collect();
    assert!(*removed.last().expect("16 packets") < keyframes[3]);
    let cut = removed[0];
    let keep = |range: std::ops::Range<usize>| -> Vec<u8> {
        range
            .filter(|i| !removed.contains(i))
            .flat_map(|i| packets[i].to_vec())
            .collect()
    };
    let mut clean = TsIngest::new(MuxerConfig::default());
    clean.push(fixture, NOW).expect("clean");
    let mut told = TsIngest::new(MuxerConfig::default());
    told.push(&keep(0..cut), NOW).expect("prefix");
    told.transport_gap(NOW);
    told.push(&keep(cut..packets.len()), NOW).expect("suffix");
    assert!(told.dropped_packets() > 0);
    assert_eq!(told.muxer().expect("muxer").breaks(), 0);
    assert!(track_frames(&told, 1) + 25 < track_frames(&clean, 1));
    assert_eq!(track_frames(&told, 2), track_frames(&clean, 2));
}
