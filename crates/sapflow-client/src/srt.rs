//! Bounded native compressed media -> MPEG-TS -> own SRT caller.
//!
//! Native code supplies a UDP socket and monotonic time; this object performs
//! no I/O and adds no reconnect/retry policy. Caller owns pacing, crypto and
//! retransmissions. Session owns reconnect/destination lifecycle separately.
//!
//! One mux-owned SharedSample and one <=1316-byte TS chunk are retained in
//! addition to Caller's explicit packet/byte/age budget. Each poll performs
//! at most one seven-packet mux step and returns at most one paced datagram.
//! `None` is not necessarily idle: honour `has_immediate_work`/`poll_timeout`.
//! A full Caller window suppresses immediate work until an ACK frees capacity.
//!
//! The input handle is cloned once for ownership-preserving validation, never
//! its payload. TS chunks move into Caller without cloning; existing mux copy
//! counters and Caller wire copies still apply. Native FFI/socket copies are
//! additional. This is a bounded path, not a claim of zero-copy publishing.

use crate::{
    media::SharedSample,
    mpegts::{MpegTsConfig, MpegTsMuxer, MuxError, MuxStats, TS_CHUNK_BYTES},
};
use sapflow_srt::{Caller, CallerConfig, CallerEvent, CallerStats, SendError};
use std::{
    fmt,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct PublisherConfig {
    pub mux: MpegTsConfig,
    pub caller: CallerConfig,
    /// Time from sample submission until its final TS chunk enters Caller.
    /// Caller's own age budget starts when each chunk is enqueued; maximum
    /// aggregate residence is therefore the sum of these independent caps.
    pub max_sample_residence: Duration,
}
impl Default for PublisherConfig {
    fn default() -> Self {
        Self {
            mux: MpegTsConfig::default(),
            caller: CallerConfig::default(),
            max_sample_residence: Duration::from_secs(1),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherConfigError {
    Limits,
    Mux(MuxError),
    Caller(sapflow_srt::caller::ConfigError),
}
impl fmt::Display for PublisherConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SRT publisher configuration: {self:?}")
    }
}
impl std::error::Error for PublisherConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    NotConnected,
    Busy,
    Closed,
    Mux(MuxError),
}
/// The exact original Arc is returned on rejection; payloads are never cloned.
pub struct RejectedSample {
    pub reason: PushError,
    pub sample: SharedSample,
}
impl fmt::Debug for RejectedSample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RejectedSample")
            .field("reason", &self.reason)
            .field("bytes", &self.sample.payload().len())
            .finish()
    }
}
impl fmt::Display for RejectedSample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SRT sample rejected: {:?}", self.reason)
    }
}
impl std::error::Error for RejectedSample {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherError {
    SampleExpired,
    Mux(MuxError),
    Transport(SendError),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherEvent {
    /// Connected is a transport handshake, not destination-observed playback.
    Transport(CallerEvent),
    /// Local packaging/residence failure; followed by Caller's cancellation.
    Failed(PublisherError),
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublisherStats {
    pub mux: MuxStats,
    pub transport: CallerStats,
    pub rejected_samples: u64,
    pub input_handle_clones: u64,
    pub chunks_enqueued: u64,
    pub backpressure_events: u64,
    pub clock_regressions: u64,
    pub pending_chunk_bytes: usize,
    pub pending_chunk_capacity: usize,
    pub peak_pending_chunk_capacity: usize,
    pub discarded_pending_chunk_bytes: u64,
}

pub struct SrtPublisher {
    caller: Caller,
    mux: Option<MpegTsMuxer>,
    pending: Option<Vec<u8>>,
    sample_since: Option<Instant>,
    max_sample_residence: Duration,
    now: Instant,
    blocked: bool,
    failure: Option<PublisherError>,
    failure_pending: bool,
    deferred_event: Option<CallerEvent>,
    stats: PublisherStats,
}
impl SrtPublisher {
    pub fn new(
        config: PublisherConfig,
        peer: SocketAddr,
        now: Instant,
    ) -> Result<Self, PublisherConfigError> {
        if config.caller.send.max_payload < TS_CHUNK_BYTES
            || config.caller.send.max_bytes < TS_CHUNK_BYTES
            || config.max_sample_residence.is_zero()
            || config.max_sample_residence > Duration::from_secs(60)
            || now.checked_add(config.max_sample_residence).is_none()
        {
            return Err(PublisherConfigError::Limits);
        }
        let mux = MpegTsMuxer::new(config.mux).map_err(PublisherConfigError::Mux)?;
        let caller = Caller::new(config.caller, peer, now).map_err(PublisherConfigError::Caller)?;
        Ok(Self {
            caller,
            mux: Some(mux),
            pending: None,
            sample_since: None,
            max_sample_residence: config.max_sample_residence,
            now,
            blocked: false,
            failure: None,
            failure_pending: false,
            deferred_event: None,
            stats: PublisherStats::default(),
        })
    }
    pub fn is_connected(&self) -> bool {
        self.caller.is_connected()
    }
    pub fn is_closed(&self) -> bool {
        self.caller.is_closed()
    }
    pub fn failure(&self) -> Option<PublisherError> {
        self.failure
    }
    /// Ready for another sample; unacknowledged TS may remain in Caller.
    pub fn is_ready(&self) -> bool {
        self.is_connected()
            && self.pending.is_none()
            && self.mux.as_ref().is_some_and(MpegTsMuxer::is_idle)
    }
    pub fn stats(&self) -> PublisherStats {
        let mut stats = self.stats;
        if let Some(mux) = &self.mux {
            stats.mux = mux.stats();
        }
        stats.transport = self.caller.stats();
        stats.pending_chunk_bytes = self.pending.as_ref().map_or(0, Vec::len);
        stats.pending_chunk_capacity = self.pending.as_ref().map_or(0, Vec::capacity);
        stats
    }
    pub fn poll_event(&mut self) -> Option<PublisherEvent> {
        let event = self
            .deferred_event
            .take()
            .or_else(|| self.caller.poll_event());
        if self.failure_pending && !matches!(event, Some(CallerEvent::Connected { .. })) {
            self.deferred_event = event;
            self.failure_pending = false;
            return self.failure.map(PublisherEvent::Failed);
        }
        event.map(PublisherEvent::Transport)
    }
    /// Backwards input clocks are clamped and counted, never extend deadlines.
    pub fn push(&mut self, now: Instant, sample: SharedSample) -> Result<(), RejectedSample> {
        self.advance(now);
        let reason = if self.is_closed() {
            Some(PushError::Closed)
        } else if !self.is_connected() {
            Some(PushError::NotConnected)
        } else if !self.is_ready() {
            Some(PushError::Busy)
        } else {
            None
        };
        if let Some(reason) = reason {
            return self.reject(reason, sample);
        }
        self.stats.input_handle_clones = self.stats.input_handle_clones.saturating_add(1);
        let result = self
            .mux
            .as_mut()
            .ok_or(MuxError::Busy)
            .and_then(|mux| mux.push(Arc::clone(&sample)));
        if let Err(error) = result {
            return self.reject(PushError::Mux(error), sample);
        }
        self.sample_since = Some(self.now);
        self.blocked = false;
        Ok(())
    }
    fn reject(&mut self, reason: PushError, sample: SharedSample) -> Result<(), RejectedSample> {
        self.stats.rejected_samples = self.stats.rejected_samples.saturating_add(1);
        Err(RejectedSample { reason, sample })
    }
    /// One bounded mux/queue step, then at most one Caller datagram. Send the
    /// returned datagram immediately; socket submission failure ends the epoch.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<(SocketAddr, Vec<u8>)> {
        self.advance(now);
        if self.has_immediate_work() {
            if self.pending.is_none() {
                match self.mux.as_mut().map(MpegTsMuxer::poll_chunk) {
                    Some(Ok(chunk)) => self.pending = chunk,
                    Some(Err(error)) => self.fail(PublisherError::Mux(error)),
                    None => {}
                }
            }
            if let Some(chunk) = self.pending.take() {
                self.stats.peak_pending_chunk_capacity =
                    self.stats.peak_pending_chunk_capacity.max(chunk.capacity());
                match self.caller.try_enqueue(self.now, chunk) {
                    Ok(()) => {
                        self.stats.chunks_enqueued = self.stats.chunks_enqueued.saturating_add(1)
                    }
                    Err((SendError::QueueFull, chunk)) => {
                        self.pending = Some(chunk);
                        self.blocked = true;
                        self.stats.backpressure_events =
                            self.stats.backpressure_events.saturating_add(1);
                    }
                    Err((error, _)) => self.fail(PublisherError::Transport(error)),
                }
            }
            if self.pending.is_none() && self.mux.as_ref().is_some_and(MpegTsMuxer::is_idle) {
                self.sample_since = None;
            }
        }
        let output = self.caller.poll_transmit(self.now);
        self.release_if_closed();
        output
    }
    pub fn handle_datagram(&mut self, now: Instant, peer: SocketAddr, bytes: &[u8]) {
        self.advance(now);
        let before = self.caller.stats().send;
        self.caller.handle_datagram(self.now, peer, bytes);
        let after = self.caller.stats().send;
        if after.buffered_packets < before.buffered_packets
            || after.buffered_bytes < before.buffered_bytes
        {
            self.blocked = false;
        }
        self.release_if_closed();
    }
    pub fn handle_timeout(&mut self, now: Instant) {
        self.advance(now);
    }
    /// True means another bounded mux step is useful now, even if the previous
    /// poll returned None because Caller pacing delayed the next datagram.
    pub fn has_immediate_work(&self) -> bool {
        self.is_connected()
            && !self.blocked
            && (self.pending.is_some() || self.mux.as_ref().is_some_and(|mux| !mux.is_idle()))
    }
    pub fn poll_timeout(&self) -> Option<Instant> {
        let mut next = self.caller.poll_timeout();
        if self.has_immediate_work() {
            next = Some(next.map_or(self.now, |at| at.min(self.now)));
        }
        if let Some(at) = self
            .sample_since
            .and_then(|at| at.checked_add(self.max_sample_residence))
        {
            next = Some(next.map_or(at, |next| next.min(at)));
        }
        next
    }
    /// Cancel immediately; releases the sample, pending chunk and all Caller
    /// queues/keys. One best-effort shutdown may still be polled afterwards.
    pub fn stop(&mut self, now: Instant) {
        if now < self.now {
            self.stats.clock_regressions = self.stats.clock_regressions.saturating_add(1);
        }
        self.now = self.now.max(now);
        self.caller.stop(self.now);
        self.release_if_closed();
    }
    fn advance(&mut self, now: Instant) {
        if now < self.now {
            self.stats.clock_regressions = self.stats.clock_regressions.saturating_add(1);
        }
        self.now = self.now.max(now);
        self.caller.handle_timeout(self.now);
        self.release_if_closed();
        if self
            .sample_since
            .is_some_and(|at| self.now.saturating_duration_since(at) >= self.max_sample_residence)
        {
            self.fail(PublisherError::SampleExpired);
        }
    }
    fn fail(&mut self, error: PublisherError) {
        if self.failure.is_none() {
            self.failure = Some(error);
            self.failure_pending = true;
        }
        self.caller.stop(self.now);
        self.release_if_closed();
    }
    fn release_if_closed(&mut self) {
        if !self.caller.is_closed() {
            return;
        }
        if let Some(mux) = self.mux.take() {
            self.stats.mux = mux.stats();
            self.stats.mux.discarded_sample_bytes = self
                .stats
                .mux
                .discarded_sample_bytes
                .saturating_add(self.stats.mux.buffered_sample_bytes as u64);
            self.stats.mux.buffered_sample_bytes = 0;
            self.stats.mux.buffered_media_duration_ns = 0;
            self.stats.mux.piece_metadata_bytes = 0;
        }
        if let Some(chunk) = self.pending.take() {
            self.stats.discarded_pending_chunk_bytes = self
                .stats
                .discarded_pending_chunk_bytes
                .saturating_add(chunk.len() as u64);
        }
        self.sample_since = None;
        self.blocked = false;
    }
}

#[cfg(test)]
#[path = "srt_tests.rs"]
mod tests;
