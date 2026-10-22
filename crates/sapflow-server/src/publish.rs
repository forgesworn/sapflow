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
use crate::stream::{Claim, LiveStream, Retirement, Source, SourceKind};

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
            session.recorder_factory = Self::recorder_factory(app, &req.path, &callbacks);
            if claim == Claim::Fresh {
                session.recorder = session
                    .recorder_factory
                    .as_ref()
                    .map(RecorderFactory::start);
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
        if !session.owns_connection(self.generation) {
            return Err(sapflow_hls::IngestError::PresentationExpired);
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
                return Err(error);
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
        let before = stream
            .lock()
            .ingest
            .muxer()
            .expect("AAC muxer")
            .next_part_id();
        publication.end();
        let after = stream
            .lock()
            .ingest
            .muxer()
            .expect("retained AAC state")
            .next_part_id();
        assert!(
            after > before,
            "normal end must seal the final accepted AAC part"
        );
        let body = fixture.completion("finalised").await;
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
