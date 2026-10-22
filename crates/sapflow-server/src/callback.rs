//! The stream-health callback to v4v: the only outbound call sapflow makes.
//!
//! Events go through one queue and are sent in order, so an `unpublish`
//! never overtakes the `publish` before it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use sapflow_core::{StreamEvent, StreamPath, callback_url};
use tokio::sync::mpsc;

use crate::publish_observer::{CallbackScope, CallbackTicket, RecordingObservation};

const QUEUE: usize = 512;
const MAX_BODY: usize = 16 * 1024;

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

async fn run(template: String, secret: Option<String>, mut rx: mpsc::Receiver<Queued>) {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    while let Some(mut queued) = rx.recv().await {
        let url = callback_url(&template, &queued.path, secret.as_deref());
        let mut delay = Duration::from_millis(500);
        let mut delivered = false;
        for attempt in 1..=4 {
            let req = Request::builder()
                .method(Method::POST)
                .uri(&url)
                .header("content-type", "application/json")
                .body(Full::new(queued.body.clone()));
            let Ok(req) = req else {
                tracing::error!(event = queued.name, "callback URL is not valid");
                break;
            };
            match tokio::time::timeout(Duration::from_secs(5), client.request(req)).await {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    delivered = true;
                    tracing::debug!(event = queued.name, "callback delivered");
                    break;
                }
                Ok(Ok(resp)) if resp.status().is_client_error() => {
                    // v4v refused it (wrong token or path): retrying won't help.
                    tracing::warn!(event = queued.name, status = %resp.status(), "callback refused");
                    break;
                }
                Ok(Ok(resp)) => {
                    tracing::warn!(event = queued.name, status = %resp.status(), attempt, "callback failed");
                }
                Ok(Err(_)) => {
                    // Client errors may contain the secret-bearing URL.
                    tracing::warn!(event = queued.name, attempt, "callback transport failed");
                }
                Err(_) => tracing::warn!(event = queued.name, attempt, "callback timed out"),
            }
            if attempt < 4 {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
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

    #[tokio::test]
    async fn real_http_ack_updates_receipts_and_client_refusal_is_terminal() {
        use axum::{Router, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        let router = Router::new().route(
            "/callback",
            post(move || {
                let count = count.clone();
                async move {
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        axum::http::StatusCode::NO_CONTENT
                    } else {
                        axum::http::StatusCode::FORBIDDEN
                    }
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks = Callbacks::start(
            Some(format!("http://{addr}/callback")),
            Some("fixture-token".into()),
        )
        .scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        tokio::time::timeout(Duration::from_secs(2), async {
            while scope.observations.callback_state() != "delivered" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("HTTP acknowledgement");
        callbacks.send(&scope.key().path, StreamEvent::Unpublish);
        tokio::time::timeout(Duration::from_secs(2), async {
            while scope.observations.callback_state() != "failed" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("terminal refusal");
        assert_eq!(received.load(Ordering::SeqCst), 2);
        assert_eq!(scope.observations.counts().callbacks_delivered, 1);
        assert_eq!(scope.observations.counts().callbacks_failed, 1);
        drop(callbacks);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn real_http_server_failure_exhausts_four_attempts_without_false_delivery() {
        use axum::{Router, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let received = Arc::new(AtomicUsize::new(0));
        let count = received.clone();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        let router = Router::new().route(
            "/callback",
            post(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve");
        });
        let scope = fixture_scope("a", 1, 1, 1);
        let callbacks =
            Callbacks::start(Some(format!("http://{addr}/callback")), None).scoped(scope.clone());
        callbacks.send(&scope.key().path, StreamEvent::Publish);
        tokio::time::timeout(Duration::from_secs(6), async {
            while scope.observations.callback_state() != "failed" {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bounded retry exhaustion");
        assert_eq!(received.load(Ordering::SeqCst), 4);
        assert_eq!(scope.observations.counts().callbacks_delivered, 0);
        drop(callbacks);
        server.abort();
        let _ = server.await;
    }
}
