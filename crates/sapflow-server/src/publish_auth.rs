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
//! password: neither is logged, and nothing the app answers is kept beyond
//! the admission it decides.
//!
//! With `SAPFLOW_SRT_APP_KEYS=1`, an SRT request also carries
//! `"srtKey":"accepted"` ([`SRT_KEY_FIELD`]): sapflow can check a passphrase
//! the app holds for this stream. The app may then answer 2xx with that
//! passphrase in one `sapflow-srt-passphrase` header
//! ([`SRT_PASSPHRASE_HEADER`]), 10 to 79 bytes once the spaces and tabs
//! around it are dropped; the caller is admitted only if
//! its SRT key material was made with it. The value goes straight into
//! zeroising storage and is never logged. A header that is repeated or out of
//! bounds refuses the publisher as an app that did not answer usably would.
//! RTMP and WHIP requests never carry the field, and their answers' headers
//! are not read.

use std::net::IpAddr;
use std::time::Duration;

use sapflow_core::PublishRequest;

use sapflow_srt::crypto::{CryptoConfig, KeySize};

use crate::callback::{Failure, HeaderRead, attempt, attempt_reading};
use crate::http_lite::Endpoint;

/// The request field, and its value, that says this sapflow checks an SRT
/// passphrase the app returns. Set by the binary, never by configuration.
pub(crate) const SRT_KEY_FIELD: (&str, &str) = ("srtKey", "accepted");

/// The 2xx answer's header that carries the stream's SRT passphrase.
pub(crate) const SRT_PASSPHRASE_HEADER: &str = "sapflow-srt-passphrase";

/// The whole exchange, connect to status. An RTMP encoder gets 10 s for its
/// handshake and publish request together, so this stays well inside that.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    Srt,
    Rtmp,
    /// WHIP, by the name mediamtx gives it.
    Webrtc,
}

impl Protocol {
    fn name(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Rtmp => "rtmp",
            Self::Webrtc => "webrtc",
        }
    }
}

/// What the app's answer means for this publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// 2xx.
    Admitted,
    /// 401 (wrong key): counts as a failed attempt against the address's
    /// throttle.
    Denied,
    /// 403 (a gig that cannot take a publisher): counts as a failed attempt
    /// too, and WHIP answers it 403 rather than 401.
    Forbidden,
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
        Ok(401) => Verdict::Denied,
        Ok(403) => Verdict::Forbidden,
        Ok(429) => Verdict::Throttled,
        Ok(_) | Err(_) => Verdict::Unavailable,
    }
}

/// The request body. A missing user or password is sent empty, as mediamtx
/// does. `query` is sent empty: sapflow does not keep the raw RTMP query or
/// stream id, and the password already travels in `password`.
pub(crate) fn body(req: &PublishRequest, protocol: Protocol, from: IpAddr) -> Vec<u8> {
    to_bytes(&value(req, protocol, from))
}

/// [`body`] for SRT with [`SRT_KEY_FIELD`] added.
fn body_offering_srt_key(req: &PublishRequest, from: IpAddr) -> Vec<u8> {
    let mut value = value(req, Protocol::Srt, from);
    if let Some(map) = value.as_object_mut() {
        map.insert(SRT_KEY_FIELD.0.into(), SRT_KEY_FIELD.1.into());
    }
    to_bytes(&value)
}

fn value(req: &PublishRequest, protocol: Protocol, from: IpAddr) -> serde_json::Value {
    serde_json::json!({
        "user": req.user.as_deref().unwrap_or(""),
        "password": req.pass.as_deref().unwrap_or(""),
        "ip": from.to_string(),
        "action": "publish",
        "path": req.path.as_path(),
        "protocol": protocol.name(),
        "id": null,
        "query": "",
    })
}

fn to_bytes(value: &serde_json::Value) -> Vec<u8> {
    // Serialising a JSON value of strings cannot fail; an empty body would
    // only be refused by the app.
    serde_json::to_vec(value).unwrap_or_default()
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
    log_outcome(protocol, &outcome);
    classify(outcome)
}

/// Asks the app about an SRT publisher, once, offering to check a passphrase
/// it holds ([`SRT_KEY_FIELD`]). Returns the verdict and, on 2xx, the
/// passphrase from [`SRT_PASSPHRASE_HEADER`] if the app sent one. The key
/// length in it is a placeholder: the caller's own is used.
pub(crate) async fn ask_srt_key(
    endpoint: &Endpoint,
    req: &PublishRequest,
    from: IpAddr,
) -> (Verdict, Option<CryptoConfig>) {
    ask_srt_key_within(endpoint, req, from, TIMEOUT).await
}

async fn ask_srt_key_within(
    endpoint: &Endpoint,
    req: &PublishRequest,
    from: IpAddr,
    limit: Duration,
) -> (Verdict, Option<CryptoConfig>) {
    let outcome = attempt_reading(
        endpoint,
        &body_offering_srt_key(req, from),
        limit,
        Some(SRT_PASSPHRASE_HEADER),
    )
    .await;
    let (outcome, header) = match outcome {
        Ok((status, header)) => (Ok(status), header),
        Err(failure) => (Err(failure), HeaderRead::Absent),
    };
    log_outcome(Protocol::Srt, &outcome);
    let verdict = classify(outcome);
    if verdict != Verdict::Admitted {
        return (verdict, None);
    }
    match header {
        HeaderRead::Absent => (verdict, None),
        // `CryptoConfig::new` takes the bytes into its own zeroising storage
        // and checks the SRT bounds: 10 to 79 bytes, no NUL.
        HeaderRead::One(mut value) => {
            match CryptoConfig::new(std::mem::take(&mut *value), KeySize::Aes128) {
                Ok(key) => (verdict, Some(key)),
                Err(_) => {
                    tracing::debug!(
                        "publish authorisation answered with an unusable SRT passphrase"
                    );
                    (Verdict::Unavailable, None)
                }
            }
        }
        HeaderRead::Repeated => {
            tracing::debug!("publish authorisation answered with a repeated SRT passphrase header");
            (Verdict::Unavailable, None)
        }
    }
}

fn log_outcome(protocol: Protocol, outcome: &Result<u16, Failure>) {
    // Refusals are logged, rate-limited, by the caller (`App::log_refusal`).
    // Only the status or the kind of failure is logged here, at debug.
    match outcome {
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
        /// 200 with these `sapflow-srt-passphrase` header values, one line
        /// each.
        Passphrase(&'static [&'static str]),
        /// As `Passphrase`, after this many milliseconds.
        SlowPassphrase(u64, &'static [&'static str]),
        /// Read the request, then never answer.
        Silent,
    }

    pub(crate) struct Stub {
        pub endpoint: Endpoint,
        /// The endpoint's URL, for a test's `SAPFLOW_PUBLISH_AUTH_URL`.
        pub url: String,
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
                        Answer::Passphrase(values) | Answer::SlowPassphrase(_, values) => {
                            if let Answer::SlowPassphrase(ms, _) = answer {
                                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                            }
                            let mut reply = String::from("HTTP/1.1 200 X\r\ncontent-length: 0\r\n");
                            for value in values {
                                reply.push_str(&format!("sapflow-srt-passphrase: {value}\r\n"));
                            }
                            reply.push_str("connection: close\r\n\r\n");
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
            url: url(port),
            requests,
            task,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
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
        assert_eq!(classify(Ok(401)), Verdict::Denied);
        assert_eq!(classify(Ok(403)), Verdict::Forbidden);
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
                let refusal =
                    Publication::begin_authorised(&fixture.app, &req, SourceKind::Ts, from())
                        .await
                        .err()
                        .expect("refused");
                if status == 401 {
                    assert!(matches!(
                        refusal,
                        Refusal::Request(PublishError::Unauthorized)
                    ));
                } else {
                    assert!(matches!(refusal, Refusal::Forbidden));
                }
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

    const SRT_KEY: &str = "pw_srt_00112233445566778899aabbccddeeff";

    fn mediamtx_body(protocol: &str) -> serde_json::Value {
        serde_json::json!({
            "user": "encoder", "password": PASSWORD, "ip": "198.51.100.7",
            "action": "publish", "path": "live/event-a",
            "protocol": protocol, "id": null, "query": "",
        })
    }

    #[tokio::test]
    async fn without_app_keys_the_request_is_unchanged_and_the_header_ignored() {
        // The app answers with a passphrase, but app keys are off: the
        // request is mediamtx's, byte for byte, and the header is not read.
        let stub = stub::start(Answer::Passphrase(&[SRT_KEY])).await;
        let fixture = fixture(stub.endpoint.clone());
        assert!(!fixture.app.config.srt_app_keys);
        let stream_id = format!("publish:live/event-a:encoder:{PASSWORD}");
        let admission = crate::srt::admit(&fixture.app, &stream_id, from(), 0)
            .await
            .expect("admitted");
        // Admitted but not claimed: the listener claims once it knows the
        // caller's key will be accepted. No passphrase was taken.
        let crate::srt::Admission::Pending(pending) = admission else {
            panic!("expected an unclaimed admission")
        };
        assert!(pending.key.is_none());
        assert!(fixture.app.streams.get("event-a").is_none());
        let requests = stub.requests.lock().expect("log").clone();
        let (_, sent) = requests.last().expect("request");
        assert_eq!(*sent, mediamtx_body("srt"));
        assert_eq!(
            body(&srt(&stream_id), Protocol::Srt, from()),
            serde_json::to_vec(&mediamtx_body("srt")).expect("JSON")
        );
    }

    #[tokio::test]
    async fn with_app_keys_srt_offers_the_field_and_rtmp_does_not() {
        let stub = stub::start(Answer::Passphrase(&[SRT_KEY])).await;
        let mut fixture = fixture(stub.endpoint.clone());
        Arc::get_mut(&mut fixture.app)
            .expect("sole App")
            .config
            .srt_app_keys = true;
        let stream_id = format!("publish:live/event-a:encoder:{PASSWORD}");
        let admission = crate::srt::admit(&fixture.app, &stream_id, from(), 0)
            .await
            .expect("admitted");
        // Keyed: nothing is claimed until the listener checks the key.
        let crate::srt::Admission::Pending(keyed) = admission else {
            panic!("expected a keyed admission")
        };
        assert!(fixture.app.streams.get("event-a").is_none());
        let mut with_field = mediamtx_body("srt");
        with_field["srtKey"] = "accepted".into();
        assert_eq!(stub.last(), with_field);
        // An admission still holding its key cannot be claimed unchecked.
        let mut admitted = keyed.admitted;
        let req = keyed.req;
        let mut again = Publication::admit_srt_key(&fixture.app, &req, from())
            .await
            .expect("admitted");
        assert!(matches!(
            Publication::claim_admitted(&fixture.app, &req, SourceKind::Ts, again),
            Err(Refusal::Request(PublishError::Unauthorized))
        ));
        assert!(fixture.app.streams.get("event-a").is_none());
        assert!(admitted.take_srt_key().is_none(), "taken by srt::admit");
        drop(admitted);
        // RTMP: no field, and the answer's header is ignored.
        let rtmp =
            PublishRequest::parse_rtmp("live", &format!("event-a?user=encoder&pass={PASSWORD}"))
                .expect("rtmp");
        let publication =
            Publication::begin_authorised(&fixture.app, &rtmp, SourceKind::Flv, from())
                .await
                .expect("RTMP admitted as before");
        assert_eq!(stub.last(), mediamtx_body("rtmp"));
        publication.end();
        again = Publication::admit(&fixture.app, &rtmp, SourceKind::Es, from())
            .await
            .expect("WHIP admitted as before");
        assert!(again.take_srt_key().is_none());
        assert_eq!(stub.last(), mediamtx_body("webrtc"));
    }

    #[tokio::test]
    async fn an_unusable_passphrase_header_refuses_without_falling_back() {
        const LONG: &str =
            "pw_srt_0123456789012345678901234567890123456789012345678901234567890123456789012";
        assert_eq!(LONG.len(), 80);
        for values in [&["short"][..], &[LONG], &[SRT_KEY, SRT_KEY], &[""]] {
            let stub = stub::start(Answer::Passphrase(values)).await;
            let req = srt(&format!("publish:live/event-a:encoder:{PASSWORD}"));
            for _ in 0..12 {
                let (verdict, key) = ask_srt_key(&stub.endpoint, &req, from()).await;
                assert_eq!(verdict, Verdict::Unavailable, "{}", values.len());
                assert!(key.is_none());
            }
        }
        // Bounds: 10 and 79 bytes are passphrases.
        for value in ["pw_srt_abc", &LONG[..79]] {
            let leaked: &'static str = Box::leak(value.to_owned().into_boxed_str());
            let values: &'static [&'static str] = Box::leak(vec![leaked].into_boxed_slice());
            let stub = stub::start(Answer::Passphrase(values)).await;
            let req = srt("publish:live/event-a");
            let (verdict, key) = ask_srt_key(&stub.endpoint, &req, from()).await;
            assert_eq!(
                (verdict, key.is_some()),
                (Verdict::Admitted, true),
                "{}",
                value.len()
            );
        }
        // A refusal's header is never read: 401 stays 401.
        let stub = stub::start(Answer::Status(401)).await;
        let req = srt("publish:live/event-a");
        let (verdict, key) = ask_srt_key(&stub.endpoint, &req, from()).await;
        assert_eq!((verdict, key.is_none()), (Verdict::Denied, true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn neither_the_password_nor_the_url_reaches_logs_or_refusals() {
        // A capture of this thread's logs that other tests' threads cannot
        // hide (`log_capture`).
        let captured = crate::log_capture::Capture::start();
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
        let logs = captured.text();
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
