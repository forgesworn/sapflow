//! The receive side of a WHIP session's media, sans-IO: RTP packets in,
//! access units, Opus frames, loss reports and RTCP feedback out.
//!
//! A [`Receiver`] holds up to [`MAX_STREAMS`] media streams, each an H.264
//! or Opus payload type from the SDP answer, optionally with an RTX
//! payload type and SSRC (RFC 4588). The caller hands in packets that
//! SRTP has already authenticated, and the RTCP that SRTCP has, with the
//! current time, and calls [`Receiver::handle_timeout`] when
//! [`Receiver::poll_timeout`] says. It then drains [`Receiver::poll_event`]
//! and [`Receiver::poll_feedback`], which writes the RTCP datagrams to send.
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
//!   negotiated): when the caller asks ([`Receiver::request_keyframe`],
//!   as soon as SRTP is up), on the first packet, every keyframe interval,
//!   and at once after unrecoverable loss, rate-limited. After loss, every
//!   frame up to the next IDR is dropped and reported
//!   ([`Event::FrameSkipped`]);
//! - reception statistics for its receiver report block
//!   (RFC 3550 Appendix A.3 and A.8: each sequence number counted once,
//!   RTX not counted for the media SSRC, jitter from first arrivals only,
//!   LSR and DLSR from its sender reports).
//!
//! For the session:
//!
//! - receiver reports every [`ReportConfig::interval`] (1 s, fixed; see
//!   there), and RTCP datagrams that start with a receiver report and the
//!   SDES CNAME (RFC 3550 section 6.1), unless reduced-size RTCP was
//!   negotiated, carrying the NACK, PLI, FIR and transport-cc feedback, up
//!   to [`ReportConfig::max_datagram`] bytes each;
//! - transport-wide congestion control feedback ([`crate::twcc`]) when
//!   [`ReceiverConfig::transport_cc`] is set, for every packet with the
//!   extension, whatever stream it belongs to;
//! - one 90 kHz timeline ([`crate::timeline`]): media is held at the start
//!   until every stream's first sender report, for at most 1.5 s, then
//!   [`Event::Aligned`] says how the streams were aligned, and every
//!   [`Event::AccessUnit`] and [`Event::Audio`] carries its `pts`. The
//!   receiver stamps them itself, rather than leaving a mapper to the
//!   server, because only it knows each frame's arrival time (the fallback
//!   and the bound on sender timestamps) and the order frames leave the
//!   reorder buffer, and the start hold reuses its bounded event queue.

use crate::h264::{self, AccessUnit, H264Error, H264Limits, H264Output, Incomplete};
use crate::opus::{self, FrameKind, OpusError, OpusFrame, OpusLimits, OpusOutput};
use crate::reorder::{Inserted, Released, Reorder, Settings};
use crate::rtcp::build::{self, BuildError, CompoundBuilder};
use crate::rtcp::{Compound, Packet, ReportBlock};
use crate::rtp::RtpPacket;
use crate::rtx::{self, Rtx, RtxError};
use crate::stats::{Arrival, Reception};
use crate::timeline::{Alignment, Timeline, TimelineConfig};
use crate::twcc::{Recorder, TransportCcConfig, TransportCcStats};
use std::collections::VecDeque;
use std::fmt;
use std::time::{Duration, Instant};

/// Most streams one receiver holds (a WHIP answer accepts one video and
/// one audio section).
pub const MAX_STREAMS: usize = 4;
/// Most feedback packets queued. Beyond it, transport-cc feedback is
/// dropped and counted ([`TransportCcStats::dropped`]); NACK, PLI and FIR
/// are already bounded by their rate limits.
pub const MAX_QUEUED_FEEDBACK: usize = 256;

/// Bytes reserved in each datagram for the SDES CNAME: a chunk with the
/// longest CNAME, 255 bytes.
pub const SDES_ROOM: usize = 268;
/// The smallest feedback packet room: a transport-cc packet of
/// [`crate::twcc::MIN_PACKET`] bytes.
pub const MIN_ITEM: usize = crate::twcc::MIN_PACKET;

/// Receiver reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportConfig {
    /// Time between receiver reports, fixed, at least 10 ms; the first is
    /// half of it after the first media packet (RFC 3550 section 6.2
    /// halves the first interval). Default 1 s.
    ///
    /// Fixed rather than RFC 3550 section 6.3's randomised interval: the
    /// randomisation keeps many participants from reporting in step, and a
    /// WHIP session has one sender and one receiver. The interval it would
    /// compute for 5% of a few Mbit/s is under its 5 s floor (or the
    /// reduced floor of 360 / session kbit/s seconds), and one report a
    /// second, about 60 bytes, gives the sender loss and RTT often enough
    /// for rate control. Compounds sent between reports for feedback carry
    /// an empty receiver report, so the fraction lost always covers one
    /// interval.
    pub interval: Duration,
    /// Reduced-size RTCP (RFC 5506) was negotiated (`a=rtcp-rsize`):
    /// feedback between reports goes without a receiver report and SDES.
    /// Default false.
    pub reduced_size: bool,
    /// Largest RTCP datagram before SRTCP, in bytes. Feedback packets are
    /// sized when they are made to fit after the largest receiver report
    /// and SDES CNAME this receiver could send (8 bytes, 24 per stream and
    /// [`SDES_ROOM`]); what does not fit waits for the next datagram. So it
    /// is a hard cap when it is at least that header and [`MIN_ITEM`]
    /// together (436 bytes for four streams); below that, a packet that
    /// cannot fit still goes, alone. Default 1200.
    pub max_datagram: usize,
}
impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            reduced_size: false,
            max_datagram: 1200,
        }
    }
}

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
    /// A sequence jump larger than this, either way, is a possible
    /// restart, confirmed by the next packet in sequence; a smaller one is
    /// loss forward and lateness backward. Default 3000 (RFC 3550 Appendix
    /// A.1's `MAX_DROPOUT`). A.1's `MAX_MISORDER` is not used backward:
    /// retransmissions arrive late by design, hundreds of packets behind
    /// at high packet rates, and must not read as a restart.
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
    /// 8192. A frame is queued only with room for a discontinuity before
    /// it; the start hold ends at half the queue; [`Event::Aligned`] may
    /// take one more.
    pub max_events: usize,
    /// Receiver reports.
    pub reports: ReportConfig,
    /// Transport-cc, if negotiated. Default none.
    pub transport_cc: Option<TransportCcConfig>,
    /// The start hold and timeline bounds.
    pub timeline: TimelineConfig,
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
            reports: ReportConfig::default(),
            transport_cc: None,
            timeline: TimelineConfig::default(),
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
impl Codec {
    /// Its RTP clock rate.
    pub fn clock_rate(self) -> u32 {
        match self {
            Self::H264 => 90_000,
            Self::Opus => 48_000,
        }
    }
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
    /// A frame's RTP timestamp went back by more than
    /// [`TimelineConfig::max_lead`], jumped more than that past the
    /// timeline's arrival bound, or fell behind arrival time by more than
    /// it: the stream re-anchored on arrival time (see [`crate::timeline`]).
    Arrival {
        /// The frame's RTP timestamp.
        timestamp: u32,
    },
    /// Two agreeing sender reports put the stream more than
    /// [`TimelineConfig::max_lead`] behind where its RTP timestamps had it,
    /// and it moved forward by more than that. Smaller errors are slewed
    /// without an event.
    SenderReport {
        /// How far ahead the stream was, in 90 kHz ticks (negative here:
        /// behind).
        drift: i64,
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
        /// Its place on the session's 90 kHz timeline: presentation and
        /// decode time (no B-frames). Never goes back within a stream.
        pts: u64,
    },
    /// An Opus frame: sent, or filled for DTX or loss ([`FrameKind`]).
    Audio {
        /// Which stream.
        stream: StreamId,
        /// The frame.
        frame: OpusFrame,
        /// Its first sample's place on the 90 kHz timeline.
        pts: u64,
    },
    /// The start hold ended: how the streams were aligned. Comes before
    /// any frame.
    Aligned {
        /// Sender reports, or arrival time and why.
        alignment: Alignment,
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
    /// Transport-wide congestion control feedback. The draft leaves the
    /// media SSRC open; it is the first stream's, or 0 before any is
    /// known.
    TransportCc {
        /// The media SSRC field.
        media_ssrc: u32,
        /// Everything after the two SSRCs ([`crate::twcc`]).
        fci: Vec<u8>,
    },
}
impl Feedback {
    /// The SSRC it is about.
    pub fn media_ssrc(&self) -> u32 {
        match self {
            Self::Nack { media_ssrc, .. }
            | Self::Pli { media_ssrc }
            | Self::Fir { media_ssrc, .. }
            | Self::TransportCc { media_ssrc, .. } => *media_ssrc,
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
            Self::TransportCc { media_ssrc, fci } => {
                build::transport_cc(out, sender_ssrc, *media_ssrc, fci)
            }
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
            Self::TransportCc { media_ssrc, fci } => {
                compound.transport_cc(*media_ssrc, fci).map(|_| ())
            }
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
    /// Timeline discontinuities: re-anchors on arrival time, and moves
    /// forward to a sender report.
    pub timeline_discontinuities: u64,
    /// Frames held to the timeline's arrival bound or continued by arrival
    /// time (see [`crate::timeline`]), without a discontinuity.
    pub timeline_clamps: u64,
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
    reception: Reception,
}

/// A sender report for an SSRC no stream has bound yet, kept until one
/// does: `(ssrc, ntp, rtp, arrival)`.
type StashedSr = (u32, u64, u32, u64);

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
    twcc: Option<Recorder>,
    /// When the next receiver report is due, from the first media packet.
    report_next: Option<u64>,
    report_due: bool,
    /// At most [`MAX_STREAMS`], newest last.
    stashed_srs: Vec<StashedSr>,
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
                max: config.max_events.max(2),
                timeline: Timeline::new(config.timeline),
                held_bytes: 0,
            },
            feedback: VecDeque::new(),
            srtt: micros(config.initial_rtt),
            rtt_measured: false,
            tokens: u64::from(config.nack_burst) * 1_000_000,
            refilled: 0,
            twcc: config.transport_cc.map(Recorder::new),
            report_next: None,
            report_due: false,
            stashed_srs: Vec::new(),
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
            reception: Reception::new(cfg.codec.clock_rate()),
        });
        self.events
            .timeline
            .add_track(cfg.codec.clock_rate(), cfg.codec == Codec::H264);
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
            stats.timeline_discontinuities = self.events.timeline.discontinuities(stream.0);
            stats.timeline_clamps = self.events.timeline.clamps(stream.0);
            stats
        })
    }

    /// The report block a receiver report would carry for a stream now,
    /// without starting a new interval. None until its SSRC is known and a
    /// packet counted.
    pub fn report_block(&self, stream: StreamId) -> Option<ReportBlock> {
        let s = self.streams.get(stream.0)?;
        let ssrc = s.ssrc?;
        s.reception
            .started()
            .then(|| s.reception.block(ssrc, self.now))
    }

    /// How the start hold ended, once it has.
    pub fn alignment(&self) -> Option<Alignment> {
        self.events.timeline.alignment()
    }

    /// Transport-cc counters, if negotiated.
    pub fn transport_cc_stats(&self) -> Option<TransportCcStats> {
        self.twcc.as_ref().map(Recorder::stats)
    }

    /// Asks for a keyframe on a video stream at the next timer (subject to
    /// the rate limit). The server calls this as soon as SRTP is up, so the
    /// first IDR comes at once; the first media packet asks too, as a
    /// fallback. A request waits until the stream's SSRC is known. Returns
    /// false for an audio stream or an unknown one.
    pub fn request_keyframe(&mut self, stream: StreamId) -> bool {
        match self.streams.get_mut(stream.0).map(|s| &mut s.media) {
            Some(Media::Video(v)) => {
                v.keyframes.pending = true;
                true
            }
            _ => false,
        }
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
        // Every packet the sender numbered, whatever stream it is for.
        self.record_transport_cc(packet);
        let known = self.streams.iter().any(|s| s.ssrc == Some(packet.ssrc()));
        let (i, is_rtx) = self.route(packet)?;
        if !is_rtx && !known {
            self.apply_stashed_sr(i, packet.ssrc());
        }
        let id = StreamId(i);
        let t = self.now;
        let interval = self.report_interval();
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
        // Media by either route starts the report timer and the start
        // hold; an RTX packet keeps its original's timestamp.
        self.report_next
            .get_or_insert(t.saturating_add(interval / 2));
        self.events.timeline.note_packet(i, packet.timestamp(), t);
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
        if !is_rtx {
            // RTX is a separate RTP stream (RFC 4588 section 6.2): it does
            // not count for the media SSRC's report block.
            let how = match inserted {
                Inserted::New | Inserted::Recovered { nacked: false, .. } => Some(Arrival::Fresh),
                Inserted::Recovered { nacked: true, .. } | Inserted::Duplicate | Inserted::Late => {
                    Some(Arrival::Repeat)
                }
                Inserted::Restart => {
                    // The probe before it, then this packet, start the run.
                    s.reception.restart();
                    let _ = s
                        .reception
                        .count(seq.wrapping_sub(1), 0, t, Arrival::Repeat);
                    Some(Arrival::Fresh)
                }
                Inserted::Probe | Inserted::TooLarge => None,
            };
            if let Some(how) = how {
                let _ = s.reception.count(seq, packet.timestamp(), t, how);
            }
        }
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

    fn record_transport_cc(&mut self, packet: &RtpPacket<'_>) {
        let Some(t) = &mut self.twcc else {
            return;
        };
        let Some(element) = packet.extension_element(t.extension_id()) else {
            return;
        };
        if t.record(self.now, element) {
            self.flush_transport_cc();
        }
    }

    /// Room for one feedback packet in a datagram, after the largest
    /// receiver report and SDES CNAME.
    fn item_room(&self) -> usize {
        let header = 8 + 24 * self.streams.len() + SDES_ROOM;
        self.config.reports.max_datagram.saturating_sub(header)
    }

    fn flush_transport_cc(&mut self) {
        let room = self.item_room();
        let media_ssrc = self.streams.iter().find_map(|s| s.ssrc).unwrap_or(0);
        let Some(t) = &mut self.twcc else {
            return;
        };
        for built in t.flush(self.now, room) {
            if self.feedback.len() >= MAX_QUEUED_FEEDBACK {
                t.note_dropped(1);
                continue;
            }
            self.feedback.push_back(Feedback::TransportCc {
                media_ssrc,
                fci: built.fci,
            });
        }
    }

    fn report_interval(&self) -> u64 {
        micros(self.config.reports.interval).max(10_000)
    }

    /// One RTCP datagram, already through SRTCP. A sender report about a
    /// stream's media SSRC gives its report block's LSR and DLSR, and its
    /// timeline mapping; one about an SSRC no stream has yet is kept (up
    /// to [`MAX_STREAMS`]) until a stream's first packet binds it. Sender
    /// reports about RTX SSRCs, and every other packet, are ignored.
    /// Returns the number of sender reports taken.
    pub fn handle_rtcp(&mut self, now: Instant, compound: &Compound<'_>) -> usize {
        self.advance(now);
        let mut taken = 0;
        for packet in compound.packets() {
            if let Ok(Packet::SenderReport(sr)) = packet
                && self.sender_report(sr.ssrc(), sr.ntp_time(), sr.rtp_time())
            {
                taken += 1;
            }
        }
        self.run();
        taken
    }

    fn sender_report(&mut self, ssrc: u32, ntp: u64, rtp: u32) -> bool {
        let at = self.now;
        if let Some(i) = self.streams.iter().position(|s| s.ssrc == Some(ssrc)) {
            self.apply_sr(i, ntp, rtp, at);
            return true;
        }
        if self.streams.iter().any(|s| s.rtx_ssrc == Some(ssrc))
            || !self.streams.iter().any(|s| s.ssrc.is_none())
        {
            return false;
        }
        self.stashed_srs.retain(|e| e.0 != ssrc);
        if self.stashed_srs.len() >= MAX_STREAMS {
            self.stashed_srs.remove(0);
        }
        self.stashed_srs.push((ssrc, ntp, rtp, at));
        true
    }

    fn apply_stashed_sr(&mut self, i: usize, ssrc: u32) {
        if let Some(at) = self.stashed_srs.iter().position(|e| e.0 == ssrc) {
            let (_, ntp, rtp, arrival) = self.stashed_srs.remove(at);
            self.apply_sr(i, ntp, rtp, arrival);
        }
    }

    fn apply_sr(&mut self, i: usize, ntp: u64, rtp: u32, arrival: u64) {
        let Some(s) = self.streams.get_mut(i) else {
            return;
        };
        s.reception.sender_report(ntp, arrival);
        if let Some(drift) = self.events.timeline.sender_report(i, ntp, rtp, arrival) {
            let _ = self.events.push(
                &mut s.stats,
                Event::Discontinuity {
                    stream: StreamId(i),
                    cause: Discontinuity::SenderReport { drift },
                },
                arrival,
            );
        }
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
        let report = self.report_next.filter(|_| !self.report_due);
        let twcc = self.twcc.as_ref().and_then(Recorder::due);
        for t in [report, twcc, self.events.timeline.hold_deadline()]
            .into_iter()
            .flatten()
        {
            next = Some(next.map_or(t, |n| n.min(t)));
        }
        // A time too far to represent is no timer.
        next.and_then(|t| {
            self.epoch
                .checked_add(Duration::from_micros(t.max(self.now)))
        })
    }

    /// The next event, in order. None while the start hold lasts.
    pub fn poll_event(&mut self) -> Option<Event> {
        if self.events.timeline.holding() {
            return None;
        }
        self.events.queue.pop_front().map(|q| q.event)
    }

    /// The next event and when the packet that released it arrived. For
    /// tests: the timeline's bounds are in terms of that arrival.
    #[cfg(any(test, feature = "testing"))]
    pub fn poll_event_at(&mut self) -> Option<(Event, Instant)> {
        if self.events.timeline.holding() {
            return None;
        }
        let q = self.events.queue.pop_front()?;
        let at = self.epoch.checked_add(Duration::from_micros(q.arrival))?;
        Some((q.event, at))
    }

    /// Appends the next RTCP datagram to send to `out`, if there is one,
    /// and returns whether it did. Call it until it returns false after
    /// every call into the receiver, and send each datagram through SRTCP.
    ///
    /// Each datagram starts with a receiver report from `sender_ssrc`: when
    /// a report is due, with a block for every stream whose SSRC is known
    /// (and the fraction lost starts a new interval), otherwise empty.
    /// Then an SDES CNAME of `cname` (1 to 255 bytes; otherwise left out,
    /// which RFC 3550 section 6.1 does not allow outside reduced-size
    /// RTCP). Then queued NACK, PLI, FIR and transport-cc feedback, oldest
    /// first, while the datagram stays within
    /// [`ReportConfig::max_datagram`] (a hard cap from 436 bytes; see
    /// there). With
    /// [`ReportConfig::reduced_size`], a datagram when no report is due
    /// carries the feedback alone.
    pub fn poll_feedback(&mut self, sender_ssrc: u32, cname: &[u8], out: &mut Vec<u8>) -> bool {
        if !self.report_due && self.feedback.is_empty() {
            return false;
        }
        let start = out.len();
        let report = std::mem::take(&mut self.report_due);
        if report || !self.config.reports.reduced_size {
            let blocks = if report {
                self.take_report_blocks()
            } else {
                Vec::new()
            };
            build::receiver_report(out, sender_ssrc, &blocks);
            // A refused CNAME appends nothing.
            let _ = build::sdes_cname(out, sender_ssrc, cname);
        }
        let max = self.config.reports.max_datagram;
        let mut items = 0usize;
        while let Some(f) = self.feedback.front() {
            let mark = out.len();
            let built = f.write(out, sender_ssrc).is_ok();
            if built && items > 0 && out.len() - start > max {
                out.truncate(mark);
                break;
            }
            self.feedback.pop_front();
            items += usize::from(built);
        }
        true
    }

    /// Report blocks for every stream with a known SSRC and a packet
    /// counted, starting each one's next interval.
    fn take_report_blocks(&mut self) -> Vec<ReportBlock> {
        let now = self.now;
        self.streams
            .iter_mut()
            .filter_map(|s| {
                let ssrc = s.ssrc?;
                if !s.reception.started() {
                    return None;
                }
                let block = s.reception.block(ssrc, now);
                s.reception.reported();
                Some(block)
            })
            .collect()
    }

    /// The next queued feedback packet, without the datagram around it.
    /// For tests; it bypasses the receiver reports.
    #[cfg(any(test, feature = "testing"))]
    pub fn poll_feedback_item(&mut self) -> Option<Feedback> {
        self.feedback.pop_front()
    }

    /// Packets awaiting transport-cc feedback.
    #[cfg(any(test, feature = "testing"))]
    pub fn transport_cc_pending(&self) -> usize {
        self.twcc.as_ref().map_or(0, Recorder::pending)
    }

    /// Events queued, held or not.
    #[cfg(any(test, feature = "testing"))]
    pub fn events_queued(&self) -> usize {
        self.events.queue.len()
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
        Some(self.now.saturating_add((need - self.tokens).div_ceil(rate)))
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
        let room = self.item_room().max(MIN_ITEM) as u64;
        self.refill();
        for (i, s) in self.streams.iter_mut().enumerate() {
            let id = StreamId(i);
            while let Some(r) = s.buffer.poll(now) {
                release(s, id, r, &mut self.events, now);
            }
            if s.cfg.nack
                && let Some(ssrc) = s.ssrc
            {
                let due = s.buffer.due_nacks(now, rtt);
                let (take, bytes) = nack_fit(&due, (self.tokens / 1_000_000).min(room));
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
        if self
            .twcc
            .as_ref()
            .and_then(Recorder::due)
            .is_some_and(|d| d <= now)
        {
            self.flush_transport_cc();
        }
        if let Some(next) = self.report_next
            && next <= now
        {
            self.report_due = true;
            let interval = self.report_interval();
            let after = next.saturating_add(interval);
            self.report_next = Some(if after > now {
                after
            } else {
                now.saturating_add(interval)
            });
        }
        self.events.check_hold(now);
    }

    /// Media not given out yet: packets the reorder buffers hold behind a
    /// gap, and events still queued (all of them during the start hold). A
    /// caller that stops polling here, because its session ended, loses
    /// them.
    pub fn unreleased(&self) -> usize {
        self.streams
            .iter()
            .map(|s| s.buffer.held().0)
            .sum::<usize>()
            .saturating_add(self.events.queue.len())
    }

    /// Events dropped because the queue was full, all streams together
    /// ([`StreamStats::events_dropped`]).
    pub fn events_dropped(&self) -> u64 {
        self.streams
            .iter()
            .map(|s| s.stats.events_dropped)
            .fold(0, u64::saturating_add)
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

/// An event and the arrival of the packet that released it.
#[derive(Debug)]
struct Queued {
    event: Event,
    arrival: u64,
}

/// The event queue, bounded by [`ReceiverConfig::max_events`], and the
/// timeline that stamps its frames.
#[derive(Debug)]
struct Events {
    queue: VecDeque<Queued>,
    max: usize,
    timeline: Timeline,
    /// Media bytes queued during the start hold.
    held_bytes: usize,
}
impl Events {
    /// Queues `event`, released by a packet that arrived at `arrival`, or
    /// drops and counts it if the queue is full.
    #[must_use]
    fn push(&mut self, stats: &mut StreamStats, event: Event, arrival: u64) -> bool {
        let holding = self.timeline.holding();
        let frame = matches!(event, Event::AccessUnit { .. } | Event::Audio { .. });
        // Once stamped, a frame may bring a discontinuity before it.
        let room = if frame && !holding { 2 } else { 1 };
        if self.queue.len() + room > self.max {
            stats.events_dropped += 1;
            return false;
        }
        if holding {
            self.held_bytes = self.held_bytes.saturating_add(media_bytes(&event));
            self.queue.push_back(Queued { event, arrival });
            self.check_hold(arrival);
        } else {
            self.admit(event, arrival);
        }
        true
    }

    /// Stamps and queues an event, after the start hold.
    fn admit(&mut self, mut event: Event, arrival: u64) {
        let mut before = None;
        match &mut event {
            Event::AccessUnit { stream, au, pts } => {
                let s = self.timeline.stamp(stream.0, au.timestamp, arrival);
                *pts = s.pts;
                before = s.reanchored.then_some((*stream, au.timestamp));
            }
            Event::Audio { stream, frame, pts } => {
                let s = self.timeline.stamp(stream.0, frame.timestamp, arrival);
                *pts = s.pts;
                before = s.reanchored.then_some((*stream, frame.timestamp));
            }
            Event::Discontinuity {
                stream,
                cause: Discontinuity::Sequence | Discontinuity::Timestamp { .. },
            } => self.timeline.discontinuity(stream.0),
            _ => {}
        }
        if let Some((stream, timestamp)) = before {
            self.queue.push_back(Queued {
                event: Event::Discontinuity {
                    stream,
                    cause: Discontinuity::Arrival { timestamp },
                },
                arrival,
            });
        }
        self.queue.push_back(Queued { event, arrival });
    }

    /// Ends the start hold if it is time, stamping what it held in order.
    fn check_hold(&mut self, now: u64) {
        let half = self.queue.len().saturating_mul(2) >= self.max;
        if let Some(alignment) = self.timeline.try_align(now, self.held_bytes, half) {
            let held = std::mem::take(&mut self.queue);
            self.held_bytes = 0;
            self.queue.push_back(Queued {
                event: Event::Aligned { alignment },
                arrival: now,
            });
            for q in held {
                self.admit(q.event, q.arrival);
            }
        }
    }
}

fn media_bytes(event: &Event) -> usize {
    match event {
        Event::AccessUnit { au, .. } => {
            au.data.len() + au.sps.iter().chain(&au.pps).map(Vec::len).sum::<usize>()
        }
        Event::Audio { frame, .. } => frame.data.len(),
        _ => 0,
    }
}

fn release(s: &mut Stream, id: StreamId, r: Released, events: &mut Events, now: u64) {
    match r {
        Released::Packet(p) => {
            let sequence = p.seq as u16;
            let arrival = p.arrival;
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
                            arrival,
                        );
                    }
                    drain_video(v, id, &mut s.stats, events, arrival);
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
                            arrival,
                        );
                    }
                    drain_audio(a, id, &mut s.stats, events, arrival);
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
                now,
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
                    drain_video(v, id, &mut s.stats, events, now);
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
                now,
            );
        }
    }
}

fn drain_video(
    v: &mut Video,
    id: StreamId,
    stats: &mut StreamStats,
    events: &mut Events,
    arrival: u64,
) {
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
                        arrival,
                    );
                }
                _ => {
                    if v.awaiting.take().is_some() {
                        v.keyframes.awaiting = false;
                    }
                    let au = Event::AccessUnit {
                        stream: id,
                        au,
                        pts: 0,
                    };
                    if events.push(stats, au, arrival) {
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
                    arrival,
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
    arrival: u64,
) {
    while let Some(o) = a.poll() {
        match o {
            OpusOutput::Frame(frame) => {
                match frame.kind {
                    FrameKind::Media => stats.audio_frames += 1,
                    FrameKind::Dtx => stats.dtx_frames += 1,
                    FrameKind::Loss => stats.loss_frames += 1,
                }
                let frame = Event::Audio {
                    stream: id,
                    frame,
                    pts: 0,
                };
                let _ = events.push(stats, frame, arrival);
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
                    arrival,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
