//! Real generated-media recording under shared backing exhaustion and recovery.
use super::loss_tests::Fixture;
use super::*;

fn configure(fixture: &mut Fixture, maximum: usize) {
    let app = Arc::get_mut(&mut fixture.app).expect("sole App");
    app.config.max_fragment_bytes = maximum;
    app.streams = Arc::new(crate::stream::Registry::with_limits(4, maximum));
}

fn expected_file(
    tracks: &crate::stream::Tracks,
    parts: &[Arc<sapflow_hls::muxer::Part>],
) -> Vec<u8> {
    let duration = parts.iter().map(|part| part.span).sum();
    let mut expected =
        sapflow_hls::fmp4::init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
            .expect("recording header")
            .to_vec();
    let mut origin = None;
    for part in parts {
        expected.extend_from_slice(
            &sapflow_hls::fmp4::rebase_part_for_file(
                &part.data,
                tracks.audio().map(|audio| audio.sample_rate),
                &mut origin,
            )
            .expect("accepted fragment"),
        );
    }
    expected
}

#[tokio::test]
async fn single_batch_exhaustion_feeds_its_accepted_parts_before_incomplete_completion() {
    let quota = Arc::new(sapflow_core::fragment::FragmentQuota::new(8 * 1024 * 1024));
    let mut probe = sapflow_hls::TsIngest::with_fragment_quota(
        sapflow_hls::MuxerConfig::default(),
        quota.clone(),
    );
    let fixture_bytes = include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts");
    for chunk in fixture_bytes.chunks(188) {
        probe
            .push(chunk, std::time::UNIX_EPOCH)
            .expect("measure first encoded segment");
        if probe.muxer().is_some_and(|muxer| muxer.next_part_id() > 0) {
            break;
        }
    }
    let capacity = quota.used();
    assert!(capacity > 0);
    let mut fixture = Fixture::new();
    configure(&mut fixture, capacity);
    let quota = fixture.app.streams.fragment_quota();
    let mut publication = fixture.begin();
    assert!(matches!(
        publication.push(|source, now| match source {
            Source::Ts(ts) => ts.push(fixture_bytes, now),
            _ => unreachable!(),
        }),
        Err(PushError::Ingest(sapflow_hls::IngestError::FragmentLimit))
    ));
    let (tracks, parts) = publication
        .stream
        .parts_after(None)
        .expect("accepted batch prefix");
    assert!(
        !parts.is_empty(),
        "actual fragments were admitted before the batch failed"
    );
    let expected = expected_file(&tracks, &parts);
    publication.end();
    let receipt = fixture.completion("incomplete").await;
    assert_eq!(
        std::fs::read(receipt["segmentPath"].as_str().expect("actual recording"))
            .expect("synced prefix"),
        expected
    );
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
    drop(parts);
    assert_eq!(quota.used(), 0);
}

#[tokio::test]
async fn byte_refusal_records_only_the_incomplete_accepted_prefix_and_fresh_publication_recovers() {
    let maximum = 8 * 1024 * 1024;
    let mut fixture = Fixture::new();
    configure(&mut fixture, maximum);
    let quota = fixture.app.streams.fragment_quota();
    let mut publication = fixture.begin();
    Fixture::feed(&mut publication);
    let (tracks, parts) = publication
        .stream
        .parts_after(None)
        .expect("accepted prefix");
    let expected = expected_file(&tracks, &parts);
    let accepted_part = publication
        .stream
        .lock()
        .ingest
        .muxer()
        .expect("media")
        .next_part_id();
    // Inject another owner's logical reservation; this is not an RSS test.
    let blocker = quota
        .try_acquire(maximum - quota.used())
        .expect("exhaust backing capacity");
    let mut refused = false;
    for chunk in include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts").chunks(1316) {
        match publication.push(|source, now| match source {
            Source::Ts(ts) => ts.push(chunk, now),
            _ => unreachable!(),
        }) {
            Ok(()) => {}
            Err(PushError::Ingest(sapflow_hls::IngestError::FragmentLimit)) => {
                refused = true;
                break;
            }
            error => panic!("unexpected admission: {error:?}"),
        }
    }
    assert!(
        refused,
        "shared byte exhaustion must reach publication policy"
    );
    assert_eq!(
        publication
            .stream
            .lock()
            .ingest
            .muxer()
            .expect("prefix remains valid")
            .next_part_id(),
        accepted_part
    );
    publication.end();
    let receipt = fixture.completion("incomplete").await;
    let actual = std::fs::read(receipt["segmentPath"].as_str().expect("actual prefix file"))
        .expect("synced file");
    assert_eq!(
        actual, expected,
        "no refused suffix may reach the recording"
    );
    drop(blocker);
    assert!(
        quota.used() > 0,
        "caller-held emitted parts keep their backing charge after writer completion"
    );
    drop(parts);
    assert_eq!(quota.used(), 0);
    let mut recovered = fixture.begin();
    Fixture::feed(&mut recovered);
    recovered.end();
    fixture.completion("finalised").await;
    assert_eq!(quota.used(), 0);
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
    assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 1);
}

#[tokio::test]
async fn terminal_only_fragment_refusal_cannot_report_a_complete_suffix() {
    let maximum = 8 * 1024 * 1024;
    let mut fixture = Fixture::new();
    configure(&mut fixture, maximum);
    let quota = fixture.app.streams.fragment_quota();
    let mut publication = fixture.begin();
    Fixture::feed(&mut publication);
    let (tracks, parts) = publication
        .stream
        .parts_after(None)
        .expect("accepted prefix");
    let expected = expected_file(&tracks, &parts);
    let blocker = quota
        .try_acquire(maximum - quota.used())
        .expect("exhaust before terminal drain");
    publication.end();
    let receipt = fixture.completion("incomplete").await;
    assert_eq!(
        std::fs::read(receipt["segmentPath"].as_str().expect("synced path")).expect("prefix bytes"),
        expected
    );
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
    drop(blocker);
    drop(parts);
    assert_eq!(quota.used(), 0);
}
