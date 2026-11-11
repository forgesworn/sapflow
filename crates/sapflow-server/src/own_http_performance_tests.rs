//! Byte-identical conventional playlist caching with generated local media.
use super::*;
use crate::stream::{Source, SourceKind};
use crate::test_support::{LANDSCAPE, PORTRAIT, app};
use sapflow_core::StreamPath;
use sapflow_hls::MuxerConfig;

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
