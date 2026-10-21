//! What happens around one publish, whichever protocol it came in on:
//! claiming the path, the callbacks, recording, and the cleanup.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sapflow_core::limits::RateCap;

use sapflow_core::{Health, PublishError, PublishRequest, StreamEvent};
use sapflow_hls::MuxerConfig;

use crate::App;
use crate::callback::Callbacks;
use crate::publish_sessions::ScopedLease;
use crate::recorder::Recorder;
use crate::stream::{Claim, LiveStream, Source, SourceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Request(PublishError),
    AlreadyLive,
    /// Too many failed attempts from this address recently.
    Throttled,
}

/// A live publisher went over the bitrate cap and is dropped.
#[derive(Debug)]
pub struct OverRate;

impl std::fmt::Display for OverRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("over the publish bitrate cap")
    }
}

pub struct Publication {
    app: Arc<App>,
    pub stream: Arc<LiveStream>,
    generation: u64,
    published: bool,
    connected: Instant,
    last_health: Instant,
    rate: RateCap,
    scope: Option<ScopedLease>,
    callbacks: Callbacks,
    grace: Duration,
}

/// A bounded connection-scoped loss notice, independent of the media queue.
/// It never looks up a path that a newer publication might now own.
#[derive(Clone)]
pub struct PublicationLoss {
    stream: Arc<LiveStream>,
    generation: u64,
    scope: Option<ScopedLease>,
}
impl PublicationLoss {
    pub fn mark_incomplete(&self) {
        let mark = || {
            self.stream
                .session()
                .mark_recording_incomplete(self.generation)
        };
        if let Some(scope) = &self.scope {
            // Revoke/end and this notice share gate -> session lock order.
            // Authority cutoff itself is not evidence of a missing sample.
            let _ = scope.with_open(mark);
        } else {
            mark();
        }
    }
}

#[derive(Debug)]
pub enum PushError {
    Ingest(sapflow_hls::IngestError),
    Revoked,
}
impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ingest(e) => e.fmt(f),
            Self::Revoked => f.write_str("publish-session admission closed"),
        }
    }
}

impl Publication {
    /// Authorises the request and claims its path, or takes over a stream
    /// held for a reconnect.
    pub fn begin(
        app: &Arc<App>,
        req: &PublishRequest,
        kind: SourceKind,
        from: IpAddr,
    ) -> Result<Self, Refusal> {
        // A visible SRT credential can never bypass transport-key possession
        // by being replayed through this legacy RTMP/global-auth entry point.
        if app.publish_sessions.is_some() {
            return Err(Refusal::Request(PublishError::Unauthorized));
        }
        let throttle = || app.auth_throttle.lock().unwrap_or_else(|e| e.into_inner());
        if !throttle().allowed(from, Instant::now()) {
            return Err(Refusal::Throttled);
        }
        if let Some(publisher) = &app.config.publisher
            && let Err(e) = req.authorise(publisher)
        {
            throttle().failed(from, Instant::now());
            return Err(Refusal::Request(e));
        }
        Self::claim(
            app,
            req,
            kind,
            None,
            app.config.max_publish_bps,
            app.config.reconnect_grace,
        )
    }

    pub async fn begin_srt(
        app: &Arc<App>,
        req: &PublishRequest,
        from: IpAddr,
    ) -> Result<Self, Refusal> {
        let Some(manager) = &app.publish_sessions else {
            return Self::begin(app, req, SourceKind::Ts, from);
        };
        if !app
            .auth_throttle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allowed(from, Instant::now())
        {
            return Err(Refusal::Throttled);
        }
        let scope = manager.admit(req).await.map_err(|error| {
            if error == crate::publish_sessions::Error::Denied {
                app.auth_throttle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .failed(from, Instant::now());
            }
            match error {
                crate::publish_sessions::Error::Busy => Refusal::AlreadyLive,
                _ => Refusal::Request(PublishError::Unauthorized),
            }
        })?;
        let (limits, grace) = scope
            .limits()
            .map_err(|_| Refusal::Request(PublishError::Unauthorized))?;
        scope
            .with_open(|| {
                Self::claim(
                    app,
                    req,
                    SourceKind::Ts,
                    Some(scope.clone()),
                    limits.max_publish_bps.min(app.config.max_publish_bps),
                    grace,
                )
            })
            .map_err(|_| Refusal::Request(PublishError::Unauthorized))?
    }

    fn claim(
        app: &Arc<App>,
        req: &PublishRequest,
        kind: SourceKind,
        scope: Option<ScopedLease>,
        max_bps: u64,
        grace: Duration,
    ) -> Result<Self, Refusal> {
        let callbacks = scope.as_ref().map_or_else(
            || app.callbacks.clone(),
            |scope| app.callbacks.scoped(scope.scope()),
        );
        let cfg = MuxerConfig {
            segment_count: app.config.segment_count,
            ..MuxerConfig::default()
        };
        let (stream, generation, claim) = app
            .streams
            .claim(&req.path, cfg, kind)
            .ok_or(Refusal::AlreadyLive)?;
        let recording = {
            let mut session = stream.session();
            if claim == Claim::Fresh {
                session.recorder = Self::recorder(app, &req.path, &callbacks);
            }
            session.recorder.is_some()
        };
        tracing::info!(
            path = %req.path.as_path(),
            ?kind,
            recording,
            resumed = claim == Claim::Resumed,
            "publisher connected"
        );
        let now = Instant::now();
        if let Some(scope) = &scope {
            scope.bind(&app.streams, &stream, generation);
        }
        Ok(Self {
            app: app.clone(),
            stream,
            generation,
            published: false,
            connected: now,
            last_health: now,
            rate: RateCap::new(max_bps, Duration::from_secs(5)),
            scope,
            callbacks,
            grace,
        })
    }

    fn recorder(
        app: &Arc<App>,
        path: &sapflow_core::StreamPath,
        callbacks: &Callbacks,
    ) -> Option<Recorder> {
        match &app.config.recordings {
            Some(dir) if app.config.record_policy.records(path) => Some(Recorder::start(
                dir,
                path,
                callbacks.clone(),
                app.config.record_min_free,
            )),
            _ => None,
        }
    }

    /// Counts bytes received from the publisher against the bitrate cap.
    pub fn received(&mut self, n: usize) -> Result<(), OverRate> {
        if self.rate.add(n, Instant::now()) {
            Ok(())
        } else {
            self.loss_notice().mark_incomplete();
            Err(OverRate)
        }
    }

    pub fn loss_notice(&self) -> PublicationLoss {
        PublicationLoss {
            stream: self.stream.clone(),
            generation: self.generation,
            scope: self.scope.clone(),
        }
    }

    /// Feeds media. An error means the publisher should be dropped.
    pub fn push(
        &mut self,
        f: impl FnOnce(
            &mut Source,
            std::time::SystemTime,
        ) -> Result<sapflow_hls::Update, sapflow_hls::IngestError>,
    ) -> Result<(), PushError> {
        let scope = self.scope.clone();
        match scope {
            Some(scope) => scope
                .with_open(|| self.push_open(f))
                .map_err(|_| PushError::Revoked)?
                .map_err(PushError::Ingest),
            None => self.push_open(f).map_err(PushError::Ingest),
        }
    }

    fn push_open(
        &mut self,
        f: impl FnOnce(
            &mut Source,
            std::time::SystemTime,
        ) -> Result<sapflow_hls::Update, sapflow_hls::IngestError>,
    ) -> Result<(), sapflow_hls::IngestError> {
        // Keep session ownership stable through ingest, the recovered callback
        // and recording rollover. Registry operations use session -> state too.
        let mut session = self.stream.session();
        let mut discarded = false;
        let result = self.stream.apply(|source, now| {
            let before = source.dropped_packets();
            let result = f(source, now);
            discarded = source.dropped_packets() != before;
            result
        });
        if discarded {
            session.mark_recording_incomplete(self.generation);
        }
        let applied = match result {
            Ok(applied) => applied,
            Err(error) => {
                session.mark_recording_incomplete(self.generation);
                return Err(error);
            }
        };
        if applied.ready && !self.published {
            session.recovered();
            self.published = true;
            if let Some(scope) = &self.scope {
                scope.media_ready();
            }
            tracing::info!(
                path = %self.stream.path.as_path(),
                ready_ms = self.connected.elapsed().as_millis() as u64,
                "stream ready"
            );
            self.callbacks.send(&self.stream.path, StreamEvent::Publish);
        }
        if applied.restarted {
            // A new init segment: the old recording ends here (its file is
            // finished and reported) and a new one starts.
            session.recorder = Self::recorder(&self.app, &self.stream.path, &self.callbacks);
        }
        if let Some(rec) = &mut session.recorder
            && let Some((init, parts)) = self.stream.parts_after(rec.last_part())
        {
            rec.feed(init, parts);
        }
        drop(session);
        self.report_health();
        Ok(())
    }

    /// Every `health_interval` while the stream is up.
    fn report_health(&mut self) {
        let interval = self.app.config.health_interval;
        if interval.is_zero() || !self.published || self.last_health.elapsed() < interval {
            return;
        }
        self.last_health = Instant::now();
        let health = {
            let st = self.stream.lock();
            let Some(m) = st.ingest.muxer() else { return };
            let Some((bps, keyframe)) = m.recent(6) else {
                return;
            };
            Health {
                bitrate_kbps: bps / 1000,
                keyframe_interval_ms: keyframe.as_millis() as u64,
                audio: m.audio().is_some(),
                breaks: m.breaks(),
            }
        };
        self.callbacks
            .send(&self.stream.path, StreamEvent::Health(health));
    }

    /// The publisher left: unpublish, finish the recording, free the path.
    pub fn end(self) {
        drop(self);
    }
    pub fn scoped_lease(&self) -> Option<ScopedLease> {
        self.scope.clone()
    }
}

/// Runs on every exit, including a panic in the connection's task, so a
/// path is never left claimed by a publisher that is gone.
///
/// v4v hears `unpublish` at once (its stage reads "reconnecting"). The
/// stream itself, with its playlist and recording, is held for
/// `reconnect_grace` so an encoder that comes back carries it on.
impl Drop for Publication {
    fn drop(&mut self) {
        if self.published {
            self.callbacks
                .send(&self.stream.path, StreamEvent::Unpublish);
        }
        let grace = self
            .stream
            .session()
            .hold_remaining(self.grace, Instant::now());
        let runtime = tokio::runtime::Handle::try_current();
        let hold = !grace.is_zero() && runtime.is_ok() && self.stream.has_content();
        let held = self.app.streams.detach(&self.stream, self.generation, hold);
        if let Some(scope) = &self.scope {
            scope.released(held);
        }
        if let (true, Ok(rt)) = (held, runtime) {
            let (app, stream, generation) =
                (self.app.clone(), self.stream.clone(), self.generation);
            rt.spawn(async move {
                tokio::time::sleep(grace).await;
                if app.streams.expire(&stream, generation) {
                    tracing::info!(path = %stream.path.as_path(), "no reconnect within the grace period; stream ended");
                }
            });
            tracing::info!(
                path = %self.stream.path.as_path(),
                grace_s = grace.as_secs(),
                "publisher left; stream held for a reconnect"
            );
        } else {
            tracing::info!(path = %self.stream.path.as_path(), "publisher left");
        }
    }
}

#[cfg(test)]
pub(crate) mod loss_tests {
    use super::*;
    use crate::publish_observer::{CallbackScope, fixture_scope};
    use sapflow_core::StreamPath;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    pub(crate) struct Fixture {
        pub app: Arc<App>,
        pub scope: Arc<CallbackScope>,
        pub callbacks: tokio::sync::mpsc::Receiver<crate::callback::Queued>,
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        pub fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target")
                .join(format!(
                    "recording-loss-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir(&root).expect("owned scratch");
            let scope = fixture_scope("a", 1, 1, 1);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 32);
            let mut config = crate::config::Config::load(
                |key| match key {
                    "SAPFLOW_OPEN_PUBLISH" => Some("1".into()),
                    _ => None,
                },
                Vec::new(),
            )
            .expect("fixture config");
            config.recordings = Some(root.clone());
            config.record_min_free = 0;
            config.reconnect_grace = Duration::ZERO;
            Self {
                app: Arc::new(App {
                    config,
                    publish_sessions: None,
                    callbacks,
                    streams: Arc::new(crate::stream::Registry::default()),
                    auth_throttle: Mutex::new(sapflow_core::limits::AuthThrottle::new(
                        10,
                        Duration::from_secs(60),
                    )),
                    refusal_log: Mutex::new(sapflow_core::limits::LogLimiter::new(
                        20,
                        Duration::from_secs(60),
                    )),
                }),
                scope,
                callbacks: receiver,
                root,
            }
        }
        pub fn begin(&self) -> Publication {
            let req = PublishRequest::parse_srt_stream_id("publish:live/event-a:fixture:fixture")
                .expect("request");
            Publication::begin(
                &self.app,
                &req,
                SourceKind::Ts,
                "127.0.0.1".parse().expect("loopback"),
            )
            .expect("begin")
        }
        pub fn feed(publication: &mut Publication) {
            for bytes in
                include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts").chunks(1316)
            {
                crate::srt::feed(publication, bytes).expect("generated fixture feed");
            }
        }
        pub async fn completion(&mut self, state: &str) -> serde_json::Value {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let item = self.callbacks.recv().await.expect("callback channel");
                    let body: serde_json::Value = serde_json::from_slice(&item.body).expect("JSON");
                    if body["event"] == "recordSegmentComplete" {
                        assert_eq!(body["recordingState"], state);
                        assert_eq!(body["sessionId"], self.scope.key().session_id.as_str());
                        let bytes = std::fs::read(body["segmentPath"].as_str().expect("path"))
                            .expect("synced prefix");
                        assert!(bytes.len() > 1024);
                        return body;
                    }
                }
            })
            .await
            .expect("bounded completion")
        }
    }

    #[tokio::test]
    async fn rate_rejection_and_ingest_failure_sync_incomplete_accepted_prefix() {
        for error in ["rate", "ingest", "discard"] {
            let mut fixture = Fixture::new();
            let mut publication = fixture.begin();
            Fixture::feed(&mut publication);
            match error {
                "rate" => assert!(publication.received(usize::MAX).is_err()),
                "ingest" => assert!(
                    publication
                        .push(|_, _| Err(sapflow_hls::IngestError::SegmentTooLarge))
                        .is_err()
                ),
                _ => {
                    // TS packet with reserved adaptation_field_control=0.
                    let mut invalid = [0xff; 188];
                    invalid[..4].copy_from_slice(&[0x47, 0, 0, 0]);
                    publication
                        .push(|source, now| match source {
                            Source::Ts(ts) => ts.push(&invalid, now),
                            _ => unreachable!(),
                        })
                        .expect("discard is nonfatal");
                    assert!(publication.stream.lock().ingest.dropped_packets() > 0);
                }
            }
            publication.end();
            fixture.completion("incomplete").await;
            assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
        }
    }

    #[tokio::test]
    async fn stale_notice_cannot_taint_new_generation_and_clean_stop_is_finalised() {
        let mut fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        let notice = old.loss_notice();
        // Explicitly retain presentation, then attach a newer connection to
        // the same stream Arc; its new file must not inherit an old notice.
        old.grace = Duration::from_secs(1);
        old.end();
        let mut new = fixture.begin();
        new.stream.session().recorder = Some(Recorder::start(
            &fixture.root,
            &StreamPath::parse("live/event-a").expect("path"),
            fixture.app.callbacks.clone(),
            0,
        ));
        notice.mark_incomplete();
        Fixture::feed(&mut new);
        new.end();
        fixture.completion("finalised").await;
        fixture.completion("finalised").await;
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
    }
}
