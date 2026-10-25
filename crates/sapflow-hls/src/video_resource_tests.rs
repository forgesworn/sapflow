//! Typed admission/credit checks using the previously existing video API.
use super::*;

const NOW: SystemTime = SystemTime::UNIX_EPOCH;
const LIVE_VIDEO_LIMIT: usize = 16_384;

fn learning() -> Muxer {
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), None);
    muxer
        .push_video(0, 0, true, Bytes::new(), NOW)
        .expect("pending IDR");
    muxer
}

fn serving() -> Muxer {
    let mut muxer = Muxer::new(MuxerConfig::default(), tests::video(), None);
    tests::feed(&mut muxer, 0..32, 30);
    assert!(muxer.timing.is_some() && muxer.has_content());
    muxer
}

fn live(muxer: &Muxer) -> usize {
    muxer.learning.len()
        + muxer.part.as_ref().map_or(0, |part| part.video.len())
        + usize::from(muxer.pending_video.is_some())
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    learning: usize,
    learning_capacity: usize,
    part: usize,
    part_capacity: usize,
    pending: Option<(i64, Bytes)>,
    segment: Option<(u64, i64, usize)>,
    offset: i64,
    rebase_to: Option<i64>,
    audio_floor: i64,
    next_part_id: u64,
    breaks: u64,
}

fn snapshot(muxer: &Muxer) -> Snapshot {
    Snapshot {
        learning: muxer.learning.len(),
        learning_capacity: muxer.learning.capacity(),
        part: muxer.part.as_ref().map_or(0, |part| part.video.len()),
        part_capacity: muxer.part.as_ref().map_or(0, |part| part.video.capacity()),
        pending: muxer
            .pending_video
            .as_ref()
            .map(|frame| (frame.dts, frame.data.clone())),
        segment: muxer
            .next_segment
            .as_ref()
            .map(|seg| (seg.id, seg.start_dts, seg.bytes)),
        offset: muxer.offset,
        rebase_to: muxer.rebase_to,
        audio_floor: muxer.audio_floor,
        next_part_id: muxer.next_part_id,
        breaks: muxer.breaks,
    }
}

fn fill_to(muxer: &mut Muxer, entries: usize) {
    while live(muxer) < entries {
        let at = muxer.pending_video.as_ref().expect("pending frame").dts;
        muxer
            .push_video(at, at, false, Bytes::new(), NOW)
            .expect("within exact entry allowance");
    }
    assert_eq!(live(muxer), entries);
}

fn exact_refusal(mut muxer: Muxer) {
    fill_to(&mut muxer, LIVE_VIDEO_LIMIT);
    let at = muxer.pending_video.as_ref().expect("pending frame").dts;
    let before = snapshot(&muxer);
    assert!(matches!(
        muxer.push_video(at, at, false, Bytes::from_static(&[1]), NOW),
        Err(MuxError::SegmentTooLarge)
    ));
    assert_eq!(
        snapshot(&muxer),
        before,
        "refusal is before any state mutation"
    );
    assert_eq!(
        muxer
            .push_video(-1, -1, false, Bytes::new(), NOW)
            .expect("existing negative-time drop"),
        Update::default()
    );
    assert_eq!(
        snapshot(&muxer),
        before,
        "ignored input does not consume credit"
    );
}

#[test]
fn typed_learning_exact_entry_limit_and_one_over_preserve_state() {
    exact_refusal(learning());
}

#[test]
fn typed_serving_exact_entry_limit_and_one_over_preserve_state() {
    exact_refusal(serving());
}

#[test]
fn real_video_part_rotation_releases_live_entry_credit() {
    let mut muxer = serving();
    fill_to(&mut muxer, LIVE_VIDEO_LIMIT - 1);
    let next =
        muxer.part.as_ref().expect("part").start_dts + muxer.timing.expect("chosen timing").target;
    let update = muxer
        .push_video(next, next, false, Bytes::new(), NOW)
        .expect("real cut within entry allowance");
    assert!(update.part);
    assert_eq!(live(&muxer), 1, "only the new pending frame remains live");
    muxer
        .push_video(next + 3000, next + 3000, false, Bytes::new(), NOW)
        .expect("credit available after rotation");
    assert_eq!(live(&muxer), 2);
}

#[test]
fn existing_gap_interrupt_and_reconfigure_release_live_entry_credit() {
    let mut muxer = learning();
    fill_to(&mut muxer, LIVE_VIDEO_LIMIT);
    let gap = muxer.pending_video.as_ref().expect("pending frame").dts + BREAK_GAP + 1;
    muxer
        .push_video(gap, gap, true, Bytes::new(), NOW)
        .expect("existing real-gap IDR recovery at full budget");
    assert_eq!(live(&muxer), 1);
    assert!(muxer.learning.is_empty());
    fill_to(&mut muxer, LIVE_VIDEO_LIMIT);
    muxer.interrupt(NOW);
    assert_eq!(live(&muxer), 0);
    assert_eq!(
        muxer
            .push_video(gap + 1, gap + 1, false, Bytes::new(), NOW)
            .expect("existing wait-for-IDR drop"),
        Update::default()
    );
    assert_eq!(live(&muxer), 0);
    muxer
        .push_video(gap + 2, gap + 2, true, Bytes::new(), NOW)
        .expect("fresh IDR after interrupt");
    assert_eq!(live(&muxer), 1);
    muxer.reconfigure(tests::video(), None, NOW);
    assert_eq!(live(&muxer), 0);
    muxer
        .push_video(gap + 3, gap + 3, true, Bytes::new(), NOW)
        .expect("fresh IDR after reconfigure");
    assert_eq!(live(&muxer), 1);
}
