//! Real HTTP boundary tests with synthetic credentials and private metadata.

use super::*;
use crate::{
    callback::Callbacks, config::Config, publish_sessions::PublishSessions, stream::Registry,
};
use std::collections::HashMap;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const TOKEN: &str = "synthetic-private-control-token-000000000000";
const SID: &str = "0123456789abcdef0123456789abcdef";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    app: Arc<App>,
}
impl Fixture {
    async fn start() -> Self {
        Self::start_with(Timeouts::default()).await
    }
    async fn start_with(timeouts: Timeouts) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "control-http-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private fixture");
        let root = root.canonicalize().expect("canonical fixture");
        let values = HashMap::from([
            ("SAPFLOW_CONTROL_ADDR", "127.0.0.1:0".to_owned()),
            ("SAPFLOW_CONTROL_TOKEN", TOKEN.to_owned()),
            (
                "SAPFLOW_SESSION_STORE",
                root.join("sessions.json")
                    .to_str()
                    .expect("fixture path")
                    .to_owned(),
            ),
        ]);
        let config =
            Config::load(|key| values.get(key).cloned(), Vec::new()).expect("scoped configuration");
        let sessions = PublishSessions::open(config.control.as_ref().expect("control"), false)
            .await
            .expect("authority");
        let app = Arc::new(App {
            recording_limits: Arc::new(crate::recorder::RecordingLimits::new(
                config.max_publishers,
            )),
            config,
            publish_sessions: Some(sessions),
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
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let task = tokio::spawn(serve_with(listener, app.clone(), timeouts));
        Self {
            root,
            address,
            task,
            app,
        }
    }
    async fn request(&self, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> Vec<u8> {
        let mut socket = tokio::net::TcpStream::connect(self.address)
            .await
            .expect("loopback client");
        let authorization = token.map_or(String::new(), |value| {
            format!("Authorization: Bearer {value}\r\n")
        });
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{authorization}Content-Length: {}\r\n\r\n",
            body.len()
        );
        socket
            .write_all(head.as_bytes())
            .await
            .expect("request headers");
        socket.write_all(body).await.expect("request body");
        // An early rejection can close with unread request bytes and reset TCP
        // after sending its full response. Require complete HTTP framing, not
        // a graceful socket EOF after the response.
        let mut reader = BufReader::new(socket.take(32 * 1024));
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let line_start = response.len();
                let count = reader.read_until(b'\n', &mut response).await?;
                if count == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                }
                if &response[line_start..] == b"\r\n" {
                    break;
                }
            }
            let headers = std::str::from_utf8(&response).expect("HTTP response headers");
            let length: usize = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .expect("control response content length")
                .1
                .trim()
                .parse()
                .expect("valid response length");
            let body_start = response.len();
            let total = body_start.checked_add(length).expect("bounded response");
            assert!(total <= 32 * 1024, "bounded response");
            response.resize(total, 0);
            reader.read_exact(&mut response[body_start..]).await?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .expect("bounded response")
        .expect("HTTP response");
        response
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn body(response: &[u8], expected: u16) -> &[u8] {
    let boundary = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .expect("HTTP header boundary");
    let headers = std::str::from_utf8(&response[..boundary]).expect("HTTP headers");
    assert!(
        headers.starts_with(&format!("HTTP/1.1 {expected} ")),
        "unexpected response status"
    );
    assert!(
        headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("cache-control: no-store"))
    );
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin")
    );
    &response[boundary + 4..]
}

#[tokio::test]
async fn authentication_and_body_limits_precede_mutation() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    for token in [None, Some("wrong-control-token")] {
        let response = fixture.request("PUT", &path, token, b"{").await;
        body(&response, 401);
    }
    let response = fixture.request("PUT", &path, Some(TOKEN), b"{").await;
    body(&response, 400);
    let response = fixture
        .request("PUT", &path, Some(TOKEN), &vec![b'x'; BODY_LIMIT + 1])
        .await;
    body(&response, 413);
    let response = fixture.request("GET", &path, Some(TOKEN), b"").await;
    body(&response, 404);
    assert!(
        !response
            .windows(TOKEN.len())
            .any(|part| part == TOKEN.as_bytes())
    );
}

#[tokio::test]
async fn durable_put_status_and_terminal_revoke_never_return_secrets() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_millis() as u64;
    let password = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let passphrase = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let wire = serde_json::json!({
        "contractVersion":1,"eventId":"event-control-fixture","eventGeneration":1,
        "sessionId":SID,"revision":1,"idempotencyKey":"cccccccccccccccccccccccccccccccc",
        "expiresAtUnixMs":now+60_000,"reconnectGraceMs":90_000,"transport":"srt",
        "streamPath":"live/event-control-fixture","publishUser":SID,"publishPassword":password,
        "srt":{"cipher":"aes256-ctr","passphrase":passphrase},
        "limits":{"maxPublishBps":1_000_000,"maxQueueBytes":65_536}
    });
    let bytes = serde_json::to_vec(&wire).expect("synthetic wire");
    for method in ["PUT", "GET"] {
        let response = fixture
            .request(
                method,
                &path,
                Some(TOKEN),
                if method == "PUT" { &bytes } else { b"" },
            )
            .await;
        let receipt: serde_json::Value =
            serde_json::from_slice(body(&response, 200)).expect("receipt");
        assert_eq!(receipt["state"], "prepared");
        assert_eq!(receipt["admission"], "open");
        for secret in [TOKEN, password, passphrase] {
            assert!(
                !response
                    .windows(secret.len())
                    .any(|part| part == secret.as_bytes())
            );
        }
    }
    let revoke = serde_json::to_vec(&serde_json::json!({
        "contractVersion":1,"eventId":"event-control-fixture","eventGeneration":1,
        "sessionId":SID,"revision":2,"idempotencyKey":"dddddddddddddddddddddddddddddddd"
    }))
    .expect("synthetic revoke");
    for _ in 0..2 {
        let response = fixture
            .request("POST", &format!("{path}/revoke"), Some(TOKEN), &revoke)
            .await;
        let receipt: serde_json::Value =
            serde_json::from_slice(body(&response, 200)).expect("receipt");
        assert_eq!(receipt["state"], "revoked");
        assert_eq!(receipt["admission"], "denied");
        assert_eq!(receipt["recording"], "never-started");
    }
    let response = fixture.request("PUT", &path, Some(TOKEN), &bytes).await;
    body(&response, 409);
    assert!(fixture.app.streams.get("event-control-fixture").is_none());
}

// Regression tests for the hand-written HTTP layer. They talk to the listener
// over real sockets, with raw bytes, because the framing is the point.

use crate::http_lite::MAX_HEAD;
use tokio::net::TcpStream;

async fn read_response<R: AsyncReadExt + Unpin>(reader: &mut R) -> Vec<u8> {
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut byte = [0u8; 1];
        while !response.ends_with(b"\r\n\r\n") {
            assert_eq!(reader.read(&mut byte).await?, 1, "response ended early");
            response.push(byte[0]);
        }
        let headers = std::str::from_utf8(&response).expect("HTTP response headers");
        let length: usize = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, value)| value.trim().parse().expect("length"));
        let start = response.len();
        response.resize(start + length, 0);
        reader.read_exact(&mut response[start..]).await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .expect("a response without waiting for more of the request")
    .expect("complete response");
    response
}

/// The error body of a refusal: exact JSON, no echo of the credential, and
/// the `body` helper's checks on status and headers.
fn failure_code(response: &[u8], status: u16) -> String {
    let value: serde_json::Value =
        serde_json::from_slice(body(response, status)).expect("JSON error body");
    assert_eq!(value["ok"], false);
    assert_eq!(value.as_object().expect("object").len(), 2);
    assert!(
        !response
            .windows(TOKEN.len())
            .any(|part| part == TOKEN.as_bytes())
    );
    value["error"].as_str().expect("error code").to_owned()
}

impl Fixture {
    async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address).await.expect("client")
    }
    /// Sends `bytes` and reads one answer, leaving the socket open: the
    /// answer must not depend on the client sending or closing anything more.
    async fn raw(&self, bytes: &[u8]) -> Vec<u8> {
        let mut socket = self.connect().await;
        socket.write_all(bytes).await.expect("request bytes");
        read_response(&mut socket).await
    }
}

fn authorised(method: &str, path: &str, extra: &str, length: &str) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n{extra}Content-Length: {length}\r\n\r\n"
    )
}

#[tokio::test]
async fn a_missing_or_malformed_credential_is_refused_before_the_body_is_read() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let short = &TOKEN[..TOKEN.len() - 1];
    for authorization in [
        String::new(),
        "Authorization: Bearer wrong-control-token\r\n".into(),
        "Authorization: Bearer\r\n".into(),
        "Authorization:\r\n".into(),
        format!("Authorization: Basic {TOKEN}\r\n"),
        format!("Authorization: bearer {TOKEN}\r\n"),
        format!("Authorization: Bearer  {TOKEN}\r\n"),
        format!("Authorization: Bearer {TOKEN}x\r\n"),
        format!("Authorization: Bearer {short}\r\n"),
        format!("Authorization: {TOKEN}\r\n"),
        // Two credentials are ambiguous, even when both are right.
        format!("Authorization: Bearer {TOKEN}\r\nAuthorization: Bearer {TOKEN}\r\n"),
        format!("Authorization: Bearer wrong\r\nAuthorization: Bearer {TOKEN}\r\n"),
    ] {
        // Declares a full body and sends none: the refusal must not wait.
        for (method, target) in [
            ("PUT", path.clone()),
            ("POST", format!("{path}/revoke")),
            ("GET", path.clone()),
            ("PUT", "/nowhere".to_owned()),
        ] {
            let request = format!(
                "{method} {target} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Length: {BODY_LIMIT}\r\n\r\n"
            );
            let response = fixture.raw(request.as_bytes()).await;
            assert_eq!(
                failure_code(&response, 401),
                "unauthorised",
                "{authorization:?}"
            );
        }
    }
}

#[tokio::test]
async fn authentication_comes_before_every_other_check() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    for request in [
        // An oversized length, ambiguous framing and a wrong method.
        format!(
            "PUT {path} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            BODY_LIMIT + 1
        ),
        format!("PUT {path} HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"),
        format!("PUT {path} HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
        format!("DELETE {path} HTTP/1.1\r\n\r\n"),
        "GET /nowhere HTTP/1.1\r\n\r\n".to_owned(),
        "GET /v1/publish-sessions/ HTTP/1.1\r\n\r\n".to_owned(),
    ] {
        let response = fixture.raw(request.as_bytes()).await;
        assert_eq!(failure_code(&response, 401), "unauthorised", "{request:?}");
    }
}

#[tokio::test]
async fn a_declared_length_over_the_limit_is_refused_before_any_body_is_read() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    for length in [
        (BODY_LIMIT + 1).to_string(),
        (BODY_LIMIT * 1_000).to_string(),
        u64::MAX.to_string(),
        "99999999999999999999999999999999".to_owned(),
    ] {
        // No body is sent, and the answer arrives anyway.
        let response = fixture
            .raw(authorised("PUT", &path, "", &length).as_bytes())
            .await;
        assert_eq!(failure_code(&response, 413), "body-too-large", "{length}");
    }
    // The limit itself is allowed: the body is read and found invalid.
    let response = fixture
        .request("PUT", &path, Some(TOKEN), &vec![b' '; BODY_LIMIT])
        .await;
    assert_eq!(failure_code(&response, 400), "invalid-request");
}

#[tokio::test]
async fn ambiguous_body_framing_is_refused_and_changes_nothing() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    for (extra, length) in [
        ("Content-Length: 2\r\n", "2"),
        ("Content-Length: 3\r\n", "2"),
        ("Transfer-Encoding: chunked\r\n", "2"),
        ("Transfer-Encoding: identity\r\n", "2"),
        ("", "2, 2"),
        ("", "+2"),
        ("", "-2"),
        ("", "0x2"),
        ("", "2 2"),
        ("", ""),
    ] {
        let request = format!("{}{{}}", authorised("PUT", &path, extra, length));
        let response = fixture.raw(request.as_bytes()).await;
        assert_eq!(
            failure_code(&response, 400),
            "invalid-request",
            "{extra:?} {length:?}"
        );
    }
    // A chunked body with no content-length at all.
    let chunked = format!(
        "PUT {path} HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"
    );
    let response = fixture.raw(chunked.as_bytes()).await;
    assert_eq!(failure_code(&response, 400), "invalid-request");
    let response = fixture.request("GET", &path, Some(TOKEN), b"").await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
}

#[tokio::test]
async fn bytes_after_the_declared_body_are_never_read_as_another_request() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let mut socket = fixture.connect().await;
    let smuggled = authorised("GET", &path, "", "0");
    socket
        .write_all(format!("{}{{{smuggled}", authorised("PUT", &path, "", "1")).as_bytes())
        .await
        .expect("request");
    let first = read_response(&mut socket).await;
    assert_eq!(failure_code(&first, 400), "invalid-request");
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut rest))
        .await
        .expect("connection closed")
        .expect("clean end");
    assert!(
        rest.is_empty(),
        "a second answer would mean a second request"
    );
}

#[tokio::test]
async fn an_oversized_or_flooded_head_is_refused() {
    let fixture = Fixture::start().await;
    // More headers than the limit of 32, with and without a credential.
    let flood: String = (0..40).map(|i| format!("X-Flood-{i}: v\r\n")).collect();
    for credential in [format!("Authorization: Bearer {TOKEN}\r\n"), String::new()] {
        let request = format!("GET / HTTP/1.1\r\n{credential}{flood}\r\n");
        let response = fixture.raw(request.as_bytes()).await;
        assert_eq!(failure_code(&response, 431), "request-header-too-large");
    }
    // One header over the head limit.
    let request = format!(
        "GET / HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nX-Big: {}\r\n\r\n",
        "a".repeat(MAX_HEAD)
    );
    let response = fixture.raw(request.as_bytes()).await;
    assert_eq!(failure_code(&response, 431), "request-header-too-large");
    // A head that never ends is cut off at the limit, not buffered further.
    let mut socket = fixture.connect().await;
    let endless = format!("GET / HTTP/1.1\r\nX-Big: {}", "a".repeat(MAX_HEAD));
    socket.write_all(endless.as_bytes()).await.expect("request");
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 431), "request-header-too-large");
    // A request line over its limit.
    let request = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(4096));
    let response = fixture.raw(request.as_bytes()).await;
    assert_eq!(failure_code(&response, 414), "uri-too-long");
}

#[tokio::test]
async fn requests_that_are_not_plain_origin_form_http_are_refused() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let tls_hello: &[u8] = b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03\r\n\r\n";
    let mut requests: Vec<Vec<u8>> = vec![tls_hello.to_vec()];
    requests.extend(
        [
            format!("GET http://localhost{path} HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"),
            format!("GET {path} HTTP/1.1\nAuthorization: Bearer {TOKEN}\n\n"),
            format!("GET {path} HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n\n"),
            format!("GET {path} HTTP/2.0\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"),
            format!("GET {path} HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\n folded\r\n\r\n"),
            "GET\r\n\r\n".to_owned(),
        ]
        .map(String::into_bytes),
    );
    for request in requests {
        let response = fixture.raw(&request).await;
        assert_eq!(
            failure_code(&response, 400),
            "invalid-request",
            "{:?}",
            String::from_utf8_lossy(&request)
        );
    }
}

#[tokio::test]
async fn wrong_methods_and_unknown_paths_are_refused_with_json_and_no_store() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    for (method, target, allow) in [
        ("DELETE", path.clone(), "GET, HEAD, PUT"),
        ("POST", path.clone(), "GET, HEAD, PUT"),
        ("PATCH", path.clone(), "GET, HEAD, PUT"),
        ("OPTIONS", path.clone(), "GET, HEAD, PUT"),
        ("PUT", format!("{path}/revoke"), "POST"),
        ("GET", format!("{path}/revoke"), "POST"),
        ("DELETE", format!("{path}/revoke"), "POST"),
    ] {
        let response = fixture
            .raw(authorised(method, &target, "", "0").as_bytes())
            .await;
        assert_eq!(
            failure_code(&response, 405),
            "method-not-allowed",
            "{method} {target}"
        );
        let head = String::from_utf8_lossy(&response).to_ascii_lowercase();
        assert!(
            head.contains(&format!("allow: {}\r\n", allow.to_ascii_lowercase())),
            "{head}"
        );
    }
    // As with the former GET route, HEAD runs the status handler and omits
    // its body. This session is unknown, so the status is 404, not 405.
    let mut socket = fixture.connect().await;
    socket
        .write_all(authorised("HEAD", &path, "", "0").as_bytes())
        .await
        .expect("request");
    let mut everything = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut everything))
        .await
        .expect("closed")
        .expect("clean end");
    let text = String::from_utf8_lossy(&everything).to_ascii_lowercase();
    assert!(text.starts_with("http/1.1 404 "), "{text}");
    assert!(text.contains("cache-control: no-store\r\n"));
    assert!(text.ends_with("\r\n\r\n"), "no body after the head: {text}");
    for target in [
        "/".to_owned(),
        "/v1".to_owned(),
        "/v1/publish-sessions".to_owned(),
        "/v1/publish-sessions/".to_owned(),
        format!("{path}/"),
        format!("{path}/revoke/"),
        format!("{path}/other"),
        "/v1/publish-sessions//revoke".to_owned(),
        format!("/v2/publish-sessions/{SID}"),
        format!("/{path}"),
    ] {
        let response = fixture
            .raw(authorised("GET", &target, "", "0").as_bytes())
            .await;
        assert_eq!(failure_code(&response, 404), "not-found", "{target}");
    }
}

#[tokio::test]
async fn the_query_is_ignored_and_the_id_is_validated_as_before() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let response = fixture
        .raw(authorised("GET", &format!("{path}?x=1&y=%00"), "", "0").as_bytes())
        .await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
    for id in [
        "short",
        "ZZZZ",
        "0123456789abcdef0123456789abcdeg",
        "%30123456789abcdef0123456789abcdef",
    ] {
        let response = fixture
            .raw(authorised("GET", &format!("/v1/publish-sessions/{id}"), "", "0").as_bytes())
            .await;
        assert_eq!(failure_code(&response, 400), "invalid-request", "{id}");
    }
}

#[tokio::test]
async fn one_request_per_connection_then_the_connection_closes() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let mut socket = fixture.connect().await;
    // Keep-alive is asked for and not given.
    socket
        .write_all(authorised("GET", &path, "Connection: keep-alive\r\n", "0").as_bytes())
        .await
        .expect("request");
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
    assert!(
        String::from_utf8_lossy(&response)
            .to_ascii_lowercase()
            .contains("connection: close\r\n")
    );
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut rest))
        .await
        .expect("closed")
        .expect("clean end");
    assert!(rest.is_empty());
}

#[tokio::test]
async fn a_head_and_body_arriving_in_small_pieces_are_assembled() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let request = format!("{}{{\"a\":1}}", authorised("PUT", &path, "", "7"));
    let mut socket = fixture.connect().await;
    for piece in request.as_bytes().chunks(5) {
        socket.write_all(piece).await.expect("piece");
        socket.flush().await.expect("flush");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 400), "invalid-request");
    // And a status request cut mid-head is answered once complete.
    let request = authorised("GET", &path, "", "0");
    let (first, second) = request.as_bytes().split_at(30);
    let mut socket = fixture.connect().await;
    socket.write_all(first).await.expect("first half");
    tokio::time::sleep(Duration::from_millis(20)).await;
    socket.write_all(second).await.expect("second half");
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
}

#[tokio::test]
async fn expect_continue_is_answered_only_to_an_authorised_valid_request() {
    let fixture = Fixture::start().await;
    let path = format!("/v1/publish-sessions/{SID}");
    let mut socket = fixture.connect().await;
    socket
        .write_all(authorised("PUT", &path, "Expect: 100-continue\r\n", "1").as_bytes())
        .await
        .expect("head");
    let mut interim = [0u8; 25];
    tokio::time::timeout(Duration::from_secs(3), socket.read_exact(&mut interim))
        .await
        .expect("interim answer")
        .expect("interim bytes");
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    socket.write_all(b"{").await.expect("body");
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 400), "invalid-request");
    // Refused requests get their refusal, not an invitation to send the body.
    let refused =
        format!("PUT {path} HTTP/1.1\r\nExpect: 100-continue\r\nContent-Length: 9\r\n\r\n");
    let response = fixture.raw(refused.as_bytes()).await;
    assert!(response.starts_with(b"HTTP/1.1 401 "));
    let response = fixture
        .raw(
            authorised(
                "PUT",
                &path,
                "Expect: 100-continue\r\n",
                &(BODY_LIMIT + 1).to_string(),
            )
            .as_bytes(),
        )
        .await;
    assert!(response.starts_with(b"HTTP/1.1 413 "));
}

#[tokio::test]
async fn a_client_that_never_finishes_its_head_is_timed_out() {
    let fixture = Fixture::start_with(Timeouts {
        head: Duration::from_millis(300),
        handler: Duration::from_secs(10),
    })
    .await;
    let started = std::time::Instant::now();
    // Silence.
    let mut socket = fixture.connect().await;
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 408), "read-timeout");
    assert!(started.elapsed() < Duration::from_secs(2));
    // A drip that would defeat a timer restarted by each byte.
    let started = std::time::Instant::now();
    let (mut reader, mut writer) = fixture.connect().await.into_split();
    let drip = tokio::spawn(async move {
        let head = b"GET /v1/publish-sessions/x HTTP/1.1\r\nHost: localhost\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        for byte in head {
            if writer.write_all(&[*byte]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    });
    let response = read_response(&mut reader).await;
    assert_eq!(failure_code(&response, 408), "read-timeout");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), reader.read_to_end(&mut rest))
        .await
        .expect("closed")
        .ok();
    drip.abort();
}

#[tokio::test]
async fn a_client_that_stalls_in_its_body_gets_the_handler_timeout() {
    let fixture = Fixture::start_with(Timeouts {
        head: Duration::from_secs(5),
        handler: Duration::from_millis(300),
    })
    .await;
    let path = format!("/v1/publish-sessions/{SID}");
    let started = std::time::Instant::now();
    let mut socket = fixture.connect().await;
    socket
        .write_all(format!("{}0123456789", authorised("PUT", &path, "", "100")).as_bytes())
        .await
        .expect("head and part of the body");
    let response = read_response(&mut socket).await;
    assert_eq!(failure_code(&response, 504), "request-timeout");
    assert!(started.elapsed() < Duration::from_secs(2));
    // The authority was never asked to do anything.
    let response = fixture.request("GET", &path, Some(TOKEN), b"").await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
}

#[tokio::test]
async fn silent_connections_do_not_hold_the_listener() {
    let fixture = Fixture::start_with(Timeouts {
        head: Duration::from_millis(200),
        handler: Duration::from_secs(10),
    })
    .await;
    let path = format!("/v1/publish-sessions/{SID}");
    // More silent connections than the listener serves at once.
    let held: Vec<TcpStream> = connect_many(&fixture, CONNECTIONS + 8).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    let response = fixture.request("GET", &path, Some(TOKEN), b"").await;
    assert_eq!(failure_code(&response, 404), "unknown-session");
    drop(held);
}

async fn connect_many(fixture: &Fixture, count: usize) -> Vec<TcpStream> {
    let mut sockets = Vec::new();
    for _ in 0..count {
        sockets.push(fixture.connect().await);
    }
    sockets
}

#[tokio::test]
async fn the_control_listener_refuses_a_non_loopback_address() {
    let fixture = Fixture::start().await;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .expect("listener");
    let error = serve(listener, fixture.app.clone())
        .await
        .expect_err("must not serve");
    assert!(error.to_string().contains("loopback"));
}
