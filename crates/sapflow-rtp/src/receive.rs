//! The receive side of a WHIP session's media, sans-IO: RTP packets in,
//! access units, Opus frames, loss reports and RTCP feedback out.
//!
//! A [`Receiver`] holds up to [`MAX_STREAMS`] media streams, each an H.264
//! or Opus payload type from the SDP answer, optionally with an RTX
//! payload type and SSRC (RFC 4588). The caller hands in packets that
//! SRTP has already authenticated, with the current time, and calls
//! [`Receiver::handle_timeout`] when [`Receiver::poll_timeout`] says. It
//! then drains [`Receiver::poll_event`] and [`Receiver::poll_feedback`].
//!
//! Each stream has:
//!
//! - a bounded reorder buffer that releases packets in sequence order,
//!   waits up to the jitter deadline (`SAPFLOW_WHIP_JITTER`, 200 ms by
//!   default) for a gap, then declares it lost and moves on;
//! - generic NACKs (RFC 4585 section 6.2.1) for the gaps, after a short
//!   reorder delay, repeated every RTT and a quarter until the jitter
//!   deadline, capped per packet and rate-limited in total. RTT is
//!   measured from NACK to retransmission of a packet NACKed once (from
//!   RTX only, where RTX was negotiated), 100 ms until then;
//! - retransmissions either on the RTX stream or, as OBS sends them, on the
//!   original SSRC and sequence number; both fill the same gap;
//! - the H.264 or Opus depacketiser ([`crate::h264`], [`crate::opus`]);
//! - for video, keyframe requests (PLI, or FIR where only FIR was
//!   negotiated): at once on the first packet, every keyframe interval,
//!   and at once after unrecoverable loss, rate-limited. After loss, every
//!   frame up to the next IDR is dropped and reported
//!   ([`Event::FrameSkipped`]).

use crate::h264::{self, AccessUnit, H264Error, H264Limits, H264Output, Incomplete};
use crate::opus::{self, FrameKind, OpusError, OpusFrame, OpusLimits, OpusOutput};
use crate::reorder::{Inserted, Released, Reorder, Settings};
use crate::rtcp::build::{self, BuildError, CompoundBuilder};
use crate::rtp::RtpPacket;
use crate::rtx::{self, Rtx, RtxError};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

/// Most streams one receiver holds (a WHIP answer accepts one video and
/// one audio section).
pub const MAX_STREAMS: usize = 4;

/// Settings for a [`Receiver`]. Every bound is per stream unless it says
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverConfig {
    /// Longest wait for a missing packet before it is declared lost
    /// (`SAPFLOW_WHIP_JITTER`). Default 200 ms.
    pub jitter: Duration,
    /// How long a gap may be plain reordering before it is NACKed, and
    /// how long the first packets are held in case earlier ones arrive
    /// late. Default 10 ms.
    pub reorder_delay: Duration,
    /// Most packets the reorder buffer spans, held and missing. Default
    /// 1024.
    pub max_packets: usize,
    /// Most payload bytes the reorder buffer holds. Default 1 536 000.
    pub max_bytes: usize,
    /// A forward sequence jump larger than this is a possible restart,
    /// confirmed by the next packet in sequence; a smaller one is loss.
    /// Default 3000 (RFC 3550 Appendix A.1's `MAX_DROPOUT`). Backwards,
    /// the bound is A.1's `MAX_MISORDER`, 100 behind what was released:
    /// closer is late, further a possible restart.
    pub max_dropout: u16,
    /// RTT assumed until one is measured. Default 100 ms.
    pub initial_rtt: Duration,
    /// Most NACKs sent for one packet. Default 4.
    pub max_nacks: u8,
    /// Ceiling on NACK traffic, all streams together, in bits per second
    /// of RTCP NACK packets as built (headers and FCI). Default 64 000.
    pub nack_bitrate: u32,
    /// Burst allowance for that ceiling, in bytes. Default 1500.
    pub nack_burst: u32,
    /// Keyframe request cadence (`SAPFLOW_WHIP_KEYFRAME_INTERVAL`), or
    /// `None` for none after the first. Default 2 s.
    pub keyframe_interval: Option<Duration>,
    /// Shortest time between two keyframe requests. Default 500 ms.
    pub keyframe_min_interval: Duration,
    /// While waiting for an IDR, how often to ask again. Default 1 s.
    pub keyframe_retry: Duration,
    /// H.264 access unit bounds.
    pub h264: H264Limits,
    /// Opus fill bounds.
    pub opus: OpusLimits,
    /// Most events queued at once, all streams together. Beyond it,
    /// events are dropped and counted (`StreamStats::events_dropped`), and
    /// a video stream that lost one waits for an IDR. A caller that drains
    /// [`Receiver::poll_event`] after every call never reaches it. Default
    /// 8192.
    pub max_events: usize,
}
impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            jitter: Duration::from_millis(200),
            reorder_delay: Duration::from_millis(10),
            max_packets: 1024,
            max_bytes: 1_536_000,
            max_dropout: 3000,
            initial_rtt: Duration::from_millis(100),
            max_nacks: 4,
            nack_bitrate: 64_000,
            nack_burst: 1500,
            keyframe_interval: Some(Duration::from_secs(2)),
            keyframe_min_interval: Duration::from_millis(500),
            keyframe_retry: Duration::from_secs(1),
            h264: H264Limits::default(),
            opus: OpusLimits::default(),
            max_events: 8192,
        }
    }
}

/// The payload format of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264, packetisation mode 1 (90 kHz).
    H264,
    /// Opus (48 kHz).
    Opus,
}

/// The RTX side of a stream, from `apt=` and `a=ssrc-group:FID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtxConfig {
    /// The RTX payload type.
    pub payload_type: u8,
    /// The RTX SSRC, if the offer named it; otherwise the first packet
    /// with the RTX payload type binds it.
    pub ssrc: Option<u32>,
}

/// One media stream, from the SDP answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Payload format.
    pub codec: Codec,
    /// Media payload type.
    pub payload_type: u8,
    /// Media SSRC, if the offer named it; otherwise the first packet with
    /// the media payload type binds it.
    pub ssrc: Option<u32>,
    /// The RTX payload type and SSRC, if negotiated.
    pub rtx: Option<RtxConfig>,
    /// `nack` was negotiated.
    pub nack: bool,
    /// `nack pli` was negotiated.
    pub pli: bool,
    /// `ccm fir` was negotiated.
    pub fir: bool,
}
impl StreamConfig {
    /// A stream with no SSRC, RTX or feedback yet.
    pub fn new(codec: Codec, payload_type: u8) -> Self {
        Self {
            codec,
            payload_type,
            ssrc: None,
            rtx: None,
            nack: false,
            pli: false,
            fir: false,
        }
    }
}

/// Which stream, in the order they were added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(usize);
impl StreamId {
    /// Its position, from 0.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Why a stream could not be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// [`MAX_STREAMS`] already.
    TooManyStreams,
    /// A payload type over 127.
    InvalidPayloadType(u8),
    /// A payload type another stream (or this stream's RTX) uses.
    PayloadTypeTaken(u8),
    /// An SSRC another stream uses.
    SsrcTaken(u32),
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiver config: {self:?}")
    }
}
impl std::error::Error for ConfigError {}

/// Why a packet was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    /// Its SSRC and payload type match no stream.
    UnknownStream,
    /// Its SSRC is a stream's, but its payload type is not.
    PayloadType {
        /// The packet's SSRC.
        ssrc: u32,
        /// Its payload type.
        payload_type: u8,
    },
    /// An RTX payload too short for its OSN.
    Rtx(RtxError),
    /// A payload larger than the reorder buffer's byte bound.
    TooLarge,
}
impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receive: {self:?}")
    }
}
impl std::error::Error for ReceiveError {}

/// What became of a packet that was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    /// Buffered or released.
    New,
    /// It filled a gap (a retransmission, or a late original).
    Recovered,
    /// A copy of a packet already held; dropped.
    Duplicate,
    /// Older than what was already released or declared lost; dropped.
    Late,
    /// Far from the stream's sequence numbers; set aside until the next
    /// packet shows whether the stream restarted.
    Probe,
    /// It confirmed a restart of the sequence numbers.
    Restart,
    /// An RTX packet with no payload (a bandwidth probe); ignored.
    RtxPadding,
}

/// A payload a depacketiser refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadError {
    /// H.264.
    H264(H264Error),
    /// Opus.
    Opus(OpusError),
}

/// Why a complete video frame was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No IDR has arrived yet.
    NoKeyframeYet,
    /// Video was lost and no IDR has arrived since.
    AfterLoss,
}

/// What kind of discontinuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discontinuity {
    /// The sequence numbers jumped and the jump was confirmed.
    Sequence,
    /// An Opus timestamp jumped further than the fill limit, or back.
    Timestamp {
        /// Where the next packet was expected.
        expected: u32,
        /// Where it was.
        timestamp: u32,
    },
}

/// What a receiver produces, per stream in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A complete access unit, from an IDR on.
    AccessUnit {
        /// Which stream.
        stream: StreamId,
        /// The access unit.
        au: AccessUnit,
    },
    /// An Opus frame: sent, or filled for DTX or loss ([`FrameKind`]).
    Audio {
        /// Which stream.
        stream: StreamId,
        /// The frame.
        frame: OpusFrame,
    },
    /// Packets that were never received within the jitter deadline:
    /// `count` from sequence number `first`.
    Lost {
        /// Which stream.
        stream: StreamId,
        /// The first lost sequence number.
        first: u16,
        /// How many in a row.
        count: u64,
    },
    /// A video frame that lost data, dropped. Keyframe requests follow and
    /// frames are skipped until an IDR.
    IncompleteFrame {
        /// Which stream.
        stream: StreamId,
        /// Its RTP timestamp.
        timestamp: u32,
        /// What it lost.
        reason: Incomplete,
    },
    /// A complete video frame dropped while waiting for an IDR.
    FrameSkipped {
        /// Which stream.
        stream: StreamId,
        /// Its RTP timestamp.
        timestamp: u32,
        /// Why.
        reason: SkipReason,
    },
    /// The stream's timeline restarted.
    Discontinuity {
        /// Which stream.
        stream: StreamId,
        /// What jumped.
        cause: Discontinuity,
    },
    /// A payload the depacketiser refused.
    Refused {
        /// Which stream.
        stream: StreamId,
        /// Its sequence number.
        sequence: u16,
        /// Why.
        error: PayloadError,
    },
}

/// RTCP feedback to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    /// A generic NACK for these sequence numbers, oldest first.
    Nack {
        /// The stream's SSRC.
        media_ssrc: u32,
        /// Missing sequence numbers.
        lost: Vec<u16>,
    },
    /// A picture loss indication.
    Pli {
        /// The stream's SSRC.
        media_ssrc: u32,
    },
    /// A full intra request.
    Fir {
        /// The stream's SSRC.
        media_ssrc: u32,
        /// Its command sequence number, new for each request.
        sequence: u8,
    },
}
impl Feedback {
    /// The SSRC it is about.
    pub fn media_ssrc(&self) -> u32 {
        match self {
            Self::Nack { media_ssrc, .. }
            | Self::Pli { media_ssrc }
            | Self::Fir { media_ssrc, .. } => *media_ssrc,
        }
    }
    /// Appends it to `out` as one RTCP packet from `sender_ssrc`, with
    /// [`crate::rtcp::build`].
    pub fn write(&self, out: &mut Vec<u8>, sender_ssrc: u32) -> Result<(), BuildError> {
        match self {
            Self::Nack { media_ssrc, lost } => build::nack(out, sender_ssrc, *media_ssrc, lost),
            Self::Pli { media_ssrc } => {
                build::pli(out, sender_ssrc, *media_ssrc);
                Ok(())
            }
            Self::Fir {
                media_ssrc,
                sequence,
            } => build::fir(out, sender_ssrc, &[(*media_ssrc, *sequence)]),
        }
    }
    /// Adds it to a compound packet.
    pub fn add_to(&self, compound: &mut CompoundBuilder<'_>) -> Result<(), BuildError> {
        match self {
            Self::Nack { media_ssrc, lost } => compound.nack(*media_ssrc, lost).map(|_| ()),
            Self::Pli { media_ssrc } => {
                compound.pli(*media_ssrc);
                Ok(())
            }
            Self::Fir {
                media_ssrc,
                sequence,
            } => compound.fir(&[(*media_ssrc, *sequence)]).map(|_| ()),
        }
    }
}

/// Counters for one stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Packets taken, retransmissions included.
    pub packets: u64,
    /// Their payload bytes.
    pub bytes: u64,
    /// Packets that filled a gap.
    pub recovered: u64,
    /// Copies of packets already held.
    pub duplicates: u64,
    /// Packets older than what was released.
    pub late: u64,
    /// RTX packets with no payload.
    pub rtx_padding: u64,
    /// Packets declared lost.
    pub lost: u64,
    /// NACK packets sent.
    pub nack_packets: u64,
    /// Sequence numbers in them, repeats counted.
    pub nacked: u64,
    /// PLIs and FIRs sent.
    pub keyframe_requests: u64,
    /// Access units delivered.
    pub frames: u64,
    /// Video frames dropped as incomplete.
    pub incomplete: u64,
    /// Complete video frames dropped while waiting for an IDR.
    pub skipped: u64,
    /// Access units that ended without a marker but were whole.
    pub missing_markers: u64,
    /// Opus frames sent.
    pub audio_frames: u64,
    /// Silence frames filled for DTX.
    pub dtx_frames: u64,
    /// Silence frames filled for loss.
    pub loss_frames: u64,
    /// Payloads refused by the depacketiser.
    pub refused: u64,
    /// Sequence and timestamp discontinuities.
    pub discontinuities: u64,
    /// Events dropped because the queue was full.
    pub events_dropped: u64,
}

/// When to ask for a keyframe.
#[derive(Debug)]
struct Keyframes {
    interval: Option<u64>,
    min_interval: u64,
    retry: u64,
    last: Option<u64>,
    pending: bool,
    awaiting: bool,
}
impl Keyframes {
    fn earliest(&self) -> u64 {
        self.last.map_or(0, |l| l.saturating_add(self.min_interval))
    }
    fn next(&self) -> Option<u64> {
        let due = if self.pending {
            Some(0)
        } else {
            let last = self.last?;
            let retry = self.awaiting.then(|| last.saturating_add(self.retry));
            let periodic = self.interval.map(|i| last.saturating_add(i));
            retry.into_iter().chain(periodic).min()
        };
        due.map(|d| d.max(self.earliest()))
    }
    fn poll(&mut self, now: u64) -> bool {
        if self.next().is_some_and(|at| at <= now) {
            self.last = Some(now);
            self.pending = false;
            return true;
        }
        false
    }
}

#[derive(Debug)]
struct Video {
    depack: h264::Depacketiser,
    awaiting: Option<SkipReason>,
    keyframes: Keyframes,
    fir_sequence: u8,
}
impl Video {
    fn unrecoverable(&mut self) {
        self.awaiting = Some(SkipReason::AfterLoss);
        self.keyframes.pending = true;
        self.keyframes.awaiting = true;
    }
}

#[derive(Debug)]
enum Media {
    Video(Box<Video>),
    Audio(opus::Depacketiser),
}

#[derive(Debug)]
struct Stream {
    cfg: StreamConfig,
    ssrc: Option<u32>,
    rtx_ssrc: Option<u32>,
    buffer: Reorder,
    media: Media,
    stats: StreamStats,
}

/// The receive side of one session's media. See the module docs.
#[derive(Debug)]
pub struct Receiver {
    config: ReceiverConfig,
    epoch: Instant,
    /// Microseconds since `epoch`, never going back.
    now: u64,
    streams: Vec<Stream>,
    events: Events,
    feedback: VecDeque<Feedback>,
    srtt: u64,
    rtt_measured: bool,
    /// NACK allowance in millionths of a byte.
    tokens: u64,
    refilled: u64,
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

impl Receiver {
    /// A receiver with no streams yet.
    pub fn new(config: ReceiverConfig, now: Instant) -> Self {
        Self {
            config,
            epoch: now,
            now: 0,
            streams: Vec::new(),
            events: Events {
                queue: VecDeque::new(),
                max: config.max_events,
            },
            feedback: VecDeque::new(),
            srtt: micros(config.initial_rtt),
            rtt_measured: false,
            tokens: u64::from(config.nack_burst) * 1_000_000,
            refilled: 0,
        }
    }

    /// Adds a stream from the SDP answer.
    pub fn add_stream(&mut self, cfg: StreamConfig) -> Result<StreamId, ConfigError> {
        if self.streams.len() >= MAX_STREAMS {
            return Err(ConfigError::TooManyStreams);
        }
        let mut pts = vec![cfg.payload_type];
        pts.extend(cfg.rtx.map(|r| r.payload_type));
        for &pt in &pts {
            if pt > 127 {
                return Err(ConfigError::InvalidPayloadType(pt));
            }
        }
        if pts.get(1).is_some_and(|&r| r == cfg.payload_type) {
            return Err(ConfigError::PayloadTypeTaken(cfg.payload_type));
        }
        let mut ssrcs: Vec<u32> = cfg.ssrc.into_iter().collect();
        ssrcs.extend(cfg.rtx.and_then(|r| r.ssrc));
        if let [a, b] = ssrcs[..]
            && a == b
        {
            return Err(ConfigError::SsrcTaken(a));
        }
        for s in &self.streams {
            for &pt in &pts {
                if pt == s.cfg.payload_type || s.cfg.rtx.is_some_and(|r| r.payload_type == pt) {
                    return Err(ConfigError::PayloadTypeTaken(pt));
                }
            }
            for &ssrc in &ssrcs {
                if s.ssrc == Some(ssrc) || s.rtx_ssrc == Some(ssrc) {
                    return Err(ConfigError::SsrcTaken(ssrc));
                }
            }
        }
        let c = &self.config;
        let settings = Settings {
            max_packets: c.max_packets.max(1),
            max_bytes: c.max_bytes,
            jitter: micros(c.jitter),
            reorder_delay: micros(c.reorder_delay),
            max_dropout: u64::from(c.max_dropout),
            max_misorder: 100,
            max_nacks: c.max_nacks,
        };
        let media = match cfg.codec {
            Codec::H264 => Media::Video(Box::new(Video {
                depack: h264::Depacketiser::new(c.h264),
                awaiting: Some(SkipReason::NoKeyframeYet),
                keyframes: Keyframes {
                    interval: c.keyframe_interval.map(micros),
                    min_interval: micros(c.keyframe_min_interval),
                    retry: micros(c.keyframe_retry),
                    last: None,
                    pending: false,
                    awaiting: true,
                },
                fir_sequence: 0,
            })),
            Codec::Opus => Media::Audio(opus::Depacketiser::new(c.opus)),
        };
        self.streams.push(Stream {
            cfg,
            ssrc: cfg.ssrc,
            rtx_ssrc: cfg.rtx.and_then(|r| r.ssrc),
            buffer: Reorder::new(settings),
            media,
            stats: StreamStats::default(),
        });
        Ok(StreamId(self.streams.len() - 1))
    }

    /// The media SSRC a stream has, configured or bound.
    pub fn ssrc(&self, stream: StreamId) -> Option<u32> {
        self.streams.get(stream.0).and_then(|s| s.ssrc)
    }

    /// A stream's counters.
    pub fn stats(&self, stream: StreamId) -> Option<StreamStats> {
        self.streams.get(stream.0).map(|s| {
            let mut stats = s.stats;
            if let Media::Video(v) = &s.media {
                stats.missing_markers = v.depack.missing_markers();
            }
            stats
        })
    }

    /// The smoothed RTT estimate: 100 ms (or the configured value) until
    /// a retransmission has been timed.
    pub fn rtt(&self) -> Duration {
        Duration::from_micros(self.srtt)
    }

    /// Whether an RTT has been measured.
    pub fn rtt_measured(&self) -> bool {
        self.rtt_measured
    }

    fn advance(&mut self, now: Instant) {
        let t = micros(now.saturating_duration_since(self.epoch));
        self.now = self.now.max(t);
    }

    fn route(&mut self, packet: &RtpPacket<'_>) -> Result<(usize, bool), ReceiveError> {
        let (ssrc, pt) = (packet.ssrc(), packet.payload_type());
        let wrong = ReceiveError::PayloadType {
            ssrc,
            payload_type: pt,
        };
        for (i, s) in self.streams.iter().enumerate() {
            if s.ssrc == Some(ssrc) {
                return if pt == s.cfg.payload_type {
                    Ok((i, false))
                } else {
                    Err(wrong)
                };
            }
            if s.rtx_ssrc == Some(ssrc) {
                return if s.cfg.rtx.is_some_and(|r| r.payload_type == pt) {
                    Ok((i, true))
                } else {
                    Err(wrong)
                };
            }
        }
        // An SSRC not yet known binds by payload type, if that side of a
        // stream has none.
        for (i, s) in self.streams.iter_mut().enumerate() {
            if s.ssrc.is_none() && pt == s.cfg.payload_type {
                s.ssrc = Some(ssrc);
                return Ok((i, false));
            }
            if s.rtx_ssrc.is_none() && s.cfg.rtx.is_some_and(|r| r.payload_type == pt) {
                s.rtx_ssrc = Some(ssrc);
                return Ok((i, true));
            }
        }
        Err(ReceiveError::UnknownStream)
    }

    /// One RTP packet, already through SRTP.
    pub fn handle_rtp(
        &mut self,
        now: Instant,
        packet: &RtpPacket<'_>,
    ) -> Result<(StreamId, Received), ReceiveError> {
        self.advance(now);
        let (i, is_rtx) = self.route(packet)?;
        let id = StreamId(i);
        let t = self.now;
        let s = self.streams.get_mut(i).ok_or(ReceiveError::UnknownStream)?;
        let (seq, payload) = if is_rtx {
            match rtx::unwrap(packet.payload()).map_err(ReceiveError::Rtx)? {
                Rtx::Padding => {
                    s.stats.rtx_padding += 1;
                    return Ok((id, Received::RtxPadding));
                }
                Rtx::Original { sequence, payload } => (sequence, payload),
            }
        } else {
            (packet.sequence(), packet.payload())
        };
        if s.stats.packets == 0
            && let Media::Video(v) = &mut s.media
        {
            v.keyframes.pending = true;
        }
        s.stats.packets += 1;
        s.stats.bytes += payload.len() as u64;
        let inserted = s
            .buffer
            .insert(t, seq, packet.timestamp(), packet.marker(), payload);
        let received = match inserted {
            Inserted::New => Received::New,
            Inserted::Recovered { nacked: _, rtt } => {
                s.stats.recovered += 1;
                // With RTX negotiated, only RTX fills time a retransmission:
                // a late original on the media SSRC would read as a short
                // RTT. Without it (OBS), the two cannot be told apart.
                if let Some(sample) = rtt
                    && (is_rtx || s.cfg.rtx.is_none())
                {
                    let sample = sample.clamp(1_000, 1_000_000);
                    self.srtt = if self.rtt_measured {
                        (7 * self.srtt + sample) / 8
                    } else {
                        sample
                    };
                    self.rtt_measured = true;
                }
                Received::Recovered
            }
            Inserted::Duplicate => {
                s.stats.duplicates += 1;
                Received::Duplicate
            }
            Inserted::Late => {
                s.stats.late += 1;
                Received::Late
            }
            Inserted::Probe => Received::Probe,
            Inserted::Restart => Received::Restart,
            Inserted::TooLarge => return Err(ReceiveError::TooLarge),
        };
        self.run();
        Ok((id, received))
    }

    /// Runs the timers due at `now`.
    pub fn handle_timeout(&mut self, now: Instant) {
        self.advance(now);
        self.run();
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be
    /// called, if anything is waiting.
    pub fn poll_timeout(&self) -> Option<Instant> {
        let rtt = self.srtt;
        let refill = self.refill_at(NACK_ONE);
        let mut next: Option<u64> = None;
        for s in &self.streams {
            let mut consider = |t: Option<u64>| {
                if let Some(t) = t {
                    next = Some(next.map_or(t, |n| n.min(t)));
                }
            };
            consider(s.buffer.next_release());
            if s.cfg.nack && s.ssrc.is_some() {
                consider(
                    s.buffer
                        .next_nack(rtt)
                        .and_then(|at| refill.map(|r| at.max(r))),
                );
            }
            if let Media::Video(v) = &s.media
                && s.ssrc.is_some()
                && (s.cfg.pli || s.cfg.fir)
            {
                consider(v.keyframes.next());
            }
        }
        next.map(|t| self.epoch + Duration::from_micros(t.max(self.now)))
    }

    /// The next event, in order.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.queue.pop_front()
    }

    /// The next RTCP feedback to send.
    pub fn poll_feedback(&mut self) -> Option<Feedback> {
        self.feedback.pop_front()
    }

    /// When the NACK allowance holds `bytes`, if it ever will.
    fn refill_at(&self, bytes: u64) -> Option<u64> {
        let need = bytes * 1_000_000;
        if self.tokens >= need {
            return Some(0);
        }
        let rate = u64::from(self.config.nack_bitrate) / 8;
        if rate == 0 || need > u64::from(self.config.nack_burst) * 1_000_000 {
            return None;
        }
        Some(self.now + (need - self.tokens).div_ceil(rate))
    }

    fn refill(&mut self) {
        let rate = u64::from(self.config.nack_bitrate) / 8;
        let elapsed = self.now.saturating_sub(self.refilled);
        self.refilled = self.now;
        let cap = u64::from(self.config.nack_burst) * 1_000_000;
        self.tokens = self
            .tokens
            .saturating_add(elapsed.saturating_mul(rate))
            .min(cap);
    }

    fn run(&mut self) {
        let now = self.now;
        let rtt = self.srtt;
        self.refill();
        for (i, s) in self.streams.iter_mut().enumerate() {
            let id = StreamId(i);
            while let Some(r) = s.buffer.poll(now) {
                release(s, id, r, &mut self.events);
            }
            if s.cfg.nack
                && let Some(ssrc) = s.ssrc
            {
                let due = s.buffer.due_nacks(now, rtt);
                let (take, bytes) = nack_fit(&due, self.tokens / 1_000_000);
                if let Some(sent) = due.get(..take)
                    && !sent.is_empty()
                {
                    self.tokens -= bytes * 1_000_000;
                    s.buffer.mark_nacked(sent, now);
                    s.stats.nack_packets += 1;
                    s.stats.nacked += sent.len() as u64;
                    self.feedback.push_back(Feedback::Nack {
                        media_ssrc: ssrc,
                        lost: sent.iter().map(|&e| e as u16).collect(),
                    });
                }
            }
            if let Media::Video(v) = &mut s.media
                && let Some(ssrc) = s.ssrc
                && (s.cfg.pli || s.cfg.fir)
                && v.keyframes.poll(now)
            {
                s.stats.keyframe_requests += 1;
                self.feedback.push_back(if s.cfg.pli {
                    Feedback::Pli { media_ssrc: ssrc }
                } else {
                    v.fir_sequence = v.fir_sequence.wrapping_add(1);
                    Feedback::Fir {
                        media_ssrc: ssrc,
                        sequence: v.fir_sequence,
                    }
                });
            }
        }
    }

    /// Packets and payload bytes a stream's reorder buffer holds.
    #[cfg(any(test, feature = "testing"))]
    pub fn buffered(&self, stream: StreamId) -> Option<(usize, usize)> {
        self.streams.get(stream.0).map(|s| s.buffer.held())
    }

    /// Whether a stream is waiting for this sequence number.
    #[cfg(any(test, feature = "testing"))]
    pub fn is_missing(&self, stream: StreamId, seq: u16) -> bool {
        self.streams
            .get(stream.0)
            .is_some_and(|s| s.buffer.is_missing(seq))
    }
}

/// Bytes of a NACK for one sequence number.
const NACK_ONE: u64 = 16;

/// How many of `due` (ascending extended sequence numbers) fit in one
/// NACK of at most `budget` bytes, and that NACK's size: 12 bytes of
/// header and SSRCs, and 4 per PID and BLP pair, each pair covering a
/// PID and the 16 after it (as [`build::nack_pairs`] packs them).
fn nack_fit(due: &[u64], budget: u64) -> (usize, u64) {
    let mut pid: Option<u64> = None;
    let mut size = 12u64;
    let mut take = 0usize;
    for &seq in due {
        let opens = pid.is_none_or(|p| seq - p > 16);
        let next = size + if opens { 4 } else { 0 };
        if next > budget || (opens && (next - 12) / 4 > build::MAX_FCI as u64) {
            break;
        }
        if opens {
            pid = Some(seq);
        }
        size = next;
        take += 1;
    }
    (take, if take == 0 { 0 } else { size })
}

/// The event queue, bounded by [`ReceiverConfig::max_events`].
#[derive(Debug)]
struct Events {
    queue: VecDeque<Event>,
    max: usize,
}
impl Events {
    /// Queues `event`, or drops and counts it if the queue is full.
    #[must_use]
    fn push(&mut self, stats: &mut StreamStats, event: Event) -> bool {
        if self.queue.len() >= self.max {
            stats.events_dropped += 1;
            return false;
        }
        self.queue.push_back(event);
        true
    }
}

fn release(s: &mut Stream, id: StreamId, r: Released, events: &mut Events) {
    match r {
        Released::Packet(p) => {
            let sequence = p.seq as u16;
            match &mut s.media {
                Media::Video(v) => {
                    if let Err(e) = v.depack.push(p.timestamp, p.marker, &p.payload) {
                        s.stats.refused += 1;
                        let _ = events.push(
                            &mut s.stats,
                            Event::Refused {
                                stream: id,
                                sequence,
                                error: PayloadError::H264(e),
                            },
                        );
                    }
                    drain_video(v, id, &mut s.stats, events);
                }
                Media::Audio(a) => {
                    if let Err(e) = a.push(p.timestamp, p.arrival, &p.payload) {
                        s.stats.refused += 1;
                        let _ = events.push(
                            &mut s.stats,
                            Event::Refused {
                                stream: id,
                                sequence,
                                error: PayloadError::Opus(e),
                            },
                        );
                    }
                    drain_audio(a, id, &mut s.stats, events);
                }
            }
        }
        Released::Lost { first, count } => {
            s.stats.lost += count;
            let _ = events.push(
                &mut s.stats,
                Event::Lost {
                    stream: id,
                    first: first as u16,
                    count,
                },
            );
            match &mut s.media {
                Media::Video(v) => {
                    v.depack.lost();
                    v.unrecoverable();
                }
                Media::Audio(a) => a.lost(),
            }
        }
        Released::Discontinuity => {
            s.stats.discontinuities += 1;
            match &mut s.media {
                Media::Video(v) => {
                    v.depack.reset();
                    drain_video(v, id, &mut s.stats, events);
                    v.unrecoverable();
                }
                Media::Audio(a) => a.reset(),
            }
            let _ = events.push(
                &mut s.stats,
                Event::Discontinuity {
                    stream: id,
                    cause: Discontinuity::Sequence,
                },
            );
        }
    }
}

fn drain_video(v: &mut Video, id: StreamId, stats: &mut StreamStats, events: &mut Events) {
    while let Some(o) = v.depack.poll() {
        match o {
            H264Output::Frame(au) => match v.awaiting {
                Some(reason) if !au.idr => {
                    stats.skipped += 1;
                    let _ = events.push(
                        stats,
                        Event::FrameSkipped {
                            stream: id,
                            timestamp: au.timestamp,
                            reason,
                        },
                    );
                }
                _ => {
                    if v.awaiting.take().is_some() {
                        v.keyframes.awaiting = false;
                    }
                    if events.push(stats, Event::AccessUnit { stream: id, au }) {
                        stats.frames += 1;
                    } else {
                        // The decoder would miss a frame: wait for an IDR.
                        v.unrecoverable();
                    }
                }
            },
            H264Output::Incomplete { timestamp, reason } => {
                stats.incomplete += 1;
                let _ = events.push(
                    stats,
                    Event::IncompleteFrame {
                        stream: id,
                        timestamp,
                        reason,
                    },
                );
                v.unrecoverable();
            }
        }
    }
}

fn drain_audio(
    a: &mut opus::Depacketiser,
    id: StreamId,
    stats: &mut StreamStats,
    events: &mut Events,
) {
    while let Some(o) = a.poll() {
        match o {
            OpusOutput::Frame(frame) => {
                match frame.kind {
                    FrameKind::Media => stats.audio_frames += 1,
                    FrameKind::Dtx => stats.dtx_frames += 1,
                    FrameKind::Loss => stats.loss_frames += 1,
                }
                let _ = events.push(stats, Event::Audio { stream: id, frame });
            }
            OpusOutput::Discontinuity {
                expected,
                timestamp,
            } => {
                stats.discontinuities += 1;
                let _ = events.push(
                    stats,
                    Event::Discontinuity {
                        stream: id,
                        cause: Discontinuity::Timestamp {
                            expected,
                            timestamp,
                        },
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
