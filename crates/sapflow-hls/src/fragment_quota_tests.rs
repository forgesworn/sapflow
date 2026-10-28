use super::tests::{aac, feed, video};
use super::*;

fn bounded(quota: &Arc<FragmentQuota>) -> Muxer {
    Muxer::with_fragment_quota(
        MuxerConfig {
            segment_count: 8,
            ..MuxerConfig::default()
        },
        Some(video()),
        Some(aac()),
        quota.clone(),
    )
    .expect("bounded fixture")
}

#[test]
fn init_capacity_is_shared_across_muxers_and_survives_a_response_alias() {
    let probe = Arc::new(FragmentQuota::new(usize::MAX));
    let muxer = bounded(&probe);
    let capacity = probe.used();
    assert!(capacity >= muxer.init().len());
    drop(muxer);
    assert_eq!(probe.used(), 0);
    let quota = Arc::new(FragmentQuota::new(capacity * 2));
    let first = bounded(&quota);
    let second = bounded(&quota);
    assert_eq!(quota.used(), capacity * 2);
    assert!(matches!(
        Muxer::with_fragment_quota(
            MuxerConfig::default(),
            Some(video()),
            Some(aac()),
            quota.clone()
        ),
        Err(MuxError::FragmentLimit)
    ));
    let alias = first.init().slice(1..);
    drop(first);
    assert_eq!(quota.used(), capacity * 2);
    drop(alias);
    assert_eq!(quota.used(), capacity);
    drop(second);
    assert_eq!(quota.used(), 0);
}

#[test]
fn evicted_fragment_bytes_remain_charged_through_segment_and_slice_owners() {
    let quota = Arc::new(FragmentQuota::new(1024 * 1024));
    let mut muxer = bounded(&quota);
    feed(&mut muxer, 0..180, 30);
    let segment = muxer.segment(7).expect("first segment");
    let part_id = segment.parts[0].id;
    let metadata = Arc::downgrade(&segment);
    let body = segment.data.clone();
    let suffix = segment.parts[0].data.slice(1..);
    let before = quota.used();
    let copy = body.clone();
    assert_eq!(quota.used(), before, "aliases do not charge again");
    drop(copy);
    drop(segment);
    feed(&mut muxer, 180..480, 30);
    assert!(muxer.segment(7).is_none());
    assert!(matches!(muxer.part(part_id), PartLookup::Missing));
    assert!(metadata.upgrade().is_none());
    drop(muxer);
    assert!(quota.used() >= body.len());
    drop(body);
    assert!(
        quota.used() > suffix.len(),
        "one-byte offset retains the whole encoded allocation"
    );
    drop(suffix);
    assert_eq!(quota.used(), 0);
}

#[test]
fn exhaustion_seals_only_the_accepted_prefix_and_new_ingest_can_recover() {
    let maximum = 1024 * 1024;
    let quota = Arc::new(FragmentQuota::new(maximum));
    let mut muxer = bounded(&quota);
    feed(&mut muxer, 0..70, 30);
    let accepted = muxer.next_part_id();
    let blocker = crate::fragment::retain(vec![0; maximum - quota.used()], Some(&quota))
        .expect("competing backing");
    let mut refused = false;
    for frame in 70..100 {
        match muxer.push_video(
            frame * 3000,
            frame * 3000,
            frame % 30 == 0,
            Bytes::from_static(b"generated"),
            SystemTime::UNIX_EPOCH,
        ) {
            Ok(_) => {}
            Err(MuxError::FragmentLimit) => {
                refused = true;
                break;
            }
            error => panic!("unexpected admission: {error:?}"),
        }
    }
    assert!(refused);
    assert_eq!(
        muxer.next_part_id(),
        accepted,
        "refused part has no published identity"
    );
    let update = muxer.interrupt(SystemTime::UNIX_EPOCH);
    assert!(update.media_loss);
    for entry in muxer.entries() {
        if let Entry::Segment(segment) = entry {
            assert_eq!(
                segment.duration,
                segment.parts.iter().map(|part| part.span).sum::<Duration>(),
                "refused suffix must not stretch accepted segment duration"
            );
        }
    }
    assert!(matches!(
        muxer.push_video(
            0,
            0,
            true,
            Bytes::from_static(b"suffix"),
            SystemTime::UNIX_EPOCH
        ),
        Err(MuxError::FragmentLimit)
    ));
    drop(blocker);
    muxer.resume_fragment_admission();
    feed(&mut muxer, 0..60, 30);
    assert!(muxer.next_part_id() > accepted);
    drop(muxer);
    assert_eq!(quota.used(), 0);
}

#[test]
fn codec_replacement_refusal_keeps_the_old_init_and_all_its_aliases_charged() {
    let maximum = 1024 * 1024;
    let quota = Arc::new(FragmentQuota::new(maximum));
    let mut muxer = bounded(&quota);
    feed(&mut muxer, 0..70, 30);
    let old_init = muxer.init().clone();
    let old_id = muxer.current_initialization_id();
    let blocker = crate::fragment::retain(vec![0; maximum - quota.used()], Some(&quota))
        .expect("competing backing");
    assert!(matches!(
        muxer.reconfigure_tracks(Some(video()), None, SystemTime::UNIX_EPOCH),
        Err(MuxError::FragmentLimit)
    ));
    assert_eq!(muxer.current_initialization_id(), old_id);
    assert_eq!(muxer.init(), &old_init);
    assert_eq!(quota.used(), maximum);
    assert!(muxer.interrupt(SystemTime::UNIX_EPOCH).media_loss);
    drop(blocker);
    muxer.resume_fragment_admission();
    muxer
        .reconfigure_tracks(Some(video()), None, SystemTime::UNIX_EPOCH)
        .expect("replacement after recovery");
    assert_eq!(muxer.current_initialization_id(), old_id + 1);
    drop(muxer);
    assert!(quota.used() >= old_init.len());
    drop(old_init);
    assert_eq!(quota.used(), 0);
}

#[test]
fn enough_room_for_new_init_does_not_hide_old_tail_fragment_refusal() {
    let probe = Arc::new(FragmentQuota::new(usize::MAX));
    let video_only =
        Muxer::with_fragment_quota(MuxerConfig::default(), Some(video()), None, probe.clone())
            .expect("probe init");
    let init_capacity = probe.used();
    drop(video_only);
    let maximum = 1024 * 1024;
    let quota = Arc::new(FragmentQuota::new(maximum));
    let mut muxer = bounded(&quota);
    feed(&mut muxer, 0..70, 30);
    let old_init = muxer.init().clone();
    let old_id = muxer.current_initialization_id();
    let blocker = quota
        .try_acquire(maximum - quota.used() - init_capacity)
        .expect("leave room only for new initialization");
    assert!(
        matches!(
            muxer.reconfigure_tracks(Some(video()), None, SystemTime::UNIX_EPOCH),
            Err(MuxError::FragmentLimit)
        ),
        "old tail refusal must be returned before installing the successor init"
    );
    assert_eq!(muxer.current_initialization_id(), old_id);
    assert_eq!(muxer.init(), &old_init);
    assert_eq!(muxer.audio(), Some(&aac()));
    assert_eq!(
        quota.used(),
        maximum - init_capacity,
        "unused successor init releases its reservation"
    );
    assert!(muxer.interrupt(SystemTime::UNIX_EPOCH).media_loss);
    drop(blocker);
    drop(old_init);
    drop(muxer);
    assert_eq!(quota.used(), 0);
}
