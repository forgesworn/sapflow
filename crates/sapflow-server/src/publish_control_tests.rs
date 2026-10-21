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
        let task = tokio::spawn(serve(listener, app.clone()));
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
