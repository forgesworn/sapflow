//! A stop on SIGTERM or ctrl-c, as at every `docker compose up` redeploy or
//! restart: every stream ends at once, and sapflow waits, within a deadline,
//! until each recording is finished and every callback has been sent, so the
//! app hears `unpublish` and `recordSegmentComplete` as it would for an
//! encoder that left. A SIGKILL still ends sapflow with neither, but a
//! recording's owed `recordSegmentComplete` (one whose file a SIGKILL or the
//! deadline left unfinished, or whose callback was not delivered) waits in
//! the recording outbox and is sent at the next start
//! ([`crate::recording_recovery`]).

use std::time::Duration;

use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::App;
use crate::stream::Stopped;

/// How long a stop waits for recordings and callbacks. With [`RUNTIME`]
/// after it, sapflow exits within Docker's default 10 s stop timeout.
pub const DEADLINE: Duration = Duration::from_millis(7_500);

/// How long the runtime may take to shut down after the wait, for blocking
/// file work a stalled recording left. Dropping it would wait without bound.
pub const RUNTIME: Duration = Duration::from_millis(500);

/// What a stop did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub ended: Stopped,
    /// Every recording writer finished and every callback was delivered or
    /// failed before the deadline.
    pub in_time: bool,
    /// Recording writers still running when the wait ended.
    pub writers: usize,
    /// Callbacks not yet delivered or failed when the wait ended.
    pub callbacks: usize,
}

/// Ends every stream with no reconnect grace ([`crate::stream::Registry::stop_all`],
/// which also refuses every claim from then on), aborts `listeners`, then
/// waits until every recording writer has finished and the callback queue
/// has drained, or until `within` has passed.
///
/// The listeners stop after the streams have ended: dropping the SRT
/// listener marks its publishers' recordings `srt-listener-stopped`, which
/// would otherwise come first. Its connections end with it, and so does an
/// SRT handshake still being admitted, unanswered. RTMP connections end at
/// their next packet, refused as superseded; WHIP sessions end when the
/// WHIP task that feeds them is aborted.
pub async fn stop(app: &App, listeners: &[AbortHandle], within: Duration) -> Outcome {
    let started = Instant::now();
    let deadline = started + within;
    let ended = app.streams.stop_all();
    for listener in listeners {
        listener.abort();
    }
    tracing::info!(
        live = ended.live,
        held = ended.held,
        "stopping: every stream ended; waiting for recordings and callbacks"
    );
    let in_time = tokio::time::timeout_at(deadline, async {
        app.recording_limits.writers_finished().await;
        app.callbacks.drained().await;
    })
    .await
    .is_ok();
    let outcome = Outcome {
        ended,
        in_time,
        writers: app.recording_limits.writers_running(),
        callbacks: app.callbacks.pending(),
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if in_time {
        tracing::info!(
            live = ended.live,
            held = ended.held,
            elapsed_ms,
            "stopped: every recording finished and every callback sent or failed"
        );
    } else {
        tracing::warn!(
            live = ended.live,
            held = ended.held,
            writers_running = outcome.writers,
            callbacks_pending = outcome.callbacks,
            elapsed_ms,
            "stop deadline passed; exiting with recordings or callbacks unfinished"
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::loss_tests::Fixture;
    use crate::publish::{Publication, Refusal};
    use crate::recorder::RecordingLimits;
    use crate::stream::SourceKind;
    use sapflow_core::PublishRequest;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    /// The loss fixture with unscoped callbacks, which take any path, and
    /// every callback body collected as the sender would take it.
    fn fixture(grace: Duration, limits: Option<RecordingLimits>) -> (Fixture, Seen) {
        let mut fixture = Fixture::new();
        let (callbacks, mut rx) = crate::callback::legacy_test_queue(64);
        let app = Arc::get_mut(&mut fixture.app).expect("unshared fixture app");
        app.callbacks = callbacks;
        app.config.reconnect_grace = grace;
        if let Some(limits) = limits {
            app.recording_limits = Arc::new(limits);
        }
        let seen = Seen::default();
        let log = seen.clone();
        tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                let body = serde_json::from_slice(&item.body).expect("JSON");
                log.lock().expect("seen").push(body);
                // Dropping the item is the sender being done with it.
            }
        });
        (fixture, seen)
    }

    fn begin(fixture: &Fixture, event: &str) -> Result<Publication, Refusal> {
        let req =
            PublishRequest::parse_srt_stream_id(&format!("publish:live/{event}:fixture:fixture"))
                .expect("request");
        Publication::begin(
            &fixture.app,
            &req,
            SourceKind::Ts,
            "127.0.0.1".parse().expect("loopback"),
        )
    }

    /// The stream events for `event`, in order, without health reports.
    fn events(seen: &Seen, event: &str) -> Vec<serde_json::Value> {
        seen.lock()
            .expect("seen")
            .iter()
            .filter(|body| body["path"] == format!("live/{event}") && body["event"] != "health")
            .cloned()
            .collect()
    }

    fn names(events: &[serde_json::Value]) -> Vec<&str> {
        events
            .iter()
            .map(|body| body["event"].as_str().expect("event name"))
            .collect()
    }

    #[tokio::test]
    async fn a_stop_ends_live_and_held_streams_and_tells_the_app_once() {
        let (fixture, seen) = fixture(Duration::from_secs(60), None);
        let mut live = begin(&fixture, "event-live").expect("live");
        Fixture::feed(&mut live);
        let mut held = begin(&fixture, "event-held").expect("held");
        Fixture::feed(&mut held);
        // The publisher left: `unpublish` now, the stream held for 60 s.
        drop(held);
        assert!(fixture.app.streams.get("event-held").is_some());
        let listener = tokio::spawn(std::future::pending::<()>());

        let started = std::time::Instant::now();
        let outcome = stop(&fixture.app, &[listener.abort_handle()], DEADLINE).await;
        assert!(started.elapsed() < DEADLINE);
        assert_eq!(
            outcome,
            Outcome {
                ended: Stopped { live: 1, held: 1 },
                in_time: true,
                writers: 0,
                callbacks: 0,
            }
        );
        assert!(listener.await.expect_err("aborted").is_cancelled());
        assert!(fixture.app.streams.get("event-live").is_none());
        assert!(fixture.app.streams.get("event-held").is_none());
        assert!(matches!(
            begin(&fixture, "event-new"),
            Err(Refusal::Stopping)
        ));
        assert!(matches!(
            begin(&fixture, "event-live"),
            Err(Refusal::Stopping)
        ));

        // The live connection ends after the stop: its stream is closed, so
        // its push is refused, and its drop owes the app nothing more.
        assert!(crate::srt::feed(&mut live, &[0x47; 188]).is_err());
        drop(live);
        tokio::time::timeout(Duration::from_secs(1), fixture.app.callbacks.drained())
            .await
            .expect("drained");

        let live = events(&seen, "event-live");
        assert_eq!(
            names(&live),
            ["publish", "unpublish", "recordSegmentComplete"]
        );
        assert_eq!(live[2]["recordingState"], "incomplete");
        assert_eq!(live[2]["recordingReason"], "server-stopped");
        let held = events(&seen, "event-held");
        assert_eq!(
            names(&held),
            ["publish", "unpublish", "recordSegmentComplete"]
        );
        assert_eq!(held[2]["recordingState"], "finalised");
        assert_eq!(held[2]["recordingReason"], serde_json::Value::Null);
        for body in [&live[2], &held[2]] {
            let file = std::fs::read(body["segmentPath"].as_str().expect("path")).expect("file");
            assert!(file.len() > 1024, "synced recording");
        }
    }

    #[tokio::test]
    async fn a_stop_keeps_a_reason_the_recording_already_had() {
        let (fixture, seen) = fixture(Duration::ZERO, None);
        let mut live = begin(&fixture, "event-a").expect("live");
        Fixture::feed(&mut live);
        live.loss_notice()
            .mark_incomplete(crate::recorder::IncompleteReason::OverRate);
        let outcome = stop(&fixture.app, &[], DEADLINE).await;
        assert!(outcome.in_time);
        let events = events(&seen, "event-a");
        assert_eq!(
            names(&events),
            ["publish", "unpublish", "recordSegmentComplete"]
        );
        assert_eq!(events[2]["recordingReason"], "over-bitrate");
        drop(live);
    }

    #[tokio::test]
    async fn a_stop_right_after_a_takeover_still_sends_the_owed_unpublish() {
        let (fixture, seen) = fixture(Duration::from_secs(60), None);
        let mut old = begin(&fixture, "event-a").expect("first connection");
        Fixture::feed(&mut old);
        // The encoder reconnects, as on a network change, and is stopped
        // before its new connection has sent `publish`.
        let new = begin(&fixture, "event-a").expect("takeover");
        drop(old);
        let outcome = stop(&fixture.app, &[], DEADLINE).await;
        assert_eq!(outcome.ended, Stopped { live: 1, held: 0 });
        drop(new);
        tokio::time::timeout(Duration::from_secs(1), fixture.app.callbacks.drained())
            .await
            .expect("drained");
        let events = events(&seen, "event-a");
        assert_eq!(
            names(&events),
            ["publish", "unpublish", "recordSegmentComplete"]
        );
        assert_eq!(events[2]["recordingReason"], "server-stopped");
    }

    #[tokio::test]
    async fn the_deadline_bounds_a_stop_whose_writer_never_finishes() {
        let (fixture, seen) = fixture(
            Duration::ZERO,
            Some(RecordingLimits::with_stalled_files(16)),
        );
        let mut live = begin(&fixture, "event-stalled").expect("live");
        Fixture::feed(&mut live);
        let within = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let outcome = stop(&fixture.app, &[], within).await;
        let elapsed = started.elapsed();
        assert!(elapsed >= within, "{elapsed:?}");
        assert!(elapsed < within + Duration::from_secs(1), "{elapsed:?}");
        assert_eq!(outcome.ended, Stopped { live: 1, held: 0 });
        assert!(!outcome.in_time);
        assert_eq!(outcome.writers, 1);
        // The app was still told the stage is gone; no recording was reported.
        tokio::time::timeout(Duration::from_secs(1), fixture.app.callbacks.drained())
            .await
            .expect("drained");
        assert_eq!(
            names(&events(&seen, "event-stalled")),
            ["publish", "unpublish"]
        );
        drop(live);
    }
}
