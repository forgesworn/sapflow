//! Real viewer sockets against generated-media presentations. These tests
//! compare immutable bytes, not merely successful playlist statuses.
use crate::App;
use crate::stream::SourceKind;
use crate::test_support::{AUDIO, EVENT, PORTRAIT, Pending, app, begin, feed, viewer};
use bytes::Bytes;
use sapflow_core::StreamPath;
use sapflow_hls::muxer::Entry;
use sapflow_hls::{PartLookup, TrackSet, playlist};
use std::net::SocketAddr;
use std::sync::Arc;

/// The viewer event loop on a private port.
struct Viewer(SocketAddr);

impl Viewer {
    fn start(app: &Arc<App>) -> Self {
        Self(viewer(app))
    }

    /// GET and HEAD agree, carry the same metadata, and GET returns exactly
    /// the expected bytes.
    fn served(&self, path: &str, expected: &Bytes, content_type: &str, cache: &str) {
        let get = Pending::new(self.0, "GET", path).response(false);
        assert_eq!(get.status, 200, "GET {path}");
        assert_eq!(get.body.as_slice(), expected.as_ref(), "{path}");
        assert_eq!(get.header("content-type"), content_type);
        assert_eq!(get.header("cache-control"), cache);
        assert_eq!(get.header("access-control-allow-origin"), "*");
        let head = Pending::new(self.0, "HEAD", path).response(true);
        assert_eq!(head.status, get.status);
        for name in [
            "content-type",
            "cache-control",
            "content-length",
            "access-control-allow-origin",
        ] {
            assert_eq!(head.header(name), get.header(name), "HEAD {path}: {name}");
        }
        assert!(head.body.is_empty());
    }

    fn missing(&self, path: &str) {
        self.status(path, 404);
    }

    fn status(&self, path: &str, expected: u16) {
        for method in ["GET", "HEAD"] {
            assert_eq!(
                Pending::new(self.0, method, path)
                    .response(method == "HEAD")
                    .status,
                expected,
                "{method} {path}"
            );
        }
    }

    fn full_range(&self, path: &str, expected: &Bytes) {
        // The viewer path ignores Range and returns the full 200.
        for method in ["GET", "HEAD"] {
            let response = Pending::with_headers(self.0, method, path, "Range: bytes=1-3\r\n")
                .response(method == "HEAD");
            assert_eq!(response.status, 200);
            assert_eq!(
                response.header("content-length"),
                expected.len().to_string()
            );
            assert!(!response.headers.contains_key("content-range"));
            assert!(!response.headers.contains_key("transfer-encoding"));
            if method == "GET" {
                assert_eq!(response.body.as_slice(), expected.as_ref());
            } else {
                assert!(response.body.is_empty());
            }
        }
    }
}

fn close_held(app: &App) {
    let path = StreamPath::parse("live/event-epochs").expect("fixture path");
    let (stream, generation, _) = app
        .streams
        .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Ts)
        .expect("held presentation claim");
    app.streams.detach(&stream, generation, false);
    assert!(stream.lock().closed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_preserves_aac_terminal_and_retained_codec_epoch_bytes() {
    let (app, _scratch) = app(false);
    let viewer = Viewer::start(&app);
    // The viewer listener is public, never the private session/control API.
    for path in [
        "/v1/publish-sessions/0123456789abcdef0123456789abcdef",
        "/v1/publish-sessions/0123456789abcdef0123456789abcdef/revoke",
        "/api/native/live-events/event-epochs/session",
    ] {
        viewer.missing(path);
    }
    let mut audio = begin(&app);
    feed(&mut audio, AUDIO);
    let stream = audio.stream.clone();
    let (prefix, init, media, hint) = {
        let state = stream.lock();
        let muxer = state.ingest.muxer().expect("live AAC muxer");
        assert_eq!(muxer.tracks(), TrackSet::AudioOnly);
        assert!(muxer.video().is_none());
        let hint = muxer.next_part_id();
        assert!(matches!(muxer.part(hint), PartLookup::Next));
        (
            state.prefix.clone(),
            muxer.init().clone(),
            state.media.clone().expect("AAC playlist"),
            hint,
        )
    };
    for query in [
        "_HLS_msn=not-a-number",
        "_HLS_msn=18446744073709551616",
        "_HLS_part=1",
        "_HLS_msn=999999&_HLS_part=0",
    ] {
        viewer.status(&format!("/live/{EVENT}/stream.m3u8?{query}"), 400);
    }
    // Invalid query validation still precedes the unknown-stream lookup.
    viewer.status("/live/event-absent/stream.m3u8?_HLS_msn=invalid", 400);
    viewer.served(
        &format!("/live/{EVENT}/stream.m3u8"),
        &media,
        "application/vnd.apple.mpegurl",
        "no-cache",
    );
    viewer.served(
        &format!("/live/{EVENT}/{}", playlist::init_name(&prefix)),
        &init,
        "video/mp4",
        "public, max-age=3600",
    );
    let path = format!("/live/{EVENT}/{}", playlist::part_name(&prefix, hint));
    let mut parked = Pending::new(viewer.0, "GET", &path);
    parked.assert_waiting();
    audio.end();
    let terminal = {
        let state = stream.lock();
        let muxer = state.ingest.muxer().expect("terminal AAC muxer");
        let PartLookup::Found(part) = muxer.part(hint) else {
            panic!("EOF must seal the pending AAC part")
        };
        assert!(part.independent);
        part.data.clone()
    };
    let response = parked.response(false);
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_slice(), terminal.as_ref());
    assert_eq!(
        response.header("content-length"),
        terminal.len().to_string()
    );
    viewer.served(&path, &terminal, "video/mp4", "public, max-age=3600");
    close_held(&app);
    viewer.missing(&path);
    viewer.missing(&format!("/live/{EVENT}/stream.m3u8"));

    let mut video = begin(&app);
    feed(&mut video, PORTRAIT);
    viewer.missing(&path); // A new presentation cannot serve the old AAC URI.
    let retained_stream = video.stream.clone();
    let (prefix, av_init, av_part, av_segment) = {
        let state = retained_stream.lock();
        let muxer = state.ingest.muxer().expect("AV muxer");
        assert_eq!(muxer.tracks(), TrackSet::AudioVideo);
        let segment = muxer
            .entries()
            .iter()
            .find_map(|entry| match entry {
                Entry::Segment(segment) => Some(segment),
                _ => None,
            })
            .expect("completed AV segment");
        let part = segment.parts.first().expect("completed AV part");
        (
            state.prefix.clone(),
            muxer.init().clone(),
            (part.id, part.data.clone()),
            (segment.id, {
                assert!(
                    segment.data.chunks().len() > 1,
                    "multi-part segment fixture"
                );
                let mut joined = Vec::with_capacity(segment.data.len());
                for chunk in segment.data.chunks() {
                    joined.extend_from_slice(chunk);
                }
                Bytes::from(joined) // Test oracle only; neither server joins media.
            }),
        )
    };
    video.end();
    let mut replacement = begin(&app);
    assert!(Arc::ptr_eq(&retained_stream, &replacement.stream));
    feed(&mut replacement, AUDIO);
    let (audio_init_id, audio_init, audio_part) = {
        let state = retained_stream.lock();
        assert_eq!(state.prefix, prefix);
        let muxer = state.ingest.muxer().expect("replacement AAC muxer");
        assert_eq!(muxer.tracks(), TrackSet::AudioOnly);
        let init_id = muxer.current_initialization_id();
        assert_eq!(init_id, 1);
        let part = muxer
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                Entry::Segment(segment) => Some(segment),
                _ => None,
            })
            .flat_map(|segment| &segment.parts)
            .find(|part| part.initialization.id == init_id)
            .expect("AAC epoch part");
        (init_id, muxer.init().clone(), (part.id, part.data.clone()))
    };
    viewer.full_range(
        &format!(
            "/live/{EVENT}/{}",
            playlist::segment_name(&prefix, av_segment.0)
        ),
        &av_segment.1,
    );
    for (file, bytes) in [
        (playlist::init_name(&prefix), av_init),
        (playlist::part_name(&prefix, av_part.0), av_part.1),
        (playlist::segment_name(&prefix, av_segment.0), av_segment.1),
        (
            playlist::epoch_init_name(&prefix, audio_init_id),
            audio_init,
        ),
        (playlist::part_name(&prefix, audio_part.0), audio_part.1),
    ] {
        viewer.served(
            &format!("/live/{EVENT}/{file}"),
            &bytes,
            "video/mp4",
            "public, max-age=3600",
        );
    }
    replacement.end();
    close_held(&app);
    for file in [
        playlist::init_name(&prefix),
        playlist::epoch_init_name(&prefix, audio_init_id),
        playlist::part_name(&prefix, av_part.0),
        playlist::part_name(&prefix, audio_part.0),
    ] {
        viewer.missing(&format!("/live/{EVENT}/{file}"));
    }
}
