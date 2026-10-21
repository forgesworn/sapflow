//! Typed native publishing over caller-owned TCP/TLS. Network IO never enters
//! this state machine; publication acceptance is not observed media delivery.

use std::sync::{Arc, Mutex, MutexGuard};

use sapflow_client::{
    CodecConfig,
    transport_progress::{StartupProgressEvent, StartupWatermark},
};
use sapflow_hls::rtmp::client::{ClientError, Publisher, PublisherConfig, PublisherState};

use crate::{MAX_ENCODED_BUFFER_BYTES, MediaSample};
// C0+C1+C2. Proof only: do not infer a counter baseline from the first peer ACK.
// Existing peer-window accounting remains post-handshake and is unchanged.
const RTMP_ACK_HANDSHAKE_MARGIN: u64 = 3073;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRtmpState {
    Handshaking,
    Connecting,
    CreatingStream,
    AwaitingPublish,
    /// The server accepted publication; this does not confirm media delivery.
    Publishing,
    Finishing,
    Closed,
    Failed,
}

impl From<PublisherState> for NativeRtmpState {
    fn from(state: PublisherState) -> Self {
        match state {
            PublisherState::Handshaking => Self::Handshaking,
            PublisherState::Connecting => Self::Connecting,
            PublisherState::CreatingStream => Self::CreatingStream,
            PublisherState::AwaitingPublish => Self::AwaitingPublish,
            PublisherState::Publishing => Self::Publishing,
            PublisherState::Finishing => Self::Finishing,
            PublisherState::Closed => Self::Closed,
            PublisherState::Failed => Self::Failed,
        }
    }
}

/// Snapshot taken after a successful send and output ownership transfer.
/// Output can be empty while the peer acknowledgement window is full.
#[derive(Debug, PartialEq, Eq, uniffi::Record)]
pub struct NativeRtmpSendOutput {
    /// At most 8 MiB of owned wire bytes, including any previously queued bytes.
    /// Adds no copy beyond `Publisher::take_output`: partial window takes can
    /// copy wire bytes, and native marshalling can add another copy.
    pub output: Vec<u8>,
    /// Bytes still retained by the publisher, excluding `output` above.
    pub queued_output_bytes: u64,
    /// Absolute monotonic protocol deadline after output transfer.
    /// None does not remove the caller's socket-write watchdog obligation.
    pub next_deadline_ms: Option<u64>,
    /// Publication acceptance does not confirm that the origin received media.
    pub state: NativeRtmpState,
    pub startup_submitted: bool,
    pub peer_media_progress: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRtmpEvent {
    Sending,
    PeerMediaProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Error)]
pub enum NativeRtmpError {
    InvalidConfig,
    Busy,
    Closed,
    NotPublishing,
    Backpressure,
    InvalidMedia,
    ProtocolViolation,
    Refused,
    Timeout,
    UnsupportedHandshake,
}

impl std::fmt::Display for NativeRtmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid native RTMP configuration",
            Self::Busy => "native RTMP state is busy",
            Self::Closed => "native RTMP connection is closed",
            Self::NotPublishing => "origin has not accepted publication",
            Self::Backpressure => "native RTMP output is full; media was not queued",
            Self::InvalidMedia => "invalid or discontinuous RTMP media",
            Self::ProtocolViolation => "RTMP protocol failed",
            Self::Refused => "origin refused RTMP publication",
            Self::Timeout => "RTMP connection timed out",
            Self::UnsupportedHandshake => "digest server handshake is unsupported",
        })
    }
}

impl std::error::Error for NativeRtmpError {}

impl From<ClientError> for NativeRtmpError {
    fn from(error: ClientError) -> Self {
        match error {
            ClientError::InvalidConfig => Self::InvalidConfig,
            ClientError::Closed | ClientError::Disconnected => Self::Closed,
            ClientError::NotPublishing => Self::NotPublishing,
            ClientError::Backpressure => Self::Backpressure,
            ClientError::InvalidMedia | ClientError::MediaTooLarge => Self::InvalidMedia,
            ClientError::Refused(_) => Self::Refused,
            ClientError::Timeout => Self::Timeout,
            ClientError::UnsupportedDigestHandshake => Self::UnsupportedHandshake,
            _ => Self::ProtocolViolation,
        }
    }
}

#[derive(Default)]
struct Track {
    config: Option<Arc<CodecConfig>>,
    end_ns: Option<u64>,
}

struct State {
    publisher: Publisher,
    epoch_ns: u64,
    video: Track,
    audio: Track,
    startup: Option<StartupWatermark>,
}

#[derive(uniffi::Object)]
pub struct NativeRtmpPublisher {
    state: Mutex<State>,
}

impl NativeRtmpPublisher {
    fn state_guard(&self) -> Result<MutexGuard<'_, State>, NativeRtmpError> {
        self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => NativeRtmpError::Busy,
            std::sync::TryLockError::Poisoned(poisoned) => {
                // A panic may have interrupted configuration or output updates.
                // Keep the poison permanent and discard every retained sample
                // and byte; later calls must never resume this attempt.
                let mut state = poisoned.into_inner();
                state.publisher.cancel();
                state.video = Track::default();
                state.audio = Track::default();
                NativeRtmpError::Closed
            }
        })
    }
}

#[uniffi::export]
impl NativeRtmpPublisher {
    /// `media_epoch_ns` is shared by audio and video. Native adapters which have
    /// already rebased compressed timestamps to their common capture epoch use
    /// zero. `now_ms` is monotonic transport time, not media presentation time.
    #[uniffi::constructor]
    pub fn new(
        app: String,
        stream_key: String,
        tc_url: String,
        media_epoch_ns: u64,
        now_ms: u64,
    ) -> Result<Arc<Self>, NativeRtmpError> {
        let mut config = PublisherConfig::new(app, stream_key, tc_url);
        config.max_media_bytes = MAX_ENCODED_BUFFER_BYTES + 5;
        config.max_output_bytes = 8 * 1024 * 1024;
        let publisher = Publisher::new(config, now_ms, now_ms)?;
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                publisher,
                epoch_ns: media_epoch_ns,
                video: Track::default(),
                audio: Track::default(),
                startup: None,
            }),
        }))
    }

    /// Cold immutable track declaration, before the first sample. Legacy callers
    /// may omit it and publish normally with startup progress unknown.
    pub fn declare_tracks(&self, has_video: bool, has_audio: bool) -> Result<(), NativeRtmpError> {
        let mut state = self.state_guard()?;
        let candidate =
            StartupWatermark::new(has_video, has_audio).ok_or(NativeRtmpError::InvalidConfig)?;
        if let Some(current) = &state.startup {
            return if current.tracks() == candidate.tracks() {
                Ok(())
            } else {
                Err(NativeRtmpError::InvalidConfig)
            };
        }
        if state.video.end_ns.is_some()
            || state.audio.end_ns.is_some()
            || matches!(
                state.publisher.state(),
                PublisherState::Closed | PublisherState::Failed | PublisherState::Finishing
            )
        {
            return Err(NativeRtmpError::InvalidConfig);
        }
        state.startup = Some(candidate);
        Ok(())
    }
    /// Drain after existing socket reads, not by polling per sample. Each event
    /// occurs at most once per publisher. No payload is retained by events.
    pub fn poll_event(&self) -> Result<Option<NativeRtmpEvent>, NativeRtmpError> {
        let mut state = self.state_guard()?;
        if state.publisher.state() != PublisherState::Publishing {
            return Ok(None);
        }
        Ok(state
            .startup
            .as_mut()
            .and_then(StartupWatermark::poll_event)
            .map(|event| match event {
                StartupProgressEvent::Sending => NativeRtmpEvent::Sending,
                StartupProgressEvent::PeerMediaProgress => NativeRtmpEvent::PeerMediaProgress,
            }))
    }
    /// Feed one bounded ordered socket/TLS read. A failure terminates this
    /// attempt. Drop any previously taken but unwritten output on failure.
    pub fn feed(&self, bytes: &[u8], now_ms: u64) -> Result<(), NativeRtmpError> {
        let mut state = self.state_guard()?;
        if let Err(error) = state.publisher.feed(bytes, now_ms) {
            if let Some(startup) = &mut state.startup {
                startup.cancel();
            }
            return Err(error.into());
        }
        let acknowledged = state.publisher.peer_acknowledged_bytes();
        if state.publisher.state() == PublisherState::Publishing
            && let Some(startup) = &mut state.startup
        {
            startup.acknowledge_with_margin(acknowledged, RTMP_ACK_HANDSHAKE_MARGIN);
        }
        Ok(())
    }

    pub fn tick(&self, now_ms: u64) -> Result<(), NativeRtmpError> {
        Ok(self.state_guard()?.publisher.tick(now_ms)?)
    }

    /// Arm one real monotonic timer; reschedule after feed/send/takeOutput.
    /// Before send, tick with current time even when the peer is silent.
    /// None means no timer is needed. Socket write deadlines remain caller-owned.
    pub fn next_deadline_ms(&self) -> Result<Option<u64>, NativeRtmpError> {
        Ok(self.state_guard()?.publisher.next_deadline_ms())
    }

    /// Owned wire bytes, at most 8 MiB. Keep one outstanding bounded network
    /// write; take more only after write completion. This operation is not an
    /// acknowledgement that these bytes reached the origin.
    pub fn take_output(&self) -> Result<Vec<u8>, NativeRtmpError> {
        Ok(self.state_guard()?.publisher.take_output())
    }

    pub fn queued_output_bytes(&self) -> Result<u64, NativeRtmpError> {
        Ok(self.state_guard()?.publisher.queued_output_bytes() as u64)
    }

    pub fn state(&self) -> Result<NativeRtmpState, NativeRtmpError> {
        Ok(self.state_guard()?.publisher.state().into())
    }

    /// Queue a validated immutable sample. AVC/AAC configuration is emitted once
    /// per track, with that track's first sample timestamp. Backpressure leaves
    /// sample timing uncommitted, so retrying this same handle is safe. A config
    /// already emitted before saturation is retained and is not emitted twice.
    pub fn send(&self, sample: Arc<MediaSample>) -> Result<(), NativeRtmpError> {
        let mut state = self.state_guard()?;
        send_sample(&mut state, &sample)
    }

    /// Tick, validate/queue one sample, transfer available output, and snapshot
    /// state under one non-blocking guard. Equivalent to those legacy calls in
    /// that order with no intervening caller. Only call when the native driver
    /// can own another bounded write; keep at most one outstanding write.
    ///
    /// On error no output is transferred. Backpressure leaves this sample's
    /// timing uncommitted; retain its handle and drain/acknowledge existing
    /// output through the legacy APIs before retrying. A configuration emitted
    /// before saturation remains queued, just as for `send`. Tick failures can
    /// terminate the attempt before any sample is accepted.
    ///
    /// Bytes returned here have not reached the origin. The caller still owns
    /// the socket-write deadline and must cancel on a failed/expired write.
    pub fn send_and_take_output(
        &self,
        sample: Arc<MediaSample>,
        now_ms: u64,
    ) -> Result<NativeRtmpSendOutput, NativeRtmpError> {
        let mut state = self.state_guard()?;
        state.publisher.tick(now_ms)?;
        send_sample(&mut state, &sample)?;
        let output = state.publisher.take_output();
        Ok(NativeRtmpSendOutput {
            output,
            queued_output_bytes: state.publisher.queued_output_bytes() as u64,
            next_deadline_ms: state.publisher.next_deadline_ms(),
            state: state.publisher.state().into(),
            startup_submitted: state
                .startup
                .as_ref()
                .is_some_and(StartupWatermark::submitted),
            peer_media_progress: state
                .startup
                .as_ref()
                .is_some_and(StartupWatermark::peer_acknowledged),
        })
    }

    /// Queue closeStream. Drain its output before closing TCP/TLS; a reply is
    /// optional, so the caller must not wait indefinitely for Closed.
    pub fn finish(&self) -> Result<(), NativeRtmpError> {
        Ok(self.state_guard()?.publisher.finish()?)
    }

    pub fn cancel(&self) -> Result<(), NativeRtmpError> {
        let mut state = self.state_guard()?;
        state.publisher.cancel();
        if let Some(startup) = &mut state.startup {
            startup.cancel();
        }
        state.video = Track::default();
        state.audio = Track::default();
        Ok(())
    }
}

fn send_sample(state: &mut State, sample: &MediaSample) -> Result<(), NativeRtmpError> {
    if state.publisher.state() != PublisherState::Publishing {
        return Err(match state.publisher.state() {
            PublisherState::Closed | PublisherState::Failed | PublisherState::Finishing => {
                NativeRtmpError::Closed
            }
            _ => NativeRtmpError::NotPublishing,
        });
    }
    let encoded = &sample.sample;
    if state
        .startup
        .as_ref()
        .is_some_and(|m| !m.declares(encoded.is_video()))
    {
        return Err(NativeRtmpError::InvalidMedia);
    }
    let timing = encoded.timing();
    let dts_ns = timing.decode_time_ns();
    let end_ns = timing
        .decode_end_ns()
        .map_err(|_| NativeRtmpError::InvalidMedia)?;
    let pts_ns =
        u64::try_from(timing.pts() as u128 * 1_000_000_000 / u128::from(timing.timescale()))
            .map_err(|_| NativeRtmpError::InvalidMedia)?;
    let dts_ms = dts_ns
        .checked_sub(state.epoch_ns)
        .ok_or(NativeRtmpError::InvalidMedia)?
        / 1_000_000;
    let pts_ms = pts_ns
        .checked_sub(state.epoch_ns)
        .ok_or(NativeRtmpError::InvalidMedia)?
        / 1_000_000;
    let composition_ms = i128::from(pts_ms) - i128::from(dts_ms);
    if !(-8_388_608..=8_388_607).contains(&composition_ms) {
        return Err(NativeRtmpError::InvalidMedia);
    }
    let track = if encoded.is_video() {
        &state.video
    } else {
        &state.audio
    };
    if track.end_ns.is_some_and(|end| dts_ns < end)
        || track
            .config
            .as_ref()
            .is_some_and(|config| !Arc::ptr_eq(config, encoded.config()))
        || (encoded.is_video() && track.end_ns.is_none() && !encoded.is_keyframe())
    {
        return Err(NativeRtmpError::InvalidMedia);
    }
    let needs_config = track.config.is_none();
    // The cast is the specified RTMP timestamp rollover at 2^32 ms.
    let timestamp = dts_ms as u32;
    match encoded.config().as_ref() {
        CodecConfig::H264(config) => {
            if needs_config {
                let avcc = avc_configuration(config.sps(), config.pps())?;
                state.publisher.send_avc_config_at(timestamp, &avcc)?;
                state.video.config = Some(encoded.config().clone());
            }
            state.publisher.send_avc(
                timestamp,
                composition_ms as i32,
                encoded.is_keyframe(),
                encoded.payload(),
            )?;
            state.video.end_ns = Some(end_ns);
        }
        CodecConfig::Aac(config) => {
            if needs_config {
                state
                    .publisher
                    .send_aac_config_at(timestamp, &config.audio_specific_config())?;
                state.audio.config = Some(encoded.config().clone());
            }
            state.publisher.send_aac(timestamp, encoded.payload())?;
            state.audio.end_ns = Some(end_ns);
        }
    }
    if let Some(end) = state.publisher.output_end_position()
        && let Some(startup) = &mut state.startup
    {
        startup.complete_sample(encoded.is_video(), encoded.is_keyframe(), end);
    }
    Ok(())
}

fn avc_configuration(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, NativeRtmpError> {
    let [_, profile, compatibility, level, ..] = sps else {
        return Err(NativeRtmpError::InvalidMedia);
    };
    let sps_len = u16::try_from(sps.len()).map_err(|_| NativeRtmpError::InvalidMedia)?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| NativeRtmpError::InvalidMedia)?;
    let mut bytes = Vec::with_capacity(11 + sps.len() + pps.len());
    bytes.extend_from_slice(&[1, *profile, *compatibility, *level, 0xff, 0xe1]);
    bytes.extend_from_slice(&sps_len.to_be_bytes());
    bytes.extend_from_slice(sps);
    bytes.push(1);
    bytes.extend_from_slice(&pps_len.to_be_bytes());
    bytes.extend_from_slice(pps);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaCodec;
    use sapflow_hls::rtmp::{Connection, Event};

    const SPS: &[u8] = &[
        0x67, 0x4d, 0x40, 0x0d, 0xec, 0xa0, 0xa0, 0xcf, 0xcf, 0x80, 0x88, 0, 0, 3, 0, 8, 0, 0, 3,
        1, 0xe0, 0x78, 0xa1, 0x4c, 0xb0,
    ];
    const PPS: &[u8] = &[0x68, 0xef, 0xbc, 0x80];

    fn ready(epoch_ns: u64) -> (Arc<NativeRtmpPublisher>, Connection) {
        let publisher = NativeRtmpPublisher::new(
            "live".into(),
            "event".into(),
            "rtmps://origin.invalid/live".into(),
            epoch_ns,
            0,
        )
        .expect("publisher");
        let mut server = Connection::new(91);
        let mut accepted = false;
        for _ in 0..12 {
            server
                .feed(&publisher.take_output().expect("output"))
                .expect("server feed");
            if !accepted {
                while let Some(event) = server.poll().expect("server poll") {
                    if matches!(event, Event::Publish { .. }) {
                        accepted = true;
                        server.accept_publish();
                    }
                }
            }
            publisher.feed(&server.take_output(), 1).expect("peer feed");
            if publisher.state().expect("state") == NativeRtmpState::Publishing {
                return (publisher, server);
            }
        }
        panic!("publisher did not start");
    }

    fn events(server: &mut Connection, publisher: &NativeRtmpPublisher) -> Vec<Event> {
        server
            .feed(&publisher.take_output().expect("output"))
            .expect("feed");
        let mut events = Vec::new();
        while let Some(event) = server.poll().expect("poll") {
            events.push(event);
        }
        events
    }

    #[test]
    fn backpressure_cannot_commit_startup_candidate_or_timing() {
        let (publisher, _) = ready(0);
        publisher
            .declare_tracks(false, true)
            .expect("valid recovery fixture operation");
        let codec = MediaCodec::aac(&[0x11, 0x90]).expect("valid recovery fixture operation");
        let sample = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("valid recovery fixture operation");
        {
            let mut state = publisher
                .state_guard()
                .expect("valid recovery fixture operation");
            // Fill the already bounded native output, rather than mutate sample limits.
            while state.publisher.send_aac_config_at(0, &[0x11, 0x90]).is_ok() {}
        }
        assert_eq!(publisher.send(sample), Err(NativeRtmpError::Backpressure));
        let state = publisher
            .state_guard()
            .expect("valid recovery fixture operation");
        assert!(
            !state
                .startup
                .as_ref()
                .expect("valid recovery fixture operation")
                .submitted()
        );
        assert_eq!(state.audio.end_ns, None);
    }

    fn peer_ack(end: u64) -> Vec<u8> {
        let mut wire = vec![2, 0, 0, 0, 0, 0, 4, 3, 0, 0, 0, 0];
        wire.extend_from_slice(&(end as u32).to_be_bytes());
        wire
    }

    #[test]
    fn startup_watermark_requires_complete_idr_then_audio_and_exact_peer_wire_ack() {
        let (publisher, _) = ready(0);
        publisher
            .declare_tracks(true, true)
            .expect("valid recovery fixture operation");
        assert_eq!(
            publisher.declare_tracks(false, true),
            Err(NativeRtmpError::InvalidConfig)
        );
        let video = MediaCodec::h264(SPS, PPS).expect("valid recovery fixture operation");
        let audio = MediaCodec::aac(&[0x11, 0x90]).expect("valid recovery fixture operation");
        let early_audio = audio
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("valid recovery fixture operation");
        let early = publisher
            .send_and_take_output(early_audio, 2)
            .expect("valid recovery fixture operation");
        assert!(!early.startup_submitted && !early.peer_media_progress);
        let idr = video
            .sample(&[0, 0, 0, 2, 0x65, 0x80], 0, 0, 3000, 90_000, true)
            .expect("valid recovery fixture operation");
        let idr = publisher
            .send_and_take_output(idr, 3)
            .expect("valid recovery fixture operation");
        assert!(!idr.startup_submitted);
        let next_audio = audio
            .sample(&[2], 1024, 1024, 1024, 48_000, false)
            .expect("valid recovery fixture operation");
        let result = publisher
            .send_and_take_output(next_audio, 4)
            .expect("valid recovery fixture operation");
        assert!(result.startup_submitted && !result.peer_media_progress);
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            Some(NativeRtmpEvent::Sending)
        );
        let target = publisher
            .state_guard()
            .expect("valid recovery fixture operation")
            .startup
            .as_ref()
            .expect("valid recovery fixture operation")
            .end_position()
            .expect("valid recovery fixture operation");
        publisher
            .feed(&peer_ack(target - 1), 5)
            .expect("valid recovery fixture operation");
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            None
        );
        publisher
            .feed(&peer_ack(target), 6)
            .expect("bare target ACK");
        assert_eq!(publisher.poll_event().expect("event"), None);
        // Additional wire makes a handshake-inclusive control-only ACK valid
        // under the existing counter parser, but it must not prove media.
        let padding = audio
            .sample(&vec![0x21; 4096], 2048, 2048, 1024, 48_000, false)
            .expect("padding AAC");
        publisher
            .send_and_take_output(padding, 7)
            .expect("extra wire");
        publisher
            .feed(&peer_ack(target.max(3073)), 8)
            .expect("potential handshake-inclusive ACK");
        assert_eq!(publisher.poll_event().expect("event"), None);
        publisher
            .feed(&peer_ack(target + 3072), 9)
            .expect("partial guard");
        assert_eq!(publisher.poll_event().expect("event"), None);
        publisher
            .feed(&peer_ack(target + 3073), 10)
            .expect("complete conservative guard");
        assert_eq!(
            publisher.poll_event().expect("event"),
            Some(NativeRtmpEvent::PeerMediaProgress)
        );
        publisher
            .feed(&peer_ack(target + 3073), 11)
            .expect("duplicate guard");
        assert_eq!(publisher.poll_event().expect("event"), None);
        publisher
            .cancel()
            .expect("valid recovery fixture operation");
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            None
        );
    }

    #[test]
    fn legacy_progress_is_unknown_and_track_declaration_is_cold_and_bounded() {
        let (publisher, _) = ready(0);
        assert_eq!(
            publisher.declare_tracks(false, false),
            Err(NativeRtmpError::InvalidConfig)
        );
        let codec = MediaCodec::aac(&[0x11, 0x90]).expect("valid recovery fixture operation");
        let sample = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("valid recovery fixture operation");
        let result = publisher
            .send_and_take_output(sample, 2)
            .expect("valid recovery fixture operation");
        assert!(!result.startup_submitted && !result.peer_media_progress);
        let target = publisher
            .state_guard()
            .expect("valid recovery fixture operation")
            .publisher
            .output_end_position()
            .expect("valid recovery fixture operation");
        publisher
            .feed(&peer_ack(target), 3)
            .expect("valid recovery fixture operation");
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            None
        );
        assert_eq!(
            publisher.declare_tracks(false, true),
            Err(NativeRtmpError::InvalidConfig)
        );
    }

    #[test]
    fn startup_partial_peer_window_cannot_ack_unsent_media_and_cancel_discards_proof() {
        let (publisher, _) = ready(0);
        publisher
            .declare_tracks(false, true)
            .expect("valid recovery fixture operation");
        let codec = MediaCodec::aac(&[0x11, 0x90]).expect("valid recovery fixture operation");
        // Existing peer-window fixture forces all newly submitted media to remain queued.
        publisher
            .feed(&[2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0], 2)
            .expect("valid recovery fixture operation");
        let sample = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("valid recovery fixture operation");
        let result = publisher
            .send_and_take_output(sample, 3)
            .expect("valid recovery fixture operation");
        assert!(result.startup_submitted && !result.peer_media_progress);
        assert!(result.output.is_empty());
        let target = publisher
            .state_guard()
            .expect("valid recovery fixture operation")
            .startup
            .as_ref()
            .expect("valid recovery fixture operation")
            .end_position()
            .expect("valid recovery fixture operation");
        assert!(publisher.feed(&peer_ack(target), 4).is_err()); // ACK beyond transferred wire is invalid
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            None
        );
        publisher
            .cancel()
            .expect("valid recovery fixture operation");
        assert_eq!(
            publisher
                .poll_event()
                .expect("valid recovery fixture operation"),
            None
        );
    }

    #[test]
    fn fused_send_matches_legacy_wire_offsets_and_post_transfer_snapshot() {
        let (legacy, mut legacy_server) = ready(1_000_000_000);
        let (fused, mut fused_server) = ready(1_000_000_000);
        let video = MediaCodec::h264(SPS, PPS).expect("video");
        let audio = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let samples = [
            video
                .sample(
                    &[0, 0, 0, 2, 0x65, 0x80],
                    189_000,
                    180_000,
                    3000,
                    90_000,
                    true,
                )
                .expect("video"),
            audio
                .sample(&[1, 2, 3], 120_000, 120_000, 1024, 48_000, false)
                .expect("audio"),
            audio
                .sample(&[4, 5, 6], 121_024, 121_024, 1024, 48_000, false)
                .expect("next audio"),
        ];
        let mut decoded = Vec::new();
        for (index, sample) in samples.into_iter().enumerate() {
            let now = 20_000 + index as u64;
            legacy.tick(now).expect("tick");
            legacy.send(sample.clone()).expect("send");
            let expected = legacy.take_output().expect("output");
            let result = fused.send_and_take_output(sample, now).expect("fused");
            assert_eq!(result.output, expected);
            assert!(result.output.len() <= 8 * 1024 * 1024);
            assert_eq!(
                result.queued_output_bytes,
                legacy.queued_output_bytes().expect("queued")
            );
            assert_eq!(
                result.next_deadline_ms,
                legacy.next_deadline_ms().expect("deadline")
            );
            assert_eq!(result.state, legacy.state().expect("state"));
            assert_eq!(result.next_deadline_ms, None);
            assert!(fused.take_output().expect("already transferred").is_empty());
            legacy_server.feed(&expected).expect("legacy peer");
            fused_server.feed(&result.output).expect("fused peer");
            loop {
                let left = legacy_server.poll().expect("legacy event");
                let right = fused_server.poll().expect("fused event");
                assert_eq!(left, right);
                let Some(event) = right else { break };
                decoded.push(event);
            }
        }
        assert_eq!(decoded.len(), 5, "one config per track, three media units");
        assert!(matches!(
            decoded[0],
            Event::Video {
                timestamp: 1000,
                ..
            }
        ));
        assert!(matches!(
            decoded[2],
            Event::Audio {
                timestamp: 1500,
                ..
            }
        ));
        assert!(matches!(
            decoded[4],
            Event::Audio {
                timestamp: 1521,
                ..
            }
        ));
    }

    #[test]
    fn fused_send_error_never_takes_existing_output_or_commits_invalid_timing() {
        let (publisher, mut server) = ready(0);
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let first = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("first");
        publisher
            .send(first.clone())
            .expect("queued before fused call");
        let before = publisher.queued_output_bytes().expect("queued");
        assert_eq!(
            publisher.send_and_take_output(first, 2),
            Err(NativeRtmpError::InvalidMedia)
        );
        assert_eq!(publisher.queued_output_bytes(), Ok(before));
        let next = codec
            .sample(&[2], 1024, 1024, 1024, 48_000, false)
            .expect("next");
        let result = publisher
            .send_and_take_output(next, 3)
            .expect("valid successor");
        server.feed(&result.output).expect("wire");
        let mut count = 0;
        while server.poll().expect("event").is_some() {
            count += 1;
        }
        assert_eq!(
            count, 3,
            "prior config/media survive, followed by successor"
        );
    }

    #[test]
    fn fused_send_preserves_busy_prepublication_closed_and_clock_errors() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let sample = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("sample");
        let fresh = NativeRtmpPublisher::new(
            "live".into(),
            "event".into(),
            "rtmp://origin.invalid/live".into(),
            0,
            0,
        )
        .expect("new");
        let before = fresh.queued_output_bytes().expect("handshake");
        assert_eq!(
            fresh.send_and_take_output(sample.clone(), 1),
            Err(NativeRtmpError::NotPublishing)
        );
        assert_eq!(fresh.queued_output_bytes(), Ok(before));
        let held = fresh.state_guard().expect("guard");
        assert_eq!(
            fresh.send_and_take_output(sample.clone(), 2),
            Err(NativeRtmpError::Busy)
        );
        drop(held);
        fresh.cancel().expect("cancel");
        assert_eq!(
            fresh.send_and_take_output(sample.clone(), 2),
            Err(NativeRtmpError::Closed)
        );
        let (publisher, _) = ready(0);
        assert_eq!(
            publisher.send_and_take_output(sample, 0),
            Err(NativeRtmpError::ProtocolViolation)
        );
        assert_eq!(publisher.state(), Ok(NativeRtmpState::Failed));
        assert_eq!(publisher.queued_output_bytes(), Ok(0));
    }

    #[test]
    fn fused_send_returns_window_blocked_deadline_and_times_out_before_acceptance() {
        let (publisher, _) = ready(0);
        // Independent RTMP fmt-0 SetPeerBandwidth vector: hard one-byte window.
        // Already-sent setup bytes exceed it, so no output can transfer.
        publisher
            .feed(&[2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0, 0, 0, 0, 1, 0], 20_000)
            .expect("window");
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let first = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("first");
        let result = publisher
            .send_and_take_output(first, 20_000)
            .expect("accepted but blocked");
        assert!(result.output.is_empty());
        assert!(result.queued_output_bytes > 0);
        assert_eq!(result.next_deadline_ms, Some(21_000));
        assert_eq!(result.state, NativeRtmpState::Publishing);
        let next = codec
            .sample(&[2], 1024, 1024, 1024, 48_000, false)
            .expect("next");
        let before = publisher.state_guard().expect("guard").audio.end_ns;
        assert_eq!(
            publisher.send_and_take_output(next, 21_000),
            Err(NativeRtmpError::Timeout)
        );
        assert_eq!(publisher.state_guard().expect("guard").audio.end_ns, before);
        assert_eq!(publisher.state(), Ok(NativeRtmpState::Failed));
        assert_eq!(publisher.queued_output_bytes(), Ok(0));
        assert_eq!(publisher.next_deadline_ms(), Ok(None));
    }

    #[test]
    fn fused_backpressure_preserves_sample_for_retry_after_legacy_drain() {
        let (publisher, mut server) = ready(0);
        let codec = MediaCodec::h264(SPS, PPS).expect("video");
        let mut payload = vec![0; 2 * 1024 * 1024];
        let size = (payload.len() - 4) as u32;
        payload[..4].copy_from_slice(&size.to_be_bytes());
        payload[4] = 0x65;
        for frame in 0..3 {
            publisher
                .send(
                    codec
                        .sample(&payload, frame * 3000, frame * 3000, 3000, 90_000, true)
                        .expect("sample"),
                )
                .expect("fill");
        }
        let retry = codec
            .sample(&payload, 9000, 9000, 3000, 90_000, true)
            .expect("retry");
        let before = publisher.queued_output_bytes().expect("queued");
        assert_eq!(
            publisher.send_and_take_output(retry.clone(), 2),
            Err(NativeRtmpError::Backpressure)
        );
        assert_eq!(publisher.queued_output_bytes(), Ok(before));
        while publisher.queued_output_bytes().expect("queued") != 0 {
            let bytes = publisher.take_output().expect("drain");
            assert!(!bytes.is_empty());
            for read in bytes.chunks(16 * 1024) {
                server.feed(read).expect("read");
                while server.poll().expect("media").is_some() {}
                publisher.feed(&server.take_output(), 3).expect("ACK");
            }
        }
        let result = publisher
            .send_and_take_output(retry, 3)
            .expect("retry succeeds");
        assert!(!result.output.is_empty());
        assert!(result.output.len() as u64 + result.queued_output_bytes <= 8 * 1024 * 1024);
    }

    #[test]
    fn fused_partial_configuration_backpressure_never_duplicates_config() {
        let (publisher, mut server) = ready(0);
        let video = MediaCodec::h264(SPS, PPS).expect("video");
        let mut payload = vec![0; 2 * 1024 * 1024];
        let size = (payload.len() - 4) as u32;
        payload[..4].copy_from_slice(&size.to_be_bytes());
        payload[4] = 0x65;
        for frame in 0..3 {
            publisher
                .send(
                    video
                        .sample(&payload, frame * 3000, frame * 3000, 3000, 90_000, true)
                        .expect("video sample"),
                )
                .expect("fill");
        }
        let audio = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        // Opaque codec payload tests transport admission, not AAC decoding.
        let retry = audio
            .sample(&vec![1; 2 * 1024 * 1024], 0, 0, 1024, 48_000, false)
            .expect("audio sample");
        let before = publisher.queued_output_bytes().expect("queued");
        assert_eq!(
            publisher.send_and_take_output(retry.clone(), 2),
            Err(NativeRtmpError::Backpressure)
        );
        assert!(publisher.queued_output_bytes().expect("queued config") > before);
        {
            let state = publisher.state_guard().expect("guard");
            assert!(state.audio.config.is_some());
            assert!(state.audio.end_ns.is_none());
        }
        let mut configurations = 0;
        while publisher.queued_output_bytes().expect("queued") != 0 {
            let bytes = publisher.take_output().expect("drain");
            assert!(!bytes.is_empty());
            for read in bytes.chunks(16 * 1024) {
                server.feed(read).expect("read");
                while let Some(event) = server.poll().expect("media") {
                    if let Event::Audio { data, .. } = event {
                        assert_eq!(data, [0xaf, 0, 0x11, 0x88]);
                        configurations += 1;
                    }
                }
                publisher.feed(&server.take_output(), 3).expect("ACK");
            }
        }
        assert_eq!(configurations, 1);
        let result = publisher.send_and_take_output(retry, 3).expect("retry");
        let mut media = 0;
        let mut bytes = result.output;
        loop {
            // A fused take is still limited by the peer's remaining credit.
            // Acknowledge bounded reads and drain the remainder; never assume
            // that one foreign call transfers the entire large access unit.
            for read in bytes.chunks(16 * 1024) {
                server.feed(read).expect("retry read");
                while let Some(event) = server.poll().expect("retry event") {
                    if let Event::Audio { data, .. } = event {
                        assert_eq!(&data[..2], &[0xaf, 1], "no duplicate configuration");
                        media += 1;
                    }
                }
                publisher.feed(&server.take_output(), 3).expect("retry ACK");
            }
            if publisher.queued_output_bytes().expect("remaining") == 0 {
                break;
            }
            bytes = publisher.take_output().expect("remaining output");
            assert!(!bytes.is_empty(), "origin acknowledges the partial take");
        }
        assert_eq!(media, 1);
    }

    #[test]
    fn fused_partial_window_take_matches_legacy_snapshot_and_preserves_media_deadline() {
        let (legacy, _) = ready(0);
        let (fused, _) = ready(0);
        let outstanding = legacy
            .state_guard()
            .expect("guard")
            .publisher
            .outstanding_output_bytes();
        let window = u32::try_from(outstanding + 32).expect("bounded setup");
        let mut control = vec![2, 0, 0, 0, 0, 0, 5, 6, 0, 0, 0, 0];
        control.extend_from_slice(&window.to_be_bytes());
        control.push(0); // hard SetPeerBandwidth with 32 bytes of credit
        legacy.feed(&control, 100).expect("legacy window");
        fused.feed(&control, 100).expect("fused window");
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let sample = codec
            .sample(&[1; 64], 0, 0, 1024, 48_000, false)
            .expect("sample");
        legacy.tick(100).expect("tick");
        legacy.send(sample.clone()).expect("send");
        let expected = legacy.take_output().expect("partial take");
        let result = fused.send_and_take_output(sample, 100).expect("fused");
        assert_eq!(result.output, expected);
        assert_eq!(result.output.len(), 32);
        assert!(result.queued_output_bytes > 0);
        assert_eq!(
            result.queued_output_bytes,
            legacy.queued_output_bytes().expect("queued")
        );
        assert_eq!(result.next_deadline_ms, Some(1100));
        assert_eq!(
            result.next_deadline_ms,
            legacy.next_deadline_ms().expect("deadline")
        );
        assert_eq!(result.state, NativeRtmpState::Publishing);
        assert!(fused.take_output().expect("credit exhausted").is_empty());
        fused.tick(900).expect("within original wire deadline");
        assert_eq!(fused.next_deadline_ms(), Ok(Some(1100)));
    }

    #[test]
    fn fused_startup_timeout_has_priority_over_send_and_matches_legacy_tick() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let sample = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("sample");
        let create = || {
            NativeRtmpPublisher::new(
                "live".into(),
                "event".into(),
                "rtmp://origin.invalid/live".into(),
                0,
                0,
            )
            .expect("publisher")
        };
        let legacy = create();
        let fused = create();
        assert!(fused.queued_output_bytes().expect("handshake") > 0);
        assert_eq!(legacy.tick(15_000), Err(NativeRtmpError::Timeout));
        assert_eq!(
            fused.send_and_take_output(sample, 15_000),
            Err(NativeRtmpError::Timeout)
        );
        assert_eq!(fused.state(), legacy.state());
        assert_eq!(fused.queued_output_bytes(), Ok(0));
        assert_eq!(fused.next_deadline_ms(), Ok(None));
        let state = fused.state_guard().expect("guard");
        assert!(state.audio.config.is_none() && state.audio.end_ns.is_none());
    }

    #[test]
    fn typed_configuration_and_shared_epoch_preserve_track_offsets() {
        let (publisher, mut server) = ready(1_000_000_000);
        let video = MediaCodec::h264(SPS, PPS).expect("video codec");
        let audio = MediaCodec::aac(&[0x11, 0x88]).expect("48 kHz mono AAC");
        publisher
            .send(
                video
                    .sample(
                        &[0, 0, 0, 2, 0x65, 0x80],
                        189_000,
                        180_000,
                        3000,
                        90_000,
                        true,
                    )
                    .expect("video"),
            )
            .expect("send video");
        publisher
            .send(
                audio
                    .sample(&[1, 2, 3], 120_000, 120_000, 1024, 48_000, false)
                    .expect("audio"),
            )
            .expect("send audio");
        let got = events(&mut server, &publisher);
        assert_eq!(got.len(), 4);
        let Event::Video { timestamp, data } = &got[0] else {
            panic!("configuration");
        };
        assert_eq!(*timestamp, 1000);
        let mut expected = vec![
            0x17,
            0,
            0,
            0,
            0,
            1,
            0x4d,
            0x40,
            0x0d,
            0xff,
            0xe1,
            0,
            SPS.len() as u8,
        ];
        expected.extend_from_slice(SPS);
        expected.extend_from_slice(&[1, 0, PPS.len() as u8]);
        expected.extend_from_slice(PPS);
        assert_eq!(*data, expected);
        assert_eq!(
            got[1],
            Event::Video {
                timestamp: 1000,
                data: vec![0x17, 1, 0, 0, 100, 0, 0, 0, 2, 0x65, 0x80]
            }
        );
        assert_eq!(
            got[2],
            Event::Audio {
                timestamp: 1500,
                data: vec![0xaf, 0, 0x11, 0x88]
            }
        );
        assert_eq!(
            got[3],
            Event::Audio {
                timestamp: 1500,
                data: vec![0xaf, 1, 1, 2, 3]
            }
        );
        publisher.finish().expect("finish");
        assert_eq!(
            publisher.state().expect("state"),
            NativeRtmpState::Finishing
        );
        assert_eq!(events(&mut server, &publisher), vec![Event::Finished]);
        publisher.cancel().expect("cancel");
        assert_eq!(publisher.state().expect("state"), NativeRtmpState::Closed);
    }

    #[test]
    fn native_deadline_is_absolute_and_disappears_after_output_transfer() {
        let (publisher, _) = ready(0);
        assert_eq!(publisher.next_deadline_ms(), Ok(None));
        publisher.tick(20_000).expect("fresh transport clock");
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        publisher
            .send(
                codec
                    .sample(&[1], 0, 0, 1024, 48_000, false)
                    .expect("sample"),
            )
            .expect("queued");
        assert_eq!(publisher.next_deadline_ms(), Ok(Some(21_000)));
        assert!(!publisher.take_output().expect("owned output").is_empty());
        assert_eq!(publisher.next_deadline_ms(), Ok(None));
        publisher.cancel().expect("cancel");
        assert_eq!(publisher.next_deadline_ms(), Ok(None));
    }

    #[test]
    fn poisoned_state_discards_output_and_configs_permanently() {
        let (publisher, _) = ready(0);
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        publisher
            .send(
                codec
                    .sample(&[1], 0, 0, 1024, 48_000, false)
                    .expect("sample"),
            )
            .expect("queued");
        assert!(publisher.queued_output_bytes().expect("bytes") > 0);
        let poisoned = publisher.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.state.lock().expect("guard");
                panic!("injected interrupted native call");
            })
            .join()
            .is_err()
        );
        assert_eq!(publisher.state(), Err(NativeRtmpError::Closed));
        assert_eq!(
            publisher.send_and_take_output(
                codec
                    .sample(&[2], 1024, 1024, 1024, 48_000, false)
                    .expect("sample"),
                2,
            ),
            Err(NativeRtmpError::Closed)
        );
        assert_eq!(publisher.take_output(), Err(NativeRtmpError::Closed));
        assert_eq!(publisher.cancel(), Err(NativeRtmpError::Closed));
        let recovered = match publisher.state.lock() {
            Err(poisoned) => poisoned.into_inner(),
            Ok(_) => panic!("state must remain poisoned"),
        };
        assert_eq!(recovered.publisher.state(), PublisherState::Closed);
        assert_eq!(recovered.publisher.queued_output_bytes(), 0);
        assert!(recovered.audio.config.is_none() && recovered.video.config.is_none());
    }

    #[test]
    fn invalid_epoch_continuity_configuration_and_prepublication_are_refused() {
        let codec = MediaCodec::aac(&[0x11, 0x88]).expect("AAC");
        let early = codec
            .sample(&[1], 0, 0, 1024, 48_000, false)
            .expect("sample");
        let fresh = NativeRtmpPublisher::new(
            "live".into(),
            "event".into(),
            "rtmps://origin.invalid/live".into(),
            0,
            0,
        )
        .expect("publisher");
        assert_eq!(
            fresh.send(early.clone()),
            Err(NativeRtmpError::NotPublishing)
        );
        let (publisher, _) = ready(1_000_000_000);
        assert_eq!(publisher.send(early), Err(NativeRtmpError::InvalidMedia));
        assert_eq!(publisher.queued_output_bytes(), Ok(0));
        let sample = codec
            .sample(&[1], 48_000, 48_000, 1024, 48_000, false)
            .expect("sample");
        publisher.send(sample.clone()).expect("first");
        let before = publisher.queued_output_bytes().expect("size");
        assert_eq!(publisher.send(sample), Err(NativeRtmpError::InvalidMedia));
        assert_eq!(publisher.queued_output_bytes(), Ok(before));
        let changed = MediaCodec::aac(&[0x11, 0x88]).expect("new epoch");
        assert_eq!(
            publisher.send(
                changed
                    .sample(&[1], 49_024, 49_024, 1024, 48_000, false)
                    .expect("sample")
            ),
            Err(NativeRtmpError::InvalidMedia)
        );
        let held = publisher.state_guard().expect("guard");
        assert_eq!(publisher.state(), Err(NativeRtmpError::Busy));
        drop(held);
    }

    #[test]
    fn backpressure_does_not_commit_sample_timing_and_retry_is_safe() {
        let (publisher, mut server) = ready(0);
        let codec = MediaCodec::h264(SPS, PPS).expect("video codec");
        let mut payload = vec![0; 2 * 1024 * 1024];
        let nal_len = (payload.len() - 4) as u32;
        payload[..4].copy_from_slice(&nal_len.to_be_bytes());
        payload[4] = 0x65;
        for frame in 0..3 {
            publisher
                .send(
                    codec
                        .sample(&payload, frame * 3000, frame * 3000, 3000, 90_000, true)
                        .expect("sample"),
                )
                .expect("queued");
        }
        let retry = codec
            .sample(&payload, 9000, 9000, 3000, 90_000, true)
            .expect("sample");
        let before = publisher.queued_output_bytes().expect("size");
        assert_eq!(
            publisher.send(retry.clone()),
            Err(NativeRtmpError::Backpressure)
        );
        assert_eq!(publisher.queued_output_bytes(), Ok(before));
        // Draining is bounded ownership transfer. Feed in bounded reads, as a
        // real TCP driver must, to respect the ingest server's input budget.
        while publisher.queued_output_bytes().expect("queued") != 0 {
            let bytes = publisher.take_output().expect("drain");
            assert!(
                !bytes.is_empty(),
                "origin must acknowledge each full window"
            );
            for read in bytes.chunks(16 * 1024) {
                server.feed(read).expect("read");
                while server.poll().expect("media").is_some() {}
                publisher.feed(&server.take_output(), 3).expect("peer ACK");
            }
        }
        publisher.send(retry).expect("retry after drain");
        assert!(publisher.queued_output_bytes().expect("size") <= 8 * 1024 * 1024);
    }
}
