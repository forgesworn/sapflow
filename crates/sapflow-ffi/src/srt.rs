//! Typed native binding to the real bounded MPEG-TS/SRT publisher.
//!
//! The platform owns UDP, numeric address resolution, scheduling and session
//! reconnect policy. Drain a bounded batch while immediate work remains, then
//! yield. Keep at most one returned datagram outstanding; a failed UDP send
//! must call `socket_send_failed` and discard that datagram.
//!
//! Input MediaSample handles share compressed payloads. This boundary adds one
//! Arc clone per submission, plus the SDK validation handle clone. TS staging,
//! serialisation and transport wire copies remain visible in statistics.
//! UniFFI output serialisation and native socket submission add platform copies.
//!
//! Passphrase Vec ownership enters the core's zeroising storage immediately,
//! including invalid configuration paths. Neither this object nor its config
//! implements Debug for secrets. Foreign arrays, UniFFI conversion buffers and
//! platform crash dumps are outside this erasure guarantee. AES-CTR does not
//! authenticate packets or peers; SRT controls and stream IDs remain clear.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use sapflow_client::{
    CodecConfig,
    mpegts::{MpegTsConfig, MuxError},
    srt::{PublisherConfig, PublisherError, PublisherEvent, PushError, SrtPublisher},
};
use sapflow_srt::{
    CallerCloseReason, CallerConfig, CallerEvent, SendConfig,
    crypto::{CryptoConfig, KeySize},
};

use crate::{MediaCodec, MediaSample};

const MAX_DATAGRAM: usize = 1500;
const MAX_ADDRESS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeSrtCipher {
    Aes128,
    Aes256,
}
impl From<NativeSrtCipher> for KeySize {
    fn from(value: NativeSrtCipher) -> Self {
        match value {
            NativeSrtCipher::Aes128 => Self::Aes128,
            NativeSrtCipher::Aes256 => Self::Aes256,
        }
    }
}

/// The secret is deliberately a separate constructor argument. None for both
/// cipher and passphrase explicitly selects plaintext; any mismatch is refused.
#[derive(Clone, uniffi::Record)]
pub struct NativeSrtConfig {
    pub destination: String,
    pub stream_id: String,
    pub cipher: Option<NativeSrtCipher>,
    pub latency_ms: u32,
    pub max_buffered_packets: u32,
    pub max_buffered_bytes: u32,
    pub max_packet_age_ms: u32,
    pub max_sample_residence_ms: u32,
    /// SRT packet bytes per second, including originals and retransmissions.
    pub bandwidth_bytes_per_second: u64,
}
impl Default for NativeSrtConfig {
    fn default() -> Self {
        Self {
            destination: String::new(),
            stream_id: String::new(),
            cipher: None,
            latency_ms: 120,
            max_buffered_packets: 4096,
            max_buffered_bytes: 4 * 1024 * 1024,
            max_packet_age_ms: 3000,
            max_sample_residence_ms: 1000,
            bandwidth_bytes_per_second: 1_250_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Error)]
pub enum NativeSrtError {
    InvalidConfig,
    InvalidMedia,
    ConfigurationChanged,
    InvalidDatagram,
    ClockWentBackwards,
    ClockOverflow,
    Busy,
    Backpressure,
    NotConnected,
    Closed,
}
impl std::fmt::Display for NativeSrtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid native SRT configuration",
            Self::InvalidMedia => "invalid or discontinuous SRT media",
            Self::ConfigurationChanged => "SRT track configuration changed within the epoch",
            Self::InvalidDatagram => "invalid native SRT datagram or numeric source address",
            Self::ClockWentBackwards => "native SRT monotonic clock went backwards",
            Self::ClockOverflow => "native SRT clock exceeds its representation",
            Self::Busy => "native SRT state is busy",
            Self::Backpressure => "native SRT publisher still holds a media sample",
            Self::NotConnected => "native SRT transport has not connected",
            Self::Closed => "native SRT connection is closed",
        })
    }
}
impl std::error::Error for NativeSrtError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeSrtState {
    Connecting,
    /// Handshake complete, not destination-observed playback.
    Connected,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeSrtCloseReason {
    Cancelled,
    SocketSendFailed,
    ConnectTimeout,
    HandshakeLimit,
    PeerIdle,
    Refused,
    UnsupportedPeer,
    Crypto,
    SampleExpired,
    MediaExpired,
    RetryLimit,
    PeerClosed,
    PeerError,
    PackagingFailed,
}
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeSrtEvent {
    Connected {
        latency_ms: u32,
        max_payload_bytes: u32,
    },
    Failed {
        reason: NativeSrtCloseReason,
    },
    Closed {
        reason: NativeSrtCloseReason,
        peer_code: Option<u32>,
    },
}
#[derive(uniffi::Record)]
pub struct NativeSrtDatagram {
    /// The same numeric peer fixed at construction; no DNS is performed.
    pub destination: String,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Default, uniffi::Record)]
pub struct NativeSrtStats {
    pub accepted_samples: u64,
    pub accepted_sample_bytes: u64,
    pub buffered_sample_bytes: u64,
    pub buffered_sample_duration_ns: u64,
    pub discarded_sample_bytes: u64,
    pub pending_chunk_bytes: u64,
    pub pending_chunk_capacity: u64,
    pub discarded_pending_chunk_bytes: u64,
    pub buffered_packets: u64,
    pub buffered_bytes: u64,
    pub peak_buffered_bytes: u64,
    pub sent_packets: u64,
    pub sent_bytes: u64,
    pub retransmitted_packets: u64,
    pub retransmitted_bytes: u64,
    pub acknowledged_bytes: u64,
    pub abandoned_bytes: u64,
    pub encrypted_packets: u64,
    pub km_requests: u64,
    pub km_responses: u64,
    pub rtt_us: u64,
    pub malformed_datagrams: u64,
    pub stray_datagrams: u64,
    pub rejected_datagrams: u64,
    pub backpressure_events: u64,
    pub ffi_input_handle_clones: u64,
    pub sdk_input_handle_clones: u64,
    pub ts_output_allocations: u64,
    pub elementary_staging_copy_bytes: u64,
    pub packet_payload_copy_bytes: u64,
    pub writer_payload_copy_bytes: u64,
    pub ts_serialised_bytes: u64,
    pub returned_datagrams: u64,
    pub returned_datagram_bytes: u64,
}

struct State {
    publisher: SrtPublisher,
    epoch: Instant,
    epoch_us: u64,
    latest_us: u64,
    latest: Instant,
    socket_failed: bool,
    handle_clones: u64,
    rejected_datagrams: u64,
    returned_datagrams: u64,
    returned_datagram_bytes: u64,
}
impl State {
    fn clock(&mut self, now_us: u64) -> Result<Instant, NativeSrtError> {
        if now_us < self.latest_us {
            return Err(NativeSrtError::ClockWentBackwards);
        }
        let elapsed = now_us - self.epoch_us;
        // Caller uses signed microseconds internally; refuse its saturation.
        if elapsed > i64::MAX as u64 {
            return Err(NativeSrtError::ClockOverflow);
        }
        let now = self
            .epoch
            .checked_add(Duration::from_micros(elapsed))
            .ok_or(NativeSrtError::ClockOverflow)?;
        self.latest = now;
        self.latest_us = now_us;
        Ok(now)
    }
    fn cancel(&mut self) {
        self.publisher.stop(self.latest);
    }
}

#[derive(uniffi::Object)]
pub struct NativeSrtPublisher {
    state: Mutex<State>,
}
impl NativeSrtPublisher {
    fn state_guard(&self) -> Result<MutexGuard<'_, State>, NativeSrtError> {
        self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => NativeSrtError::Busy,
            std::sync::TryLockError::Poisoned(poisoned) => {
                // Never resume state which a panic could have partly mutated.
                // Leave the poison set and release all media, packets and keys.
                poisoned.into_inner().cancel();
                NativeSrtError::Closed
            }
        })
    }
}

fn address(value: &str) -> Option<SocketAddr> {
    (value.len() <= MAX_ADDRESS)
        .then(|| value.parse().ok())
        .flatten()
}
fn local_reason(error: PublisherError) -> NativeSrtCloseReason {
    match error {
        PublisherError::SampleExpired => NativeSrtCloseReason::SampleExpired,
        PublisherError::Mux(_) | PublisherError::Transport(_) => {
            NativeSrtCloseReason::PackagingFailed
        }
    }
}
fn close_reason(reason: CallerCloseReason) -> (NativeSrtCloseReason, Option<u32>) {
    use NativeSrtCloseReason as N;
    match reason {
        CallerCloseReason::Cancelled => (N::Cancelled, None),
        CallerCloseReason::ConnectTimeout => (N::ConnectTimeout, None),
        CallerCloseReason::HandshakeLimit => (N::HandshakeLimit, None),
        CallerCloseReason::PeerIdle => (N::PeerIdle, None),
        CallerCloseReason::Rejected(code) => (N::Refused, Some(code)),
        CallerCloseReason::UnsupportedPeer => (N::UnsupportedPeer, None),
        CallerCloseReason::Crypto => (N::Crypto, None),
        CallerCloseReason::MediaExpired => (N::MediaExpired, None),
        CallerCloseReason::RetryLimit => (N::RetryLimit, None),
        CallerCloseReason::PeerClosed => (N::PeerClosed, None),
        CallerCloseReason::PeerError(code) => (N::PeerError, Some(code)),
    }
}

#[uniffi::export]
impl NativeSrtPublisher {
    /// Native timestamps must already share their capture epoch; no A/V
    /// retiming is performed. Codec configurations are frozen for this object.
    #[uniffi::constructor]
    pub fn new(
        config: NativeSrtConfig,
        video: Option<Arc<MediaCodec>>,
        audio: Option<Arc<MediaCodec>>,
        passphrase: Option<Vec<u8>>,
        now_us: u64,
    ) -> Result<Arc<Self>, NativeSrtError> {
        // Transfer the secret before any other validation can return. Even a
        // mismatched cipher/secret enters the core's zeroising owner first.
        let crypto = passphrase
            .map(|bytes| {
                CryptoConfig::new(
                    bytes,
                    config.cipher.unwrap_or(NativeSrtCipher::Aes128).into(),
                )
            })
            .transpose()
            .map_err(|_| NativeSrtError::InvalidConfig)?;
        if crypto.is_some() != config.cipher.is_some() || now_us.checked_add(60_000_000).is_none() {
            return Err(NativeSrtError::InvalidConfig);
        }
        let peer = address(&config.destination).ok_or(NativeSrtError::InvalidConfig)?;
        if peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || peer.port() == 0
            || matches!(peer.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(NativeSrtError::InvalidConfig);
        }
        let mut mux = MpegTsConfig::default();
        if let Some(codec) = video {
            let CodecConfig::H264(value) = codec.config().as_ref() else {
                return Err(NativeSrtError::InvalidConfig);
            };
            mux.video = Some(value.clone());
        }
        if let Some(codec) = audio {
            let CodecConfig::Aac(value) = codec.config().as_ref() else {
                return Err(NativeSrtError::InvalidConfig);
            };
            mux.audio = Some(*value);
        }
        let config = PublisherConfig {
            mux,
            max_sample_residence: Duration::from_millis(u64::from(config.max_sample_residence_ms)),
            caller: CallerConfig {
                stream_id: config.stream_id,
                crypto,
                latency: Duration::from_millis(u64::from(config.latency_ms)),
                send: SendConfig {
                    max_packets: config.max_buffered_packets as usize,
                    max_bytes: config.max_buffered_bytes as usize,
                    max_age_us: u64::from(config.max_packet_age_ms) * 1000,
                    bandwidth: config.bandwidth_bytes_per_second,
                    ..SendConfig::default()
                },
                ..CallerConfig::default()
            },
        };
        let epoch = Instant::now();
        let publisher =
            SrtPublisher::new(config, peer, epoch).map_err(|_| NativeSrtError::InvalidConfig)?;
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                publisher,
                epoch,
                epoch_us: now_us,
                latest_us: now_us,
                latest: epoch,
                socket_failed: false,
                handle_clones: 0,
                rejected_datagrams: 0,
                returned_datagrams: 0,
                returned_datagram_bytes: 0,
            }),
        }))
    }

    /// The caller retains its MediaSample on error. Busy/backpressure may be
    /// retried by the bounded native ingress worker; codec callbacks never wait.
    pub fn push(&self, sample: Arc<MediaSample>, now_us: u64) -> Result<(), NativeSrtError> {
        let mut state = self.state_guard()?;
        let now = state.clock(now_us)?;
        state.handle_clones = state.handle_clones.saturating_add(1);
        state
            .publisher
            .push(now, Arc::clone(&sample.sample))
            .map_err(|rejected| match rejected.reason {
                PushError::NotConnected => NativeSrtError::NotConnected,
                PushError::Busy => NativeSrtError::Backpressure,
                PushError::Closed => NativeSrtError::Closed,
                PushError::Mux(MuxError::ConfigurationChanged | MuxError::TrackNotConfigured) => {
                    NativeSrtError::ConfigurationChanged
                }
                PushError::Mux(_) => NativeSrtError::InvalidMedia,
            })
    }
    /// One complete UDP datagram, never a concatenated receive buffer. Wrong
    /// peers are counted and ignored by Caller. Oversized buffers are refused
    /// before protocol parsing or payload ownership allocation.
    pub fn feed(&self, source: String, bytes: &[u8], now_us: u64) -> Result<(), NativeSrtError> {
        let mut state = self.state_guard()?;
        let now = state.clock(now_us)?;
        let peer = address(&source);
        if bytes.len() > MAX_DATAGRAM || peer.is_none() {
            state.rejected_datagrams = state.rejected_datagrams.saturating_add(1);
            // Malformed input cannot postpone the sample/connection deadline.
            state.publisher.handle_timeout(now);
            return Err(NativeSrtError::InvalidDatagram);
        }
        if let Some(peer) = peer {
            state.publisher.handle_datagram(now, peer, bytes);
        }
        Ok(())
    }
    pub fn tick(&self, now_us: u64) -> Result<(), NativeSrtError> {
        let mut state = self.state_guard()?;
        let now = state.clock(now_us)?;
        state.publisher.handle_timeout(now);
        Ok(())
    }
    /// One bounded mux step and at most one datagram. None may still leave
    /// immediate mux work; use has_immediate_work or next_deadline_us.
    pub fn poll_datagram(&self, now_us: u64) -> Result<Option<NativeSrtDatagram>, NativeSrtError> {
        let mut state = self.state_guard()?;
        let now = state.clock(now_us)?;
        let Some((peer, bytes)) = state.publisher.poll_transmit(now) else {
            return Ok(None);
        };
        if bytes.len() > MAX_DATAGRAM {
            state.cancel();
            return Err(NativeSrtError::InvalidDatagram);
        }
        state.returned_datagrams = state.returned_datagrams.saturating_add(1);
        state.returned_datagram_bytes = state
            .returned_datagram_bytes
            .saturating_add(bytes.len() as u64);
        Ok(Some(NativeSrtDatagram {
            destination: peer.to_string(),
            bytes,
        }))
    }
    pub fn is_ready(&self) -> Result<bool, NativeSrtError> {
        Ok(self.state_guard()?.publisher.is_ready())
    }
    pub fn has_immediate_work(&self) -> Result<bool, NativeSrtError> {
        Ok(self.state_guard()?.publisher.has_immediate_work())
    }
    /// Absolute native monotonic microseconds. Retain real clock precision;
    /// rounding the input to milliseconds would quantise packet pacing. None means no timer.
    pub fn next_deadline_us(&self) -> Result<Option<u64>, NativeSrtError> {
        let state = self.state_guard()?;
        state
            .publisher
            .poll_timeout()
            .map(|at| {
                let delta = at
                    .saturating_duration_since(state.epoch)
                    .as_nanos()
                    .div_ceil(1000);
                let delta = u64::try_from(delta).map_err(|_| NativeSrtError::ClockOverflow)?;
                state
                    .epoch_us
                    .checked_add(delta)
                    .ok_or(NativeSrtError::ClockOverflow)
            })
            .transpose()
    }
    pub fn state(&self) -> Result<NativeSrtState, NativeSrtError> {
        let state = self.state_guard()?;
        Ok(if state.publisher.is_closed() {
            NativeSrtState::Closed
        } else if state.publisher.is_connected() {
            NativeSrtState::Connected
        } else {
            NativeSrtState::Connecting
        })
    }
    pub fn poll_event(&self) -> Result<Option<NativeSrtEvent>, NativeSrtError> {
        let mut state = self.state_guard()?;
        Ok(state.publisher.poll_event().map(|event| match event {
            PublisherEvent::Transport(CallerEvent::Connected {
                latency,
                max_payload,
                ..
            }) => NativeSrtEvent::Connected {
                latency_ms: latency.as_millis() as u32,
                max_payload_bytes: max_payload as u32,
            },
            PublisherEvent::Failed(error) => NativeSrtEvent::Failed {
                reason: local_reason(error),
            },
            PublisherEvent::Transport(CallerEvent::Closed { reason }) => {
                let (mut reason, peer_code) = close_reason(reason);
                if reason == NativeSrtCloseReason::Cancelled {
                    if state.socket_failed {
                        reason = NativeSrtCloseReason::SocketSendFailed;
                    } else if let Some(error) = state.publisher.failure() {
                        reason = local_reason(error);
                    }
                }
                NativeSrtEvent::Closed { reason, peer_code }
            }
        }))
    }
    /// Cancellation always releases media, even if now_us is invalid. The
    /// invalid clock is still reported; it cannot prevent cleanup.
    pub fn stop(&self, now_us: u64) -> Result<(), NativeSrtError> {
        let mut state = self.state_guard()?;
        let time = state.clock(now_us);
        state.cancel();
        time.map(|_| ())
    }
    /// Call after a returned datagram cannot be submitted in full. The native
    /// worker discards its outstanding datagram and ends this attempt.
    pub fn socket_send_failed(&self, now_us: u64) -> Result<(), NativeSrtError> {
        let mut state = self.state_guard()?;
        let time = state.clock(now_us);
        if !state.publisher.is_closed() {
            state.socket_failed = true;
        }
        state.cancel();
        time.map(|_| ())
    }
    pub fn stats(&self) -> Result<NativeSrtStats, NativeSrtError> {
        let state = self.state_guard()?;
        let stats = state.publisher.stats();
        let send = stats.transport.send;
        Ok(NativeSrtStats {
            accepted_samples: stats.mux.accepted_samples,
            accepted_sample_bytes: stats.mux.accepted_sample_bytes,
            buffered_sample_bytes: stats.mux.buffered_sample_bytes as u64,
            buffered_sample_duration_ns: stats.mux.buffered_media_duration_ns,
            discarded_sample_bytes: stats.mux.discarded_sample_bytes,
            pending_chunk_bytes: stats.pending_chunk_bytes as u64,
            pending_chunk_capacity: stats.pending_chunk_capacity as u64,
            discarded_pending_chunk_bytes: stats.discarded_pending_chunk_bytes,
            buffered_packets: send.buffered_packets as u64,
            buffered_bytes: send.buffered_bytes as u64,
            peak_buffered_bytes: send.peak_buffered_bytes as u64,
            sent_packets: send.sent_packets,
            sent_bytes: send.sent_bytes,
            retransmitted_packets: send.retransmitted_packets,
            retransmitted_bytes: send.retransmitted_bytes,
            acknowledged_bytes: send.acknowledged_bytes,
            abandoned_bytes: send.abandoned_bytes,
            encrypted_packets: send.encrypted_packets,
            km_requests: send.km_requests,
            km_responses: send.km_responses,
            rtt_us: send.rtt_us,
            malformed_datagrams: stats.transport.malformed,
            stray_datagrams: stats.transport.stray,
            rejected_datagrams: state.rejected_datagrams,
            backpressure_events: stats.backpressure_events,
            ffi_input_handle_clones: state.handle_clones,
            sdk_input_handle_clones: stats.input_handle_clones,
            ts_output_allocations: stats.mux.output_allocations,
            elementary_staging_copy_bytes: stats.mux.elementary_staging_copy_bytes,
            packet_payload_copy_bytes: stats.mux.packet_payload_copy_bytes,
            writer_payload_copy_bytes: stats.mux.writer_payload_copy_bytes,
            ts_serialised_bytes: stats.mux.emitted_bytes,
            returned_datagrams: state.returned_datagrams,
            returned_datagram_bytes: state.returned_datagram_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sapflow_client::mpegts::MpegTsMuxer;
    use sapflow_srt::{Config, Event, Listener};

    const SOURCE: &str = "127.0.0.1:9001";
    const PEER: &str = "127.0.0.1:9000";
    const SECRET: &[u8] = b"native SRT test passphrase";
    const START_US: u64 = 1_000_000;

    fn audio() -> Arc<MediaCodec> {
        MediaCodec::aac(&[0x11, 0x90]).expect("AAC")
    }
    fn config() -> NativeSrtConfig {
        NativeSrtConfig {
            destination: PEER.into(),
            stream_id: "publish:test".into(),
            cipher: Some(NativeSrtCipher::Aes256),
            ..NativeSrtConfig::default()
        }
    }
    fn publisher(config: NativeSrtConfig) -> Arc<NativeSrtPublisher> {
        NativeSrtPublisher::new(config, None, Some(audio()), Some(SECRET.to_vec()), START_US)
            .expect("publisher")
    }
    fn sample(size: usize) -> Arc<MediaSample> {
        audio()
            .sample(&vec![0x21; size], 0, 0, 1024, 48000, false)
            .expect("sample")
    }
    fn exchange(
        publisher: &NativeSrtPublisher,
        listener: &mut Listener,
        n: u64,
        received: &mut Vec<u8>,
    ) {
        let at = publisher.state.lock().expect("state").epoch + Duration::from_millis(n);
        let now_us = START_US + n * 1000;
        publisher.tick(now_us).expect("tick");
        listener.handle_timeout(at);
        for _ in 0..32 {
            match publisher.poll_datagram(now_us).expect("poll") {
                Some(datagram) => {
                    assert_eq!(datagram.destination, PEER);
                    assert!(datagram.bytes.len() <= MAX_DATAGRAM);
                    listener.handle_datagram(at, SOURCE.parse().expect("source"), &datagram.bytes);
                }
                None if publisher.has_immediate_work().expect("work") => continue,
                None => break,
            }
        }
        while let Some(event) = listener.poll_event() {
            match event {
                Event::Connect { id, stream_id, .. } => {
                    assert_eq!(stream_id, b"publish:test");
                    assert!(listener.accept(id, at));
                }
                Event::Data { payload, .. } => received.extend(payload),
                Event::Closed { .. } => {}
            }
        }
        while let Some((_, bytes)) = listener.poll_transmit() {
            publisher.feed(PEER.into(), &bytes, now_us).expect("feed");
        }
    }
    fn open(config: NativeSrtConfig) -> (Arc<NativeSrtPublisher>, Listener) {
        let key_size = config.cipher.expect("encrypted test profile").into();
        let publisher = publisher(config);
        let epoch = publisher.state.lock().expect("state").epoch;
        let mut listener = Listener::new(
            Config {
                crypto: Some(CryptoConfig::new(SECRET.to_vec(), key_size).expect("key")),
                require_encryption: true,
                ..Config::default()
            },
            epoch,
        );
        for n in 0..10 {
            exchange(&publisher, &mut listener, n, &mut Vec::new());
        }
        assert_eq!(publisher.state().expect("state"), NativeSrtState::Connected);
        assert!(publisher.is_ready().expect("ready"));
        (publisher, listener)
    }

    #[test]
    fn encrypted_native_handle_mux_and_transport_deliver_real_ts() {
        let (publisher, mut listener) = open(config());
        let media = sample(8000);
        let weak = Arc::downgrade(&media.sample);
        let audio = match audio().config().as_ref() {
            CodecConfig::Aac(value) => *value,
            _ => panic!("AAC"),
        };
        let mut mux = MpegTsMuxer::new(MpegTsConfig {
            audio: Some(audio),
            ..MpegTsConfig::default()
        })
        .expect("mux");
        mux.push(Arc::clone(&media.sample)).expect("sample");
        let mut expected = Vec::new();
        while let Some(chunk) = mux.poll_chunk().expect("chunk") {
            expected.extend(chunk);
        }
        publisher
            .push(Arc::clone(&media), START_US + 10000)
            .expect("push");
        assert_eq!(Arc::strong_count(&media.sample), 2);
        drop(media);
        let mut received = Vec::new();
        for n in 10..500 {
            exchange(&publisher, &mut listener, n, &mut received);
        }
        assert_eq!(received, expected);
        assert!(received.len() > 1316);
        assert!(weak.upgrade().is_none());
        let stats = publisher.stats().expect("stats");
        assert_eq!(stats.accepted_samples, 1);
        assert_eq!(stats.ffi_input_handle_clones, 1);
        assert_eq!(stats.sdk_input_handle_clones, 1);
        assert!(stats.encrypted_packets > 1);
        assert_eq!(stats.buffered_bytes, 0);
        assert!(stats.elementary_staging_copy_bytes > 0);
        assert!(matches!(
            publisher.poll_event().expect("event"),
            Some(NativeSrtEvent::Connected { .. })
        ));
        publisher.stop(START_US + 500000).expect("stop");
        assert_eq!(
            publisher.poll_event().expect("event"),
            Some(NativeSrtEvent::Closed {
                reason: NativeSrtCloseReason::Cancelled,
                peer_code: None,
            })
        );
        assert!(publisher.poll_event().expect("event").is_none());
    }

    #[test]
    fn constructor_refuses_invalid_track_peer_crypto_and_limits() {
        for destination in [
            "example.invalid:9000",
            "127.0.0.1:0",
            "0.0.0.0:9000",
            "255.255.255.255:9000",
            "224.0.0.1:9000",
        ] {
            let mut cfg = config();
            cfg.destination = destination.into();
            assert!(matches!(
                NativeSrtPublisher::new(cfg, None, Some(audio()), Some(SECRET.to_vec()), 0),
                Err(NativeSrtError::InvalidConfig)
            ));
        }
        let mut cfg = config();
        cfg.destination = "x".repeat(MAX_ADDRESS + 1);
        assert!(
            NativeSrtPublisher::new(cfg, None, Some(audio()), Some(SECRET.to_vec()), 0).is_err()
        );
        assert!(
            NativeSrtPublisher::new(config(), Some(audio()), None, Some(SECRET.to_vec()), 0)
                .is_err()
        );
        assert!(NativeSrtPublisher::new(config(), None, None, Some(SECRET.to_vec()), 0).is_err());
        assert!(NativeSrtPublisher::new(config(), None, Some(audio()), None, 0).is_err());
        let mut plaintext = config();
        plaintext.cipher = None;
        assert!(
            NativeSrtPublisher::new(
                plaintext.clone(),
                None,
                Some(audio()),
                Some(SECRET.to_vec()),
                0
            )
            .is_err()
        );
        assert!(NativeSrtPublisher::new(plaintext, None, Some(audio()), None, 0).is_ok());
        for secret in [vec![1; 9], vec![1; 80], vec![0; 16]] {
            assert!(
                NativeSrtPublisher::new(config(), None, Some(audio()), Some(secret), 0).is_err()
            );
        }
        for field in 0..5 {
            let mut cfg = config();
            match field {
                0 => cfg.max_buffered_bytes = 1315,
                1 => cfg.max_buffered_packets = 0,
                2 => cfg.max_packet_age_ms = 60_001,
                3 => cfg.max_sample_residence_ms = 0,
                _ => cfg.bandwidth_bytes_per_second = 0,
            }
            assert!(
                NativeSrtPublisher::new(cfg, None, Some(audio()), Some(SECRET.to_vec()), 0)
                    .is_err()
            );
        }
        assert!(
            NativeSrtPublisher::new(
                config(),
                None,
                Some(audio()),
                Some(SECRET.to_vec()),
                u64::MAX
            )
            .is_err()
        );
    }

    #[test]
    fn datagram_and_clock_bounds_are_atomic_and_wrong_peer_is_ignored() {
        let publisher = publisher(config());
        assert!(
            publisher
                .next_deadline_us()
                .expect("deadline")
                .is_some_and(|time| time == START_US)
        );
        assert_eq!(
            publisher.feed(SOURCE.into(), &[0; MAX_DATAGRAM + 1], START_US),
            Err(NativeSrtError::InvalidDatagram)
        );
        assert_eq!(
            publisher.feed("x".repeat(MAX_ADDRESS + 1), &[0; 16], START_US),
            Err(NativeSrtError::InvalidDatagram)
        );
        publisher
            .feed(SOURCE.into(), &[0; 16], START_US)
            .expect("stray");
        assert_eq!(publisher.stats().expect("stats").stray_datagrams, 1);
        assert_eq!(publisher.stats().expect("stats").rejected_datagrams, 2);
        publisher.tick(START_US + 10000).expect("tick");
        assert_eq!(
            publisher.tick(START_US + 9000),
            Err(NativeSrtError::ClockWentBackwards)
        );
        assert_eq!(publisher.tick(u64::MAX), Err(NativeSrtError::ClockOverflow));
        assert_eq!(
            publisher.state.lock().expect("state").latest_us,
            START_US + 10000
        );
        assert_eq!(
            publisher.stop(START_US),
            Err(NativeSrtError::ClockWentBackwards)
        );
        assert_eq!(publisher.state().expect("state"), NativeSrtState::Closed);
        assert!(publisher.next_deadline_us().expect("deadline").is_none());
    }

    #[test]
    fn native_backpressure_retains_handle_and_stop_releases_every_budget() {
        let mut cfg = config();
        cfg.max_buffered_bytes = 1316;
        let (publisher, _) = open(cfg);
        let media = sample(8000);
        let weak = Arc::downgrade(&media.sample);
        publisher.push(media, START_US + 10000).expect("push");
        publisher.poll_datagram(START_US + 10000).expect("poll");
        publisher.poll_datagram(START_US + 10000).expect("poll");
        assert!(!publisher.has_immediate_work().expect("work"));
        assert!(
            publisher
                .next_deadline_us()
                .expect("deadline")
                .is_some_and(|at| at > START_US + 10000)
        );
        assert_eq!(
            publisher.push(sample(5), START_US + 10000),
            Err(NativeSrtError::Backpressure)
        );
        let before = publisher.stats().expect("stats");
        for _ in 0..100 {
            publisher.poll_datagram(START_US + 10000).expect("poll");
        }
        assert_eq!(
            publisher.stats().expect("stats").ts_output_allocations,
            before.ts_output_allocations
        );
        assert_eq!(before.pending_chunk_capacity, 1316);
        assert!(weak.upgrade().is_some());
        publisher
            .socket_send_failed(START_US + 10000)
            .expect("socket failure");
        assert!(weak.upgrade().is_none());
        let stats = publisher.stats().expect("stats");
        assert_eq!(
            stats.buffered_bytes + stats.buffered_sample_bytes + stats.pending_chunk_capacity,
            0
        );
        assert!(stats.discarded_pending_chunk_bytes > 0);
        assert_eq!(publisher.state().expect("state"), NativeSrtState::Closed);
        assert!(matches!(
            publisher.poll_event().expect("connected"),
            Some(NativeSrtEvent::Connected { .. })
        ));
        assert_eq!(
            publisher.poll_event().expect("closed"),
            Some(NativeSrtEvent::Closed {
                reason: NativeSrtCloseReason::SocketSendFailed,
                peer_code: None,
            })
        );
        publisher.stop(START_US + 11000).expect("idempotent stop");
        assert_eq!(
            publisher.push(sample(5), START_US + 11000),
            Err(NativeSrtError::Closed)
        );
    }

    #[test]
    fn mutex_busy_is_nonblocking_and_poison_permanently_releases_sample() {
        let (publisher, _) = open(config());
        let media = sample(8000);
        let weak = Arc::downgrade(&media.sample);
        publisher.push(media, START_US + 10000).expect("push");
        {
            let _guard = publisher.state.lock().expect("lock");
            assert_eq!(publisher.is_ready(), Err(NativeSrtError::Busy));
            assert_eq!(publisher.stop(START_US + 10000), Err(NativeSrtError::Busy));
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = publisher.state.lock().expect("lock");
            panic!("test interrupted state update");
        }));
        assert!(panic.is_err());
        assert!(weak.upgrade().is_some());
        assert_eq!(publisher.state(), Err(NativeSrtError::Closed));
        assert!(weak.upgrade().is_none());
        assert_eq!(
            publisher.tick(START_US + 11000),
            Err(NativeSrtError::Closed)
        );
        assert!(publisher.state.is_poisoned());
    }

    #[test]
    fn sample_expiry_reports_local_failure_and_frozen_configuration_is_enforced() {
        let mut cfg = config();
        cfg.max_sample_residence_ms = 1;
        let (publisher, _) = open(cfg);
        let changed = MediaCodec::aac(&[0x12, 0x10])
            .expect("44.1k")
            .sample(&[1, 2, 3], 0, 0, 1024, 44100, false)
            .expect("sample");
        assert_eq!(
            publisher.push(changed, START_US + 10000),
            Err(NativeSrtError::ConfigurationChanged)
        );
        let media = sample(8000);
        let weak = Arc::downgrade(&media.sample);
        publisher.push(media, START_US + 10000).expect("push");
        assert_eq!(
            publisher.feed(PEER.into(), &[0; MAX_DATAGRAM + 1], START_US + 11000),
            Err(NativeSrtError::InvalidDatagram)
        );
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            publisher.poll_event().expect("event"),
            Some(NativeSrtEvent::Connected { .. })
        ));
        assert_eq!(
            publisher.poll_event().expect("failed"),
            Some(NativeSrtEvent::Failed {
                reason: NativeSrtCloseReason::SampleExpired
            })
        );
        assert_eq!(
            publisher.poll_event().expect("closed"),
            Some(NativeSrtEvent::Closed {
                reason: NativeSrtCloseReason::SampleExpired,
                peer_code: None
            })
        );
    }

    #[test]
    fn wrong_secret_is_rejected_without_plaintext_delivery() {
        let publisher = publisher(config());
        let epoch = publisher.state.lock().expect("state").epoch;
        let mut listener = Listener::new(
            Config {
                crypto: Some(
                    CryptoConfig::new(b"different test secret".to_vec(), KeySize::Aes256)
                        .expect("key"),
                ),
                require_encryption: true,
                ..Config::default()
            },
            epoch,
        );
        for n in 0..20 {
            let at = epoch + Duration::from_millis(n);
            publisher.tick(START_US + n * 1000).expect("tick");
            if let Some(datagram) = publisher.poll_datagram(START_US + n * 1000).expect("poll") {
                listener.handle_datagram(at, SOURCE.parse().expect("source"), &datagram.bytes);
            }
            while let Some(event) = listener.poll_event() {
                match event {
                    Event::Connect { id, .. } => {
                        assert!(!listener.accept(id, at));
                    }
                    Event::Data { .. } => panic!("wrong secret delivered media"),
                    Event::Closed { .. } => {}
                }
            }
            while let Some((_, bytes)) = listener.poll_transmit() {
                publisher
                    .feed(PEER.into(), &bytes, START_US + n * 1000)
                    .expect("feed");
            }
        }
        assert_eq!(publisher.state().expect("state"), NativeSrtState::Closed);
        assert_eq!(publisher.stats().expect("stats").sent_packets, 0);
        assert_eq!(
            publisher.poll_event().expect("closed"),
            Some(NativeSrtEvent::Closed {
                reason: NativeSrtCloseReason::Refused,
                peer_code: Some(1010),
            })
        );
    }
    #[test]
    fn aes128_negotiates_and_microsecond_clock_preserves_pacing_rate() {
        let mut cfg = config();
        cfg.cipher = Some(NativeSrtCipher::Aes128);
        cfg.bandwidth_bytes_per_second = 1_000_000;
        let (publisher, _) = open(cfg);
        publisher
            .push(sample(8000), START_US + 10000)
            .expect("sample");
        assert!(
            publisher
                .poll_datagram(START_US + 10000)
                .expect("first")
                .is_some()
        );
        for _ in 0..16 {
            publisher.poll_datagram(START_US + 10000).expect("mux");
        }
        assert!(!publisher.has_immediate_work().expect("work"));
        // 1332 wire bytes at 1 MB/s require 1332 us. Preserve that exact
        // deadline; rounding to 2 ms would impose a lower throughput ceiling.
        assert_eq!(
            publisher.next_deadline_us().expect("deadline"),
            Some(START_US + 11_332)
        );
        assert_eq!(publisher.stats().expect("stats").encrypted_packets, 1);
    }
    #[test]
    fn both_frozen_tracks_allow_ordinary_audio_video_callback_skew() {
        let video = MediaCodec::h264(
            &[
                0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0,
                0, 3, 1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
            ],
            &[0x68, 0xef, 0xbc, 0x80],
        )
        .expect("H264");
        let audio = audio();
        assert!(
            NativeSrtPublisher::new(
                config(),
                None,
                Some(Arc::clone(&video)),
                Some(SECRET.to_vec()),
                START_US
            )
            .is_err()
        );
        let publisher = NativeSrtPublisher::new(
            config(),
            Some(Arc::clone(&video)),
            Some(Arc::clone(&audio)),
            Some(SECRET.to_vec()),
            START_US,
        )
        .expect("A/V publisher");
        let epoch = publisher.state.lock().expect("state").epoch;
        let mut listener = Listener::new(
            Config {
                crypto: Some(CryptoConfig::new(SECRET.to_vec(), KeySize::Aes256).expect("key")),
                require_encryption: true,
                ..Config::default()
            },
            epoch,
        );
        for n in 0..10 {
            exchange(&publisher, &mut listener, n, &mut Vec::new());
        }
        let mut inputs = std::collections::VecDeque::from([
            video
                .sample(&[0, 0, 0, 3, 0x65, 0x80, 0], 0, 0, 3000, 90000, true)
                .expect("IDR"),
            audio
                .sample(&[0x21; 100], 2048, 2048, 1024, 48000, false)
                .expect("AAC ahead"),
            video
                .sample(&[0, 0, 0, 3, 0x41, 0x80, 0], 3000, 3000, 3000, 90000, false)
                .expect("video behind audio"),
        ]);
        let CodecConfig::H264(video_config) = video.config().as_ref() else {
            panic!("H264");
        };
        let CodecConfig::Aac(audio_config) = audio.config().as_ref() else {
            panic!("AAC");
        };
        let mut mux = MpegTsMuxer::new(MpegTsConfig {
            video: Some(video_config.clone()),
            audio: Some(*audio_config),
            ..MpegTsConfig::default()
        })
        .expect("mux");
        let mut expected = Vec::new();
        for input in &inputs {
            mux.push(Arc::clone(&input.sample)).expect("mux sample");
            while let Some(chunk) = mux.poll_chunk().expect("mux chunk") {
                expected.extend(chunk);
            }
        }
        let mut received = Vec::new();
        for n in 10..500 {
            if publisher.is_ready().expect("ready")
                && let Some(input) = inputs.pop_front()
            {
                publisher
                    .push(input, START_US + n * 1000)
                    .expect("submit interleaved sample");
            }
            exchange(&publisher, &mut listener, n, &mut received);
        }
        assert!(inputs.is_empty());
        assert_eq!(received, expected);
        assert_eq!(publisher.stats().expect("stats").accepted_samples, 3);
    }
}
