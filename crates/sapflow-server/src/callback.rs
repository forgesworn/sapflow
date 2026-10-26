//! The stream-health callback to v4v: the only outbound call sapflow makes.
//!
//! Events go through one queue and are sent in order, so an `unpublish`
//! never overtakes the `publish` before it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use sapflow_core::{StreamEvent, StreamPath, callback_url};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::http_lite::{self, Endpoint, ParsedResponse};
use crate::publish_observer::{CallbackScope, CallbackTicket, RecordingObservation};

const QUEUE: usize = 512;
const MAX_BODY: usize = 16 * 1024;
/// Connect, send and read the answer's head, per attempt.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Interim `1xx` answers skipped before a final one; more is a broken peer.
const MAX_INTERIM: usize = 4;

pub(crate) struct Queued {
    path: StreamPath,
    name: &'static str,
    pub(crate) body: Bytes,
    ticket: Option<CallbackTicket>,
}

#[derive(Clone)]
pub struct Callbacks {
    tx: Option<mpsc::Sender<Queued>>,
    scope: Option<Arc<CallbackScope>>,
    recording: Option<RecordingObservation>,
}

impl Callbacks {
    /// Starts the sender. With no URL configured, events are only logged.
    pub fn start(url: Option<String>, secret: Option<String>) -> Self {
        let Some(url) = url else {
            return Self {
                tx: None,
                scope: None,
                recording: None,
            };
        };
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(run(url, secret, rx));
        Self {
            tx: Some(tx),
            scope: None,
            recording: None,
        }
    }

    pub fn scoped(&self, scope: Arc<CallbackScope>) -> Self {
        Self {
            tx: self.tx.clone(),
            scope: Some(scope),
            recording: None,
        }
    }

    pub(crate) fn for_recording(&self) -> Result<(Self, Option<RecordingObservation>), ()> {
        let Some(scope) = &self.scope else {
            return Ok((self.clone(), None));
        };
        let recording = scope.recording().ok_or(())?;
        let mut callbacks = self.clone();
        callbacks.recording = Some(recording.clone());
        Ok((callbacks, Some(recording)))
    }

    pub fn send(&self, path: &StreamPath, event: StreamEvent) {
        tracing::info!(path = %path.as_path(), event = event.name(), "stream event");
        // Allocation, sequencing and enqueue are serialised per immutable
        // scope. Concurrent recorder/health tasks cannot enqueue sequence N+1
        // ahead of N. No network or disk operation runs under this lock.
        let _order = self.scope.as_ref().map(|scope| scope.event_lock());
        let health = matches!(event, StreamEvent::Health(_));
        let Some(tx) = &self.tx else {
            if let Some(scope) = &self.scope
                && scope.observations.callbacks_enabled()
            {
                scope.observations.callback_failed();
            }
            return;
        };
        let Some(body) = self.body(path, &event) else {
            if let Some(scope) = &self.scope {
                scope.observations.callback_failed();
            }
            tracing::warn!(event = event.name(), "callback body refused");
            return;
        };
        let ticket = self
            .scope
            .as_ref()
            .map(|scope| scope.observations.callback_ticket());
        let queued = Queued {
            path: path.clone(),
            name: event.name(),
            body: Bytes::from(body),
            ticket,
        };
        if let Err(error) = tx.try_send(queued) {
            let (mut queued, unavailable) = match error {
                mpsc::error::TrySendError::Full(queued) => (queued, false),
                mpsc::error::TrySendError::Closed(queued) => (queued, true),
            };
            if let Some(ticket) = queued.ticket.take() {
                if health && !unavailable {
                    ticket.dropped_health();
                } else {
                    ticket.finish(false);
                }
            }
            tracing::warn!(
                event = event.name(),
                health_dropped = health && !unavailable,
                unavailable,
                "callback queue full or unavailable"
            );
        }
    }

    fn body(&self, path: &StreamPath, event: &StreamEvent) -> Option<String> {
        let legacy = event.callback_body(path);
        if legacy.len() > MAX_BODY {
            return None;
        }
        let Some(scope) = &self.scope else {
            return Some(legacy);
        };
        if scope.key().path != *path {
            return None;
        }
        let sequence = scope.next_sequence()?;
        let mut body: serde_json::Value = serde_json::from_str(&legacy).ok()?;
        let object = body.as_object_mut()?;
        object.insert("contractVersion".into(), 1.into());
        object.insert("eventId".into(), path.event_id().into());
        object.insert(
            "eventGeneration".into(),
            scope.key().event_generation.get().into(),
        );
        object.insert("sessionId".into(), scope.key().session_id.as_str().into());
        object.insert("connectionEpoch".into(), scope.connection_epoch().into());
        object.insert("eventSequence".into(), sequence.into());
        if let StreamEvent::RecordSegmentComplete {
            recording_tracks, ..
        } = event
        {
            let recording = self.recording.as_ref()?;
            object.insert("recordingId".into(), recording.id().into());
            object.insert(
                "recordingState".into(),
                recording.completion_state()?.into(),
            );
            object.insert(
                "recordingTracks".into(),
                serde_json::json!({
                    "hasVideo": recording_tracks.has_video(),
                    "hasAudio": recording_tracks.has_audio(),
                }),
            );
        }
        let body = serde_json::to_string(&body).ok()?;
        (body.len() <= MAX_BODY).then_some(body)
    }
}

/// A queue without a publish-session scope: the legacy, global-publisher mode.
#[cfg(test)]
pub(crate) fn legacy_test_queue(capacity: usize) -> (Callbacks, mpsc::Receiver<Queued>) {
    let (tx, rx) = mpsc::channel(capacity);
    (
        Callbacks {
            tx: Some(tx),
            scope: None,
            recording: None,
        },
        rx,
    )
}

#[cfg(test)]
pub(crate) fn test_queue(
    scope: Arc<CallbackScope>,
    capacity: usize,
) -> (Callbacks, mpsc::Receiver<Queued>) {
    let (tx, rx) = mpsc::channel(capacity);
    (
        Callbacks {
            tx: Some(tx),
            scope: Some(scope),
            recording: None,
        },
        rx,
    )
}

/// Why an attempt got no status. Neither carries the error text: it may
/// contain the secret-bearing URL.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Timeout,
    Transport,
}

/// One `POST`, then the answer's status. The connection is plain HTTP/1.1 and
/// used once (`connection: close`). Only the status line and headers are read,
/// within `MAX_HEAD`; the body is never read. A redirect is a status like any
/// other: the `location` is never followed.
async fn post(endpoint: &Endpoint, body: &[u8]) -> std::io::Result<u16> {
    let mut socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;
    // A receiver may answer, and close, before it has read the whole body.
    // Keep a write error to report only if no answer arrives.
    let wrote = socket
        .write_all(&endpoint.post("application/json", body))
        .await;
    let mut buf = Vec::with_capacity(512);
    let mut interim = 0;
    loop {
        match http_lite::parse_response(&buf) {
            ParsedResponse::Complete { status, used }
                if (100..200).contains(&status) && status != 101 =>
            {
                interim += 1;
                if interim > MAX_INTERIM {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
                buf.drain(..used);
                continue;
            }
            ParsedResponse::Complete { status, .. } => return Ok(status),
            ParsedResponse::Malformed => return Err(std::io::ErrorKind::InvalidData.into()),
            ParsedResponse::Partial => {}
        }
        let mut chunk = [0u8; 2048];
        let room = http_lite::MAX_HEAD
            .saturating_sub(buf.len())
            .min(chunk.len());
        match socket.read(&mut chunk[..room]).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            Ok(_) => {
                return Err(wrote
                    .err()
                    .unwrap_or(std::io::ErrorKind::UnexpectedEof.into()));
            }
            Err(e) => return Err(wrote.err().unwrap_or(e)),
        }
    }
}

async fn attempt(endpoint: &Endpoint, body: &[u8], limit: Duration) -> Result<u16, Failure> {
    match tokio::time::timeout(limit, post(endpoint, body)).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(_)) => Err(Failure::Transport),
        Err(_) => Err(Failure::Timeout),
    }
}

async fn run(template: String, secret: Option<String>, mut rx: mpsc::Receiver<Queued>) {
    while let Some(mut queued) = rx.recv().await {
        let url = callback_url(&template, &queued.path, secret.as_deref());
        let mut delay = Duration::from_millis(500);
        let mut delivered = false;
        // The URL carries the secret: it is parsed here and never logged.
        if let Some(endpoint) = Endpoint::parse(&url) {
            for attempt_number in 1..=4 {
                match attempt(&endpoint, &queued.body, ATTEMPT_TIMEOUT).await {
                    Ok(status) if (200..300).contains(&status) => {
                        delivered = true;
                        tracing::debug!(event = queued.name, "callback delivered");
                        break;
                    }
                    Ok(status) if (400..500).contains(&status) => {
                        // v4v refused it (wrong token or path): retrying won't help.
                        tracing::warn!(event = queued.name, status, "callback refused");
                        break;
                    }
                    Ok(status) => {
                        tracing::warn!(
                            event = queued.name,
                            status,
                            attempt = attempt_number,
                            "callback failed"
                        );
                    }
                    Err(Failure::Transport) => {
                        tracing::warn!(
                            event = queued.name,
                            attempt = attempt_number,
                            "callback transport failed"
                        );
                    }
                    Err(Failure::Timeout) => {
                        tracing::warn!(
                            event = queued.name,
                            attempt = attempt_number,
                            "callback timed out"
                        );
                    }
                }
                if attempt_number < 4 {
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        } else {
            tracing::error!(event = queued.name, "callback URL is not valid");
        }
        if let Some(ticket) = queued.ticket.take() {
            ticket.finish(delivered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish_observer::fixture_scope;
    use sapflow_core::Health;

    fn health() -> StreamEvent {
        StreamEvent::Health(Health {
            bitrate_kbps: 1000,
            keyframe_interval_ms: Some(1000),
            audio: true,
            breaks: 0,
        })
    }
    fn completion() -> StreamEvent {
        StreamEvent::RecordSegmentComplete {
            segment_path: "/fixture/recording.mp4".into(),
            duration_secs: 1.0,
            recording_tracks: sapflow_core::RecordingTracks::AudioVideo,
        }
    }

    #[test]
    fn scoped_completion_has_exact_immutable_topology_and_frozen_retry_body() {
        use sapflow_core::RecordingTracks;
        for tracks in [
            RecordingTracks::AudioOnly,
            RecordingTracks::VideoOnly,
            RecordingTracks::AudioVideo,
        ] {
            for incomplete in [false, true] {
                let scope = fixture_scope("a", 2, 11, 3);
                let (callbacks, mut rx) = test_queue(scope.clone(), 4);
                let (writer_callbacks, recording) = callbacks.for_recording().expect("writer");
                let recording = recording.expect("scoped");
                recording.active();
                if incomplete {
                    recording.synced_incomplete();
                } else {
                    recording.finalised();
                }
                writer_callbacks.send(
                    &scope.key().path,
                    StreamEvent::RecordSegmentComplete {
                        segment_path: "/fixture/recording.mp4".into(),
                        duration_secs: 1.0,
                        recording_tracks: tracks,
                    },
                );
                let queued = rx.try_recv().expect("completion");
                let expected = serde_json::json!({
                    "event": "recordSegmentComplete", "path": "live/event-a",
                    "segmentPath": "/fixture/recording.mp4", "segmentDuration": "1",
                    "contractVersion": 1, "eventId": "event-a", "eventGeneration": 2,
                    "sessionId": scope.key().session_id.as_str(), "connectionEpoch": 3,
                    "eventSequence": 1, "recordingId": recording.id(),
                    "recordingState": if incomplete { "incomplete" } else { "finalised" },
                    "recordingTracks": {"hasVideo": tracks.has_video(), "hasAudio": tracks.has_audio()},
                });
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&queued.body).expect("JSON"),
                    expected
                );
                // Later telemetry advances the shared sequencer, but retries
                // clone the queued Bytes, never reconstructing this receipt.
                callbacks.send(&scope.key().path, StreamEvent::Publish);
                let next: serde_json::Value =
                    serde_json::from_slice(&rx.try_recv().expect("later event").body)
                        .expect("JSON");
                assert_eq!(next["eventSequence"], 2);
                for _ in 0..4 {
                    let retry_body = queued.body.clone();
                    assert_eq!(retry_body.as_ptr(), queued.body.as_ptr());
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&retry_body)
                            .expect("same JSON"),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn bounded_queue_drops_health_visibly_and_fails_terminal_overflow() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, mut rx) = test_queue(scope.clone(), 1);
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        assert_eq!(scope.observations.callback_state(), "pending");
        callbacks.send(&scope.key().path, health());
        assert_eq!(scope.observations.counts().health_dropped, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 0);
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        assert_eq!(scope.observations.callback_state(), "failed");
        assert_eq!(rx.len(), 1);
        let mut first = rx.try_recv().expect("first only");
        first.ticket.take().expect("ticket").finish(true);
        assert!(rx.try_recv().is_err());
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        assert_eq!(scope.observations.callback_state(), "failed");
    }

    #[test]
    fn closed_callback_worker_is_failure_even_for_health() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, receiver) = test_queue(scope.clone(), 1);
        drop(receiver);
        callbacks.send(&scope.key().path, health());
        assert_eq!(scope.observations.callback_state(), "failed");
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        assert_eq!(scope.observations.counts().health_dropped, 0);
    }

    #[test]
    fn dropping_receiver_and_unsynchronised_completion_cannot_report_delivery() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, rx) = test_queue(scope.clone(), 2);
        let (recording_callbacks, recording) = callbacks.for_recording().expect("recording");
        let recording = recording.expect("scoped");
        recording.active();
        recording.failed();
        recording_callbacks.send(&scope.key().path, completion());
        assert_eq!(rx.len(), 0); // A failed sync cannot advertise a recording.
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        drop(rx);
        assert_eq!(scope.observations.counts().callbacks_pending, 0);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        assert_eq!(scope.observations.counts().callbacks_failed, 2);
    }

    #[test]
    fn late_recording_scope_never_uses_a_replacement_identity() {
        let old = fixture_scope("a", 1, 11, 3);
        let new = fixture_scope("a", 2, 22, 4);
        let (callbacks, mut rx) = test_queue(old.clone(), 4);
        let (recording_callbacks, recording) = callbacks.for_recording().expect("recording");
        let recording = recording.expect("scoped");
        recording.active();
        callbacks
            .scoped(new.clone())
            .send(&new.key().path, StreamEvent::Publish);
        recording.finalising();
        recording.finalised();
        recording_callbacks.send(&old.key().path, completion());
        let current: serde_json::Value =
            serde_json::from_slice(&rx.try_recv().expect("new publish").body).expect("JSON");
        let previous: serde_json::Value =
            serde_json::from_slice(&rx.try_recv().expect("late completion").body).expect("JSON");
        assert_eq!(current["eventGeneration"], 2);
        assert_eq!(current["sessionId"], new.key().session_id.as_str());
        assert_eq!(previous["eventGeneration"], 1);
        assert_eq!(previous["sessionId"], old.key().session_id.as_str());
        assert_eq!(previous["connectionEpoch"], 3);
        assert_eq!(previous["eventSequence"], 1);
        assert_eq!(previous["recordingId"], recording.id());
        assert_eq!(previous["recordingState"], "finalised");
        assert_eq!(previous["contractVersion"], 1);
        assert_eq!(previous["eventId"], "event-a");
    }

    #[test]
    fn concurrent_producers_share_one_ordered_positive_sequencer() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, mut rx) = test_queue(scope.clone(), 128);
        std::thread::scope(|threads| {
            for _ in 0..8 {
                let callbacks = callbacks.clone();
                let path = scope.key().path.clone();
                threads.spawn(move || {
                    for _ in 0..8 {
                        callbacks.send(&path, StreamEvent::Publish);
                    }
                });
            }
        });
        for sequence in 1..=64 {
            let mut queued = rx.try_recv().expect("ordered message");
            let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
            assert_eq!(body["eventSequence"], sequence);
            queued.ticket.take().expect("ticket").finish(true);
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(scope.observations.callback_state(), "delivered");
    }

    #[test]
    fn wrong_event_and_oversized_bodies_fail_closed_but_legacy_shape_is_retained() {
        let scope = fixture_scope("a", 1, 1, 1);
        let (callbacks, rx) = test_queue(scope.clone(), 2);
        callbacks.send(
            &StreamPath::parse("live/event-other").expect("path"),
            StreamEvent::Publish,
        );
        assert_eq!(rx.len(), 0);
        assert_eq!(scope.observations.callback_state(), "failed");
        let legacy = Callbacks {
            tx: None,
            scope: None,
            recording: None,
        };
        let body = legacy
            .body(&scope.key().path, &StreamEvent::Publish)
            .expect("legacy");
        assert_eq!(body, StreamEvent::Publish.callback_body(&scope.key().path));
        assert!(!body.contains("sessionId"));
        let completion = completion();
        let body = legacy
            .body(&scope.key().path, &completion)
            .expect("legacy completion");
        assert_eq!(body, completion.callback_body(&scope.key().path));
        assert_eq!(
            body,
            r#"{"event":"recordSegmentComplete","path":"live/event-a","segmentPath":"/fixture/recording.mp4","segmentDuration":"1"}"#
        );
        assert!(
            legacy
                .body(
                    &scope.key().path,
                    &StreamEvent::RecordSegmentComplete {
                        segment_path: "x".repeat(MAX_BODY),
                        duration_secs: 1.0,
                        recording_tracks: sapflow_core::RecordingTracks::AudioVideo,
                    }
                )
                .is_none()
        );
    }

    /// What a receiver saw: one complete `POST`.
    struct Seen {
        method: String,
        target: String,
        host: Option<String>,
        content_type: Option<String>,
        connection: Option<String>,
        declared_length: Option<usize>,
        body: Vec<u8>,
    }

    /// A plain-socket stand-in for v4v, parsing requests with `httparse`
    /// separately from the client's own code. `answer` gets the index of each
    /// request and returns the raw bytes to send, or `None` to stay silent.
    struct Receiver {
        addr: std::net::SocketAddr,
        seen: Arc<std::sync::Mutex<Vec<Seen>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Receiver {
        async fn start(answer: impl Fn(usize) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("listener");
            let addr = listener.local_addr().expect("addr");
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let answer = Arc::new(answer);
            let log = seen.clone();
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        return;
                    };
                    let (log, answer) = (log.clone(), answer.clone());
                    tokio::spawn(async move {
                        let Some(request) = Self::read(&mut socket).await else {
                            return;
                        };
                        let index = {
                            let mut log = log.lock().expect("log");
                            log.push(request);
                            log.len() - 1
                        };
                        match answer(index) {
                            Some(bytes) => {
                                let _ = socket.write_all(&bytes).await;
                                let _ = socket.shutdown().await;
                            }
                            None => std::future::pending::<()>().await,
                        }
                    });
                }
            });
            Self { addr, seen, task }
        }

        async fn read(socket: &mut TcpStream) -> Option<Seen> {
            let mut buf = Vec::new();
            loop {
                let mut headers = [httparse::EMPTY_HEADER; 32];
                let mut req = httparse::Request::new(&mut headers);
                if let Ok(httparse::Status::Complete(used)) = req.parse(&buf) {
                    let header = |name: &str| {
                        req.headers
                            .iter()
                            .find(|h| h.name.eq_ignore_ascii_case(name))
                            .map(|h| String::from_utf8_lossy(h.value).into_owned())
                    };
                    let declared_length = header("content-length").and_then(|v| v.parse().ok());
                    let mut body = buf[used..].to_vec();
                    while body.len() < declared_length.unwrap_or(0) {
                        let mut chunk = [0u8; 4096];
                        let n = socket.read(&mut chunk).await.ok()?;
                        if n == 0 {
                            return None;
                        }
                        body.extend_from_slice(&chunk[..n]);
                    }
                    return Some(Seen {
                        method: req.method?.to_owned(),
                        target: req.path?.to_owned(),
                        host: header("host"),
                        content_type: header("content-type"),
                        connection: header("connection"),
                        declared_length,
                        body,
                    });
                }
                let mut chunk = [0u8; 4096];
                let n = socket.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        }

        fn count(&self) -> usize {
            self.seen.lock().expect("log").len()
        }
        fn url(&self) -> String {
            format!("http://{}/callback", self.addr)
        }
        fn endpoint(&self) -> Endpoint {
            Endpoint::parse(&self.url()).expect("receiver URL")
        }
    }

    impl Drop for Receiver {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn answer_status(code: u16) -> Option<Vec<u8>> {
        Some(http_lite::response(code, &[], b""))
    }

    async fn state_is(scope: &CallbackScope, state: &str, limit: Duration) {
        tokio::time::timeout(limit, async {
            while scope.observations.callback_state() != state {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("callback state {state}"));
    }

    #[tokio::test]
    async fn real_http_ack_updates_receipts_and_client_refusal_is_terminal() {
        let receiver = Receiver::start(|hit| answer_status(if hit == 0 { 204 } else { 403 })).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), Some("fixture-token".into()))
            .scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "delivered", Duration::from_secs(2)).await;
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        state_is(&scope, "failed", Duration::from_secs(2)).await;
        assert_eq!(receiver.count(), 2);
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        // The request on the wire: what v4v has always been sent.
        let seen = receiver.seen.lock().expect("log");
        let first = &seen[0];
        assert_eq!(first.method, "POST");
        assert_eq!(first.target, "/callback?token=fixture-token");
        assert_eq!(
            first.host.as_deref(),
            Some(format!("{}", receiver.addr).as_str())
        );
        assert_eq!(first.content_type.as_deref(), Some("application/json"));
        assert_eq!(first.connection.as_deref(), Some("close"));
        assert_eq!(first.declared_length, Some(first.body.len()));
        let body: serde_json::Value = serde_json::from_slice(&first.body).expect("JSON body");
        assert_eq!(body["event"], "publish");
        assert_eq!(body["eventSequence"], 1);
        let second: serde_json::Value = serde_json::from_slice(&seen[1].body).expect("JSON body");
        assert_eq!(second["event"], "unpublish");
        assert_eq!(second["eventSequence"], 2);
        drop(seen);
        drop(callbacks);
    }

    #[tokio::test]
    async fn real_http_server_failure_exhausts_four_attempts_without_false_delivery() {
        let receiver = Receiver::start(|_| answer_status(503)).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        drop(callbacks);
    }

    #[tokio::test]
    async fn malformed_successful_heads_exhaust_retries_without_false_delivery() {
        let receiver = Receiver::start(|hit| {
            Some(match hit % 4 {
                0 => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n".to_vec(),
                1 => b"HTTP/1.1 200 OK\r\nContent-Length: nope\r\n\r\n".to_vec(),
                2 => b"HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551616\r\n\r\n".to_vec(),
                _ => b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
            })
        })
        .await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        drop(callbacks);
    }

    #[tokio::test]
    async fn a_redirect_is_a_failed_attempt_and_is_never_followed() {
        let elsewhere = Receiver::start(|_| answer_status(200)).await;
        let location = elsewhere.url();
        let receiver = Receiver::start(move |_| {
            Some(http_lite::response(
                302,
                &[("location", location.as_str())],
                b"",
            ))
        })
        .await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        state_is(&scope, "failed", Duration::from_secs(6)).await;
        assert_eq!(receiver.count(), 4);
        assert_eq!(elsewhere.count(), 0);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        drop(callbacks);
    }

    #[tokio::test]
    async fn events_are_delivered_one_at_a_time_in_order() {
        let receiver = Receiver::start(|_| answer_status(204)).await;
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(Some(receiver.url()), None).scoped(scope.clone());
        let events = [
            StreamEvent::Publish,
            health(),
            health(),
            StreamEvent::Unpublish,
        ];
        for event in events {
            callbacks.send(&scope.key().path, event);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while receiver.count() < 4 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all four delivered");
        let sequences: Vec<u64> = receiver
            .seen
            .lock()
            .expect("log")
            .iter()
            .map(|seen| {
                serde_json::from_slice::<serde_json::Value>(&seen.body).expect("JSON")["eventSequence"]
                    .as_u64()
                    .expect("sequence")
            })
            .collect();
        assert_eq!(sequences, [1, 2, 3, 4]);
        state_is(&scope, "delivered", Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn an_invalid_callback_url_fails_at_once_without_connecting() {
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks =
            Callbacks::start(Some("http://127.0.0.1:9/a b".into()), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        // No retry delays: the first backoff alone is 500 ms.
        state_is(&scope, "failed", Duration::from_millis(400)).await;
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
    }

    #[tokio::test]
    async fn interim_answers_are_skipped_but_not_without_limit() {
        let interim = || b"HTTP/1.1 100 Continue\r\n\r\n".to_vec();
        let receiver = Receiver::start(move |hit| {
            let mut bytes = interim();
            if hit == 0 {
                bytes.extend_from_slice(&http_lite::response(204, &[], b""));
            } else {
                for _ in 0..MAX_INTERIM {
                    bytes.extend_from_slice(&interim());
                }
                bytes.extend_from_slice(&http_lite::response(204, &[], b""));
            }
            Some(bytes)
        })
        .await;
        let limit = Duration::from_secs(2);
        assert_eq!(attempt(&receiver.endpoint(), b"{}", limit).await, Ok(204));
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", limit).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn an_unbounded_response_head_is_cut_off() {
        let receiver = Receiver::start(|_| {
            let mut bytes = b"HTTP/1.1 200 OK\r\nx-junk: ".to_vec();
            bytes.resize(256 * 1024, b'a');
            Some(bytes)
        })
        .await;
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn a_silent_receiver_times_out_and_a_closed_port_is_a_transport_failure() {
        let receiver = Receiver::start(|_| None).await;
        let started = std::time::Instant::now();
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_millis(150)).await,
            Err(Failure::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let closed = {
            let listener =
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
            let addr = listener.local_addr().expect("addr");
            Endpoint::parse(&format!("http://{addr}/callback")).expect("URL")
        };
        assert_eq!(
            attempt(&closed, b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }

    #[tokio::test]
    async fn an_answer_that_is_not_http_is_a_transport_failure() {
        let receiver = Receiver::start(|_| Some(b"SSH-2.0-not-http\r\n".to_vec())).await;
        assert_eq!(
            attempt(&receiver.endpoint(), b"{}", Duration::from_secs(2)).await,
            Err(Failure::Transport)
        );
    }
}
