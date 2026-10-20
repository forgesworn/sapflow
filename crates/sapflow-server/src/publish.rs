//! What happens around one publish, whichever protocol it came in on:
//! claiming the path, the callbacks, recording, and the cleanup.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sapflow_core::limits::RateCap;

use sapflow_core::{PublishError, PublishRequest, StreamEvent};
use sapflow_hls::MuxerConfig;

use crate::App;
use crate::recorder::Recorder;
use crate::stream::{LiveStream, Source, SourceKind};

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
    published: bool,
    connected: Instant,
    recorder: Option<Recorder>,
    rate: RateCap,
}

impl Publication {
    /// Authorises the request and claims its path.
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
        let stream = app
            .streams
            .claim(&req.path, cfg, kind)
            .ok_or(Refusal::AlreadyLive)?;
        let recorder = match &app.config.recordings {
            Some(dir) if app.config.record_policy.records(&req.path) => Some(Recorder::start(
                dir,
                &req.path,
                app.callbacks.clone(),
                app.config.record_min_free,
            )),
            _ => None,
        };
        tracing::info!(path = %req.path.as_path(), ?kind, recording = recorder.is_some(), "publisher connected");
        Ok(Self {
            app: app.clone(),
            stream,
            published: false,
            connected: Instant::now(),
            recorder,
            rate: RateCap::new(app.config.max_publish_bps, Duration::from_secs(5)),
        })
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
        let ready = self.stream.apply(f)?;
        if ready && !self.published {
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
        if let Some(rec) = &mut self.recorder
            && let Some((init, parts)) = self.stream.parts_after(rec.last_part())
        {
            rec.feed(init, parts);
        }
        Ok(())
    }

    /// The publisher left: unpublish, finish the recording, free the path.
    pub fn end(self) {
        drop(self);
    }
}

/// Runs on every exit, including a panic in the connection's task, so a
/// path is never left claimed by a publisher that is gone.
impl Drop for Publication {
    fn drop(&mut self) {
        if self.published {
            self.app
                .callbacks
                .send(&self.stream.path, StreamEvent::Unpublish);
        }
        // Dropping the recorder closes its queue; the writer then finishes
        // the file and sends recordSegmentComplete.
        self.recorder.take();
        self.app.streams.release(&self.stream);
        tracing::info!(path = %self.stream.path.as_path(), "publisher left");
    }
}
