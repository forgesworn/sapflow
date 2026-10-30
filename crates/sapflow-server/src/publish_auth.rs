//! Asking the app whether a publisher may publish (`SAPFLOW_PUBLISH_AUTH_URL`).
//!
//! The request is mediamtx's `authMethod: http` one, so the app serves both
//! media servers from one endpoint: a JSON `POST` with the publisher's user
//! and password as it gave them, its source address, the path and the
//! protocol. Any 2xx admits; anything else, or no answer within `TIMEOUT`,
//! refuses. The connection is plain HTTP, used once, and only the answer's
//! status line and headers are read (bounded by `http_lite::MAX_HEAD`); the
//! body is never read. A redirect is a refusal like any other status.
//!
//! The URL carries the app's token and the body carries the publisher's
//! password: neither is logged, and nothing the app answers is kept.

use std::net::IpAddr;
use std::time::Duration;

use sapflow_core::PublishRequest;

use crate::callback::{Failure, attempt};
use crate::http_lite::Endpoint;

/// The whole exchange, connect to status. An RTMP encoder gets 10 s for its
/// handshake and publish request together, so this stays well inside that.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    Srt,
    Rtmp,
}

impl Protocol {
    fn name(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Rtmp => "rtmp",
        }
    }
}

/// What the app's answer means for this publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 2xx.
    Admitted,
    /// 401 (wrong key) or 403 (a gig that cannot take a publisher): counts
    /// as a failed attempt against the address's throttle.
    Denied,
    /// 429: the app is already throttling this address.
    Throttled,
    /// Any other status, a timeout or a transport failure. Refused, but not
    /// counted against the publisher: an app restart must not lock an
    /// encoder out.
    Unavailable,
}

/// The verdict for one attempt's outcome.
pub(crate) fn classify(outcome: Result<u16, Failure>) -> Verdict {
    match outcome {
        Ok(200..=299) => Verdict::Admitted,
        Ok(401 | 403) => Verdict::Denied,
        Ok(429) => Verdict::Throttled,
        Ok(_) | Err(_) => Verdict::Unavailable,
    }
}

/// The request body. A missing user or password is sent empty, as mediamtx
/// does. `query` is sent empty: sapflow does not keep the raw RTMP query or
/// stream id, and the password already travels in `password`.
pub(crate) fn body(req: &PublishRequest, protocol: Protocol, from: IpAddr) -> Vec<u8> {
    let value = serde_json::json!({
        "user": req.user.as_deref().unwrap_or(""),
        "password": req.pass.as_deref().unwrap_or(""),
        "ip": from.to_string(),
        "action": "publish",
        "path": req.path.as_path(),
        "protocol": protocol.name(),
        "id": null,
        "query": "",
    });
    // Serialising a JSON value of strings cannot fail; an empty body would
    // only be refused by the app.
    serde_json::to_vec(&value).unwrap_or_default()
}

/// Asks the app about this publisher, once.
pub(crate) async fn ask(
    endpoint: &Endpoint,
    req: &PublishRequest,
    protocol: Protocol,
    from: IpAddr,
) -> Verdict {
    ask_within(endpoint, req, protocol, from, TIMEOUT).await
}

async fn ask_within(
    endpoint: &Endpoint,
    req: &PublishRequest,
    protocol: Protocol,
    from: IpAddr,
    limit: Duration,
) -> Verdict {
    let outcome = attempt(endpoint, &body(req, protocol, from), limit).await;
    // Refusals are logged, rate-limited, by the caller (`App::log_refusal`).
    // Only the status or the kind of failure is logged here, at debug.
    match &outcome {
        Ok(status) => tracing::debug!(
            protocol = protocol.name(),
            status,
            "publish authorisation answered"
        ),
        Err(failure) => tracing::debug!(
            protocol = protocol.name(),
            ?failure,
            "publish authorisation got no answer"
        ),
    }
    classify(outcome)
}

/// An in-process stand-in for the app's publish-auth endpoint.
#[cfg(test)]
pub(crate) mod stub {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::http_lite::Endpoint;

    /// The token the stub's URL carries, to check it never leaks.
    pub(crate) const TOKEN: &str = "stub-app-token-never-logged";

    #[derive(Clone, Copy)]
    pub(crate) enum Answer {
        Status(u16),
        /// Read the request, then never answer.
        Silent,
    }

    pub(crate) struct Stub {
        pub endpoint: Endpoint,
        /// Each request's head and body, as received.
        pub requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Stub {
        pub(crate) fn count(&self) -> usize {
            self.requests.lock().expect("stub log").len()
        }
        pub(crate) fn last(&self) -> serde_json::Value {
            self.requests
                .lock()
                .expect("stub log")
                .last()
                .expect("a request")
                .1
                .clone()
        }
    }

    fn url(port: u16) -> String {
        format!("http://127.0.0.1:{port}/api/media/publish-auth?token={TOKEN}&server=sapflow")
    }

    /// An endpoint where nothing listens.
    pub(crate) async fn unreachable() -> Endpoint {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        Endpoint::parse(&url(port)).expect("stub URL")
    }

    pub(crate) async fn start(answer: Answer) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head, body) = loop {
                        let Ok(n) = socket.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).into_owned();
                        let Some((head, body)) = text.split_once("\r\n\r\n") else {
                            continue;
                        };
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if body.len() >= length {
                            break (head.to_owned(), body.to_owned());
                        }
                    };
                    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                    log.lock().expect("stub log").push((head, json));
                    match answer {
                        Answer::Status(status) => {
                            let reply = format!(
                                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            );
                            let _ = socket.write_all(reply.as_bytes()).await;
                        }
                        Answer::Silent => {
                            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        }
                    }
                });
            }
        });
        Stub {
            endpoint: Endpoint::parse(&url(port)).expect("stub URL"),
            requests,
            task,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use sapflow_core::{PublishError, PublishRequest};

    use super::stub::{self, Answer, TOKEN};
    use super::*;
    use crate::publish::{Publication, Refusal, loss_tests::Fixture};
    use crate::stream::SourceKind;

    const PASSWORD: &str = "per-gig-secret-never-logged";

    fn srt(stream_id: &str) -> PublishRequest {
        PublishRequest::parse_srt_stream_id(stream_id).expect("request")
    }

    fn from() -> IpAddr {
        "198.51.100.7".parse().expect("documentation address")
    }

    /// An app whose publishers are authorised by `endpoint`.
    fn fixture(endpoint: Endpoint) -> Fixture {
        let mut fixture = Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.config.publish_auth = Some(endpoint);
        fixture
    }

    #[test]
    fn statuses_map_to_verdicts() {
        for status in [200, 204, 299] {
            assert_eq!(classify(Ok(status)), Verdict::Admitted, "{status}");
        }
        for status in [401, 403] {
            assert_eq!(classify(Ok(status)), Verdict::Denied, "{status}");
        }
        assert_eq!(classify(Ok(429)), Verdict::Throttled);
        for status in [101, 199, 301, 302, 400, 404, 409, 500, 502, 503] {
            assert_eq!(classify(Ok(status)), Verdict::Unavailable, "{status}");
        }
        assert_eq!(classify(Err(Failure::Timeout)), Verdict::Unavailable);
        assert_eq!(classify(Err(Failure::Transport)), Verdict::Unavailable);
    }

    #[tokio::test]
    async fn every_credential_form_reaches_the_app_as_mediamtx_sends_it() {
        let stub = stub::start(Answer::Status(200)).await;
        for (req, protocol, user) in [
            (
                srt(&format!("publish:live/event-a1:encoder:{PASSWORD}")),
                Protocol::Srt,
                "encoder",
            ),
            (
                srt(&format!(
                    "#!::m=publish,r=live/event-a1,u=encoder,s={PASSWORD}"
                )),
                Protocol::Srt,
                "encoder",
            ),
            (
                PublishRequest::parse_rtmp(
                    "live",
                    &format!("event-a1?user=encoder&pass={PASSWORD}"),
                )
                .expect("rtmp"),
                Protocol::Rtmp,
                "encoder",
            ),
            // No user name: sent empty, the password still reaches the app.
            (
                PublishRequest::parse_rtmp("live", &format!("event-a1?pass={PASSWORD}"))
                    .expect("rtmp"),
                Protocol::Rtmp,
                "",
            ),
        ] {
            assert_eq!(
                ask(&stub.endpoint, &req, protocol, from()).await,
                Verdict::Admitted
            );
            assert_eq!(
                stub.last(),
                serde_json::json!({
                    "user": user, "password": PASSWORD, "ip": "198.51.100.7",
                    "action": "publish", "path": "live/event-a1",
                    "protocol": protocol.name(), "id": null, "query": "",
                })
            );
        }
        // Percent-encoded RTMP credentials reach the app decoded.
        let req =
            PublishRequest::parse_rtmp("live", "event-a1?user=u&pass=s%26cret").expect("rtmp");
        ask(&stub.endpoint, &req, Protocol::Rtmp, from()).await;
        assert_eq!(stub.last()["password"], "s&cret");
        // No credentials at all: still asked, with both empty.
        ask(
            &stub.endpoint,
            &srt("publish:live/event-a1"),
            Protocol::Srt,
            from(),
        )
        .await;
        assert_eq!(
            (stub.last()["user"].clone(), stub.last()["password"].clone()),
            ("".into(), "".into())
        );
        let requests = stub.requests.lock().expect("log");
        let (head, _) = requests.first().expect("request");
        assert!(head.starts_with(&format!(
            "POST /api/media/publish-auth?token={TOKEN}&server=sapflow HTTP/1.1\r\n"
        )));
        assert!(head.contains("\r\ncontent-type: application/json\r\n"));
    }

    #[tokio::test]
    async fn an_admitted_publisher_claims_its_path() {
        let stub = stub::start(Answer::Status(204)).await;
        let fixture = fixture(stub.endpoint.clone());
        let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
        let publication = Publication::begin_srt(&fixture.app, &req, from())
            .await
            .expect("admitted by the app");
        assert_eq!(publication.stream.path.as_path(), "live/event-a");
        assert_eq!(stub.count(), 1);
        publication.end();
        // The synchronous entry point never admits in this mode, so a caller
        // that forgets to ask the app cannot fall through to open publishing.
        assert!(matches!(
            Publication::begin(&fixture.app, &req, SourceKind::Flv, from()),
            Err(Refusal::Request(PublishError::Unauthorized))
        ));
        assert_eq!(stub.count(), 1);
    }

    #[tokio::test]
    async fn wrong_keys_are_refused_and_throttled() {
        for status in [401, 403] {
            let stub = stub::start(Answer::Status(status)).await;
            let fixture = fixture(stub.endpoint.clone());
            let req = srt("publish:live/event-a:encoder:wrong-key");
            // The fixture's throttle allows ten failures a minute.
            for _ in 0..10 {
                assert!(matches!(
                    Publication::begin_authorised(&fixture.app, &req, SourceKind::Ts, from()).await,
                    Err(Refusal::Request(PublishError::Unauthorized))
                ));
            }
            assert!(matches!(
                Publication::begin_authorised(&fixture.app, &req, SourceKind::Flv, from()).await,
                Err(Refusal::Throttled)
            ));
            // The throttled attempt never reached the app.
            assert_eq!(stub.count(), 10, "{status}");
            assert!(fixture.app.streams.get("event-a").is_none());
        }
    }

    #[tokio::test]
    async fn an_app_that_is_down_refuses_without_throttling() {
        for answer in [
            Answer::Status(500),
            Answer::Status(503),
            Answer::Status(302),
            Answer::Status(429),
        ] {
            let stub = stub::start(answer).await;
            let fixture = fixture(stub.endpoint.clone());
            let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
            for _ in 0..12 {
                let refusal =
                    Publication::begin_authorised(&fixture.app, &req, SourceKind::Ts, from())
                        .await
                        .err()
                        .expect("refused");
                assert!(matches!(
                    refusal,
                    Refusal::AuthUnavailable | Refusal::Throttled
                ));
            }
            // Past the local throttle's ten: every attempt still reached the app.
            assert_eq!(stub.count(), 12);
            assert!(fixture.app.streams.get("event-a").is_none());
        }
        let fixture = fixture(stub::unreachable().await);
        let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
        for _ in 0..12 {
            assert!(matches!(
                Publication::begin_authorised(&fixture.app, &req, SourceKind::Ts, from()).await,
                Err(Refusal::AuthUnavailable)
            ));
        }
    }

    #[tokio::test]
    async fn a_silent_app_times_out_and_refuses() {
        let stub = stub::start(Answer::Silent).await;
        let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
        let started = std::time::Instant::now();
        assert_eq!(
            ask_within(
                &stub.endpoint,
                &req,
                Protocol::Srt,
                from(),
                Duration::from_millis(200)
            )
            .await,
            Verdict::Unavailable
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(stub.count(), 1);
    }

    /// A tracing writer that keeps everything written to it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn neither_the_password_nor_the_url_reaches_logs_or_refusals() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
        let mut refusals = Vec::new();
        for answer in [
            Answer::Status(401),
            Answer::Status(500),
            Answer::Status(200),
        ] {
            let stub = stub::start(answer).await;
            let fixture = fixture(stub.endpoint.clone());
            match Publication::begin_srt(&fixture.app, &req, from()).await {
                Ok(publication) => publication.end(),
                Err(refusal) => {
                    fixture.app.log_refusal("srt", &refusal);
                    refusals.push(format!("{refusal:?}"));
                }
            }
            refusals.push(format!("{:?}", fixture.app.config));
            refusals.push(format!("{:?}", stub.endpoint));
        }
        let fixture = fixture(stub::unreachable().await);
        let refusal = Publication::begin_srt(&fixture.app, &req, from())
            .await
            .err()
            .expect("refused");
        fixture.app.log_refusal("srt", &refusal);
        refusals.push(format!("{refusal:?}"));
        let logs = String::from_utf8(captured.0.lock().expect("log").clone()).expect("UTF-8");
        assert!(
            logs.contains("publish authorisation"),
            "the paths were logged: {logs}"
        );
        for text in refusals.iter().chain([&logs]) {
            assert!(!text.contains(PASSWORD), "{text}");
            assert!(!text.contains(TOKEN), "{text}");
            assert!(!text.contains("publish-auth?"), "{text}");
        }
    }
}
