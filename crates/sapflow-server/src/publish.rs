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
#[cfg(test)]
use crate::recorder::Recorder;
use crate::recorder::RecorderFactory;
use crate::stream::{Claim, ClaimError, LiveStream, Retirement, Source, SourceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Request(PublishError),
    AlreadyLive,
    /// The configured per-protocol RTMP publication cap is already owned.
    PublisherLimit,
    /// All resident presentations are owned, including held or removed streams.
    PresentationLimit,
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
    /// A newer connection has taken this stream over.
    Superseded,
}
impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ingest(e) => e.fmt(f),
            Self::Revoked => f.write_str("publish-session admission closed"),
            Self::Superseded => f.write_str("a newer connection took this stream over"),
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
        // Global-publisher mode: the one authorised publisher may replace its
        // own stale connection. Scoped sessions keep their single owner.
        let recorder_factory = Self::recorder_factory(app, &req.path, &callbacks);
        let mut recording = false;
        let (stream, generation, claim) = app
            .streams
            .claim_configured(
                &req.path,
                cfg,
                kind,
                scope.is_none(),
                app.config.max_publishers,
                |session, claim| {
                    session.recorder_factory = recorder_factory;
                    if claim == Claim::Fresh {
                        session.recorder = session
                            .recorder_factory
                            .as_ref()
                            .map(RecorderFactory::start);
                    }
                    recording = session.recorder.is_some();
                },
            )
            .map_err(|error| match error {
                ClaimError::AlreadyLive => Refusal::AlreadyLive,
                ClaimError::PublisherLimit => Refusal::PublisherLimit,
                ClaimError::PresentationLimit => Refusal::PresentationLimit,
            })?;
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

    fn recorder_factory(
        app: &Arc<App>,
        path: &sapflow_core::StreamPath,
        callbacks: &Callbacks,
    ) -> Option<RecorderFactory> {
        match &app.config.recordings {
            Some(dir) if app.config.record_policy.records(path) => Some(RecorderFactory::new(
                dir,
                path,
                callbacks.clone(),
                app.config.record_min_free,
                app.recording_limits.clone(),
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
                .map_err(|_| PushError::Revoked)?,
            None => self.push_open(f),
        }
    }

    /// Whether this connection still owns its stream.
    fn owns(&self, session: &crate::stream::Session) -> bool {
        session.owns_connection(self.generation)
    }

    fn push_open(
        &mut self,
        f: impl FnOnce(
            &mut Source,
            std::time::SystemTime,
        ) -> Result<sapflow_hls::Update, sapflow_hls::IngestError>,
    ) -> Result<(), PushError> {
        // Keep session ownership stable through ingest, the recovered callback
        // and recording rollover. Registry operations use session -> state too.
        let mut session = self.stream.session();
        if !self.owns(&session) {
            return Err(PushError::Superseded);
        }
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
                return Err(PushError::Ingest(error));
            }
        };
        if applied.media_loss {
            session.mark_recording_incomplete(self.generation);
        }
        let ready = !self.published
            && if self.scope.is_some() {
                self.stream
                    .fresh_media_locked(&session, self.generation)
                    .is_some()
            } else {
                applied.ready
            };
        if ready {
            session.recovered();
            self.published = true;
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
            session.recorder = session
                .recorder_factory
                .as_ref()
                .map(RecorderFactory::start);
            // One ingest batch can both begin an epoch and discover loss.
            // Without sample-level attribution, conservatively taint both
            // files before feeding any parts into the successor recorder.
            if applied.media_loss || discarded {
                session.mark_recording_incomplete(self.generation);
            }
        }
        if let Some(rec) = &mut session.recorder
            && rec.is_accepting()
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
                keyframe_interval_ms: m.video().map(|_| keyframe.as_millis() as u64),
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
        let runtime = tokio::runtime::Handle::try_current();
        let retirement =
            self.app
                .streams
                .retire(&self.stream, self.generation, self.grace, runtime.is_ok());
        if matches!(retirement, Retirement::Stale) {
            if let Some(scope) = &self.scope {
                scope.released(false);
            } else {
                tracing::info!(path = %self.stream.path.as_path(), "replaced publisher closed");
            }
            return;
        }
        if self.published {
            self.callbacks
                .send(&self.stream.path, StreamEvent::Unpublish);
        }
        // The last complete AAC PES can create the first servable part only
        // at EOF. Registry decides whether to hold after draining it.
        let held = matches!(retirement, Retirement::Held(_));
        if let Some(scope) = &self.scope {
            scope.released(held);
        }
        if let (Retirement::Held(grace), Ok(rt)) = (retirement, runtime) {
            let (registry, stream, generation) = (
                Arc::downgrade(&self.app.streams),
                Arc::downgrade(&self.stream),
                self.generation,
            );
            rt.spawn(async move {
                tokio::time::sleep(grace).await;
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                let Some(stream) = stream.upgrade() else {
                    return;
                };
                if registry.expire(&stream, generation) {
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
#[path = "publish_terminal_tests.rs"]
pub(crate) mod terminal_tests;

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
        pub(super) root: PathBuf,
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
                    recording_limits: Arc::new(crate::recorder::RecordingLimits::new(
                        config.max_publishers,
                    )),
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
            self.begin_kind(SourceKind::Ts)
        }
        fn begin_kind(&self, kind: SourceKind) -> Publication {
            let req = PublishRequest::parse_srt_stream_id("publish:live/event-a:fixture:fixture")
                .expect("request");
            Publication::begin(
                &self.app,
                &req,
                kind,
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
    async fn error_after_epoch_mutation_preserves_only_the_prior_recording_prefix() {
        let mut fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        let stream = old.stream.clone();
        old.grace = Duration::from_secs(1);
        old.end();
        let (tracks, parts) = stream.parts_after(None).expect("sealed old epoch");
        let duration = parts
            .iter()
            .fold(Duration::ZERO, |sum, part| sum + part.span);
        let mut expected =
            sapflow_hls::fmp4::init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
                .expect("old codec header")
                .to_vec();
        let mut origin = None;
        for part in parts {
            expected.extend_from_slice(
                &sapflow_hls::fmp4::rebase_part_for_file(
                    &part.data,
                    tracks.audio().map(|a| a.sample_rate),
                    &mut origin,
                )
                .expect("old accepted part"),
            );
        }
        let mut replacement = fixture.begin_kind(SourceKind::Flv);
        let mut restarted = false;
        assert!(
            replacement
                .push(|source, now| {
                    let Source::Flv(flv) = source else {
                        unreachable!()
                    };
                    flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now)?;
                    flv.audio_at(0, &[0xaf, 0, 0x12, 0x10], now)?;
                    let mut sample = vec![0x5a; 66];
                    sample[..2].copy_from_slice(&[0xaf, 1]);
                    for frame in 0..120u32 {
                        let ms = (u64::from(frame) * 1024 * 1000 / 44_100) as u32;
                        restarted |= flv.audio_at(ms, &sample, now)?.restarted;
                    }
                    Err(sapflow_hls::IngestError::InvalidAudio)
                })
                .is_err()
        );
        assert!(
            restarted,
            "actual muxer changed before the later batch error"
        );
        replacement.end();
        let body = fixture.completion("incomplete").await;
        let actual = std::fs::read(body["segmentPath"].as_str().expect("prefix path"))
            .expect("synced old prefix");
        assert_eq!(
            actual, expected,
            "new-epoch samples cannot enter the old-codec file"
        );
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
    }

    #[tokio::test]
    async fn one_batch_codec_rollover_and_audio_loss_taints_the_successor_recording() {
        let mut fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        old.grace = Duration::from_secs(1);
        old.end();
        let mut replacement = fixture.begin_kind(SourceKind::Flv);
        let mut observed = sapflow_hls::Update::default();
        replacement
            .push(|source, now| {
                let Source::Flv(flv) = source else {
                    unreachable!()
                };
                let mut update = flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now)?;
                update.merge(flv.audio_at(0, &[0xaf, 0, 0x12, 0x10], now)?);
                let mut sample = vec![0x5a; 66];
                sample[..2].copy_from_slice(&[0xaf, 1]);
                for frame in 0..120u32 {
                    let index = if frame >= 50 { frame + 3 } else { frame };
                    let ms = (u64::from(index) * 1024 * 1000 / 44_100) as u32;
                    update.merge(flv.audio_at(ms, &sample, now)?);
                }
                observed = update;
                Ok(update)
            })
            .expect("one real codec-change and timestamp-gap batch");
        assert!(observed.restarted && observed.media_loss);
        replacement.end();
        let first = fixture.completion("incomplete").await;
        let second = fixture.completion("incomplete").await;
        assert_ne!(first["segmentPath"], second["segmentPath"]);
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
    }

    #[tokio::test]
    async fn declared_rtmp_audio_timestamp_loss_cannot_report_a_finalised_recording() {
        for timing in ["clean", "missing", "repeated"] {
            let mut fixture = Fixture::new();
            let mut publication = fixture.begin_kind(SourceKind::Flv);
            publication
                .push(|source, now| match source {
                    Source::Flv(flv) => flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now),
                    _ => unreachable!(),
                })
                .expect("explicit AAC topology");
            publication
                .push(|source, now| match source {
                    Source::Flv(flv) => flv.audio_at(0, &[0xaf, 0, 0x11, 0x88], now),
                    _ => unreachable!(),
                })
                .expect("canonical 48 kHz mono ASC");
            // Original placeholders exercise real ingest, loss propagation and
            // writer completion, not independent decoding of AAC payloads.
            let mut sample = vec![0x5a; 66];
            sample[..2].copy_from_slice(&[0xaf, 1]);
            for frame in 0..120u32 {
                let index = match timing {
                    "missing" if frame >= 50 => frame + 3,
                    "repeated" if frame == 50 => frame - 1,
                    _ => frame,
                };
                let ms = (u64::from(index) * 1024 * 1000 / 48_000) as u32;
                publication
                    .push(|source, now| match source {
                        Source::Flv(flv) => flv.audio_at(ms, &sample, now),
                        _ => unreachable!(),
                    })
                    .expect("bounded timestamp recovery");
            }
            publication.end();
            let expected = if timing == "clean" {
                "finalised"
            } else {
                "incomplete"
            };
            fixture.completion(expected).await;
            assert_eq!(
                fixture.scope.observations.counts().recordings_finalised,
                u64::from(timing == "clean")
            );
        }
    }

    #[tokio::test]
    async fn zero_grace_audio_end_seals_the_known_tail_before_recording_completion() {
        let mut fixture = Fixture::new();
        let mut publication = fixture.begin();
        for packet in
            include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts").chunks(1316)
        {
            crate::srt::feed(&mut publication, packet).expect("generated AAC fixture");
        }
        let stream = publication.stream.clone();
        publication.end();
        assert!(stream.lock().closed);
        assert!(
            stream.lock().ingest.muxer().is_none(),
            "terminal drain precedes cache release"
        );
        let body = fixture.completion("finalised").await;
        assert_eq!(
            super::terminal_tests::recording_samples(&body),
            283,
            "all generated AAC frames, including the sealed tail, reach the actual file"
        );
        let seconds: f64 = body["segmentDuration"]
            .as_str()
            .expect("duration")
            .parse()
            .expect("seconds");
        assert!(
            seconds >= 6.0,
            "the last accepted audio frame must reach the recording"
        );
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
    async fn a_new_connection_takes_over_and_the_old_one_stays_quiet() {
        let mut fixture = Fixture::new();
        let mut old = fixture.begin();
        Fixture::feed(&mut old);
        // A second connection with the same authority, before the first has
        // timed out, as when an encoder changes network.
        let mut new = fixture.begin();
        assert!(matches!(
            crate::srt::feed(&mut old, &[0x47; 188]).map_err(|e| e.contains("newer connection")),
            Err(true)
        ));
        let events = |fixture: &mut Fixture| {
            let mut names = Vec::new();
            while let Ok(item) = fixture.callbacks.try_recv() {
                let body: serde_json::Value = serde_json::from_slice(&item.body).expect("JSON");
                names.push(body["event"].as_str().unwrap_or_default().to_owned());
            }
            names
        };
        let before = events(&mut fixture);
        drop(old);
        assert!(
            !events(&mut fixture).contains(&"unpublish".to_owned()),
            "the replaced connection sent unpublish (before: {before:?})"
        );
        Fixture::feed(&mut new);
        assert!(new.stream.session().owns_connection(new.generation));
        new.end();
        fixture.completion("finalised").await;
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

#[cfg(test)]
mod publication_capacity_tests {
    use super::*;

    fn configure(fixture: &mut loss_tests::Fixture, limit: usize) {
        let app = Arc::get_mut(&mut fixture.app).expect("sole fixture App");
        app.config.max_publishers = limit;
        app.streams = Arc::new(crate::stream::Registry::default());
    }

    fn request(id: &str) -> PublishRequest {
        PublishRequest::parse_rtmp("live", &format!("event-{id}")).expect("generated request")
    }

    #[tokio::test]
    async fn resident_refusal_keeps_the_existing_recording_complete_and_recovers_after_release() {
        let mut fixture = loss_tests::Fixture::new();
        Arc::get_mut(&mut fixture.app).expect("sole App").streams =
            Arc::new(crate::stream::Registry::new(1));
        let mut original = fixture.begin();
        loss_tests::Fixture::feed(&mut original);
        let retained = original.stream.clone();
        let queued = fixture.callbacks.len();
        let from = "127.0.0.1".parse().expect("loopback");
        assert!(matches!(
            Publication::begin(&fixture.app, &request("next"), SourceKind::Ts, from),
            Err(Refusal::PresentationLimit)
        ));
        assert_eq!(
            fixture.callbacks.len(),
            queued,
            "refusal starts no recorder or callbacks"
        );
        original.end();
        fixture.completion("finalised").await;
        assert!(
            matches!(
                Publication::begin(&fixture.app, &request("next"), SourceKind::Ts, from),
                Err(Refusal::PresentationLimit)
            ),
            "removed but owned presentation remains charged"
        );
        drop(retained);
        let mut next = Publication::begin(&fixture.app, &request("a"), SourceKind::Ts, from)
            .expect("released resident reservation");
        loss_tests::Fixture::feed(&mut next);
        next.end();
        fixture.completion("finalised").await;
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 2);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
    }

    #[tokio::test]
    async fn rejected_cross_protocol_takeover_keeps_original_recording_and_callbacks_intact() {
        let mut fixture = loss_tests::Fixture::new();
        configure(&mut fixture, 1);
        let holder = Publication::begin(
            &fixture.app,
            &request("holder"),
            SourceKind::Flv,
            "127.0.0.1".parse().expect("loopback"),
        )
        .expect("capacity holder");
        let mut original = fixture.begin();
        loss_tests::Fixture::feed(&mut original);
        let generation = original.generation;
        let last_part = original
            .stream
            .session()
            .recorder
            .as_ref()
            .expect("real recording")
            .last_part();
        let queued = fixture.callbacks.len();
        assert!(matches!(
            Publication::begin(
                &fixture.app,
                &request("a"),
                SourceKind::Flv,
                "127.0.0.1".parse().expect("loopback")
            ),
            Err(Refusal::PublisherLimit)
        ));
        assert!(original.stream.session().owns_connection(generation));
        assert_eq!(
            original
                .stream
                .session()
                .recorder
                .as_ref()
                .expect("unchanged recorder")
                .last_part(),
            last_part
        );
        assert_eq!(fixture.callbacks.len(), queued);
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 0);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
        original.end();
        fixture.completion("finalised").await;
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
        holder.end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
    }

    #[test]
    fn auth_refusal_and_delayed_old_publication_drop_cannot_change_current_owner_credit() {
        let mut fixture = loss_tests::Fixture::new();
        configure(&mut fixture, 1);
        let app = Arc::get_mut(&mut fixture.app).expect("sole App");
        app.config.recordings = None;
        app.config.publisher = Some(sapflow_core::Publisher::new("fixture", "secret"));
        let allowed = PublishRequest::parse_rtmp("live", "event-a?user=fixture&pass=secret")
            .expect("request");
        let from = "127.0.0.1".parse().expect("loopback");
        assert!(matches!(
            Publication::begin(&fixture.app, &request("a"), SourceKind::Flv, from),
            Err(Refusal::Request(PublishError::Unauthorized))
        ));
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        let old = Publication::begin(&fixture.app, &allowed, SourceKind::Flv, from).expect("old");
        let original_generation = old.generation;
        assert!(matches!(
            Publication::begin(&fixture.app, &request("a"), SourceKind::Flv, from),
            Err(Refusal::Request(PublishError::Unauthorized))
        ));
        assert!(old.stream.session().owns_connection(original_generation));
        let new = Publication::begin(&fixture.app, &allowed, SourceKind::Flv, from)
            .expect("replacement at capacity");
        assert!(Arc::ptr_eq(&old.stream, &new.stream));
        old.end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 1);
        let other = PublishRequest::parse_rtmp("live", "event-other?user=fixture&pass=secret")
            .expect("request");
        assert!(matches!(
            Publication::begin(&fixture.app, &other, SourceKind::Flv, from),
            Err(Refusal::PublisherLimit)
        ));
        new.end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        assert!(
            fixture.callbacks.is_empty(),
            "no media readiness or false unpublish callback"
        );
    }

    #[tokio::test]
    async fn full_cap_same_path_takeover_keeps_recording_and_old_drop_cannot_release_successor() {
        let mut fixture = loss_tests::Fixture::new();
        configure(&mut fixture, 1);
        let from = "127.0.0.1".parse().expect("loopback");
        let mut old = Publication::begin(&fixture.app, &request("a"), SourceKind::Flv, from)
            .expect("first RTMP owner");
        old.push(|source, now| {
            let Source::Flv(flv) = source else {
                panic!("FLV")
            };
            flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now)
        })
        .expect("declared audio");
        old.push(|source, now| {
            let Source::Flv(flv) = source else {
                panic!("FLV")
            };
            flv.audio_at(0, &[0xaf, 0, 0x11, 0x88], now)
        })
        .expect("AAC config");
        for frame in 0..64u64 {
            old.push(|source, now| {
                let Source::Flv(flv) = source else {
                    panic!("FLV")
                };
                // Storage/control fixture only, not a decode-quality assertion.
                flv.audio_at(
                    (frame * 1024 * 1000 / 48_000) as u32,
                    &[0xaf, 1, 0x5a, 0x5a, 0x5a],
                    now,
                )
            })
            .expect("bounded accepted audio");
        }
        assert!(old.published);
        let prior = old
            .stream
            .session()
            .recorder
            .as_ref()
            .expect("active recorder")
            .last_part()
            .expect("recorded accepted prefix");
        let generation = old.generation;
        let queued = fixture.callbacks.len();
        let new = Publication::begin(&fixture.app, &request("a"), SourceKind::Flv, from)
            .expect("same-path replacement while cap is full");
        assert!(Arc::ptr_eq(&old.stream, &new.stream));
        assert!(new.generation > generation);
        assert!(!old.stream.session().owns_connection(generation));
        assert!(new.stream.session().owns_connection(new.generation));
        assert!(
            new.stream
                .session()
                .recorder
                .as_ref()
                .expect("retained recorder")
                .last_part()
                .is_some_and(|part| part >= prior)
        );
        assert_eq!(
            fixture.callbacks.len(),
            queued,
            "admission emits no false lifecycle event"
        );
        old.end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 1);
        assert_eq!(
            fixture.callbacks.len(),
            queued,
            "late old Drop emits no Unpublish"
        );
        assert!(matches!(
            Publication::begin(&fixture.app, &request("other"), SourceKind::Flv, from),
            Err(Refusal::PublisherLimit)
        ));
        new.end();
        assert_eq!(fixture.app.streams.active_rtmp_publishers(), 0);
        fixture.completion("finalised").await;
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
    }
}

#[cfg(test)]
#[path = "publish_fragment_tests.rs"]
mod fragment_tests;

#[cfg(test)]
mod configured_publication_cap_regression {
    use super::*;

    // Intentionally uses only pre-existing APIs so the same body can be run
    // against the unmodified server as a genuine behavioural red witness.
    #[test]
    fn configured_rtmp_limit_refuses_a_second_distinct_current_owner() {
        let mut fixture = loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole fixture App");
        app.config.recordings = None;
        app.config.max_publishers = 1;
        app.streams = Arc::new(crate::stream::Registry::default());
        let from = "127.0.0.1".parse().expect("loopback");
        let first_request = PublishRequest::parse_rtmp("live", "event-first").expect("request");
        let second_request = PublishRequest::parse_rtmp("live", "event-second").expect("request");
        let first = Publication::begin(&fixture.app, &first_request, SourceKind::Flv, from)
            .expect("first configured publication slot");
        let second = Publication::begin(&fixture.app, &second_request, SourceKind::Flv, from);
        let refused = second.is_err();
        // Always release accepted ownership before reporting the assertion.
        drop(second);
        first.end();
        assert!(
            refused,
            "max_publishers=1 accepted two different RTMP publication owners"
        );
    }
}

#[cfg(test)]
mod grace_timer_lifetime_tests {
    use super::*;

    fn fixture(grace: Duration) -> loss_tests::Fixture {
        let mut fixture = loss_tests::Fixture::new();
        let app = Arc::get_mut(&mut fixture.app).expect("sole fixture App");
        app.config.recordings = None;
        app.config.reconnect_grace = grace;
        fixture
    }

    fn with_content(fixture: &loss_tests::Fixture) -> Publication {
        let mut publication = fixture.begin();
        loss_tests::Fixture::feed(&mut publication);
        assert!(
            publication.stream.has_content(),
            "real generated TS must qualify for grace"
        );
        publication
    }

    async fn after_old_timer(grace: Duration) {
        tokio::time::sleep(grace + Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
    }

    // Old-API deterministic red: let the spawned future enter its pending
    // sleep before checking release, while the deadline stays sixty seconds.
    #[tokio::test]
    async fn terminated_stream_is_not_pinned_by_its_obsolete_grace_timer() {
        let fixture = fixture(Duration::from_secs(60));
        let publication = with_content(&fixture);
        let stream = publication.stream.clone();
        let generation = publication.generation;
        let weak = Arc::downgrade(&stream);
        publication.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(fixture.app.streams.get("event-a").is_some());
        assert!(fixture.app.streams.terminate(&stream, generation));
        assert!(stream.lock().closed);
        drop(stream);
        assert!(
            weak.upgrade().is_none(),
            "an obsolete reconnect timer kept a removed stream strongly alive"
        );
    }

    #[tokio::test]
    async fn pending_grace_timer_does_not_keep_app_or_registry_alive() {
        let fixture = fixture(Duration::from_secs(60));
        let publication = with_content(&fixture);
        let app = Arc::downgrade(&fixture.app);
        let registry = Arc::downgrade(&fixture.app.streams);
        let stream = Arc::downgrade(&publication.stream);
        publication.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(fixture);
        assert!(app.upgrade().is_none(), "timer kept App alive");
        assert!(
            registry.upgrade().is_none(),
            "timer kept Registry payload alive"
        );
        assert!(
            stream.upgrade().is_none(),
            "timer kept Stream payload alive"
        );
    }

    #[tokio::test]
    async fn registry_alone_keeps_a_valid_hold_until_its_timer_expires() {
        let fixture = fixture(Duration::from_secs(1));
        let publication = with_content(&fixture);
        let stream = Arc::downgrade(&publication.stream);
        publication.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(fixture.app.streams.get("event-a").is_some());
        assert!(
            stream.upgrade().is_some(),
            "registry owns valid held presentation"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.app.streams.get("event-a").is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bounded actual grace expiry");
        assert!(stream.upgrade().is_none());
    }

    #[tokio::test]
    async fn same_stream_reconnect_survives_the_old_timer_deadline() {
        let grace = Duration::from_secs(1);
        let fixture = fixture(grace);
        let old = with_content(&fixture);
        let stream = old.stream.clone();
        let generation = old.generation;
        old.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut resumed = fixture.begin();
        assert!(Arc::ptr_eq(&stream, &resumed.stream));
        assert!(resumed.generation > generation);
        resumed.grace = Duration::from_secs(60);
        loss_tests::Fixture::feed(&mut resumed);
        let current = resumed.generation;
        resumed.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        // The successor is detached too: attached=true cannot mask a broken
        // old-generation check when the original one-second timer wakes.
        after_old_timer(grace).await;
        assert!(
            fixture
                .app
                .streams
                .get("event-a")
                .is_some_and(|held| Arc::ptr_eq(&held, &stream))
        );
        assert!(!stream.session().owns_connection(current));
        assert!(!stream.lock().closed);
        assert!(!fixture.app.streams.expire(&stream, generation));
        assert!(fixture.app.streams.terminate(&stream, current));
    }

    #[tokio::test]
    async fn fresh_same_path_generation_zero_survives_the_old_object_timer() {
        let grace = Duration::from_secs(1);
        let fixture = fixture(grace);
        let old = with_content(&fixture);
        let old_stream = old.stream.clone();
        let old_generation = old.generation;
        old.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(fixture.app.streams.terminate(&old_stream, old_generation));
        let mut fresh = fixture.begin();
        assert_eq!(
            fresh.generation, old_generation,
            "fresh object reuses generation zero"
        );
        assert!(!Arc::ptr_eq(&old_stream, &fresh.stream));
        fresh.grace = Duration::from_secs(60);
        loss_tests::Fixture::feed(&mut fresh);
        let current = fresh.generation;
        let successor = fresh.stream.clone();
        fresh.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Both objects are detached and use generation zero. Keep old_stream
        // alive so its Weak upgrades; exact-object/closed guards must protect
        // the valid held successor instead of an attached-state shortcut.
        after_old_timer(grace).await;
        assert!(
            fixture
                .app
                .streams
                .get("event-a")
                .is_some_and(|held| Arc::ptr_eq(&held, &successor))
        );
        assert!(!successor.session().owns_connection(current));
        assert!(!successor.lock().closed);
        assert!(!fixture.app.streams.expire(&old_stream, old_generation));
        assert!(fixture.app.streams.terminate(&successor, current));
    }

    #[tokio::test]
    async fn separate_registry_can_expire_a_hold_after_app_is_dropped() {
        let fixture = fixture(Duration::from_secs(1));
        let publication = with_content(&fixture);
        let registry = fixture.app.streams.clone();
        let app = Arc::downgrade(&fixture.app);
        let stream = Arc::downgrade(&publication.stream);
        publication.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(fixture);
        assert!(app.upgrade().is_none(), "timer needs Registry, not App");
        assert!(registry.get("event-a").is_some());
        tokio::time::timeout(Duration::from_secs(3), async {
            while registry.get("event-a").is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("held Registry still gets its actual expiry");
        assert!(stream.upgrade().is_none());
    }

    #[tokio::test]
    async fn genuine_strong_stream_and_response_bytes_remain_valid_after_termination() {
        let fixture = fixture(Duration::from_secs(60));
        let publication = with_content(&fixture);
        let stream = publication.stream.clone();
        let weak = Arc::downgrade(&stream);
        let generation = publication.generation;
        let (_, parts) = stream.parts_after(None).expect("generated sealed parts");
        let bytes = parts.first().expect("at least one real part").data.clone();
        let expected = (
            bytes.len(),
            bytes.iter().fold(0u64, |sum, byte| {
                sum.wrapping_mul(257).wrapping_add(u64::from(*byte))
            }),
        );
        assert!(expected.0 > 0);
        drop(parts);
        publication.end();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(fixture.app.streams.terminate(&stream, generation));
        assert!(
            weak.upgrade().is_some(),
            "actual owner must keep Stream alive"
        );
        drop(stream);
        assert!(weak.upgrade().is_none(), "only the obsolete timer remains");
        assert_eq!(
            (
                bytes.len(),
                bytes.iter().fold(0u64, |sum, byte| sum
                    .wrapping_mul(257)
                    .wrapping_add(u64::from(*byte)))
            ),
            expected
        );
    }
}
