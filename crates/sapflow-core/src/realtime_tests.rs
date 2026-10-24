use super::*;

fn queue(bytes: u32, frames: u32) -> Queue {
    Queue::new(Limits::new(bytes, frames, 8, 1_000).expect("limits"))
}
fn frame(id: u32, stream: u32, kind: Kind, sequence: u32, bytes: u32) -> Frame {
    Frame {
        id,
        stream,
        kind,
        sequence,
        bytes,
        deadline_us: 10_000,
    }
}
fn offer(q: &mut Queue, f: Frame, now: u64) -> Offered {
    q.offer(f, now).expect("forward clock and valid metadata")
}

#[test]
fn validates_all_resource_bounds() {
    for args in [
        (0, 1, 1, 1),
        (16 * 1024 * 1024 + 1, 1, 1, 1),
        (1, 0, 1, 1),
        (1, 4097, 1, 1),
        (1, 1, 0, 1),
        (1, 1, 257, 1),
        (1, 1, 1, 0),
        (1, 1, 1, 60_000_001),
    ] {
        assert_eq!(
            Limits::new(args.0, args.1, args.2, args.3),
            Err(Error::Limits)
        );
    }
    assert!(Limits::new(16 * 1024 * 1024, 4096, 256, 60_000_000).is_ok());
}

#[test]
fn audio_is_popped_before_video_without_reordering_either_track() {
    let mut q = queue(100, 8);
    for f in [
        frame(1, 1, Kind::VideoKey, 0, 10),
        frame(2, 1, Kind::VideoDelta, 1, 10),
        frame(3, 2, Kind::Audio, 0, 10),
        frame(4, 2, Kind::Audio, 1, 10),
    ] {
        assert!(offer(&mut q, f, 0).accepted);
    }
    let ids: Vec<_> = (0..4)
        .map(|_| q.pop(0).expect("clock").frame.expect("queued").id)
        .collect();
    assert_eq!(ids, [3, 4, 1, 2]);
    assert_eq!((q.bytes(), q.frames()), (0, 0));
}

#[test]
fn initial_delta_waits_for_keyframe_and_other_stream_remains_independent() {
    let mut q = queue(100, 8);
    let delta = offer(&mut q, frame(1, 1, Kind::VideoDelta, 0, 10), 0);
    assert!(!delta.accepted && delta.waiting_for_keyframe);
    assert!(offer(&mut q, frame(2, 2, Kind::Audio, 0, 10), 0).accepted);
    assert!(offer(&mut q, frame(3, 1, Kind::VideoKey, 1, 10), 0).accepted);
    assert!(offer(&mut q, frame(4, 1, Kind::VideoDelta, 2, 10), 0).accepted);
    assert!(!q.waiting_for_keyframe(1));
}

#[test]
fn video_gap_flushes_only_that_stream_and_recovers_at_later_key() {
    let mut q = queue(100, 8);
    assert!(offer(&mut q, frame(1, 1, Kind::VideoKey, 10, 10), 0).accepted);
    assert!(offer(&mut q, frame(2, 2, Kind::VideoKey, 0, 10), 0).accepted);
    let missing = offer(&mut q, frame(3, 1, Kind::VideoDelta, 12, 10), 0);
    assert!(!missing.accepted && missing.waiting_for_keyframe);
    assert_eq!(missing.changes.discarded, [1]);
    assert_eq!(q.frames(), 1);
    let reordered = offer(&mut q, frame(4, 1, Kind::VideoDelta, 11, 10), 0);
    assert!(!reordered.accepted);
    assert!(offer(&mut q, frame(5, 1, Kind::VideoKey, 13, 10), 0).accepted);
}

#[test]
fn reordered_duplicate_frame_does_not_break_a_valid_video_chain() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::VideoKey, 7, 10), 0);
    offer(&mut q, frame(2, 1, Kind::VideoDelta, 8, 10), 0);
    let old = offer(&mut q, frame(3, 1, Kind::VideoDelta, 7, 10), 0);
    assert!(!old.accepted);
    assert!(old.changes.discarded.is_empty());
    assert!(!q.waiting_for_keyframe(1));
    assert!(offer(&mut q, frame(4, 1, Kind::VideoDelta, 9, 10), 0).accepted);
}

#[test]
fn audio_loss_does_not_require_contiguous_sequence() {
    let mut q = queue(100, 8);
    assert!(offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0).accepted);
    assert!(offer(&mut q, frame(2, 1, Kind::Audio, 7, 10), 0).accepted);
    assert!(!offer(&mut q, frame(3, 1, Kind::Audio, 6, 10), 0).accepted);
    assert_eq!(q.frames(), 2);
}

#[test]
fn frame_deadlines_are_independent_of_enqueue_order_and_expire_on_tick() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0);
    let mut soon = frame(2, 1, Kind::Audio, 1, 10);
    soon.deadline_us = 5;
    offer(&mut q, soon, 0);
    assert_eq!(q.tick(5).expect("clock").discarded, [2]);
    assert_eq!(q.pop(5).expect("clock").frame.expect("audio").id, 1);
}

#[test]
fn expired_video_invalidates_dependants_but_not_audio_or_other_video() {
    let mut q = queue(100, 8);
    let mut key = frame(1, 1, Kind::VideoKey, 0, 10);
    key.deadline_us = 5;
    offer(&mut q, key, 0);
    offer(&mut q, frame(2, 1, Kind::VideoDelta, 1, 10), 0);
    offer(&mut q, frame(3, 2, Kind::Audio, 0, 10), 0);
    offer(&mut q, frame(4, 3, Kind::VideoKey, 0, 10), 0);
    let mut discarded = q.tick(5).expect("clock").discarded;
    discarded.sort_unstable();
    assert_eq!(discarded, [1, 2]);
    assert!(q.waiting_for_keyframe(1));
    assert!(!q.waiting_for_keyframe(3));
    assert_eq!((q.frames(), q.bytes()), (2, 20));
}

#[test]
fn residence_limit_applies_when_capture_stops_and_deadline_is_far_away() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 10);
    assert!(q.tick(1009).expect("clock").discarded.is_empty());
    assert_eq!(q.tick(1010).expect("clock").discarded, [1]);
    assert!(q.pop(1010).expect("clock").frame.is_none());
}

#[test]
fn rejected_expired_or_oversized_video_breaks_previous_dependencies() {
    for expired in [false, true] {
        let mut q = queue(100, 8);
        offer(&mut q, frame(1, 1, Kind::VideoKey, 0, 10), 10);
        let mut incoming = frame(2, 1, Kind::VideoDelta, 1, if expired { 10 } else { 101 });
        if expired {
            incoming.deadline_us = 10;
        }
        let result = offer(&mut q, incoming, 10);
        assert!(!result.accepted && result.waiting_for_keyframe);
        assert_eq!(result.changes.discarded, [1]);
    }
}

#[test]
fn video_never_evicts_audio_under_byte_pressure() {
    let mut q = queue(20, 8);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 15), 0);
    let result = offer(&mut q, frame(2, 2, Kind::VideoKey, 0, 10), 0);
    assert!(!result.accepted);
    assert!(result.changes.discarded.is_empty());
    assert_eq!(q.pop(0).expect("clock").frame.expect("audio").id, 1);
}

#[test]
fn audio_evicts_video_before_any_older_audio() {
    let mut q = queue(30, 8);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0);
    offer(&mut q, frame(2, 2, Kind::VideoKey, 0, 10), 0);
    offer(&mut q, frame(3, 2, Kind::VideoDelta, 1, 10), 0);
    let result = offer(&mut q, frame(4, 1, Kind::Audio, 1, 10), 0);
    assert!(result.accepted);
    let mut discarded = result.changes.discarded;
    discarded.sort_unstable();
    assert_eq!(discarded, [2, 3]);
    assert_eq!((q.frames(), q.bytes()), (2, 20));
}

#[test]
fn latest_audio_replaces_stale_backlog_at_frame_limit() {
    let mut q = queue(100, 2);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0);
    offer(&mut q, frame(2, 1, Kind::Audio, 1, 10), 0);
    let result = offer(&mut q, frame(3, 1, Kind::Audio, 2, 10), 0);
    assert!(result.accepted);
    assert_eq!(result.changes.discarded, [1]);
    assert_eq!(q.frames(), 2);
}

#[test]
fn video_overflow_requires_keyframe_instead_of_sending_broken_delta() {
    let mut q = queue(20, 8);
    offer(&mut q, frame(1, 1, Kind::VideoKey, 0, 10), 0);
    offer(&mut q, frame(2, 1, Kind::VideoDelta, 1, 10), 0);
    let result = offer(&mut q, frame(3, 1, Kind::VideoDelta, 2, 10), 0);
    assert!(!result.accepted && result.waiting_for_keyframe);
    assert_eq!(q.frames(), 0);
    assert!(offer(&mut q, frame(4, 1, Kind::VideoKey, 3, 20), 0).accepted);
}

#[test]
fn stream_kind_is_fixed_and_stream_lifetimes_stay_bounded_after_drain() {
    let mut q = Queue::new(Limits::new(100, 8, 1, 1000).expect("limits"));
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0);
    q.pop(0).expect("clock");
    assert_eq!(
        q.offer(frame(2, 1, Kind::VideoKey, 1, 10), 0),
        Err(Error::StreamKind)
    );
    assert_eq!(
        q.offer(frame(3, 2, Kind::Audio, 0, 10), 0),
        Err(Error::Streams)
    );
    assert_eq!(q.streams(), 1);
}

#[test]
fn invalid_scalar_and_duplicate_id_do_not_admit_or_discard_other_frames() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::Audio, 0, 10), 0);
    assert_eq!(
        q.offer(frame(1, 1, Kind::Audio, 1, 10), 0),
        Err(Error::DuplicateId)
    );
    for f in [
        frame(0, 1, Kind::Audio, 1, 10),
        frame(2, 0, Kind::Audio, 1, 10),
        frame(2, 1, Kind::Audio, 1, 0),
    ] {
        assert_eq!(q.offer(f, 0), Err(Error::Frame));
    }
    assert_eq!((q.frames(), q.bytes()), (1, 10));
}

#[test]
fn backwards_clock_is_rejected_and_sequence_cannot_wrap() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::VideoKey, u32::MAX, 10), 10);
    assert_eq!(q.pop(9), Err(Error::Clock));
    assert_eq!(q.tick(9), Err(Error::Clock));
    assert_eq!(
        q.offer(frame(2, 1, Kind::VideoDelta, 0, 10), 9),
        Err(Error::Clock)
    );
    assert!(!offer(&mut q, frame(3, 1, Kind::VideoKey, 0, 10), 10).accepted);
    assert_eq!(q.frames(), 1);
}

#[test]
fn close_releases_payloads_and_cannot_be_reopened() {
    let mut q = queue(100, 8);
    offer(&mut q, frame(1, 1, Kind::VideoKey, 0, 10), 0);
    offer(&mut q, frame(2, 2, Kind::Audio, 0, 10), 0);
    assert_eq!(q.close().discarded, [1, 2]);
    assert_eq!((q.frames(), q.bytes(), q.streams()), (0, 0, 0));
    assert_eq!(
        q.offer(frame(3, 1, Kind::VideoKey, 1, 10), 0),
        Err(Error::Closed)
    );
    assert!(q.close().discarded.is_empty());
}
