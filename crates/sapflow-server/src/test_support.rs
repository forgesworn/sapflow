//! Shared fixtures for the viewer tests: generated media, a throwaway `App`,
//! a publication, a running viewer event loop and a plain-socket client.

use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use sapflow_core::PublishRequest;

use crate::App;
use crate::callback::Callbacks;
use crate::config::Config;
use crate::own_http;
use crate::publish::Publication;
use crate::stream::{Registry, Source, SourceKind};

pub(crate) const PORTRAIT: &[u8] =
    include_bytes!("../../sapflow-hls/tests/fixtures/epoch-portrait.ts");
pub(crate) const LANDSCAPE: &[u8] =
    include_bytes!("../../sapflow-hls/tests/fixtures/epoch-landscape.ts");
pub(crate) const AUDIO: &[u8] =
    include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts");
/// The stream the fixtures publish to: `live/event-epochs`.
pub(crate) const EVENT: &str = "event-epochs";

const BODY_LIMIT: usize = 8 * 1024 * 1024;
static TEMP: AtomicU64 = AtomicU64::new(0);

/// A task-owned scratch directory, removed on drop.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn app(record: bool) -> (Arc<App>, Scratch) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "origin-epoch-test-{}-{}",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir(&root).expect("new task-owned scratch");
    let mut config = Config::load(
        |key| match key {
            "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
            "SAPFLOW_RECORDINGS" => Some(
                root.to_str()
                    .expect("valid epoch regression fixture")
                    .into(),
            ),
            "SAPFLOW_RECORD_MIN_FREE_GB" => Some("0".into()),
            _ => None,
        },
        Vec::new(),
    )
    .expect("valid epoch regression fixture");
    if !record {
        config.recordings = None;
    }
    (
        Arc::new(App {
            recording_limits: Arc::new(crate::recorder::RecordingLimits::new(
                config.max_publishers,
            )),
            publish_sessions: None,
            config,
            streams: Arc::new(Registry::default()),
            callbacks: Callbacks::start(None, None),
            auth_throttle: Mutex::new(sapflow_core::limits::AuthThrottle::new(
                10,
                Duration::from_secs(60),
            )),
            refusal_log: Mutex::new(sapflow_core::limits::LogLimiter::new(
                20,
                Duration::from_secs(60),
            )),
        }),
        Scratch(root),
    )
}

pub(crate) fn begin(app: &Arc<App>) -> Publication {
    let req = PublishRequest::parse_srt_stream_id("publish:live/event-epochs:fixture:fixture")
        .expect("valid epoch regression fixture");
    Publication::begin(
        app,
        &req,
        SourceKind::Ts,
        "127.0.0.1".parse().expect("valid epoch regression fixture"),
    )
    .expect("valid epoch regression fixture")
}

/// Pushes generated MPEG-TS into a publication.
pub(crate) fn feed(publication: &mut Publication, bytes: &[u8]) {
    publication
        .push(|source, now| match source {
            Source::Ts(ts) => ts.push(bytes, now),
            _ => unreachable!(),
        })
        .expect("generated MPEG-TS input");
}

/// Starts one viewer worker on a private loopback port. Like the production
/// worker it lives for the process; the test binary's exit closes it.
pub(crate) fn viewer(app: &Arc<App>) -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("private viewer listener");
    let address = listener.local_addr().expect("viewer address");
    own_http::spawn(app.clone(), listener, 1, own_http::Timeouts::default())
        .expect("viewer worker");
    address
}

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    pub(crate) fn header(&self, key: &str) -> &str {
        self.headers.get(key).expect("required response header")
    }
}

/// A request already sent on a `Connection: close` socket, whose response is
/// read later: a viewer that may be parked.
pub(crate) struct Pending(BufReader<TcpStream>);

impl Pending {
    pub(crate) fn new(address: SocketAddr, method: &str, path: &str) -> Self {
        Self::with_headers(address, method, path, "")
    }

    pub(crate) fn with_headers(
        address: SocketAddr,
        method: &str,
        path: &str,
        headers: &str,
    ) -> Self {
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

    /// Asserts that no response byte has arrived yet.
    pub(crate) fn assert_waiting(&mut self) {
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("short pending probe");
        let mut byte = [0];
        let error = self
            .0
            .get_ref()
            .peek(&mut byte)
            .expect_err("a parked request must not respond before its media exists");
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("restore request deadline");
    }

    pub(crate) fn response(mut self, head_only: bool) -> Response {
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
