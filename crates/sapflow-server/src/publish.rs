//! What happens around one publish, whichever protocol it came in on:
//! claiming the path, the callbacks, recording, and the cleanup.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sapflow_core::limits::RateCap;

use sapflow_core::{Health, PublishError, PublishRequest, StreamEvent};
use sapflow_hls::MuxerConfig;

use crate::App;
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
                session.recorder = Self::recorder(app, &req.path);
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
        Ok(Self {
            app: app.clone(),
            stream,
            generation,
            published: false,
            connected: now,
            last_health: now,
            rate: RateCap::new(app.config.max_publish_bps, Duration::from_secs(5)),
        })
    }

    fn recorder(app: &Arc<App>, path: &sapflow_core::StreamPath) -> Option<Recorder> {
        match &app.config.recordings {
            Some(dir) if app.config.record_policy.records(path) => Some(Recorder::start(
                dir,
                path,
                app.callbacks.clone(),
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
            Err(OverRate)
        }
    }

    /// Feeds media. An error means the publisher should be dropped.
    pub fn push(
        &mut self,
        f: impl FnOnce(
            &mut Source,
            std::time::SystemTime,
        ) -> Result<sapflow_hls::Update, sapflow_hls::IngestError>,
    ) -> Result<(), sapflow_hls::IngestError> {
        let applied = self.stream.apply(f)?;
        if applied.ready && !self.published {
            self.published = true;
            tracing::info!(
                path = %self.stream.path.as_path(),
                ready_ms = self.connected.elapsed().as_millis() as u64,
                "stream ready"
            );
            self.app
                .callbacks
                .send(&self.stream.path, StreamEvent::Publish);
        }
        let mut session = self.stream.session();
        if applied.restarted {
            // A new init segment: the old recording ends here (its file is
            // finished and reported) and a new one starts.
            session.recorder = Self::recorder(&self.app, &self.stream.path);
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
        self.app
            .callbacks
            .send(&self.stream.path, StreamEvent::Health(health));
    }

    /// The publisher left: unpublish, finish the recording, free the path.
    pub fn end(self) {
        drop(self);
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
            self.app
                .callbacks
                .send(&self.stream.path, StreamEvent::Unpublish);
        }
        let grace = self.app.config.reconnect_grace;
        let runtime = tokio::runtime::Handle::try_current();
        let hold = !grace.is_zero() && runtime.is_ok() && self.stream.has_content();
        let held = self.app.streams.detach(&self.stream, self.generation, hold);
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
