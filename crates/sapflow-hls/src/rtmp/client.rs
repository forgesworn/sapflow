//! Bounded, sans-IO RTMP publishing over a caller-owned ordered byte stream.
//!
//! The caller owns TCP, TLS (including certificate/hostname validation),
//! socket write deadlines and reconnection policy. Drain [`Publisher::take_output`]
//! into a separately bounded socket queue; taking bytes is an ownership transfer,
//! not evidence that they reached the peer. Never retry an input after `feed`
//! fails. Media backpressure is retryable and changes no protocol state.
//!
//! This client implements the original version-3 handshake and checks S2's
//! time/random echo. Digest handshakes are explicitly refused, not accepted
//! without validation. The deterministic handshake filler is not cryptography;
//! RTMP itself supplies no confidentiality or server authentication.

use std::collections::VecDeque;

use super::amf0::{self, Amf0Error, Value};
use super::chunk::{ChunkError, ChunkReader, ChunkWriter, MAX_HEADER, Message, Read};
use super::handshake::PACKET;

const OUT_CHUNK: u32 = 4096;
const MAX_WIRE_MESSAGE: usize = 0x00ff_ffff;
const MAX_CONFIG_STRING: usize = 4096;
const MIN_ACK_WINDOW: u32 = 4096;

/// Per-connection payload bounds, excluding output transferred to the caller.
/// Input includes incomplete chunks and all partially assembled messages. A
/// reusable FLV assembly buffer additionally retains at most `max_media_bytes`
/// of payload. Allocator capacities and chunk-header state are additional costs.
#[derive(Clone)]
pub struct PublisherConfig {
    pub app: String,
    /// Stream name/key, including any destination-authorised query parameters.
    pub stream_key: String,
    /// The destination's RTMP(S) application URL. This module does not open it.
    pub tc_url: String,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_media_bytes: usize,
    /// Deadline for the complete handshake/connect/createStream/publish exchange.
    pub startup_timeout_ms: u64,
    /// Maximum residence of media wire bytes still owned by this publisher.
    /// Drive `next_deadline_ms` with a real timer, including after publication.
    pub max_output_age_ms: u64,
    /// Optional receive-idle deadline after publication. Some origins send no
    /// unsolicited controls, so the default is disabled.
    pub receive_idle_timeout_ms: Option<u64>,
}

impl PublisherConfig {
    pub fn new(app: String, stream_key: String, tc_url: String) -> Self {
        Self {
            app,
            stream_key,
            tc_url,
            max_input_bytes: 1 << 20,
            max_output_bytes: 4 << 20,
            max_media_bytes: 1 << 20,
            startup_timeout_ms: 15_000,
            max_output_age_ms: 1000,
            receive_idle_timeout_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherState {
    Handshaking,
    Connecting,
    CreatingStream,
    AwaitingPublish,
    Publishing,
    Finishing,
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    InvalidConfig,
    UnsupportedVersion(u8),
    UnsupportedDigestHandshake,
    InvalidHandshakeEcho,
    Chunk(ChunkError),
    Amf0(Amf0Error),
    Malformed(u8),
    UnexpectedResponse,
    Refused(String),
    InputOverBudget,
    /// Media was not queued. Drain output and retry or drop by media policy.
    Backpressure,
    MediaTooLarge,
    InvalidMedia,
    NotPublishing,
    Closed,
    Disconnected,
    Timeout,
    TimeWentBackwards,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig => f.write_str("invalid RTMP publisher configuration"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported RTMP version {v}"),
            Self::UnsupportedDigestHandshake => f.write_str("digest server handshake unsupported"),
            Self::InvalidHandshakeEcho => f.write_str("invalid RTMP server handshake echo"),
            Self::Chunk(e) => write!(f, "{e}"),
            Self::Amf0(e) => write!(f, "{e}"),
            Self::Malformed(t) => write!(f, "malformed RTMP message type {t}"),
            Self::UnexpectedResponse => f.write_str("unexpected RTMP response"),
            Self::Refused(code) => write!(f, "RTMP publication refused: {code}"),
            Self::InputOverBudget => f.write_str("RTMP input budget exceeded"),
            Self::Backpressure => f.write_str("RTMP output budget exhausted"),
            Self::MediaTooLarge => f.write_str("RTMP media message too large"),
            Self::InvalidMedia => f.write_str("invalid RTMP media"),
            Self::NotPublishing => f.write_str("RTMP publish not yet accepted"),
            Self::Closed => f.write_str("RTMP publisher closed"),
            Self::Disconnected => f.write_str("RTMP transport disconnected"),
            Self::Timeout => f.write_str("RTMP publisher timed out"),
            Self::TimeWentBackwards => f.write_str("RTMP monotonic clock went backwards"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<ChunkError> for ClientError {
    fn from(value: ChunkError) -> Self {
        Self::Chunk(value)
    }
}

impl From<Amf0Error> for ClientError {
    fn from(value: Amf0Error) -> Self {
        Self::Amf0(value)
    }
}

pub struct Publisher {
    config: PublisherConfig,
    state: PublisherState,
    reader: ChunkReader,
    writer: ChunkWriter,
    input: Vec<u8>,
    output: Vec<u8>,
    pending_output: VecDeque<PendingOutput>,
    pending_bytes: usize,
    retained_pending_bytes: usize,
    handshake_output_bytes: usize,
    sent: u64,
    peer_acknowledged: u64,
    peer_limit_type: Option<u8>,
    blocked_since_ms: Option<u64>,
    media_output_since_ms: Option<u64>,
    flv_scratch: Vec<u8>,
    c1: Option<Box<[u8; PACKET]>>,
    stream_id: u32,
    started_ms: u64,
    last_now_ms: u64,
    last_received_ms: u64,
    received: u64,
    acknowledged_at: u64,
    ack_window: Option<u32>,
    peer_bandwidth: Option<u32>,
    peer_acknowledgement: u32,
    avc_configured: bool,
    aac_configured: bool,
}

struct PendingOutput {
    bytes: Vec<u8>,
    position: usize,
}

impl Publisher {
    /// Starts C0/C1 immediately. `now_ms` and later times use one monotonic clock.
    pub fn new(config: PublisherConfig, seed: u64, now_ms: u64) -> Result<Self, ClientError> {
        if [&config.app, &config.stream_key, &config.tc_url]
            .iter()
            .any(|s| s.is_empty() || s.len() > MAX_CONFIG_STRING || s.contains('\0'))
            || !(1 + 2 * PACKET..=16 << 20).contains(&config.max_input_bytes)
            || !(8192..=64 << 20).contains(&config.max_output_bytes)
            || !(1..=MAX_WIRE_MESSAGE).contains(&config.max_media_bytes)
            || config.startup_timeout_ms == 0
            || config.max_output_age_ms == 0
            || config.receive_idle_timeout_ms == Some(0)
        {
            return Err(ClientError::InvalidConfig);
        }
        let mut c1 = Box::new([0u8; PACKET]);
        let mut rng = seed;
        for part in c1[8..].chunks_mut(8) {
            rng = rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            part.copy_from_slice(&z.to_le_bytes()[..part.len()]);
        }
        let mut output = Vec::with_capacity(PACKET + 1);
        output.push(3);
        output.extend_from_slice(c1.as_slice());
        Ok(Self {
            reader: ChunkReader::new(config.max_input_bytes),
            config,
            state: PublisherState::Handshaking,
            writer: ChunkWriter::default(),
            input: Vec::new(),
            output,
            pending_output: VecDeque::new(),
            pending_bytes: 0,
            retained_pending_bytes: 0,
            handshake_output_bytes: PACKET + 1,
            sent: 0,
            peer_acknowledged: 0,
            peer_limit_type: None,
            blocked_since_ms: None,
            media_output_since_ms: None,
            flv_scratch: Vec::new(),
            c1: Some(c1),
            stream_id: 0,
            started_ms: now_ms,
            last_now_ms: now_ms,
            last_received_ms: now_ms,
            received: 0,
            acknowledged_at: 0,
            ack_window: None,
            peer_bandwidth: None,
            peer_acknowledgement: 0,
            avc_configured: false,
            aac_configured: false,
        })
    }

    pub fn state(&self) -> PublisherState {
        self.state
    }

    pub fn queued_output_bytes(&self) -> usize {
        self.output.len() + self.pending_bytes
    }

    pub fn buffered_input_bytes(&self) -> usize {
        self.input.len().saturating_add(self.reader.buffered())
    }

    /// Last peer acknowledgement sequence, modulo 2^32; this is not a delivery
    /// acknowledgement for a particular media sample.
    pub fn peer_acknowledgement(&self) -> u32 {
        self.peer_acknowledgement
    }

    /// Wire bytes transferred after the handshake and not yet cumulatively
    /// acknowledged. Neither transfer nor RTMP acknowledgement proves playback.
    pub fn outstanding_output_bytes(&self) -> u64 {
        self.sent - self.peer_acknowledged
    }

    pub fn peer_output_window(&self) -> Option<u32> {
        self.peer_bandwidth
    }

    /// Absolute monotonic deadline. Refresh transport time with `tick` before
    /// queueing media; arm one timer after each operation, cancelling it on None.
    /// A window-blocked publisher needs this even when the origin is silent.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        if matches!(self.state, PublisherState::Closed | PublisherState::Failed) {
            return None;
        }
        let startup = (!matches!(
            self.state,
            PublisherState::Publishing | PublisherState::Finishing
        ))
        .then(|| {
            self.started_ms
                .saturating_add(self.config.startup_timeout_ms)
        });
        let blocked = self
            .blocked_since_ms
            .map(|since| since.saturating_add(self.config.startup_timeout_ms));
        let media = self
            .media_output_since_ms
            .map(|since| since.saturating_add(self.config.max_output_age_ms));
        let idle = (self.state == PublisherState::Publishing)
            .then_some(self.config.receive_idle_timeout_ms)
            .flatten()
            .map(|limit| self.last_received_ms.saturating_add(limit));
        [startup, blocked, media, idle].into_iter().flatten().min()
    }

    /// Transfer only bytes admitted by the peer's outstanding-byte window.
    /// Empty output with queued bytes means wait for an acknowledgement. The
    /// caller keeps one write in flight and must drive `tick` while blocked.
    /// Handshake bytes precede RTMP acknowledgement accounting. Partial takes
    /// add one wire copy; retained segment prefixes remain in the local budget
    /// until their segment is released, avoiding repeated buffer compaction.
    pub fn take_output(&mut self) -> Vec<u8> {
        let credit = self.peer_bandwidth.map_or(usize::MAX, |window| {
            u64::from(window).saturating_sub(self.sent - self.peer_acknowledged) as usize
        });
        let limit = credit.saturating_add(self.handshake_output_bytes);
        if self.queued_output_bytes() == 0 {
            self.blocked_since_ms = None;
            self.media_output_since_ms = None;
            return Vec::new();
        }
        if limit == 0 {
            self.blocked_since_ms.get_or_insert(self.last_now_ms);
            return Vec::new();
        }
        let mut result = Vec::new();
        while result.len() < limit {
            if self.pending_output.is_empty() {
                if self.output.len() <= limit - result.len() {
                    if result.is_empty() {
                        result = std::mem::take(&mut self.output);
                    } else {
                        result.append(&mut self.output);
                    }
                    break;
                }
                let bytes = std::mem::take(&mut self.output);
                self.pending_bytes += bytes.len();
                self.retained_pending_bytes += bytes.len();
                self.pending_output
                    .push_back(PendingOutput { bytes, position: 0 });
            }
            let Some(front) = self.pending_output.front_mut() else {
                break;
            };
            let count = (front.bytes.len() - front.position).min(limit - result.len());
            result.extend_from_slice(&front.bytes[front.position..front.position + count]);
            front.position += count;
            self.pending_bytes -= count;
            if front.position == front.bytes.len()
                && let Some(done) = self.pending_output.pop_front()
            {
                self.retained_pending_bytes -= done.bytes.len();
            }
        }
        let handshake = result.len().min(self.handshake_output_bytes);
        self.handshake_output_bytes -= handshake;
        let Some(sent) = self.sent.checked_add((result.len() - handshake) as u64) else {
            self.release(PublisherState::Failed);
            return Vec::new();
        };
        self.sent = sent;
        if self.queued_output_bytes() == 0 {
            self.media_output_since_ms = None;
        }
        if self.queued_output_bytes() > 0
            && self
                .peer_bandwidth
                .is_some_and(|window| self.sent - self.peer_acknowledged >= u64::from(window))
        {
            self.blocked_since_ms.get_or_insert(self.last_now_ms);
        } else {
            self.blocked_since_ms = None;
        }
        result
    }

    fn retained_output_bytes(&self) -> usize {
        self.output.len() + self.retained_pending_bytes
    }

    /// Return a fully written output buffer for reuse. The caller must retain
    /// any partially written buffer itself. Oversized/unused buffers are dropped.
    /// This avoids a fresh chunk-output allocation per sample when the transport
    /// can return ownership after writes. Returns whether the buffer was retained.
    pub fn recycle_output(&mut self, mut buffer: Vec<u8>) -> bool {
        if matches!(self.state, PublisherState::Closed | PublisherState::Failed)
            || !self.output.is_empty()
            || buffer.capacity() > self.config.max_output_bytes
            || buffer.capacity() <= self.output.capacity()
        {
            return false;
        }
        buffer.clear();
        self.output = buffer;
        true
    }

    /// Feed an ordered transport read. Protocol/control failures close the client
    /// and discard all queued media, including a refusal after Publish.Start.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Result<(), ClientError> {
        self.tick(now_ms)?;
        let result = self.feed_inner(bytes, now_ms);
        if result.is_err() {
            self.release(PublisherState::Failed);
        }
        result
    }

    fn feed_inner(&mut self, bytes: &[u8], now_ms: u64) -> Result<(), ClientError> {
        if bytes.len().saturating_add(self.buffered_input_bytes()) > self.config.max_input_bytes {
            return Err(ClientError::InputOverBudget);
        }
        self.input.extend_from_slice(bytes);
        if !bytes.is_empty() {
            self.last_received_ms = now_ms;
        }
        // Count post-handshake bytes, matching the existing server session.
        let mut incoming = bytes.len();
        if self.state == PublisherState::Handshaking {
            if let Some(&version) = self.input.first()
                && version != 3
            {
                return Err(ClientError::UnsupportedVersion(version));
            }
            if self.input.len() < 1 + 2 * PACKET {
                return Ok(());
            }
            let s1 = &self.input[1..1 + PACKET];
            let s2 = &self.input[1 + PACKET..1 + 2 * PACKET];
            if s1[4..8] != [0, 0, 0, 0] {
                return Err(ClientError::UnsupportedDigestHandshake);
            }
            let c1 = self.c1.as_ref().ok_or(ClientError::UnexpectedResponse)?;
            // The second time field is the peer's receipt time and may differ.
            if s2[..4] != c1[..4] || s2[8..] != c1[8..] {
                return Err(ClientError::InvalidHandshakeEcho);
            }
            if self.retained_output_bytes().saturating_add(PACKET) > self.config.max_output_bytes {
                return Err(ClientError::Backpressure);
            }
            self.output.extend_from_slice(s1);
            self.handshake_output_bytes += PACKET;
            self.input.drain(..1 + 2 * PACKET);
            self.c1 = None;
            incoming = self.input.len();
            self.control(1, &OUT_CHUNK.to_be_bytes())?;
            self.writer.set_chunk_size(OUT_CHUNK);
            self.command(
                0,
                &[
                    Value::String("connect".into()),
                    Value::Number(1.0),
                    Value::Object(vec![
                        ("app".into(), Value::String(self.config.app.clone())),
                        ("tcUrl".into(), Value::String(self.config.tc_url.clone())),
                        ("type".into(), Value::String("nonprivate".into())),
                        (
                            "flashVer".into(),
                            Value::String("FMLE/3.0 (compatible; Sapflow)".into()),
                        ),
                        ("objectEncoding".into(), Value::Number(0.0)),
                    ]),
                ],
            )?;
            self.state = PublisherState::Connecting;
        }
        self.received = self.received.wrapping_add(incoming as u64);
        let mut pos = 0;
        loop {
            match self.reader.read(&self.input[pos..])? {
                Read::NeedMore => break,
                Read::Chunk { consumed, message } => {
                    pos += consumed;
                    if let Some(message) = message {
                        self.message(message)?;
                        if self.state == PublisherState::Closed {
                            return Ok(());
                        }
                    }
                }
            }
        }
        self.input.drain(..pos);
        if let Some(window) = self.ack_window
            && self.received.wrapping_sub(self.acknowledged_at) >= u64::from(window)
        {
            self.control(3, &(self.received as u32).to_be_bytes())?;
            self.acknowledged_at = self.received;
        }
        Ok(())
    }

    /// Drive the startup/optional receive-idle deadline even when no bytes arrive.
    pub fn tick(&mut self, now_ms: u64) -> Result<(), ClientError> {
        if matches!(self.state, PublisherState::Closed | PublisherState::Failed) {
            return Err(ClientError::Closed);
        }
        let error = if now_ms < self.last_now_ms {
            Some(ClientError::TimeWentBackwards)
        } else if self
            .next_deadline_ms()
            .is_some_and(|deadline| now_ms >= deadline)
        {
            Some(ClientError::Timeout)
        } else {
            None
        };
        self.last_now_ms = now_ms;
        if let Some(error) = error {
            self.release(PublisherState::Failed);
            return Err(error);
        }
        Ok(())
    }

    /// Immediate cancellation. The caller closes its transport and drops any
    /// separately held output; no buffered media or handshake remains here.
    pub fn cancel(&mut self) {
        self.release(PublisherState::Closed);
    }

    /// Report socket EOF/reset. A new transport requires a new publisher.
    pub fn disconnected(&mut self) -> Result<(), ClientError> {
        if matches!(self.state, PublisherState::Closed | PublisherState::Failed) {
            return Err(ClientError::Closed);
        }
        self.release(PublisherState::Failed);
        Err(ClientError::Disconnected)
    }

    /// Queue closeStream after all already queued media. Backpressure is retryable.
    /// On success, drain the final output before closing the transport. A matching
    /// optional reply transitions Finishing to Closed; origins need not reply.
    pub fn finish(&mut self) -> Result<(), ClientError> {
        self.require_publishing()?;
        self.command(
            self.stream_id,
            &[
                Value::String("closeStream".into()),
                Value::Number(3.0),
                Value::Null,
            ],
        )?;
        self.state = PublisherState::Finishing;
        self.started_ms = self.last_now_ms;
        Ok(())
    }

    /// AVCDecoderConfigurationRecord to an FLV AVC sequence header.
    pub fn send_avc_config(&mut self, data: &[u8]) -> Result<(), ClientError> {
        self.send_avc_config_at(0, data)
    }

    /// Resend a changed/repeated AVC configuration at the current wrapping DTS,
    /// before the next access unit using it. The client does not rewrite timestamps.
    pub fn send_avc_config_at(&mut self, timestamp: u32, data: &[u8]) -> Result<(), ClientError> {
        self.require_publishing()?;
        if data.is_empty() {
            return Err(ClientError::InvalidMedia);
        }
        self.media(6, 9, timestamp, &[0x17, 0, 0, 0, 0], data)?;
        self.avc_configured = true;
        Ok(())
    }

    /// AAC AudioSpecificConfig to an FLV AAC sequence header.
    pub fn send_aac_config(&mut self, data: &[u8]) -> Result<(), ClientError> {
        self.send_aac_config_at(0, data)
    }

    /// Resend AudioSpecificConfig at the current wrapping timestamp before the
    /// next access unit using it. The platform owns codec-change detection.
    pub fn send_aac_config_at(&mut self, timestamp: u32, data: &[u8]) -> Result<(), ClientError> {
        self.require_publishing()?;
        if data.is_empty() {
            return Err(ClientError::InvalidMedia);
        }
        self.media(4, 8, timestamp, &[0xaf, 0], data)?;
        self.aac_configured = true;
        Ok(())
    }

    /// Length-prefixed AVC access unit (AVCC, not Annex B). Timestamp is DTS;
    /// composition offset is signed milliseconds in the FLV 24-bit range.
    pub fn send_avc(
        &mut self,
        timestamp: u32,
        composition_ms: i32,
        keyframe: bool,
        data: &[u8],
    ) -> Result<(), ClientError> {
        self.require_publishing()?;
        if !self.avc_configured
            || data.is_empty()
            || !(-8_388_608..=8_388_607).contains(&composition_ms)
        {
            return Err(ClientError::InvalidMedia);
        }
        let offset = composition_ms.to_be_bytes();
        self.media(
            6,
            9,
            timestamp,
            &[
                if keyframe { 0x17 } else { 0x27 },
                1,
                offset[1],
                offset[2],
                offset[3],
            ],
            data,
        )
    }

    /// A raw AAC access unit without ADTS framing.
    pub fn send_aac(&mut self, timestamp: u32, data: &[u8]) -> Result<(), ClientError> {
        self.require_publishing()?;
        if !self.aac_configured || data.is_empty() {
            return Err(ClientError::InvalidMedia);
        }
        self.media(4, 8, timestamp, &[0xaf, 1], data)
    }

    fn require_publishing(&self) -> Result<(), ClientError> {
        match self.state {
            PublisherState::Publishing => Ok(()),
            PublisherState::Closed | PublisherState::Failed | PublisherState::Finishing => {
                Err(ClientError::Closed)
            }
            _ => Err(ClientError::NotPublishing),
        }
    }

    fn release(&mut self, state: PublisherState) {
        self.state = state;
        self.input = Vec::new();
        self.output = Vec::new();
        self.pending_output.clear();
        self.pending_bytes = 0;
        self.retained_pending_bytes = 0;
        self.handshake_output_bytes = 0;
        self.blocked_since_ms = None;
        self.media_output_since_ms = None;
        self.flv_scratch = Vec::new();
        self.c1 = None;
        self.reader = ChunkReader::new(self.config.max_input_bytes);
        self.config.stream_key = String::new();
        self.avc_configured = false;
        self.aac_configured = false;
    }

    fn message(&mut self, message: Message) -> Result<(), ClientError> {
        let p = message.payload.as_slice();
        match message.type_id {
            1..=6 if message.csid != 2 || message.stream_id != 0 => {
                return Err(ClientError::Malformed(message.type_id));
            }
            1 => {
                let size = exact_u32(p).ok_or(ClientError::Malformed(1))?;
                if size as usize > self.config.max_input_bytes.saturating_sub(MAX_HEADER) {
                    return Err(ClientError::InputOverBudget);
                }
                self.reader.set_chunk_size(size)?;
            }
            2 => self
                .reader
                .abort(exact_u32(p).ok_or(ClientError::Malformed(2))?),
            3 => {
                let sequence = exact_u32(p).ok_or(ClientError::Malformed(3))?;
                let advance = u64::from(sequence.wrapping_sub(self.peer_acknowledged as u32));
                if advance > self.sent - self.peer_acknowledged {
                    return Err(ClientError::Malformed(3));
                }
                self.peer_acknowledged += advance;
                self.peer_acknowledgement = sequence;
                if advance != 0 {
                    self.blocked_since_ms = None;
                }
            }
            4 => {
                let [a, b, rest @ ..] = p else {
                    return Err(ClientError::Malformed(4));
                };
                let event = u16::from_be_bytes([*a, *b]);
                match event {
                    6 => {
                        let value = exact_u32(rest).ok_or(ClientError::Malformed(4))?;
                        let mut reply = [0u8; 6];
                        reply[..2].copy_from_slice(&7u16.to_be_bytes());
                        reply[2..].copy_from_slice(&value.to_be_bytes());
                        self.control(4, &reply)?;
                    }
                    0 | 1 | 2 | 4 | 7 => {
                        let id = exact_u32(rest).ok_or(ClientError::Malformed(4))?;
                        if event == 1
                            && id == self.stream_id
                            && self.state == PublisherState::Publishing
                        {
                            return Err(ClientError::Disconnected);
                        }
                    }
                    3 if rest.len() != 8 => return Err(ClientError::Malformed(4)),
                    _ => {}
                }
            }
            5 => {
                self.ack_window = Some(
                    exact_u32(p)
                        .filter(|v| *v != 0)
                        .ok_or(ClientError::Malformed(5))?
                        .max(MIN_ACK_WINDOW),
                )
            }
            6 => {
                let [a, b, c, d, limit] = p else {
                    return Err(ClientError::Malformed(6));
                };
                if *limit > 2 {
                    return Err(ClientError::Malformed(6));
                }
                let bandwidth = u32::from_be_bytes([*a, *b, *c, *d]);
                if bandwidth == 0 {
                    return Err(ClientError::Malformed(6));
                }
                // Adobe RTMP 5.4.5: Soft narrows the current window; Dynamic
                // acts as Hard only after Hard. Ignored Dynamic changes no state.
                let effective = match *limit {
                    0 => Some((bandwidth, 0)),
                    1 => Some((
                        self.peer_bandwidth
                            .map_or(bandwidth, |old| old.min(bandwidth)),
                        1,
                    )),
                    2 if self.peer_limit_type == Some(0) => Some((bandwidth, 0)),
                    _ => None,
                };
                if let Some((window, kind)) = effective {
                    if self.peer_bandwidth != Some(window) {
                        self.control(5, &window.to_be_bytes())?;
                        self.peer_bandwidth = Some(window);
                    }
                    self.peer_limit_type = Some(kind);
                }
            }
            17 | 20 => {
                let body = if message.type_id == 17 {
                    p.strip_prefix(&[0]).ok_or(ClientError::Malformed(17))?
                } else {
                    p
                };
                self.command_in(message.stream_id, &amf0::decode_all(body)?)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn command_in(&mut self, stream: u32, values: &[Value]) -> Result<(), ClientError> {
        let name = values
            .first()
            .and_then(Value::as_str)
            .ok_or(ClientError::UnexpectedResponse)?;
        let txn = values
            .get(1)
            .and_then(Value::as_number)
            .ok_or(ClientError::UnexpectedResponse)?;
        let status = values.get(3);
        let code = status.and_then(|s| s.get("code")).and_then(Value::as_str);
        if name == "_error"
            || (name == "onStatus"
                && status.and_then(|s| s.get("level")).and_then(Value::as_str) == Some("error"))
        {
            return Err(ClientError::Refused(
                code.unwrap_or("remote error").chars().take(256).collect(),
            ));
        }
        match (name, self.state) {
            (
                "closeStream" | "FCUnpublish" | "onFCUnpublish",
                PublisherState::Publishing | PublisherState::Finishing,
            ) if stream == self.stream_id || (stream == 0 && name != "closeStream") => {
                self.release(PublisherState::Closed);
            }
            ("_result", PublisherState::Finishing)
                if txn == 3.0 && (stream == 0 || stream == self.stream_id) =>
            {
                self.release(PublisherState::Closed);
            }
            // Some origins acknowledge a transaction-zero publish command after
            // sending Publish.Start. It cannot open the media gate by itself.
            ("_result", PublisherState::Publishing)
                if txn == 0.0 && (stream == 0 || stream == self.stream_id) => {}
            ("_result", PublisherState::Connecting) if txn == 1.0 && stream == 0 => {
                if code != Some("NetConnection.Connect.Success") {
                    return Err(ClientError::UnexpectedResponse);
                }
                self.command(
                    0,
                    &[
                        Value::String("createStream".into()),
                        Value::Number(2.0),
                        Value::Null,
                    ],
                )?;
                self.state = PublisherState::CreatingStream;
            }
            ("_result", PublisherState::CreatingStream) if txn == 2.0 && stream == 0 => {
                let id = status
                    .and_then(Value::as_number)
                    .filter(|n| {
                        n.is_finite() && *n >= 1.0 && *n <= f64::from(u32::MAX) && n.fract() == 0.0
                    })
                    .ok_or(ClientError::UnexpectedResponse)? as u32;
                self.command(
                    id,
                    &[
                        Value::String("publish".into()),
                        Value::Number(0.0),
                        Value::Null,
                        Value::String(self.config.stream_key.clone()),
                        Value::String("live".into()),
                    ],
                )?;
                self.stream_id = id;
                self.state = PublisherState::AwaitingPublish;
            }
            ("onStatus", PublisherState::AwaitingPublish) if stream == self.stream_id => {
                if code != Some("NetStream.Publish.Start") {
                    return Err(ClientError::UnexpectedResponse);
                }
                self.state = PublisherState::Publishing;
                self.config.stream_key = String::new();
            }
            ("onStatus", PublisherState::Publishing | PublisherState::Finishing)
                if stream == self.stream_id =>
            {
                if matches!(
                    code,
                    Some("NetStream.Unpublish.Success" | "NetStream.Publish.Stop")
                ) {
                    self.release(PublisherState::Closed);
                } else if code != Some("NetStream.Publish.Start") {
                    return Err(ClientError::UnexpectedResponse);
                }
            }
            ("_result", _) | ("onStatus", _) => return Err(ClientError::UnexpectedResponse),
            _ => {}
        }
        Ok(())
    }

    fn control(&mut self, kind: u8, data: &[u8]) -> Result<(), ClientError> {
        self.write(2, kind, 0, 0, data)
    }

    fn command(&mut self, stream: u32, values: &[Value]) -> Result<(), ClientError> {
        let mut data = Vec::new();
        for value in values {
            amf0::encode(value, &mut data);
        }
        self.write(3, 20, stream, 0, &data)
    }

    fn wire_size(&self, size: usize, timestamp: u32) -> usize {
        let chunk = if self.state == PublisherState::Handshaking {
            128
        } else {
            OUT_CHUNK as usize
        };
        let extension = if timestamp >= 0x00ff_ffff { 4 } else { 0 };
        12 + extension + size + size.saturating_sub(1) / chunk * (1 + extension)
    }

    fn write(
        &mut self,
        csid: u8,
        kind: u8,
        stream: u32,
        timestamp: u32,
        data: &[u8],
    ) -> Result<(), ClientError> {
        if data.len() > MAX_WIRE_MESSAGE {
            return Err(ClientError::MediaTooLarge);
        }
        if self
            .retained_output_bytes()
            .saturating_add(self.wire_size(data.len(), timestamp))
            > self.config.max_output_bytes
        {
            return Err(ClientError::Backpressure);
        }
        self.writer
            .write(&mut self.output, csid, kind, stream, timestamp, data);
        Ok(())
    }

    fn media(
        &mut self,
        csid: u8,
        kind: u8,
        timestamp: u32,
        prefix: &[u8],
        data: &[u8],
    ) -> Result<(), ClientError> {
        let size = data.len().saturating_add(prefix.len());
        if size > self.config.max_media_bytes || size > MAX_WIRE_MESSAGE {
            return Err(ClientError::MediaTooLarge);
        }
        if self
            .retained_output_bytes()
            .saturating_add(self.wire_size(size, timestamp))
            > self.config.max_output_bytes
        {
            return Err(ClientError::Backpressure);
        }
        // One reusable FLV assembly and one chunk-output copy. Native performance
        // qualification must account for both; no zero-copy claim is made here.
        self.flv_scratch.clear();
        if self.flv_scratch.capacity() < size {
            self.flv_scratch.reserve_exact(size);
        }
        self.flv_scratch.extend_from_slice(prefix);
        self.flv_scratch.extend_from_slice(data);
        self.writer.write(
            &mut self.output,
            csid,
            kind,
            self.stream_id,
            timestamp,
            &self.flv_scratch,
        );
        self.media_output_since_ms.get_or_insert(self.last_now_ms);
        Ok(())
    }
}

fn exact_u32(bytes: &[u8]) -> Option<u32> {
    let [a, b, c, d] = bytes else {
        return None;
    };
    Some(u32::from_be_bytes([*a, *b, *c, *d]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtmp::{Connection, Event, Form};

    fn config() -> PublisherConfig {
        PublisherConfig::new(
            "live".into(),
            "event?token=placeholder".into(),
            "rtmps://origin.invalid/live".into(),
        )
    }

    fn client() -> Publisher {
        Publisher::new(config(), 42, 0).expect("valid config")
    }

    // Independent original-handshake peer: S2 echoes C1 except receipt time.
    fn original_response(c0c1: &[u8]) -> Vec<u8> {
        assert_eq!(c0c1.len(), PACKET + 1);
        assert_eq!(c0c1[0], 3);
        let mut response = vec![0x53; 1 + 2 * PACKET];
        response[0] = 3;
        response[1..5].copy_from_slice(&123u32.to_be_bytes());
        response[5..9].fill(0);
        response[1 + PACKET..].copy_from_slice(&c0c1[1..]);
        response[1 + PACKET + 4..1 + PACKET + 8].copy_from_slice(&456u32.to_be_bytes());
        response
    }

    fn handshake(client: &mut Publisher, fragment: usize) {
        let response = original_response(&client.take_output());
        for bytes in response.chunks(fragment) {
            client.feed(bytes, 1).expect("original handshake");
        }
        assert_eq!(client.state(), PublisherState::Connecting);
        let output = client.take_output();
        assert_eq!(&output[..PACKET], &response[1..1 + PACKET]);
    }

    // Test peer writes fmt-0 headers itself. It does not use ChunkWriter.
    fn wire(kind: u8, stream: u32, body: &[u8], chunk: usize) -> Vec<u8> {
        let mut bytes = vec![if kind <= 6 { 2 } else { 3 }, 0, 0, 0];
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        bytes.push(kind);
        bytes.extend_from_slice(&stream.to_le_bytes());
        for (index, part) in body.chunks(chunk).enumerate() {
            if index != 0 {
                bytes.push(0xc0 | if kind <= 6 { 2 } else { 3 });
            }
            bytes.extend_from_slice(part);
        }
        bytes
    }

    // Independently specified AMF0 scalar/object fixture construction.
    fn string(out: &mut Vec<u8>, value: &str) {
        out.push(2);
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }

    fn number(out: &mut Vec<u8>, value: f64) {
        out.push(0);
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn response(name: &str, txn: f64, code: Option<(&str, &str)>, id: Option<f64>) -> Vec<u8> {
        let mut body = Vec::new();
        string(&mut body, name);
        number(&mut body, txn);
        body.push(5); // null command object
        if let Some(id) = id {
            number(&mut body, id);
        } else if let Some((code, level)) = code {
            body.push(3); // object
            for (key, value) in [("level", level), ("code", code)] {
                body.extend_from_slice(&(key.len() as u16).to_be_bytes());
                body.extend_from_slice(key.as_bytes());
                string(&mut body, value);
            }
            body.extend_from_slice(&[0, 0, 9]);
        }
        body
    }

    fn publish_independently(client: &mut Publisher, fragment: usize) {
        handshake(client, fragment);
        for (body, stream, expected) in [
            (
                response(
                    "_result",
                    1.0,
                    Some(("NetConnection.Connect.Success", "status")),
                    None,
                ),
                0,
                PublisherState::CreatingStream,
            ),
            (
                response("_result", 2.0, None, Some(7.0)),
                0,
                PublisherState::AwaitingPublish,
            ),
            (
                response(
                    "onStatus",
                    0.0,
                    Some(("NetStream.Publish.Start", "status")),
                    None,
                ),
                7,
                PublisherState::Publishing,
            ),
        ] {
            for bytes in wire(20, stream, &body, 128).chunks(fragment) {
                client.feed(bytes, 2).expect("valid command response");
            }
            assert_eq!(client.state(), expected);
            client.take_output();
        }
    }

    #[test]
    fn original_handshake_and_publish_against_our_ingest_preserves_auth_gate() {
        for fragment in [1, 7, 1536, 4096] {
            let mut publisher = client();
            let mut server = Connection::new(73);
            let mut requested = false;
            for _ in 0..12 {
                for bytes in publisher.take_output().chunks(fragment) {
                    server.feed(bytes).expect("server feed");
                }
                if !requested {
                    while let Some(event) = server.poll().expect("server poll") {
                        if let Event::Publish { app, key } = event {
                            assert_eq!(app, "live");
                            assert_eq!(key, "event?token=placeholder");
                            requested = true;
                            assert_eq!(publisher.state(), PublisherState::AwaitingPublish);
                            assert_eq!(
                                publisher.send_avc_config(&[1]),
                                Err(ClientError::NotPublishing)
                            );
                            assert_eq!(publisher.queued_output_bytes(), 0);
                            assert!(server.poll().is_err(), "server still awaits authorisation");
                            server.accept_publish();
                            break;
                        }
                    }
                }
                for bytes in server.take_output().chunks(fragment) {
                    publisher.feed(bytes, 1).expect("client feed");
                }
                if publisher.state() == PublisherState::Publishing {
                    break;
                }
            }
            assert!(requested);
            assert_eq!(server.handshake_form(), Some(Form::Original));
            assert_eq!(publisher.state(), PublisherState::Publishing);
            publisher
                .send_avc_config(&[1, 100, 0, 31])
                .expect("AVC config");
            publisher
                .send_aac_config(&[0x12, 0x10])
                .expect("AAC config");
            publisher
                .send_avc(0x0100_0000, -12, true, &[0, 0, 0, 1, 0x65])
                .expect("video");
            publisher.send_aac(99, &[1, 2, 3]).expect("audio");
            publisher.finish().expect("finish");
            for bytes in publisher.take_output().chunks(fragment) {
                server.feed(bytes).expect("media feed");
            }
            let mut events = Vec::new();
            while let Some(event) = server.poll().expect("media poll") {
                events.push(event);
            }
            assert_eq!(
                events,
                vec![
                    Event::Video {
                        timestamp: 0,
                        data: vec![0x17, 0, 0, 0, 0, 1, 100, 0, 31]
                    },
                    Event::Audio {
                        timestamp: 0,
                        data: vec![0xaf, 0, 0x12, 0x10]
                    },
                    Event::Video {
                        timestamp: 0x0100_0000,
                        data: vec![0x17, 1, 0xff, 0xff, 0xf4, 0, 0, 0, 1, 0x65]
                    },
                    Event::Audio {
                        timestamp: 99,
                        data: vec![0xaf, 1, 1, 2, 3]
                    },
                    Event::Finished,
                ]
            );
        }
    }

    #[test]
    fn independent_fragmented_commands_and_controls() {
        for fragment in [1, 7, 4096] {
            let mut publisher = client();
            publish_independently(&mut publisher, fragment);
            let mut bytes = wire(5, 0, &4096u32.to_be_bytes(), 128);
            bytes.extend(wire(1, 0, &2u32.to_be_bytes(), 128));
            bytes.extend(wire(4, 0, &[0, 6, 0x12, 0x34, 0x56, 0x78], 2));
            let acknowledged = publisher.sent as u32;
            bytes.extend(wire(3, 0, &acknowledged.to_be_bytes(), 2));
            for part in bytes.chunks(fragment) {
                publisher.feed(part, 3).expect("fragmented controls");
            }
            assert_eq!(publisher.peer_acknowledgement(), acknowledged);
            assert_eq!(
                publisher.take_output(),
                wire(4, 0, &[0, 7, 0x12, 0x34, 0x56, 0x78], 4096)
            );
            // A 4096-byte unknown message makes the receive counter cross the
            // negotiated window. The test peer uses two-byte chunk fragments.
            let body = vec![0x55; 4096];
            let bytes = wire(18, 7, &body, 2);
            for part in bytes.chunks(fragment) {
                publisher.feed(part, 4).expect("acknowledged bytes");
            }
            let ack = publisher.take_output();
            let mut reader = ChunkReader::new(8192);
            assert!(matches!(
                reader.read(&ack),
                Ok(Read::Chunk {
                    message: Some(Message { type_id: 3, .. }),
                    ..
                })
            ));
        }
    }

    #[test]
    fn refuses_digest_bad_echo_and_version_without_queuing_commands() {
        for error in [
            ClientError::UnsupportedVersion(6),
            ClientError::UnsupportedDigestHandshake,
            ClientError::InvalidHandshakeEcho,
        ] {
            let mut publisher = client();
            let mut response = original_response(&publisher.take_output());
            match error {
                ClientError::UnsupportedVersion(_) => response[0] = 6,
                ClientError::UnsupportedDigestHandshake => response[5] = 1,
                _ => {
                    let last = response.len() - 1;
                    response[last] ^= 1;
                }
            }
            assert_eq!(publisher.feed(&response, 1), Err(error));
            assert_eq!(publisher.state(), PublisherState::Failed);
            assert!(publisher.take_output().is_empty());
            assert_eq!(publisher.buffered_input_bytes(), 0);
        }
    }

    #[test]
    fn refusal_and_unsolicited_status_cannot_open_media_gate() {
        let mut publisher = client();
        handshake(&mut publisher, 1);
        let denied = response(
            "_error",
            1.0,
            Some(("NetConnection.Connect.Rejected", "error")),
            None,
        );
        assert_eq!(
            publisher.feed(&wire(20, 0, &denied, 128), 2),
            Err(ClientError::Refused(
                "NetConnection.Connect.Rejected".into()
            ))
        );
        assert_eq!(publisher.send_aac_config(&[1]), Err(ClientError::Closed));

        let mut publisher = client();
        handshake(&mut publisher, 1);
        let early = response(
            "onStatus",
            0.0,
            Some(("NetStream.Publish.Start", "status")),
            None,
        );
        assert_eq!(
            publisher.feed(&wire(20, 7, &early, 128), 2),
            Err(ClientError::UnexpectedResponse)
        );
        assert_eq!(publisher.state(), PublisherState::Failed);

        let mut publisher = client();
        publish_independently(&mut publisher, 1);
        let refused = response(
            "onStatus",
            0.0,
            Some(("NetStream.Publish.BadName", "error")),
            None,
        );
        publisher.send_aac_config(&[1]).expect("queued media");
        assert!(matches!(
            publisher.feed(&wire(20, 7, &refused, 128), 3),
            Err(ClientError::Refused(_))
        ));
        assert!(publisher.take_output().is_empty());
    }

    #[test]
    fn hostile_lengths_controls_and_stream_ids_fail_closed() {
        let mut publisher = client();
        let huge = vec![0u8; publisher.config.max_input_bytes + 1];
        assert_eq!(publisher.feed(&huge, 1), Err(ClientError::InputOverBudget));
        for (kind, payload) in [
            (1, vec![0, 0, 0, 0]),
            (3, vec![0; 3]),
            (4, vec![0, 6, 0]),
            (5, vec![0; 4]),
            (6, vec![0; 5]),
            (17, vec![1]),
        ] {
            let mut publisher = client();
            handshake(&mut publisher, 1);
            assert!(publisher.feed(&wire(kind, 0, &payload, 128), 2).is_err());
            assert_eq!(publisher.state(), PublisherState::Failed);
            assert!(publisher.take_output().is_empty());
        }
        for id in [
            0.0,
            -1.0,
            1.5,
            f64::NAN,
            f64::INFINITY,
            f64::from(u32::MAX) + 1.0,
        ] {
            let mut publisher = client();
            handshake(&mut publisher, 7);
            let connected = response(
                "_result",
                1.0,
                Some(("NetConnection.Connect.Success", "status")),
                None,
            );
            publisher
                .feed(&wire(20, 0, &connected, 128), 2)
                .expect("connect result");
            let created = response("_result", 2.0, None, Some(id));
            assert_eq!(
                publisher.feed(&wire(20, 0, &created, 128), 3),
                Err(ClientError::UnexpectedResponse)
            );
        }
    }

    #[test]
    fn saturation_is_bounded_and_media_retry_is_atomic() {
        let mut config = config();
        config.max_output_bytes = 8192;
        let mut publisher = Publisher::new(config, 1, 0).expect("config");
        publish_independently(&mut publisher, 1);
        publisher.send_avc_config(&[1]).expect("config");
        publisher.take_output();
        let media = vec![0x65; 8000];
        publisher.send_avc(1, 0, true, &media).expect("first fits");
        let queued = publisher.queued_output_bytes();
        assert_eq!(
            publisher.send_avc(2, 0, true, &media),
            Err(ClientError::Backpressure)
        );
        assert_eq!(publisher.queued_output_bytes(), queued);
        assert!(queued <= 8192);
        publisher.take_output();
        publisher
            .send_avc(2, 0, true, &media)
            .expect("retry after drain");
        assert_eq!(
            publisher.send_avc(3, 0, true, &vec![0; 1 << 20]),
            Err(ClientError::MediaTooLarge)
        );
        publisher.cancel();
        assert_eq!(publisher.state(), PublisherState::Closed);
        assert_eq!(publisher.queued_output_bytes(), 0);
        assert_eq!(publisher.buffered_input_bytes(), 0);
        assert_eq!(publisher.send_avc_config(&[1]), Err(ClientError::Closed));
    }

    #[test]
    fn control_output_saturation_is_terminal_and_discards_media() {
        let mut config = config();
        config.max_output_bytes = 8192;
        let mut publisher = Publisher::new(config, 1, 0).expect("config");
        publish_independently(&mut publisher, 7);
        let ping = wire(4, 0, &[0, 6, 0, 0, 0, 1], 128);
        let bytes = ping.repeat(500);
        assert_eq!(publisher.feed(&bytes, 3), Err(ClientError::Backpressure));
        assert_eq!(publisher.state(), PublisherState::Failed);
        assert_eq!(publisher.queued_output_bytes(), 0);
    }

    #[test]
    fn disconnect_timeout_and_clock_regression_release_state() {
        let mut publisher = client();
        assert_eq!(publisher.disconnected(), Err(ClientError::Disconnected));
        assert_eq!(publisher.state(), PublisherState::Failed);
        assert_eq!(publisher.queued_output_bytes(), 0);
        let mut publisher = client();
        assert_eq!(publisher.tick(15_000), Err(ClientError::Timeout));
        assert_eq!(publisher.queued_output_bytes(), 0);
        let mut publisher = client();
        publisher.tick(10).expect("forward");
        assert_eq!(publisher.feed(&[], 9), Err(ClientError::TimeWentBackwards));
        let mut config = config();
        config.receive_idle_timeout_ms = Some(100);
        let mut publisher = Publisher::new(config, 1, 0).expect("config");
        publish_independently(&mut publisher, 1);
        assert_eq!(publisher.tick(102), Err(ClientError::Timeout));
        assert_eq!(publisher.buffered_input_bytes(), 0);
    }

    #[test]
    fn independent_command_ordering_and_optional_close_reply() {
        for (txn, stream, body) in [
            (2.0, 0, response("_result", 2.0, None, Some(7.0))),
            (
                1.0,
                7,
                response(
                    "_result",
                    1.0,
                    Some(("NetConnection.Connect.Success", "status")),
                    None,
                ),
            ),
        ] {
            let mut publisher = client();
            handshake(&mut publisher, 7);
            assert_eq!(
                publisher.feed(&wire(20, stream, &body, 128), 2),
                Err(ClientError::UnexpectedResponse),
                "transaction {txn} on stream {stream}"
            );
            assert_eq!(publisher.send_aac_config(&[1]), Err(ClientError::Closed));
        }
        let mut publisher = client();
        handshake(&mut publisher, 7);
        let connected = response(
            "_result",
            1.0,
            Some(("NetConnection.Connect.Success", "status")),
            None,
        );
        publisher
            .feed(&wire(20, 0, &connected, 128), 2)
            .expect("connect");
        let mut expected = Vec::new();
        string(&mut expected, "createStream");
        number(&mut expected, 2.0);
        expected.push(5);
        assert_eq!(publisher.take_output(), wire(20, 0, &expected, 4096));
        let created = response("_result", 2.0, None, Some(7.0));
        publisher
            .feed(&wire(20, 0, &created, 128), 3)
            .expect("created");
        expected.clear();
        string(&mut expected, "publish");
        number(&mut expected, 0.0);
        expected.push(5);
        string(&mut expected, "event?token=placeholder");
        string(&mut expected, "live");
        assert_eq!(publisher.take_output(), wire(20, 7, &expected, 4096));
        let start = response(
            "onStatus",
            0.0,
            Some(("NetStream.Publish.Start", "status")),
            None,
        );
        publisher
            .feed(&wire(20, 7, &start, 128), 4)
            .expect("publish start");
        let optional = response("_result", 0.0, None, None);
        publisher
            .feed(&wire(20, 0, &optional, 128), 4)
            .expect("optional publish result");
        publisher.finish().expect("finish");
        assert_eq!(publisher.state(), PublisherState::Finishing);
        assert_eq!(publisher.send_aac_config(&[1]), Err(ClientError::Closed));
        expected.clear();
        string(&mut expected, "closeStream");
        number(&mut expected, 3.0);
        expected.push(5);
        assert_eq!(publisher.take_output(), wire(20, 7, &expected, 4096));
        publisher
            .feed(&wire(20, 0, &response("_result", 3.0, None, None), 128), 5)
            .expect("close reply");
        assert_eq!(publisher.state(), PublisherState::Closed);
    }

    #[test]
    fn remote_unpublish_discards_queued_media_and_cannot_be_reopened() {
        for (name, stream) in [("FCUnpublish", 0), ("onFCUnpublish", 0), ("closeStream", 7)] {
            let mut publisher = client();
            publish_independently(&mut publisher, 7);
            publisher.send_avc_config(&[1]).expect("queued config");
            publisher
                .send_avc(1, 0, true, &[0, 0, 0, 1, 0x65])
                .expect("queued media");
            let stop = response(name, 0.0, None, None);
            let mut bytes = wire(20, stream, &stop, 128);
            // A status coalesced behind the stop must not revive the session.
            bytes.extend(wire(
                20,
                7,
                &response(
                    "onStatus",
                    0.0,
                    Some(("NetStream.Publish.Start", "status")),
                    None,
                ),
                128,
            ));
            publisher.feed(&bytes, 3).expect("remote stop");
            assert_eq!(publisher.state(), PublisherState::Closed);
            assert!(publisher.take_output().is_empty());
            assert_eq!(publisher.buffered_input_bytes(), 0);
            assert_eq!(publisher.feed(&[], 4), Err(ClientError::Closed));
        }
    }

    fn media_wire(csid: u8, kind: u8, timestamp: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![csid];
        bytes.extend_from_slice(&timestamp.min(0x00ff_ffff).to_be_bytes()[1..]);
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        bytes.push(kind);
        bytes.extend_from_slice(&7u32.to_le_bytes());
        if timestamp >= 0x00ff_ffff {
            bytes.extend_from_slice(&timestamp.to_be_bytes());
        }
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn oldest_wire_media_deadline_is_not_renewed_by_ack_trickle() {
        let mut publisher = client();
        assert_eq!(publisher.next_deadline_ms(), Some(15_000));
        publish_independently(&mut publisher, 7);
        assert_eq!(publisher.next_deadline_ms(), None);
        publisher
            .tick(20_000)
            .expect("silent published origin is allowed");
        let base = publisher.sent as u32;
        publisher
            .feed(&wire(3, 0, &base.to_be_bytes(), 128), 20_000)
            .expect("ACK");
        publisher
            .feed(&wire(6, 0, &[0, 0, 0, 1, 0], 128), 20_000)
            .expect("tiny window");
        publisher
            .send_aac_config(&[0x11, 0x88])
            .expect("media config");
        assert_eq!(publisher.next_deadline_ms(), Some(21_000));
        let mut sequence = base;
        for now in [20_100, 20_400, 20_900] {
            assert_eq!(publisher.take_output().len(), 1);
            sequence = sequence.wrapping_add(1);
            publisher
                .feed(&wire(3, 0, &sequence.to_be_bytes(), 128), now)
                .expect("trickle");
            assert_eq!(publisher.next_deadline_ms(), Some(21_000));
        }
        publisher.tick(20_999).expect("before residence deadline");
        assert_eq!(publisher.tick(21_000), Err(ClientError::Timeout));
        assert_eq!(publisher.queued_output_bytes(), 0);
        assert_eq!(publisher.next_deadline_ms(), None);
    }

    #[test]
    fn draining_media_cancels_its_timer_and_finish_uses_no_old_startup_deadline() {
        let mut publisher = client();
        publish_independently(&mut publisher, 7);
        publisher.tick(20_000).expect("silent publication");
        publisher.send_aac_config(&[0x11, 0x88]).expect("config");
        assert_eq!(publisher.next_deadline_ms(), Some(21_000));
        assert!(!publisher.take_output().is_empty());
        assert_eq!(publisher.next_deadline_ms(), None);
        publisher.finish().expect("finish");
        publisher.take_output();
        publisher
            .tick(60_000)
            .expect("caller owns final close deadline");
        assert_eq!(publisher.state(), PublisherState::Finishing);
        assert_eq!(publisher.next_deadline_ms(), None);
    }

    #[test]
    fn own_origin_acknowledges_multiple_full_windows_without_stalling() {
        let mut publisher = client();
        let mut server = Connection::new(73);
        let mut accepted = false;
        for _ in 0..12 {
            server.feed(&publisher.take_output()).expect("setup feed");
            while let Some(event) = server.poll().expect("setup poll") {
                if matches!(event, Event::Publish { .. }) {
                    server.accept_publish();
                    accepted = true;
                    break;
                }
            }
            publisher
                .feed(&server.take_output(), 1)
                .expect("setup reply");
            if publisher.state() == PublisherState::Publishing {
                break;
            }
        }
        assert!(accepted);
        assert_eq!(publisher.state(), PublisherState::Publishing);
        publisher.send_aac_config(&[0x11, 0x88]).expect("config");
        let payload = vec![0x55; 64 * 1024];
        let mut audio_events = 0;
        for sequence in 0..100 {
            publisher.send_aac(sequence * 22, &payload).expect("media");
            while publisher.queued_output_bytes() != 0 {
                let bytes = publisher.take_output();
                assert!(!bytes.is_empty(), "origin must ACK by the exhausted window");
                assert!(publisher.outstanding_output_bytes() <= 2_500_000);
                for read in bytes.chunks(16 * 1024) {
                    server.feed(read).expect("bounded read");
                    while let Some(event) = server.poll().expect("media poll") {
                        if matches!(event, Event::Audio { .. }) {
                            audio_events += 1;
                        }
                    }
                    publisher
                        .feed(&server.take_output(), 3)
                        .expect("origin ACK");
                }
            }
        }
        assert_eq!(audio_events, 101);
        assert!(publisher.peer_acknowledged >= 5_000_000);
    }

    #[test]
    fn tiny_peer_windows_fragment_wire_without_reordering_or_exempt_controls() {
        for window in [1u32, 7, 16, 4096] {
            let mut publisher = client();
            publish_independently(&mut publisher, 3);
            let base = publisher.sent;
            publisher
                .feed(&wire(3, 0, &(base as u32).to_be_bytes(), 128), 3)
                .expect("ack commands");
            let mut bandwidth = window.to_be_bytes().to_vec();
            bandwidth.push(0);
            publisher
                .feed(&wire(6, 0, &bandwidth, 128), 3)
                .expect("hard window");
            publisher
                .send_aac_config(&[0x11, 0x88])
                .expect("AAC config");
            publisher.send_aac(10, &[0x55; 80]).expect("AAC media");
            publisher
                .feed(&wire(4, 0, &[0, 6, 1, 2, 3, 4], 128), 3)
                .expect("ping");
            let mut expected = wire(5, 0, &window.to_be_bytes(), 4096);
            expected.extend(media_wire(4, 8, 0, &[0xaf, 0, 0x11, 0x88]));
            let mut payload = vec![0xaf, 1];
            payload.extend_from_slice(&[0x55; 80]);
            expected.extend(media_wire(4, 8, 10, &payload));
            expected.extend(wire(4, 0, &[0, 7, 1, 2, 3, 4], 4096));
            let mut got = Vec::new();
            while publisher.queued_output_bytes() != 0 {
                let part = publisher.take_output();
                assert!(!part.is_empty());
                assert!(part.len() <= window as usize);
                got.extend(part);
                assert!(publisher.outstanding_output_bytes() <= u64::from(window));
                if publisher.queued_output_bytes() != 0 {
                    assert!(
                        publisher.take_output().is_empty(),
                        "controls have no bypass"
                    );
                }
                let ack = (base + got.len() as u64) as u32;
                publisher
                    .feed(&wire(3, 0, &ack.to_be_bytes(), 128), 3)
                    .expect("independent ACK");
            }
            assert_eq!(got, expected);
            assert_eq!(publisher.outstanding_output_bytes(), 0);
            assert_eq!(publisher.retained_output_bytes(), 0);
        }
    }

    #[test]
    fn peer_window_limit_types_follow_hard_soft_dynamic_order() {
        let mut publisher = client();
        publish_independently(&mut publisher, 7);
        for (window, kind, expected, effective_kind) in [
            (99u32, 2u8, None, None),
            (4096, 0, Some(4096), Some(0)),
            (8192, 1, Some(4096), Some(1)),
            (2048, 1, Some(2048), Some(1)),
            (1024, 2, Some(2048), Some(1)),
            (512, 0, Some(512), Some(0)),
            (1024, 2, Some(1024), Some(0)),
            (2048, 2, Some(2048), Some(0)),
        ] {
            let mut payload = window.to_be_bytes().to_vec();
            payload.push(kind);
            publisher
                .feed(&wire(6, 0, &payload, 128), 3)
                .expect("bandwidth");
            assert_eq!(publisher.peer_bandwidth, expected);
            assert_eq!(publisher.peer_limit_type, effective_kind);
        }
    }

    #[test]
    fn peer_acknowledgements_wrap_and_reject_future_or_stale_sequences() {
        for invalid in [7u32, 17] {
            let mut publisher = client();
            publish_independently(&mut publisher, 1);
            publisher.sent = (1u64 << 32) + 16;
            publisher.peer_acknowledged = (1u64 << 32) - 8;
            publisher
                .feed(&wire(3, 0, &8u32.to_be_bytes(), 128), 3)
                .expect("wrapped ACK");
            assert_eq!(publisher.peer_acknowledged, (1u64 << 32) + 8);
            assert_eq!(publisher.outstanding_output_bytes(), 8);
            publisher
                .feed(&wire(3, 0, &8u32.to_be_bytes(), 128), 3)
                .expect("duplicate ACK");
            publisher
                .send_aac_config(&[0x11, 0x88])
                .expect("queued media");
            assert_eq!(
                publisher.feed(&wire(3, 0, &invalid.to_be_bytes(), 128), 3),
                Err(ClientError::Malformed(3))
            );
            assert_eq!(publisher.state(), PublisherState::Failed);
            assert_eq!(publisher.queued_output_bytes(), 0);
        }
    }

    #[test]
    fn stalled_peer_window_preserves_budget_and_times_out_without_ack_bypass() {
        let mut config = config();
        config.max_output_bytes = 8192;
        let mut publisher = Publisher::new(config, 42, 0).expect("publisher");
        publish_independently(&mut publisher, 7);
        let base = publisher.sent as u32;
        publisher
            .feed(&wire(3, 0, &base.to_be_bytes(), 128), 3)
            .expect("command ACK");
        publisher
            .feed(&wire(6, 0, &[0, 0, 0, 1, 0], 128), 3)
            .expect("one byte window");
        publisher.send_aac_config(&[0x11, 0x88]).expect("config");
        publisher.send_aac(0, &[0x55; 8000]).expect("bounded media");
        assert_eq!(publisher.take_output().len(), 1);
        let retained = publisher.retained_output_bytes();
        let queued = publisher.queued_output_bytes();
        assert_eq!(retained, queued + 1);
        assert!(retained <= 8192);
        assert_eq!(
            publisher.send_aac(1, &[0; 256]),
            Err(ClientError::Backpressure)
        );
        assert_eq!(publisher.retained_output_bytes(), retained);
        assert!(publisher.take_output().is_empty());
        publisher
            .feed(&wire(3, 0, &base.to_be_bytes(), 128), 4)
            .expect("duplicate cannot renew deadline");
        assert_eq!(publisher.tick(15_003), Err(ClientError::Timeout));
        assert_eq!(publisher.queued_output_bytes(), 0);
        assert_eq!(publisher.retained_output_bytes(), 0);
    }

    #[test]
    fn timestamp_rollover_configuration_resend_and_buffer_reuse() {
        let mut publisher = client();
        publish_independently(&mut publisher, 7);
        assert_eq!(
            publisher.send_avc(0, 0, true, &[1]),
            Err(ClientError::InvalidMedia)
        );
        assert_eq!(publisher.send_aac(0, &[1]), Err(ClientError::InvalidMedia));
        publisher
            .send_avc_config_at(0xffff_ffe0, &[1, 2])
            .expect("config");
        publisher
            .send_aac_config_at(0xffff_ffe0, &[0x12, 0x10])
            .expect("config");
        publisher
            .send_avc(0xffff_fff0, 2, true, &[3, 4])
            .expect("pre-rollover");
        publisher
            .send_avc_config_at(10, &[5, 6])
            .expect("changed config");
        publisher
            .send_avc(20, -2, false, &[7, 8])
            .expect("post-rollover");
        let mut expected = media_wire(6, 9, 0xffff_ffe0, &[0x17, 0, 0, 0, 0, 1, 2]);
        expected.extend(media_wire(4, 8, 0xffff_ffe0, &[0xaf, 0, 0x12, 0x10]));
        expected.extend(media_wire(6, 9, 0xffff_fff0, &[0x17, 1, 0, 0, 2, 3, 4]));
        expected.extend(media_wire(6, 9, 10, &[0x17, 0, 0, 0, 0, 5, 6]));
        expected.extend(media_wire(6, 9, 20, &[0x27, 1, 0xff, 0xff, 0xfe, 7, 8]));
        let output = publisher.take_output();
        assert_eq!(output, expected);
        let capacity = output.capacity();
        assert!(publisher.recycle_output(output));
        assert_eq!(publisher.output.capacity(), capacity);
        let scratch_capacity = publisher.flv_scratch.capacity();
        publisher.send_avc(21, 0, false, &[1, 2]).expect("reuse");
        assert_eq!(publisher.flv_scratch.capacity(), scratch_capacity);
        assert_eq!(publisher.output.capacity(), capacity);
        publisher.cancel();
        assert_eq!(publisher.flv_scratch.capacity(), 0);
        assert!(!publisher.recycle_output(Vec::with_capacity(16)));
    }
}
