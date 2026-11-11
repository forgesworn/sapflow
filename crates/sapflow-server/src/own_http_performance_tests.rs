//! Notification ownership/races and byte-identical conventional playlist caching.
//! Fixtures are local, generated, finite and release their sockets on drop.
use super::*;
use crate::stream::{Source, SourceKind};
use crate::test_support::{LANDSCAPE, PORTRAIT, Scratch, app};
use sapflow_core::StreamPath;
use sapflow_hls::MuxerConfig;
use std::net::TcpStream as StdStream;

fn fixture() -> (Worker, Scratch) {
    let (app, scratch) = app(false);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("local listener");
    listener.set_nonblocking(true).expect("nonblocking");
    let worker = Worker::new(app, listener, Timeouts::default()).expect("worker");
    (worker, scratch)
}

fn connect(worker: &mut Worker) -> (usize, StdStream) {
    let id = worker.next;
    let client = StdStream::connect(worker.listener.local_addr().expect("listener address"))
        .expect("client");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Events::with_capacity(4);
    loop {
        worker.accept();
        if worker.conns.contains_key(&id) {
            return (id, client);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "accept deadline");
        worker
            .poll
            .poll(&mut events, Some(remaining))
            .expect("poll");
    }
}

fn stream(worker: &Worker, event: &str) -> Arc<LiveStream> {
    worker
        .app
        .streams
        .claim(
            &StreamPath::parse(&format!("live/{event}")).expect("path"),
            MuxerConfig::default(),
            SourceKind::Ts,
        )
        .expect("stream")
        .0
}

fn park(worker: &mut Worker, id: usize, stream: Arc<LiveStream>, seen: u64) {
    worker.conns.get_mut(&id).expect("connection").0.parked = Some(Parked {
        request: Request::Media {
            stream: stream.clone(),
            msn: None,
            part: None,
        },
        head_only: false,
        deadline: Instant::now() + Duration::from_secs(3),
        seen,
    });
    worker.park(id, stream, seen);
}

#[test]
fn changes_select_only_the_matching_stream_and_coalesce() {
    let (mut worker, _scratch) = fixture();
    let (first, _first_client) = connect(&mut worker);
    let (second, _second_client) = connect(&mut worker);
    let (other, _other_client) = connect(&mut worker);
    let a = stream(&worker, "event-notify-a");
    let b = stream(&worker, "event-notify-b");
    park(&mut worker, first, a.clone(), a.version());
    park(&mut worker, second, a.clone(), a.version());
    park(&mut worker, other, b.clone(), b.version());
    assert_eq!(worker.waiting.len(), 2);
    a.close();
    for _ in 0..100 {
        a.http_notifications.notify(a.http_key());
    }
    let mut ids = Vec::new();
    worker.collect_woken(&mut ids);
    ids.sort_unstable();
    assert_eq!(ids, vec![first, second]);
    ids.clear();
    worker.collect_woken(&mut ids);
    assert!(ids.is_empty(), "repeated updates have one pending entry");
    assert!(worker.conns[&other].0.parked.is_some());
}

#[test]
fn publish_before_subscription_is_caught_by_the_version_recheck() {
    let (mut worker, _scratch) = fixture();
    let (id, _client) = connect(&mut worker);
    let a = stream(&worker, "event-notify-race");
    let seen = a.version();
    // Equivalent to a publication after answer() returns Wait but before the
    // worker has subscribed. No subscription exists when close() notifies.
    a.close();
    park(&mut worker, id, a, seen);
    let mut ids = Vec::new();
    worker.collect_woken(&mut ids);
    assert_eq!(ids, vec![id]);
    worker.service(id, false);
    assert!(worker.parked.is_empty());
    assert!(worker.waiting.is_empty());
}

#[test]
fn last_waiter_and_worker_drop_remove_subscription_and_pending_identity() {
    let (mut worker, _scratch) = fixture();
    let (id, _client) = connect(&mut worker);
    let a = stream(&worker, "event-notify-drop");
    park(&mut worker, id, a.clone(), a.version());
    a.close();
    worker.close(id);
    let mut ids = Vec::new();
    worker.collect_woken(&mut ids);
    assert!(ids.is_empty());
    assert!(worker.waiting.is_empty());
    // Keep the wake queue alive externally to prove that worker teardown
    // unsubscribes, rather than relying on the queue being deallocated.
    let wake = worker.wake.clone();
    let (next, _next_client) = connect(&mut worker);
    park(&mut worker, next, a.clone(), a.version());
    drop(worker);
    a.http_notifications.notify(a.http_key());
    let mut pending = HashSet::new();
    wake.take_into(&mut pending);
    assert!(pending.is_empty());
}

#[test]
fn same_path_presentations_have_separate_notification_ownership() {
    let (mut old_worker, _old_scratch) = fixture();
    let (mut new_worker, _new_scratch) = fixture();
    let (old_id, _old_client) = connect(&mut old_worker);
    let (new_id, _new_client) = connect(&mut new_worker);
    let old = stream(&old_worker, "event-replacement");
    let new = stream(&new_worker, "event-replacement");
    assert_eq!(old.path, new.path);
    assert_ne!(old.http_key(), new.http_key());
    park(&mut old_worker, old_id, old.clone(), old.version());
    park(&mut new_worker, new_id, new.clone(), new.version());
    old.close();
    let mut ids = Vec::new();
    old_worker.collect_woken(&mut ids);
    assert_eq!(ids, vec![old_id]);
    ids.clear();
    new_worker.collect_woken(&mut ids);
    assert!(ids.is_empty());
}

fn feed(stream: &LiveStream, bytes: &[u8]) {
    stream
        .apply(|source, now| match source {
            Source::Ts(ts) => ts.push(bytes, now),
            _ => unreachable!(),
        })
        .expect("generated fixture");
}

fn body_bytes(body: &MediaBody) -> (Vec<u8>, *const u8) {
    let mut slices = [IoSlice::new(&[])];
    assert_eq!(body.slices(&mut slices), 1);
    // The playlist is a single immutable Bytes allocation.
    let bytes = slices[0].as_ref();
    (bytes.to_vec(), bytes.as_ptr())
}

#[test]
fn conventional_playlist_shares_bytes_and_invalidates_across_media_and_close() {
    let (mut app, _scratch) = app(false);
    Arc::get_mut(&mut app)
        .expect("only app owner")
        .config
        .low_latency = false;
    let path = StreamPath::parse("live/event-cache").expect("path");
    let stream = app
        .streams
        .claim(&path, MuxerConfig::default(), SourceKind::Ts)
        .expect("stream")
        .0;
    feed(&stream, PORTRAIT);
    let request = Request::Media {
        stream: stream.clone(),
        msn: None,
        part: None,
    };
    let first = answer(&app, &request).expect("first response");
    let second = answer(&app, &request).expect("cached response");
    assert_eq!(
        body_bytes(&first.body).1,
        body_bytes(&second.body).1,
        "identical responses share one rendered allocation"
    );
    {
        let state = stream.lock();
        let expected =
            playlist::media_with_mode(state.ingest.muxer().expect("muxer"), &state.prefix, false);
        assert_eq!(body_bytes(&first.body).0, expected.as_bytes());
    }
    feed(&stream, LANDSCAPE);
    let third = answer(&app, &request).expect("new epoch response");
    assert_ne!(
        body_bytes(&first.body).1,
        body_bytes(&third.body).1,
        "new generation cannot reuse stale bytes still held by a viewer"
    );
    {
        let state = stream.lock();
        let expected =
            playlist::media_with_mode(state.ingest.muxer().expect("muxer"), &state.prefix, false);
        assert_eq!(body_bytes(&third.body).0, expected.as_bytes());
    }
    stream.close();
    assert_eq!(
        answer(&app, &request).expect("closed response").status,
        "404 Not Found"
    );
    assert!(
        !first.body.is_empty(),
        "already owned immutable responses stay valid"
    );
}

#[test]
fn rejected_ingest_invalidates_a_conventional_playlist_after_partial_mutation() {
    let (mut app, _scratch) = app(false);
    Arc::get_mut(&mut app)
        .expect("only owner")
        .config
        .low_latency = false;
    let stream = app
        .streams
        .claim(
            &StreamPath::parse("live/event-cache-rejected").expect("path"),
            MuxerConfig::default(),
            SourceKind::Ts,
        )
        .expect("stream")
        .0;
    feed(&stream, PORTRAIT);
    let request = Request::Media {
        stream: stream.clone(),
        msn: None,
        part: None,
    };
    let before = answer(&app, &request).expect("before error");
    let result = stream.apply(|source, now| {
        let Source::Ts(ts) = source else {
            unreachable!()
        };
        ts.push(LANDSCAPE, now)?;
        Err(sapflow_hls::IngestError::InputLimitExceeded)
    });
    assert!(matches!(
        result,
        Err(sapflow_hls::IngestError::InputLimitExceeded)
    ));
    let after = answer(&app, &request).expect("after partial error");
    assert_ne!(
        body_bytes(&before.body).0,
        body_bytes(&after.body).0,
        "the fixture must actually alter the conventional playlist"
    );
    let state = stream.lock();
    let expected =
        playlist::media_with_mode(state.ingest.muxer().expect("muxer"), &state.prefix, false);
    assert_eq!(body_bytes(&after.body).0, expected.as_bytes());
}

/// The former Ready arm, with the same lock, readiness check, response headers
/// and owned response body as answer(). Used only as the microbenchmark control.
fn uncached_media_answer(request: &Request) -> Option<Response> {
    let Request::Media { stream, msn, part } = request else {
        unreachable!()
    };
    let state = stream.lock();
    assert!(!state.closed);
    let muxer = state.ingest.muxer()?;
    assert!(matches!(muxer.readiness(*msn, *part), Readiness::Ready));
    Some(Response::ok(
        Bytes::from(playlist::media_with_mode(muxer, &state.prefix, false)),
        PLAYLIST_TYPE,
        "no-cache",
    ))
}

#[test]
#[ignore = "manual release-mode conventional playlist response microbenchmark"]
fn conventional_playlist_cache_microbenchmark() {
    use std::hint::black_box;
    let (mut app, _scratch) = app(false);
    Arc::get_mut(&mut app)
        .expect("only owner")
        .config
        .low_latency = false;
    let stream = app
        .streams
        .claim(
            &StreamPath::parse("live/event-cache-bench").expect("path"),
            MuxerConfig::default(),
            SourceKind::Ts,
        )
        .expect("stream")
        .0;
    feed(&stream, PORTRAIT);
    let request = Request::Media {
        stream,
        msn: None,
        part: None,
    };
    let expected = uncached_media_answer(&request).expect("control");
    let candidate = answer(&app, &request).expect("candidate");
    assert_eq!(body_bytes(&candidate.body).0, body_bytes(&expected.body).0);
    assert_eq!(candidate.headers, expected.headers);
    let iterations = 20_000;
    let measure = |cached: bool| {
        let started = Instant::now();
        let mut bytes = 0;
        for _ in 0..iterations {
            let response = if cached {
                answer(black_box(&app), black_box(&request))
            } else {
                uncached_media_answer(black_box(&request))
            }
            .expect("ready response");
            bytes += black_box(response.body.len());
            black_box(response);
        }
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(bytes, iterations * expected.body.len());
        elapsed
    };
    for round in 0..8 {
        let (control, cached) = if round % 2 == 0 {
            (measure(false), measure(true))
        } else {
            let cached = measure(true);
            (measure(false), cached)
        };
        println!(
            "playlist-cache round={round} responses={iterations} bytes={} uncached_ns={control} cached_ns={cached}",
            expected.body.len()
        );
    }
}

#[test]
fn moving_viewers_releases_the_old_groups_high_water_storage() {
    let (mut worker, _scratch) = fixture();
    let a = stream(&worker, "event-migrate-a");
    let b = stream(&worker, "event-migrate-b");
    let clients: Vec<_> = (0..32).map(|_| connect(&mut worker)).collect();
    for (id, _) in &clients {
        park(&mut worker, *id, a.clone(), a.version());
    }
    let before = worker.waiting[&a.http_key()].connections.capacity();
    for (id, _) in &clients[1..] {
        park(&mut worker, *id, b.clone(), b.version());
    }
    let old_group = &worker.waiting[&a.http_key()].connections;
    assert_eq!(old_group.len(), 1);
    assert!(
        old_group.capacity() < before / 2,
        "each surviving stream must not keep its historical audience peak"
    );
    assert_eq!(worker.waiting[&b.http_key()].connections.len(), 31);
    assert_eq!(worker.parked.len(), 32);
}
