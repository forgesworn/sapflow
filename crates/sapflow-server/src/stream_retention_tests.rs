//! Generated-media regressions for presentation and response ownership.

use super::*;
use crate::media_body::MediaBody;
use crate::test_support::AUDIO;

fn path(id: usize) -> StreamPath {
    StreamPath::parse(&format!("live/event-retained-{id}")).expect("generated path")
}

fn attach(registry: &Registry, id: usize) -> Result<(Arc<LiveStream>, u64, Claim), ClaimError> {
    registry.claim_configured(
        &path(id),
        MuxerConfig {
            segment_count: 8,
            ..MuxerConfig::default()
        },
        SourceKind::Ts,
        true,
        1,
        |_, _| {},
    )
}

fn feed(stream: &LiveStream) {
    push(stream, AUDIO);
}

fn push(stream: &LiveStream, bytes: &[u8]) {
    stream
        .apply(|source, now| match source {
            Source::Ts(ts) => ts.push(bytes, now),
            _ => unreachable!("TS fixture"),
        })
        .expect("generated sine media");
    assert!(stream.has_content());
}

fn gather(body: &MediaBody) -> Vec<u8> {
    let mut slices = [std::io::IoSlice::new(&[]); 128];
    let used = body.slices(&mut slices);
    slices[..used]
        .iter()
        .flat_map(|slice| slice.iter().copied())
        .collect()
}

#[test]
fn encoded_backing_cannot_publish_beyond_the_shared_byte_limit() {
    let registry = Registry::with_limits(4, 1);
    let (stream, _, _) = attach(&registry, 0).expect("presentation admission is separate");
    let result = stream.apply(|source, now| match source {
        Source::Ts(ts) => ts.push(AUDIO, now),
        _ => unreachable!(),
    });
    assert!(
        matches!(result, Err(IngestError::FragmentLimit)),
        "one byte cannot admit the encoded init and fragments"
    );
    assert_eq!(
        registry.fragments.used(),
        0,
        "refused storage is not published"
    );
    assert!(!stream.has_content());
}

#[test]
fn flv_initialization_uses_the_same_shared_fragment_admission() {
    let registry = Registry::with_limits(4, 1);
    let (stream, _, _) = registry
        .claim_configured(
            &path(0),
            MuxerConfig::default(),
            SourceKind::Flv,
            true,
            1,
            |_, _| {},
        )
        .expect("RTMP presentation");
    let result = stream.apply(|source, now| {
        let Source::Flv(flv) = source else {
            unreachable!()
        };
        flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now)?;
        flv.audio_at(0, &[0xaf, 0, 0x11, 0x88], now)?;
        // Admission refuses the init before any structural sample is published.
        flv.audio_at(0, &[0xaf, 1, 1, 2, 3], now)
    });
    assert!(matches!(result, Err(IngestError::FragmentLimit)));
    assert_eq!(registry.fragments.used(), 0);
    assert!(!stream.has_content());
}

#[test]
fn exact_shared_backing_capacity_refuses_another_path_and_recovers_after_close() {
    let probe = Registry::new(4);
    let (stream, _, _) = attach(&probe, 0).expect("probe");
    feed(&stream);
    let capacity = probe.fragments.used();
    assert!(capacity > 0);
    drop(stream);
    drop(probe);

    let registry = Registry::with_limits(4, capacity);
    let (first, g1, _) = attach(&registry, 0).expect("first presentation");
    feed(&first);
    assert_eq!(registry.fragments.used(), capacity);
    let (other, g2, _) = attach(&registry, 1).expect("publication gate remains separate");
    assert!(matches!(
        other.apply(|source, now| match source {
            Source::Ts(ts) => ts.push(AUDIO, now),
            _ => unreachable!(),
        }),
        Err(IngestError::FragmentLimit)
    ));
    assert_eq!(registry.fragments.used(), capacity);
    registry.terminate(&other, g2);
    registry.terminate(&first, g1);
    assert_eq!(registry.fragments.used(), 0);
    let (fresh, _, _) = attach(&registry, 2).expect("capacity returned");
    feed(&fresh);
    assert_eq!(registry.fragments.used(), capacity);
}

#[test]
fn partial_response_and_consumed_prefix_keep_backing_charges_after_close() {
    let registry = Registry::new(4);
    let (stream, generation, _) = attach(&registry, 0).expect("presentation");
    feed(&stream);
    let mut body = {
        let state = stream.lock();
        let segment = state
            .ingest
            .muxer()
            .expect("media")
            .segment(7)
            .expect("segment");
        MediaBody::from(segment.data.clone())
    };
    let expected = gather(&body);
    let mut alias = body.clone();
    for _ in 0..3 {
        feed(&stream);
    }
    assert!(
        stream
            .lock()
            .ingest
            .muxer()
            .expect("rotated media")
            .segment(7)
            .is_none()
    );
    registry.terminate(&stream, generation);
    drop(stream);
    // Kept past the window, segment 7's charge moved from the fragment
    // budget to the retention pool beside it; it stays while a body does.
    let charged = || registry.fragments.used() + registry.retained.used();
    let charge = charged();
    assert!(charge >= expected.len());
    let mut slices = [std::io::IoSlice::new(&[]); 128];
    body.slices(&mut slices);
    let first_chunk = slices[0].len();
    body.advance(first_chunk).expect("consume prefix fragment");
    assert_eq!(
        charged(),
        charge,
        "shared chunk array still owns consumed prefix"
    );
    assert_eq!(gather(&body), expected[first_chunk..]);
    body.advance(body.len()).expect("drain first viewer");
    assert_eq!(
        charged(),
        charge,
        "second viewer retains the original backing"
    );
    assert_eq!(gather(&alias), expected);
    alias.advance(alias.len()).expect("drain final viewer");
    assert_eq!(
        (registry.fragments.used(), registry.retained.used()),
        (0, 0)
    );
}

/// Two 600 kB/s A/V streams at `segment_count` 7 on the server's shared
/// 10 MB fragment budget, A starting 30 s after B: what B keeps past its
/// window is in the retention pool, so both run, and once both have gone
/// the budget and the pool are both empty.
#[test]
fn kept_media_never_fences_a_second_stream_on_the_server_budget() {
    let registry = Registry::with_budgets(4, 10_000_000, 128 * 1024 * 1024);
    let claim = |id: usize| {
        registry
            .claim_configured(
                &path(id),
                MuxerConfig {
                    segment_count: 7,
                    ..MuxerConfig::default()
                },
                SourceKind::Es,
                true,
                1,
                |_, _| {},
            )
            .expect("presentation")
    };
    let sps = vec![
        0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00,
        0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
    ];
    let pps = vec![0x68, 0xef, 0xbc, 0x80];
    let aac =
        sapflow_hls::codec::AacConfig::from_audio_specific_config(&[0x11, 0x90]).expect("AAC");
    // One frame: its audio first, then 20 kB of video, a keyframe each second.
    let frame = |stream: &LiveStream, i: i64| {
        stream
            .apply(|source, now| {
                let Source::Es(es) = source else {
                    unreachable!("elementary streams")
                };
                if i == 0 {
                    es.parameter_sets(Some(sps.clone()), Some(pps.clone()));
                    es.audio_config(aac);
                }
                let mut audio = (i * 3000 + 1919) / 1920 * 1920;
                while audio < (i + 1) * 3000 {
                    es.audio(audio, aac, &[0x21, 0x10, 0])?;
                    audio += 1920;
                }
                let idr = i % 30 == 0;
                let mut data = (20_000u32 - 4).to_be_bytes().to_vec();
                data.push(if idr { 0x65 } else { 0x41 });
                data.resize(20_000, 0);
                let unit = sapflow_hls::codec::AccessUnit {
                    data,
                    idr,
                    ..Default::default()
                };
                es.video(i * 3000, i * 3000, unit, now)
            })
            .unwrap_or_else(|e| panic!("frame {i} refused: {e:?}"));
    };
    let (b, b_generation, _) = claim(0);
    for i in 0..30 * 30 {
        frame(&b, i);
    }
    let (a, a_generation, _) = claim(1);
    for i in 0..60 * 30 {
        frame(&b, 30 * 30 + i);
        frame(&a, i);
    }
    assert!(
        registry.retained.used() > 0,
        "media was kept past the window"
    );
    registry.terminate(&a, a_generation);
    registry.terminate(&b, b_generation);
    drop((a, b));
    assert_eq!(
        (registry.fragments.used(), registry.retained.used()),
        (0, 0)
    );
}

#[test]
fn one_publisher_cannot_leave_eight_useful_paths_in_reconnect_hold() {
    let registry = Registry::new(4);
    for id in 0..8 {
        let result = attach(&registry, id);
        if id < 4 {
            let (stream, generation, _) = result.expect("exact resident capacity");
            feed(&stream);
            assert!(matches!(
                registry.retire(&stream, generation, Duration::from_secs(90), true),
                Retirement::Held(_)
            ));
        } else {
            assert!(
                matches!(result, Err(ClaimError::PresentationLimit)),
                "path {id} exceeded four retained presentations"
            );
        }
    }
    assert_eq!(registry.map().len(), 4);
    assert_eq!(registry.presentations.active(), 4);
}

#[test]
fn refusal_preserves_existing_media_and_does_not_configure_a_recording() {
    let registry = Registry::new(1);
    let (stream, generation, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    let playlist = stream.lock().media.clone();
    let configured = std::cell::Cell::new(false);
    assert!(matches!(
        registry.claim_configured(
            &path(1),
            MuxerConfig::default(),
            SourceKind::Flv,
            true,
            1,
            |_, _| configured.set(true),
        ),
        Err(ClaimError::PresentationLimit)
    ));
    assert!(!configured.get());
    assert_eq!(
        registry.active_rtmp_publishers(),
        0,
        "failed resident admission rolls back publication credit"
    );
    assert_eq!(stream.lock().media, playlist);
    assert!(stream.session().owns_connection(generation));
    assert!(!stream.lock().closed);
}

#[test]
fn reconnect_takeover_and_codec_change_reuse_the_full_presentation_reservation() {
    let registry = Registry::new(1);
    let (stream, old, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    let (replaced, next, claim) = attach(&registry, 0).expect("takeover at capacity");
    assert_eq!(claim, Claim::Resumed);
    assert!(Arc::ptr_eq(&stream, &replaced));
    assert!(matches!(
        registry.retire(&stream, old, Duration::ZERO, false),
        Retirement::Stale
    ));
    assert!(matches!(
        registry.retire(&stream, next, Duration::from_secs(90), true),
        Retirement::Held(_)
    ));
    let (resumed, _, claim) = attach(&registry, 0).expect("reconnect at capacity");
    assert_eq!(claim, Claim::Resumed);
    assert!(Arc::ptr_eq(&stream, &resumed));
    push(
        &resumed,
        include_bytes!("../../sapflow-hls/tests/fixtures/audio-44k1-stereo.ts"),
    );
    assert_eq!(
        resumed
            .lock()
            .ingest
            .muxer()
            .expect("new codec")
            .current_initialization_id(),
        1
    );
    assert_eq!(registry.presentations.active(), 1);
    assert!(matches!(
        attach(&registry, 1),
        Err(ClaimError::PresentationLimit)
    ));
}

#[test]
fn retained_response_survives_real_playlist_eviction_and_close_in_order() {
    let registry = Registry::new(1);
    let (stream, generation, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    let (id, part_id, owner, mut body) = {
        let state = stream.lock();
        let segment = state
            .ingest
            .muxer()
            .expect("media")
            .segment(7)
            .expect("first segment");
        (
            segment.id,
            segment.parts[0].id,
            Arc::downgrade(&segment),
            MediaBody::from(segment.data.clone()),
        )
    };
    let expected = gather(&body);
    for _ in 0..3 {
        feed(&stream);
    }
    {
        let state = stream.lock();
        let muxer = state.ingest.muxer().expect("rotated window");
        assert!(muxer.segment(id).is_none());
        assert!(matches!(muxer.part(part_id), PartLookup::Missing));
    }
    assert!(owner.upgrade().is_none());
    assert_eq!(gather(&body), expected);
    assert!(registry.terminate(&stream, generation));
    drop(stream);
    assert_eq!(registry.presentations.active(), 0);
    let mut written = Vec::new();
    while !body.is_empty() {
        let bytes = gather(&body);
        let count = bytes.len().min(137);
        written.extend_from_slice(&bytes[..count]);
        body.advance(count)
            .expect("bounded partial response writes");
    }
    assert_eq!(written, expected);
}

#[test]
fn expired_reconnect_cannot_reuse_a_removed_but_still_owned_reservation() {
    let registry = Registry::new(1);
    let (stream, generation, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    stream.session().hold_remaining(
        Duration::from_secs(1),
        Instant::now() - Duration::from_secs(2),
    );
    assert!(registry.detach(&stream, generation, true));
    assert!(matches!(
        attach(&registry, 0),
        Err(ClaimError::PresentationLimit)
    ));
    assert!(stream.lock().closed);
    assert!(registry.get(path(0).event_id()).is_none());
    drop(stream);
    assert_eq!(
        attach(&registry, 0).expect("released expired owner").2,
        Claim::Fresh
    );
}

#[test]
fn removed_stream_stays_charged_through_the_final_owner() {
    let registry = Registry::new(1);
    let (stream, generation, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    let parked_owner = registry
        .get(path(0).event_id())
        .expect("parked request owner");
    let late_owner = stream.clone();
    assert!(matches!(
        registry.retire(&stream, generation, Duration::ZERO, false),
        Retirement::Ended
    ));
    assert!(registry.get(path(0).event_id()).is_none());
    assert!(
        attach(&registry, 1).is_err(),
        "registry removal is not final release"
    );
    drop(stream);
    drop(late_owner);
    assert!(
        attach(&registry, 1).is_err(),
        "parked owner still retains presentation"
    );
    drop(parked_owner);
    assert_eq!(registry.presentations.active(), 0);
    let _replacement = attach(&registry, 1).expect("final owner released capacity");
}

#[test]
fn close_releases_cached_ingest_but_preserves_an_accepted_body() {
    let registry = Registry::new(1);
    let (stream, generation, _) = attach(&registry, 0).expect("first");
    feed(&stream);
    let (segment_owner, mut body, expected) = {
        let state = stream.lock();
        let muxer = state.ingest.muxer().expect("useful media");
        let segment = muxer
            .entries()
            .iter()
            .find_map(|entry| match entry {
                sapflow_hls::muxer::Entry::Segment(segment) => Some(segment),
                _ => None,
            })
            .expect("complete segment");
        let owner = Arc::downgrade(segment);
        let body = MediaBody::from(segment.data.clone());
        let expected = gather(&body);
        (owner, body, expected)
    };
    assert!(matches!(
        registry.retire(&stream, generation, Duration::ZERO, false),
        Retirement::Ended
    ));
    assert!(stream.lock().closed);
    assert!(
        stream.lock().ingest.muxer().is_none(),
        "closed stream must release its muxer immediately"
    );
    assert!(stream.lock().media.is_none());
    assert!(stream.lock().multivariant.is_none());
    assert!(
        segment_owner.upgrade().is_none(),
        "body owns shared bytes, not cached segment"
    );
    assert_eq!(gather(&body), expected);
    let consumed = expected.len() / 2;
    body.advance(consumed).expect("partial write");
    drop(stream);
    let _replacement = attach(&registry, 1).expect("body does not pin the presentation count");
    assert_eq!(gather(&body), expected[consumed..]);
    body.advance(body.len()).expect("drain body");
    assert!(body.is_empty());
}

#[test]
fn an_opus_stream_reports_its_audio_in_fresh_media() {
    use sapflow_hls::codec::{AudioConfig, OpusConfig};
    let registry = Registry::default();
    let (stream, generation, _claim) = attach(&registry, 9).expect("presentation");
    // Nothing carries Opus into a live stream yet; stand one in with an Opus
    // muxer resumed under the TS source.
    let mut muxer = Muxer::with_audio_config(
        MuxerConfig::default(),
        None,
        Some(AudioConfig::Opus(OpusConfig::STEREO)),
        None,
    )
    .expect("Opus muxer");
    for i in 0..100 {
        muxer
            .push_audio_at(
                i * 1800,
                Bytes::from_static(&[0xfc, 0xff, 0xfe]),
                SystemTime::UNIX_EPOCH,
            )
            .expect("20 ms Opus");
    }
    stream.lock().ingest = Source::Ts(Box::new(TsIngest::resume(MuxerConfig::default(), muxer)));
    let fresh = stream
        .connection_media(generation)
        .and_then(|snapshot| snapshot.fresh_media)
        .expect("an independent Opus part");
    assert!(fresh.has_audio, "Opus is audio");
    assert!(!fresh.has_video);
}
