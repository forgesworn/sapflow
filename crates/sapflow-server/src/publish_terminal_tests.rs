//! Actual recorder completion at the accepted TS connection boundary.
use super::loss_tests::Fixture;
use super::*;
// Original generated framing placeholders test packaging/writer truth; they
// are not described as independently decodable AAC.
const ADTS48: &[u8] = &[0xff, 0xf1, 0x4c, 0x40, 1, 0x5f, 0xfc, 1, 2, 3];
pub(crate) const ADTS44: &[u8] = &[0xff, 0xf1, 0x50, 0x80, 1, 0x5f, 0xfc, 1, 2, 3];

fn tables() -> (Vec<u8>, u16) {
    // Exact generated fixture profile: PAT PID0, PMT PID4096, AAC PID256.
    let fixture = include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts");
    let mut wire = Vec::new();
    for packet in fixture.chunks_exact(188) {
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        if pid == 0 || pid == 4096 {
            wire.extend_from_slice(packet);
        }
        if pid == 4096 {
            break;
        }
    }
    assert_eq!(wire.len(), 376, "generated PAT and PMT");
    (wire, 256)
}

fn start(pid: u16, data: &[u8], declared: u16, pts: u64, counter: u8) -> Vec<u8> {
    assert!(pid <= 8191 && counter < 16 && pts < (1 << 33));
    let pes_bytes = 14 + data.len();
    assert!(pes_bytes <= 182);
    let mut packet = vec![0xff; 188];
    packet[..4].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x30 | counter]);
    let adaptation = 183 - pes_bytes;
    packet[4] = adaptation as u8;
    packet[5] = 0;
    let at = 5 + adaptation;
    packet[at..at + 9].copy_from_slice(&[
        0,
        0,
        1,
        0xc0,
        (declared >> 8) as u8,
        declared as u8,
        0x84,
        0x80,
        5,
    ]);
    packet[at + 9..at + 14].copy_from_slice(&[
        0x21 | (((pts >> 30) & 7) as u8) << 1,
        (pts >> 22) as u8,
        1 | (((pts >> 15) & 0x7f) as u8) << 1,
        (pts >> 7) as u8,
        1 | ((pts & 0x7f) as u8) << 1,
    ]);
    packet[at + 14..].copy_from_slice(data);
    packet
}

pub(crate) fn one(data: &[u8]) -> Vec<u8> {
    let (mut wire, pid) = tables();
    wire.extend(start(pid, data, 18, 90_000, 0));
    wire
}

fn feed(publication: &mut Publication, wire: &[u8]) {
    publication
        .push(|source, now| match source {
            Source::Ts(ts) => ts.push(wire, now),
            _ => unreachable!(),
        })
        .expect("accepted generated TS input");
}

pub(crate) fn recording_samples(body: &serde_json::Value) -> usize {
    fn runs(bytes: &[u8]) -> usize {
        let mut at = 0;
        let mut count = 0;
        while at < bytes.len() {
            assert!(bytes.len() - at >= 8);
            let length =
                u32::from_be_bytes(bytes[at..at + 4].try_into().expect("box length")) as usize;
            assert!(length >= 8 && length <= bytes.len() - at);
            match &bytes[at + 4..at + 8] {
                b"moof" | b"traf" => count += runs(&bytes[at + 8..at + length]),
                b"trun" => {
                    assert!(length >= 16);
                    count += u32::from_be_bytes(
                        bytes[at + 12..at + 16].try_into().expect("sample count"),
                    ) as usize;
                }
                _ => {}
            }
            at += length;
        }
        count
    }
    let bytes = std::fs::read(
        body["segmentPath"]
            .as_str()
            .expect("private generated path"),
    )
    .expect("actual synced file");
    runs(&bytes)
}

async fn completion(fixture: &mut Fixture, state: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let queued = fixture
                .callbacks
                .recv()
                .await
                .expect("actual writer callback");
            let body: serde_json::Value =
                serde_json::from_slice(&queued.body).expect("callback JSON");
            if body["event"] == "recordSegmentComplete" {
                assert_eq!(body["recordingState"], state);
                assert_eq!(body["sessionId"], fixture.scope.key().session_id.as_str());
                return body;
            }
        }
    })
    .await
    .expect("bounded actual writer finalisation")
}

#[tokio::test]
async fn zero_grace_end_drains_the_only_start_packet_into_a_real_synced_recording() {
    let mut fixture = Fixture::new();
    let mut publication = fixture.begin();
    feed(&mut publication, &one(ADTS48));
    assert!(publication.stream.lock().ingest.muxer().is_none());
    let stream = publication.stream.clone();
    publication.end();
    assert!(stream.lock().closed);
    assert!(fixture.app.streams.get("event-a").is_none());
    let body = completion(&mut fixture, "finalised").await;
    assert_eq!(recording_samples(&body), 1);
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
}

#[tokio::test]
async fn ts_resource_limit_syncs_only_the_accepted_prefix_as_incomplete() {
    for failure in ["unbounded-pes", "raw-input"] {
        let mut fixture = Fixture::new();
        let mut publication = fixture.begin();
        let (mut wire, pid) = tables();
        wire.extend(start(pid, ADTS48, 18, 90_000, 0));
        wire.extend(start(pid, ADTS48, 18, 91_920, 1));
        feed(&mut publication, &wire);
        let rejected = if failure == "raw-input" {
            publication.push(|source, now| match source {
                Source::Ts(ts) => ts.push(&vec![0; 8 * 1024 * 1024 + 1], now),
                _ => unreachable!(),
            })
        } else {
            // This START first returns the second accepted AAC frame. The new
            // video PES never completes or reaches codec/muxer processing.
            let mut first = start(pid, &[1], 0, 93_840, 2);
            let at = 5 + usize::from(first[4]);
            first[at + 3] = 0xe0;
            feed(&mut publication, &first);
            let mut total = 1;
            let mut counter = 3;
            loop {
                let mut next = [0xa5; 188];
                next[..4].copy_from_slice(&[0x47, (pid >> 8) as u8, pid as u8, 0x10 | counter]);
                let result = publication.push(|source, now| match source {
                    Source::Ts(ts) => ts.push(&next, now),
                    _ => unreachable!(),
                });
                total += 184;
                if result.is_err() {
                    assert!(
                        total > 8 * 1024 * 1024,
                        "only the PES budget rejects this valid-CC stream"
                    );
                    break result;
                }
                assert!(
                    total <= 8 * 1024 * 1024,
                    "PES limit must reject before growing further"
                );
                counter = (counter + 1) & 15;
            }
        };
        assert!(matches!(
            rejected,
            Err(PushError::Ingest(
                sapflow_hls::IngestError::InputLimitExceeded
            ))
        ));
        publication.end();
        let body = completion(&mut fixture, "incomplete").await;
        assert_eq!(
            body["recordingTracks"],
            serde_json::json!({"hasVideo": false, "hasAudio": true})
        );
        assert_eq!(
            recording_samples(&body),
            if failure == "raw-input" { 1 } else { 2 },
            "never include the rejected/held payload"
        );
        let counts = fixture.scope.observations.counts();
        assert_eq!(counts.recordings_incomplete, 1);
        assert_eq!(counts.recordings_finalised, 0);
    }
}

#[tokio::test]
async fn held_start_only_tail_resumes_without_old_parts_becoming_fresh_or_duplicated() {
    let mut fixture = Fixture::new();
    let mut first = fixture.begin();
    feed(&mut first, &one(ADTS48));
    first.grace = Duration::from_secs(1);
    let stream = first.stream.clone();
    first.end();
    assert!(Arc::ptr_eq(
        &stream,
        &fixture
            .app
            .streams
            .get("event-a")
            .expect("EOF creates holdable content")
    ));
    let mut next = fixture.begin();
    assert!(Arc::ptr_eq(&stream, &next.stream));
    assert!(
        next.stream
            .connection_media(next.generation)
            .expect("attached")
            .fresh_media
            .is_none()
    );
    feed(&mut next, &one(ADTS48));
    next.end();
    let body = completion(&mut fixture, "finalised").await;
    assert_eq!(recording_samples(&body), 2);
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
}

#[tokio::test]
async fn terminal_truncation_and_semantic_errors_taint_only_the_accepted_prefix() {
    for failure in ["pes", "ts", "asc"] {
        let mut fixture = Fixture::new();
        let mut publication = fixture.begin();
        let (mut wire, pid) = tables();
        wire.extend(start(pid, ADTS48, 18, 90_000, 0));
        match failure {
            "pes" => wire.extend(start(pid, ADTS48, 19, 91_920, 1)),
            "ts" => wire.extend(&start(pid, ADTS48, 18, 91_920, 1)[..50]),
            _ => wire.extend(start(pid, ADTS44, 18, 91_920, 1)),
        }
        feed(&mut publication, &wire);
        publication.end();
        let body = completion(&mut fixture, "incomplete").await;
        assert_eq!(recording_samples(&body), 1, "valid prefix, {failure}");
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
    }
}

#[tokio::test]
async fn stale_generation_cannot_finish_or_feed_its_successor_parser_or_recorder() {
    let mut fixture = Fixture::new();
    let mut old = fixture.begin();
    feed(&mut old, &one(ADTS48));
    let stream = old.stream.clone();
    assert!(fixture.app.streams.detach(&stream, old.generation, true));
    let mut new = fixture.begin();
    feed(&mut new, &one(ADTS48));
    let before = stream
        .lock()
        .ingest
        .muxer()
        .expect("retained muxer")
        .next_part_id();
    assert!(!fixture.app.streams.detach(&stream, old.generation, false));
    assert!(!fixture.app.streams.terminate(&stream, old.generation));
    assert!(
        old.push(|_, _| panic!("stale closure must not run"))
            .is_err()
    );
    old.end();
    assert_eq!(
        stream
            .lock()
            .ingest
            .muxer()
            .expect("current parser unaffected")
            .next_part_id(),
        before
    );
    assert!(stream.connection_media(new.generation).is_some());
    new.end();
    let body = completion(&mut fixture, "finalised").await;
    assert_eq!(recording_samples(&body), 2);
}

#[tokio::test]
async fn explicit_terminate_drains_only_accepted_input_and_repeated_drop_is_harmless() {
    let mut fixture = Fixture::new();
    let mut publication = fixture.begin();
    feed(&mut publication, &one(ADTS48));
    let stream = publication.stream.clone();
    assert!(
        fixture
            .app
            .streams
            .terminate(&stream, publication.generation)
    );
    assert!(
        !fixture
            .app
            .streams
            .terminate(&stream, publication.generation)
    );
    assert!(
        publication
            .push(|_, _| panic!("terminal scope must not ingest"))
            .is_err()
    );
    publication.end();
    let body = completion(&mut fixture, "finalised").await;
    assert_eq!(recording_samples(&body), 1);
}

#[tokio::test]
async fn terminal_only_codec_epoch_uses_new_init_and_distinct_actual_recording() {
    let mut fixture = Fixture::new();
    let mut old = fixture.begin();
    Fixture::feed(&mut old);
    old.grace = Duration::from_secs(1);
    old.end();
    let mut next = fixture.begin();
    feed(&mut next, &one(ADTS44));
    assert!(
        next.stream
            .lock()
            .ingest
            .muxer()
            .expect("old retained codec")
            .video()
            .is_some()
    );
    next.end();
    let a = completion(&mut fixture, "finalised").await;
    let b = completion(&mut fixture, "finalised").await;
    assert_ne!(a["recordingId"], b["recordingId"]);
    let (lone, av) = if recording_samples(&a) == 1 {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(recording_samples(&lone), 1);
    assert_eq!(
        lone["recordingTracks"],
        serde_json::json!({"hasVideo": false, "hasAudio": true})
    );
    assert_eq!(
        av["recordingTracks"],
        serde_json::json!({"hasVideo": true, "hasAudio": true})
    );
    let bytes =
        std::fs::read(lone["segmentPath"].as_str().expect("audio epoch file")).expect("file");
    assert!(
        !bytes.windows(4).any(|w| w == b"avc1"),
        "new AAC must not carry old video initialization"
    );
}

#[tokio::test]
async fn complete_generated_audio_fixtures_reach_actual_files_without_terminal_frame_loss() {
    for (wire, frames) in [
        (
            &include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts")[..],
            283,
        ),
        (
            &include_bytes!("../../sapflow-hls/tests/fixtures/audio-44k1-stereo.ts")[..],
            260,
        ),
    ] {
        let mut fixture = Fixture::new();
        let mut publication = fixture.begin();
        for chunk in wire.chunks(1316) {
            feed(&mut publication, chunk);
        }
        publication.end();
        let body = completion(&mut fixture, "finalised").await;
        assert_eq!(recording_samples(&body), frames);
    }
}

#[tokio::test]
async fn terminal_codec_change_then_error_cannot_append_new_aac_under_old_file_init() {
    let mut fixture = Fixture::new();
    let mut old = fixture.begin();
    Fixture::feed(&mut old);
    old.grace = Duration::from_secs(1);
    let stream = old.stream.clone();
    old.end();
    let (tracks, parts) = stream.parts_after(None).expect("sealed old epoch");
    let duration = parts
        .iter()
        .fold(Duration::ZERO, |sum, part| sum + part.span);
    let mut expected =
        sapflow_hls::fmp4::init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
            .expect("old initialization")
            .to_vec();
    let mut origin = None;
    for part in parts {
        expected.extend(
            sapflow_hls::fmp4::rebase_part_for_file(
                &part.data,
                tracks.audio().map(|a| a.sample_rate),
                &mut origin,
            )
            .expect("accepted old fragment"),
        );
    }
    let mut next = fixture.begin();
    let (mut wire, pid) = tables();
    let mut audio = ADTS44.to_vec();
    audio.extend(ADTS48);
    wire.extend(start(pid, &audio, 28, 90_000, 0));
    feed(&mut next, &wire);
    next.end();
    assert!(
        stream
            .lock()
            .ingest
            .muxer()
            .expect("new epoch is visible to HLS")
            .video()
            .is_none()
    );
    let body = completion(&mut fixture, "incomplete").await;
    assert_eq!(
        body["recordingTracks"],
        serde_json::json!({"hasVideo": true, "hasAudio": true})
    );
    let file =
        std::fs::read(body["segmentPath"].as_str().expect("old file")).expect("synced prefix");
    assert_eq!(
        file, expected,
        "do not append changed-ASC bytes to old file"
    );
    assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
}

#[tokio::test]
async fn held_terminal_tail_expires_once_but_header_only_input_does_not_hold() {
    let mut fixture = Fixture::new();
    let mut publication = fixture.begin();
    feed(&mut publication, &one(ADTS48));
    publication.grace = Duration::from_secs(1);
    let stream = publication.stream.clone();
    let generation = publication.generation;
    publication.end();
    assert!(fixture.app.streams.expire(&stream, generation));
    assert!(!fixture.app.streams.expire(&stream, generation));
    let body = completion(&mut fixture, "finalised").await;
    assert_eq!(recording_samples(&body), 1);

    let mut empty = fixture.begin();
    empty.grace = Duration::from_secs(1);
    feed(&mut empty, &tables().0);
    empty.end();
    assert!(
        fixture.app.streams.get("event-a").is_none(),
        "metadata alone is not holdable media"
    );
}

#[tokio::test]
async fn stale_published_drop_suppresses_unpublish_for_both_arc_and_generation_replacement() {
    for different_arc in [false, true] {
        let mut fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        assert!(old.published);
        let stream = old.stream.clone();
        if different_arc {
            assert!(fixture.app.streams.terminate(&stream, old.generation));
        } else {
            assert!(fixture.app.streams.detach(&stream, old.generation, true));
        }
        let mut next = fixture.begin();
        assert_eq!(!Arc::ptr_eq(&stream, &next.stream), different_arc);
        Fixture::feed(&mut next);
        assert!(next.published);
        old.end();
        assert!(next.stream.connection_media(next.generation).is_some());
        next.end();
        let mut unpublish = 0;
        let mut recordings = 0;
        let expected = if different_arc { 2 } else { 1 };
        tokio::time::timeout(Duration::from_secs(3), async {
            while recordings != expected {
                let item = fixture
                    .callbacks
                    .recv()
                    .await
                    .expect("bounded lifecycle callback");
                let body: serde_json::Value = serde_json::from_slice(&item.body).expect("JSON");
                unpublish += usize::from(body["event"] == "unpublish");
                recordings += usize::from(body["event"] == "recordSegmentComplete");
            }
        })
        .await
        .expect("actual writers finish");
        while let Ok(item) = fixture.callbacks.try_recv() {
            let body: serde_json::Value = serde_json::from_slice(&item.body).expect("JSON");
            unpublish += usize::from(body["event"] == "unpublish");
        }
        assert_eq!(
            unpublish, 1,
            "only successor emits Unpublish, differentArc={different_arc}"
        );
    }
}

#[test]
fn terminal_only_rollover_drops_without_ambient_runtime_and_syncs_on_origin_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("owned originating runtime");
    let (mut fixture, publication) = runtime.block_on(async {
        let fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        old.grace = Duration::from_secs(1);
        old.end();
        let mut next = fixture.begin();
        feed(&mut next, &one(ADTS44));
        assert!(
            next.stream
                .lock()
                .ingest
                .muxer()
                .expect("retained AV")
                .video()
                .is_some()
        );
        tokio::task::yield_now().await;
        (fixture, next)
    });
    assert!(
        tokio::runtime::Handle::try_current().is_err(),
        "test must leave runtime context"
    );
    let stream = publication.stream.clone();
    publication.end();
    assert!(
        stream.lock().closed,
        "no ambient runtime means no reconnect hold"
    );
    assert!(fixture.app.streams.get("event-a").is_none());
    runtime.block_on(async {
        let first = completion(&mut fixture, "finalised").await;
        let second = completion(&mut fixture, "finalised").await;
        assert_ne!(first["recordingId"], second["recordingId"]);
        let (audio, video) = if recording_samples(&first) == 1 {
            (first, second)
        } else {
            (second, first)
        };
        assert_eq!(recording_samples(&audio), 1);
        assert!(recording_samples(&video) > 1);
        let bytes = std::fs::read(audio["segmentPath"].as_str().expect("AAC epoch file"))
            .expect("actual new writer flush and sync completed");
        assert!(!bytes.windows(4).any(|kind| kind == b"avc1"));
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 2);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
    });
}
