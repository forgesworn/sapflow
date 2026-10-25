//! Real viewer sockets against the same generated-media presentation. These
//! tests compare immutable bytes, not merely two successful playlist statuses.
use crate::http::epoch_tests::{PORTRAIT, app, begin};
use crate::stream::{Source, SourceKind};
use crate::{App, http, own_http};
use bytes::Bytes;
use sapflow_core::StreamPath;
use sapflow_hls::muxer::Entry;
use sapflow_hls::{PartLookup, TrackSet, playlist};
use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const AUDIO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts");
const EVENT: &str = "event-epochs";
const BODY_LIMIT: usize = 8 * 1024 * 1024;

struct Backends {
    addresses: [SocketAddr; 2],
    hyper: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Backends {
    async fn start(app: Arc<App>) -> Self {
        let hyper = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private hyper listener");
        let hyper_address = hyper.local_addr().expect("hyper address");
        let router = http::router(app.clone());
        let task = tokio::spawn(http::serve(hyper, router, std::future::pending::<()>()));
        let own = std::net::TcpListener::bind("127.0.0.1:0").expect("private own listener");
        let own_address = own.local_addr().expect("own address");
        // The production own worker is process-lifetime, like the existing
        // own_http tests. The test binary's exit closes its listener/threads.
        own_http::spawn(app, own, 1, own_http::Timeouts::default()).expect("own worker");
        Self {
            addresses: [hyper_address, own_address],
            hyper: task,
        }
    }

    fn parity(&self, path: &str, expected: &Bytes, content_type: &str, cache: &str) {
        for address in self.addresses {
            let get = Pending::new(address, "GET", path).response(false);
            assert_eq!(get.status, 200, "GET {path} on {address}");
            assert_eq!(get.body.as_slice(), expected.as_ref(), "{path}");
            assert_eq!(get.header("content-type"), content_type);
            assert_eq!(get.header("cache-control"), cache);
            assert_eq!(get.header("access-control-allow-origin"), "*");
            let head = Pending::new(address, "HEAD", path).response(true);
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
    }

    fn missing(&self, path: &str) {
        self.status(path, 404);
    }

    fn status(&self, path: &str, expected: u16) {
        for address in self.addresses {
            for method in ["GET", "HEAD"] {
                assert_eq!(
                    Pending::new(address, method, path)
                        .response(method == "HEAD")
                        .status,
                    expected,
                    "{method} {path} on {address}"
                );
            }
        }
    }

    fn full_range(&self, path: &str, expected: &Bytes) {
        // Both HLS backends currently ignore Range and return the full 200.
        for address in self.addresses {
            for method in ["GET", "HEAD"] {
                let response = Pending::with_headers(address, method, path, "Range: bytes=1-3\r\n")
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
}

impl Drop for Backends {
    fn drop(&mut self) {
        self.hyper.abort();
    }
}

struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, key: &str) -> &str {
        self.headers.get(key).expect("required response header")
    }
}

struct Pending(BufReader<TcpStream>);

impl Pending {
    fn new(address: SocketAddr, method: &str, path: &str) -> Self {
        Self::with_headers(address, method, path, "")
    }

    fn with_headers(address: SocketAddr, method: &str, path: &str, headers: &str) -> Self {
        let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .expect("bounded viewer connect");
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("read deadline");
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("write deadline");
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: fixture.invalid\r\nConnection: close\r\n{headers}\r\n"
        )
        .expect("viewer request");
        Self(BufReader::new(socket))
    }

    fn assert_waiting(&mut self) {
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("short pending probe");
        let mut byte = [0];
        let error = self
            .0
            .get_ref()
            .peek(&mut byte)
            .expect_err("preload request must not respond before the terminal part exists");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("restore request deadline");
    }

    fn response(mut self, head_only: bool) -> Response {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            assert!(head.len() < 16 * 1024, "response head bound");
            let mut byte = [0];
            self.0
                .read_exact(&mut byte)
                .expect("complete response head");
            head.push(byte[0]);
        }
        let text = std::str::from_utf8(&head).expect("ASCII response head");
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .expect("status line")
            .split_whitespace()
            .nth(1)
            .expect("status")
            .parse()
            .expect("numeric status");
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').expect("header field");
            assert!(
                headers
                    .insert(name.to_ascii_lowercase(), value.trim().into())
                    .is_none()
            );
        }
        let length = headers
            .get("content-length")
            .expect("explicit response length")
            .parse::<usize>()
            .expect("bounded response length");
        assert!(length <= BODY_LIMIT);
        let mut body = vec![0; if head_only { 0 } else { length }];
        self.0
            .read_exact(&mut body)
            .expect("complete response body");
        // Connection: close also proves HEAD did not put its declared body on
        // the wire. A reset after the framed response is allowed, not required.
        let mut extra = [0];
        match self.0.read(&mut extra) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("unexpected bytes/error after framed response: {other:?}"),
        }
        Response {
            status,
            headers,
            body,
        }
    }
}

fn feed(publication: &mut crate::publish::Publication, bytes: &[u8]) {
    publication
        .push(|source, now| match source {
            Source::Ts(ts) => ts.push(bytes, now),
            _ => unreachable!(),
        })
        .expect("generated MPEG-TS input");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_http_backends_preserve_aac_terminal_and_retained_codec_epoch_bytes() {
    let (app, _scratch) = app(false);
    let backends = Backends::start(app.clone()).await;
    // Both are public viewer listeners, never private session/control APIs.
    for path in [
        "/v1/publish-sessions/0123456789abcdef0123456789abcdef",
        "/v1/publish-sessions/0123456789abcdef0123456789abcdef/revoke",
        "/api/native/live-events/event-epochs/session",
    ] {
        backends.missing(path);
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
        backends.status(&format!("/live/{EVENT}/stream.m3u8?{query}"), 400);
    }
    // Invalid query validation still precedes the unknown-stream lookup.
    backends.status("/live/event-absent/stream.m3u8?_HLS_msn=invalid", 400);
    backends.parity(
        &format!("/live/{EVENT}/stream.m3u8"),
        &media,
        "application/vnd.apple.mpegurl",
        "no-cache",
    );
    backends.parity(
        &format!("/live/{EVENT}/{}", playlist::init_name(&prefix)),
        &init,
        "video/mp4",
        "public, max-age=3600",
    );
    let path = format!("/live/{EVENT}/{}", playlist::part_name(&prefix, hint));
    let mut parked = backends
        .addresses
        .map(|address| Pending::new(address, "GET", &path));
    for request in &mut parked {
        request.assert_waiting();
    }
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
    for request in parked {
        let response = request.response(false);
        assert_eq!(response.status, 200);
        assert_eq!(response.body.as_slice(), terminal.as_ref());
        assert_eq!(
            response.header("content-length"),
            terminal.len().to_string()
        );
    }
    backends.parity(&path, &terminal, "video/mp4", "public, max-age=3600");
    close_held(&app);
    backends.missing(&path);
    backends.missing(&format!("/live/{EVENT}/stream.m3u8"));

    let mut video = begin(&app);
    feed(&mut video, PORTRAIT);
    backends.missing(&path); // A new presentation cannot serve the old AAC URI.
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
    backends.full_range(
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
        backends.parity(
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
        backends.missing(&format!("/live/{EVENT}/{file}"));
    }
}
