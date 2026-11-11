//! WHIP ingest (docs/whip-ingest.md): the signalling listener on
//! `SAPFLOW_WHIP_ADDR`, and one task that owns the media socket on
//! `SAPFLOW_WHIP_UDP_ADDR` and every session's sans-IO state through
//! `sapflow_whip::router::Router`. Nothing is bound unless WHIP is
//! configured.
//!
//! A `POST` is checked in this order, and a refusal at any step is sent
//! without going further (`whip_request.rs` holds the rules):
//!
//! 1. The head: size, header count, target length, CRLF lines.
//! 2. The route and method (404, 405).
//! 3. The bearer token's presence (401).
//! 4. The content type and the declared length (415, 413, 400), before a
//!    byte of the body is read.
//! 5. The offer and the answer (400, 422).
//! 6. Admission through the publish-auth path, as SRT and RTMP use it, and
//!    the claim on the path (401, 429, 409, 503). A WHIP claim never takes
//!    over an attached publisher.
//! 7. The session, then its ingest task, then its registration with the
//!    UDP owner (503 when the router is full), then `201`.
//!
//! The UDP owner hands each session's depacketised media to that
//! session's ingest task over a bounded queue. A full queue is media loss
//! for that publisher, marked on its recording, never back-pressure on the
//! socket: a dropped access unit makes video wait for the next IDR.
//!
//! Nothing here logs a token, an offer or a publisher's address.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sapflow_core::{PublishError, PublishRequest, StreamPath};
use sapflow_dtls::Identity;
use sapflow_hls::codec::{AccessUnit, OpusConfig};
use sapflow_hls::{IngestError, TrackSet, Update};
use sapflow_rtp::opus::FrameKind;
use sapflow_rtp::receive::{Discontinuity, SkipReason};
use sapflow_whip::ice::{Kind, classify};
use sapflow_whip::resource::{self, SessionId};
use sapflow_whip::router::{InsertError, Router, RouterConfig};
use sapflow_whip::sdp::{self, AnswerConfig, MediaKind, Offer};
use sapflow_whip::{Credentials, EndReason, Event, Session, SessionConfig, Tuple};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot, watch};

use crate::App;
use crate::config::WhipConfig;
use crate::http_lite::{self, ParsedRequest, Refusal as HeadRefusal};
use crate::publish::{Publication, PublicationLoss, Refusal};
use crate::recorder::IncompleteReason;
use crate::stream::{Source, SourceKind};
use crate::whip_request::{self, Action, Refused};

/// Signalling connections served at once; more are closed as they arrive.
const CONNECTIONS: usize = 32;
/// Media items queued per session: about 10 s of audio and video.
const QUEUE: usize = 1024;
/// Media bytes queued per session. An access unit may be 8 MiB, so 1024 of
/// them could pin 8 GiB; 16 MiB is about 3 s at the 40 Mbit/s publish cap
/// and over 20 s at a browser's 5 Mbit/s, so it is reached only when the
/// ingest stalls. At the default 16 publishers it bounds the queues to
/// 256 MiB. Reaching it is a full queue.
const QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// The whole request head, counted from accept.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// Reading the offer, admission and setup together.
const HANDLER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `DELETE` waits for the publication to be released.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

/// One media item for a session's ingest task.
#[derive(Debug)]
pub(crate) enum Item {
    Video {
        pts: u64,
        au: AccessUnit,
        /// SRTP and SRTCP bytes received since the last item.
        received: usize,
    },
    Audio {
        pts: u64,
        packet: Vec<u8>,
        received: usize,
        /// For the diagnostic when the muxer reports a break.
        trace: AudioTrace,
    },
    /// The receiver's timeline re-anchored on arrival time (or moved to a
    /// sender report) before the next frame, with no loss of its own.
    Reanchor,
}

/// The audio stream's timeline counters in `sapflow-rtp` as an event was
/// taken: their changes between two packets say which branch stamped the
/// second (`sapflow_rtp::timeline`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Timeline {
    /// Timestamps held to the bound, a little ahead of arrival time.
    pub held: u64,
    /// Timestamps held to the bound or continued by arrival time.
    pub clamps: u64,
    /// Re-anchors on arrival time and moves to a sender report.
    pub discontinuities: u64,
}

/// What the ingest keeps of an Opus packet to explain a break the muxer
/// reports: the sender's RTP timestamp (48 kHz), the packet's TOC duration
/// in samples, and the timeline's counters when it was released.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AudioTrace {
    pub rtp: u32,
    pub samples: u32,
    pub timeline: Timeline,
}

impl Item {
    /// The media bytes it holds, for the queue's byte bound.
    fn size(&self) -> usize {
        match self {
            Self::Video { au, .. } => au
                .data
                .len()
                .saturating_add(au.sps.as_ref().map_or(0, Vec::len))
                .saturating_add(au.pps.as_ref().map_or(0, Vec::len)),
            Self::Audio { packet, .. } => packet.len(),
            Self::Reanchor => 0,
        }
    }

    fn audio_trace(&self) -> Option<(u64, AudioTrace)> {
        match self {
            Self::Audio { pts, trace, .. } => Some((*pts, *trace)),
            Self::Video { .. } | Self::Reanchor => None,
        }
    }

    fn received(&self) -> usize {
        match self {
            Self::Video { received, .. } | Self::Audio { received, .. } => *received,
            Self::Reanchor => 0,
        }
    }
}

/// Which path marked a session's recording, counted path by path: logged
/// when the session ends and readable by tests, so a mark always says
/// where it came from. The recording itself keeps only the first reason.
#[derive(Debug, Default)]
pub(crate) struct LossCounts {
    /// Packets the receiver declared lost (`Lost`), after NACK and RTX.
    pub lost_packets: AtomicU64,
    /// Confirmed sequence restarts.
    pub restarts: AtomicU64,
    /// Opus timestamp jumps too big to fill, or backward.
    pub timestamp_jumps: AtomicU64,
    /// Video frames dropped because they lost data.
    pub incomplete_frames: AtomicU64,
    /// Video frames skipped while waiting for an IDR after loss.
    pub skipped_after_loss: AtomicU64,
    /// Payloads the depacketiser refused.
    pub refused: AtomicU64,
    /// Opus frames filled with silence for loss.
    pub loss_fills: AtomicU64,
    /// Media the session dropped (`Event::MediaDropped`).
    pub dropped: AtomicU64,
    /// Items the ingest queue had no room for.
    pub queue_full: AtomicU64,
    /// Ends that were not orderly.
    pub abnormal_ends: AtomicU64,
    /// Ingest updates in which the muxer itself reported media loss.
    pub muxer: AtomicU64,
    /// Timeline re-anchors with no loss of their own: not a mark, counted
    /// beside the marks.
    pub reanchors: AtomicU64,
    /// The last break the audio-only muxer reported, explained.
    pub muxer_break: Mutex<Option<String>>,
}

impl LossCounts {
    fn fields(&self) -> [(&'static str, u64); 12] {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        [
            ("lost_packets", g(&self.lost_packets)),
            ("restarts", g(&self.restarts)),
            ("timestamp_jumps", g(&self.timestamp_jumps)),
            ("incomplete_frames", g(&self.incomplete_frames)),
            ("skipped_after_loss", g(&self.skipped_after_loss)),
            ("refused", g(&self.refused)),
            ("loss_fills", g(&self.loss_fills)),
            ("dropped", g(&self.dropped)),
            ("queue_full", g(&self.queue_full)),
            ("abnormal_ends", g(&self.abnormal_ends)),
            ("muxer", g(&self.muxer)),
            ("reanchors", g(&self.reanchors)),
        ]
    }
}

impl std::fmt::Display for LossCounts {
    /// The non-zero counts, or `none`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut any = false;
        for (name, n) in self.fields() {
            if n > 0 {
                write!(f, "{}{name}={n}", if any { " " } else { "" })?;
                any = true;
            }
        }
        if !any {
            f.write_str("none")?;
        }
        let last = self.muxer_break.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(detail) = last.as_deref() {
            write!(f, "; last muxer break: {detail}")?;
        }
        Ok(())
    }
}

/// A receiver path that loses media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LossPath {
    Lost,
    Restart,
    TimestampJump,
    IncompleteFrame,
    SkippedAfterLoss,
    Refused,
    Dropped,
}

/// What the UDP owner keeps per session for its ingest task.
pub(crate) struct Feed {
    tx: mpsc::Sender<Item>,
    loss: PublicationLoss,
    /// Media bytes queued and not yet taken by the ingest task, which
    /// subtracts what it takes.
    queued: Arc<AtomicUsize>,
    /// The byte bound on `queued`.
    max_bytes: usize,
    /// SRTP and SRTCP bytes not yet passed on.
    received: usize,
    /// An access unit was dropped: skip video until the next IDR.
    skip_video: bool,
    /// A keyframe should be asked for, to end `skip_video` soon.
    keyframe: bool,
    /// Which paths marked the recording.
    counts: Arc<LossCounts>,
    /// The audio timeline's counters as the current event was taken.
    timeline: Timeline,
}

/// The ingest task has gone.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Gone;

impl Feed {
    pub(crate) fn new(tx: mpsc::Sender<Item>, loss: PublicationLoss) -> Self {
        Self::bounded(tx, loss, QUEUE_BYTES)
    }

    pub(crate) fn bounded(tx: mpsc::Sender<Item>, loss: PublicationLoss, max_bytes: usize) -> Self {
        Self {
            tx,
            loss,
            queued: Arc::new(AtomicUsize::new(0)),
            max_bytes,
            received: 0,
            skip_video: false,
            keyframe: false,
            counts: Arc::new(LossCounts::default()),
            timeline: Timeline::default(),
        }
    }

    /// The per-path loss counts, shared with the ingest task.
    pub(crate) fn counts(&self) -> Arc<LossCounts> {
        self.counts.clone()
    }

    /// The queued-bytes counter, for the ingest task.
    pub(crate) fn queued(&self) -> Arc<AtomicUsize> {
        self.queued.clone()
    }

    /// Packets the receiver gave up on or dropped so far.
    pub(crate) fn lost(&self) -> u64 {
        self.counts.lost_packets.load(Ordering::Relaxed)
            + self.counts.dropped.load(Ordering::Relaxed)
    }

    /// Whether video has just started waiting for an IDR, so a keyframe
    /// should be asked for now; reading it clears it.
    pub(crate) fn take_keyframe_request(&mut self) -> bool {
        std::mem::take(&mut self.keyframe)
    }

    /// The session ended. An end that was not orderly marks the
    /// recording: its last media may be missing with no later packet to
    /// show a gap. Loss just before an orderly end cannot be seen.
    pub(crate) fn ended(&mut self, reason: EndReason) {
        let cause = match reason {
            EndReason::ConsentExpired | EndReason::MediaTimeout => {
                IncompleteReason::TransportTimeout
            }
            EndReason::Dtls(_) | EndReason::KeysExhausted => IncompleteReason::Protocol,
            // Its state was dropped mid-call.
            EndReason::Panicked => IncompleteReason::IngestError,
            // Never connected, so nothing was recorded; or an orderly end:
            // DELETE, our close, or the publisher's close_notify.
            EndReason::HandshakeTimeout | EndReason::Closed | EndReason::PeerClosed => return,
        };
        self.counts.abnormal_ends.fetch_add(1, Ordering::Relaxed);
        self.loss.mark_incomplete(cause);
    }

    /// Passes one receiver event on. Loss is marked on the recording here.
    #[cfg(test)]
    pub(crate) fn offer(&mut self, event: sapflow_rtp::Event) -> Result<(), Gone> {
        self.push(media(event))
    }

    /// As [`Feed::offer`], with the audio timeline's counters as the event
    /// was taken, for the break diagnostic.
    pub(crate) fn offer_with(
        &mut self,
        event: sapflow_rtp::Event,
        timeline: Timeline,
    ) -> Result<(), Gone> {
        self.timeline = timeline;
        self.push(media(event))
    }

    fn mark_loss(&mut self, path: LossPath, packets: u64) {
        let c = &self.counts;
        let (counter, n) = match path {
            LossPath::Lost => (&c.lost_packets, packets),
            LossPath::Dropped => (&c.dropped, packets),
            LossPath::Restart => (&c.restarts, 1),
            LossPath::TimestampJump => (&c.timestamp_jumps, 1),
            LossPath::IncompleteFrame => (&c.incomplete_frames, 1),
            LossPath::SkippedAfterLoss => (&c.skipped_after_loss, 1),
            LossPath::Refused => (&c.refused, 1),
        };
        counter.fetch_add(n, Ordering::Relaxed);
        self.loss.mark_incomplete(IncompleteReason::MediaLoss);
    }

    /// Queues one media item for the ingest task.
    pub(crate) fn push(&mut self, media: Media) -> Result<(), Gone> {
        let item = match media {
            Media::Video { pts, au } => {
                if self.skip_video && !au.idr {
                    return Ok(());
                }
                self.skip_video = false;
                Item::Video {
                    pts,
                    au,
                    received: std::mem::take(&mut self.received),
                }
            }
            Media::Audio {
                pts,
                packet,
                lost,
                rtp,
                samples,
            } => {
                // Silence in place of lost packets: the timeline stays
                // whole, but the recording is not.
                if lost {
                    self.counts.loss_fills.fetch_add(1, Ordering::Relaxed);
                    self.loss.mark_incomplete(IncompleteReason::MediaLoss);
                }
                Item::Audio {
                    pts,
                    packet,
                    received: std::mem::take(&mut self.received),
                    trace: AudioTrace {
                        rtp,
                        samples,
                        timeline: self.timeline,
                    },
                }
            }
            Media::Loss { path, packets } => {
                self.mark_loss(path, packets);
                return Ok(());
            }
            Media::Other(name) => {
                tracing::debug!(event = name, "WHIP media event");
                return Ok(());
            }
            Media::Reanchor => {
                self.counts.reanchors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("WHIP timeline re-anchored with no loss");
                Item::Reanchor
            }
        };
        let size = item.size();
        let video = matches!(item, Item::Video { .. });
        let room = self
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |q| {
                q.checked_add(size).filter(|n| *n <= self.max_bytes)
            })
            .is_ok();
        let refused = if room {
            match self.tx.try_send(item) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(item)) => {
                    self.queued.fetch_sub(size, Ordering::AcqRel);
                    item
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(Gone),
            }
        } else {
            item
        };
        // Full, by count or by bytes: loss for this publisher, never
        // back-pressure on the socket.
        self.counts.queue_full.fetch_add(1, Ordering::Relaxed);
        self.loss.mark_incomplete(IncompleteReason::IngestQueueFull);
        if video && !self.skip_video {
            self.skip_video = true;
            self.keyframe = true;
        }
        // Keep the bytes for the rate cap.
        self.received += refused.received();
        Ok(())
    }
}

/// A receiver event, as the ingest takes it.
#[derive(Debug)]
pub(crate) enum Media {
    Video {
        pts: u64,
        au: AccessUnit,
    },
    Audio {
        pts: u64,
        packet: Vec<u8>,
        /// Silence the receiver filled in for lost packets.
        lost: bool,
        /// Its RTP timestamp and TOC duration (48 kHz), for diagnostics.
        rtp: u32,
        samples: u32,
    },
    /// Media lost, so the recording is incomplete: sequence numbers never
    /// received within the jitter wait, after NACK and RTX repair
    /// (`packets` of them); a video frame dropped because it lost data, or
    /// skipped while waiting for an IDR after loss; a confirmed sequence
    /// restart or an Opus timestamp jump too big to fill; a payload the
    /// depacketiser refused; and media the session dropped
    /// (`Event::MediaDropped`, `packets` of it). Loss is taken from the
    /// receiver, never the muxer: a lost Opus packet is filled with
    /// silence and the timeline can absorb its timestamp jump, so the
    /// muxer may see nothing missing.
    Loss {
        path: LossPath,
        packets: u64,
    },
    /// The timeline re-anchored on arrival time, or moved forward to a
    /// sender report: a jump in timestamps with no media missing (packets
    /// missing are reported on their own, and mark). Not a mark.
    Reanchor,
    /// Nothing for the ingest, by name: the start hold's alignment and
    /// frames skipped before the first IDR.
    Other(&'static str),
}

fn media(event: sapflow_rtp::Event) -> Media {
    use sapflow_rtp::Event as E;
    match event {
        E::AccessUnit { au, pts, .. } => Media::Video {
            pts,
            au: AccessUnit {
                data: au.data,
                idr: au.idr,
                sps: au.sps.into_iter().last(),
                pps: au.pps.into_iter().last(),
            },
        },
        E::Audio { frame, pts, .. } => Media::Audio {
            pts,
            lost: frame.kind == FrameKind::Loss,
            rtp: frame.timestamp,
            samples: frame.duration,
            packet: frame.data,
        },
        E::Lost { count, .. } => Media::Loss {
            path: LossPath::Lost,
            packets: count,
        },
        E::IncompleteFrame { .. } => loss(LossPath::IncompleteFrame),
        E::Refused { .. } => loss(LossPath::Refused),
        E::FrameSkipped {
            reason: SkipReason::AfterLoss,
            ..
        } => loss(LossPath::SkippedAfterLoss),
        E::Discontinuity {
            cause: Discontinuity::Sequence,
            ..
        } => loss(LossPath::Restart),
        E::Discontinuity {
            cause: Discontinuity::Timestamp { .. },
            ..
        } => loss(LossPath::TimestampJump),
        E::Discontinuity {
            cause: Discontinuity::Arrival { .. } | Discontinuity::SenderReport { .. },
            ..
        } => Media::Reanchor,
        other => Media::Other(event_name(&other)),
    }
}

fn loss(path: LossPath) -> Media {
    Media::Loss { path, packets: 0 }
}

/// A receiver event's name, for a log line that carries nothing else.
fn event_name(e: &sapflow_rtp::Event) -> &'static str {
    use sapflow_rtp::Event as E;
    match e {
        E::AccessUnit { .. } => "access unit",
        E::Audio { .. } => "audio",
        E::Aligned { .. } => "aligned",
        E::Lost { .. } => "lost",
        E::IncompleteFrame { .. } => "incomplete frame",
        E::FrameSkipped { .. } => "frame skipped",
        E::Discontinuity { .. } => "discontinuity",
        E::Refused { .. } => "refused payload",
    }
}

enum Command {
    Register {
        id: SessionId,
        session: Box<Session>,
        feed: Feed,
        reply: oneshot::Sender<Result<(), InsertError>>,
    },
    Close {
        id: SessionId,
    },
}

/// A live session's resource, for `DELETE`.
struct Resource {
    /// SHA-256 of the bearer token, which `DELETE` must repeat.
    token: [u8; 32],
    /// Which paths have marked its recording.
    losses: Arc<LossCounts>,
    /// Set once the session's publication has been released.
    released: watch::Receiver<bool>,
}

/// What the listener and the UDP owner share.
pub(crate) struct Whip {
    config: WhipConfig,
    identity: Arc<Identity>,
    commands: mpsc::Sender<Command>,
    resources: Mutex<HashMap<SessionId, Resource>>,
}

impl Whip {
    fn resources(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Resource>> {
        self.resources.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The two WHIP sockets, bound at start so a bad address stops the server.
pub(crate) struct Sockets {
    pub http: TcpListener,
    pub udp: UdpSocket,
}

impl Sockets {
    pub(crate) async fn bind(config: &WhipConfig) -> std::io::Result<Self> {
        Ok(Self {
            http: TcpListener::bind(config.addr).await?,
            udp: UdpSocket::bind(config.udp_addr).await?,
        })
    }
}

fn session_config(config: &WhipConfig) -> SessionConfig {
    SessionConfig {
        keyframe_interval: config.keyframe_interval,
        ..SessionConfig::default()
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Compares two digests without stopping at the first difference.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Serves WHIP until a socket fails, with one certificate for the
/// process.
pub(crate) async fn serve(app: Arc<App>, sockets: Sockets) -> std::io::Result<()> {
    let unix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let identity = Arc::new(
        Identity::generate(unix)
            .map_err(|_| std::io::Error::other("cannot make the WHIP certificate"))?,
    );
    serve_with(app, sockets, identity).await
}

async fn serve_with(
    app: Arc<App>,
    sockets: Sockets,
    identity: Arc<Identity>,
) -> std::io::Result<()> {
    match prepare(&app, identity)? {
        Some((whip, router, rx)) => run(app, whip, router, rx, sockets).await,
        None => std::future::pending().await,
    }
}

/// The shared state, the router and the owner's command queue, or `None`
/// with WHIP off.
type Prepared = (Arc<Whip>, Router, mpsc::Receiver<Command>);

fn prepare(app: &App, identity: Arc<Identity>) -> std::io::Result<Option<Prepared>> {
    let Some(config) = app.config.whip.clone() else {
        return Ok(None);
    };
    let router = Router::new(RouterConfig {
        max_sessions: app.config.max_publishers,
        ..RouterConfig::default()
    })
    .map_err(|_| std::io::Error::other("WHIP router settings"))?;
    let (commands, rx) = mpsc::channel(64);
    let whip = Arc::new(Whip {
        config,
        identity,
        commands,
        resources: Mutex::new(HashMap::new()),
    });
    Ok(Some((whip, router, rx)))
}

async fn run(
    app: Arc<App>,
    whip: Arc<Whip>,
    router: Router,
    rx: mpsc::Receiver<Command>,
    sockets: Sockets,
) -> std::io::Result<()> {
    tracing::info!(
        addr = %whip.config.addr,
        udp = %whip.config.udp_addr,
        "WHIP listening"
    );
    tokio::select! {
        r = own_udp(sockets.udp, router, rx) => r,
        r = listen(sockets.http, app, whip) => r,
    }
}

/// The UDP owner: the socket, the router, and each session's feed.
async fn own_udp(
    socket: UdpSocket,
    mut router: Router,
    mut commands: mpsc::Receiver<Command>,
) -> std::io::Result<()> {
    let mut feeds: HashMap<SessionId, Feed> = HashMap::new();
    // One datagram; the session takes at most 1500 bytes of it.
    let mut buf = vec![0u8; 65_536];
    let timer = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(timer);
    let mut armed = None;
    loop {
        let deadline = router.poll_timeout();
        if deadline != armed {
            if let Some(d) = deadline {
                timer.as_mut().reset(d.into());
            }
            armed = deadline;
        }
        tokio::select! {
            r = socket.recv_from(&mut buf) => match r {
                Ok((n, peer)) => {
                    if let Some(datagram) = buf.get(..n) {
                        receive(&mut router, &mut feeds, Tuple::udp(peer), datagram);
                    }
                }
                // As for SRT: one failed receive is not the end of WHIP, and
                // never of the server.
                Err(error) => tracing::debug!(%error, "WHIP receive failed"),
            },
            () = &mut timer, if armed.is_some() => {
                armed = None;
                router.handle_timeout(Instant::now());
            }
            Some(command) = commands.recv() => match command {
                Command::Register { id, session, feed, reply } => {
                    let result = router.insert(id, *session);
                    if result.is_ok() {
                        feeds.insert(id, feed);
                    }
                    let _ = reply.send(result);
                }
                Command::Close { id } => {
                    router.close(&id, Instant::now());
                }
            },
        }
        if router.poll_timeout().is_some_and(|d| d <= Instant::now()) {
            router.handle_timeout(Instant::now());
        }
        while let Some(t) = router.poll_transmit() {
            if socket.send_to(&t.bytes, t.tuple.remote).await.is_err() {
                tracing::debug!("WHIP send failed");
            }
        }
        let mut gone = Vec::new();
        let mut keyframes = Vec::new();
        while let Some((id, event)) = router.poll_event() {
            match event {
                Event::Media(e) => {
                    let timeline = audio_timeline(&router, &id);
                    if let Some(feed) = feeds.get_mut(&id) {
                        if feed.offer_with(e, timeline).is_err() {
                            gone.push(id);
                        } else if feed.take_keyframe_request() {
                            keyframes.push(id);
                        }
                    }
                }
                Event::MediaDropped { count } => {
                    if let Some(feed) = feeds.get_mut(&id) {
                        let _ = feed.push(Media::Loss {
                            path: LossPath::Dropped,
                            packets: count,
                        });
                    }
                }
                Event::Connected(profile) => {
                    tracing::info!(?profile, "WHIP session connected");
                }
                Event::Ended(reason) => {
                    // Dropping the feed ends the ingest task, which
                    // releases the publication.
                    // The ingest task may still add muxer reports after this.
                    if let Some(mut f) = feeds.remove(&id) {
                        f.ended(reason);
                        let (lost, losses) = (f.lost(), f.counts());
                        tracing::info!(?reason, lost, %losses, "WHIP session ended");
                    }
                }
                Event::Nominated => {}
            }
        }
        // Video waits for an IDR after a full queue: ask for one now rather
        // than at the next keyframe interval.
        for id in keyframes {
            router.request_keyframe(&id, Instant::now());
            while let Some(t) = router.poll_transmit() {
                if socket.send_to(&t.bytes, t.tuple.remote).await.is_err() {
                    tracing::debug!("WHIP send failed");
                }
            }
        }
        for id in gone {
            router.close(&id, Instant::now());
            feeds.remove(&id);
        }
    }
}

/// The session's audio timeline counters now, or zeros.
fn audio_timeline(router: &Router, id: &SessionId) -> Timeline {
    router
        .session(id)
        .and_then(|s| s.receiver().zip(s.stream(MediaKind::Audio)))
        .and_then(|(rx, stream)| rx.stats(stream))
        .map_or_else(Timeline::default, |st| Timeline {
            held: st.timeline_held,
            clamps: st.timeline_clamps,
            discontinuities: st.timeline_discontinuities,
        })
}

/// Explains a break the audio-only muxer reported at `pts`, against the
/// packet before it: the muxer expects each timestamp within 2 ms of the
/// previous one plus that packet's TOC duration.
fn explain_break(prev: (u64, AudioTrace), pts: u64, trace: AudioTrace) -> String {
    let (prev_pts, p) = prev;
    let expected = prev_pts.saturating_add(u64::from(p.samples) * 15 / 8);
    let step = i128::from(pts) - i128::from(expected);
    let delta = |now: u64, before: u64| now.saturating_sub(before);
    let held = delta(trace.timeline.held, p.timeline.held);
    let clamps = delta(trace.timeline.clamps, p.timeline.clamps);
    format!(
        "step {:+.2} ms (pts {pts}, expected {expected}); sender RTP step {} samples for a {}-sample packet; this packet {} samples; timeline since the last packet: held to the bound +{held}, continued by arrival +{}, discontinuities +{}",
        step as f64 / 90.0,
        trace.rtp.wrapping_sub(p.rtp),
        p.samples,
        trace.samples,
        clamps.saturating_sub(held),
        delta(trace.timeline.discontinuities, p.timeline.discontinuities),
    )
}

/// One datagram into the router. Only SRTP and SRTCP the session
/// authenticated count against its publisher's bitrate cap, so bytes
/// spoofed from its tuple cannot push it over.
fn receive(
    router: &mut Router,
    feeds: &mut HashMap<SessionId, Feed>,
    tuple: Tuple,
    datagram: &[u8],
) {
    let authenticated = |s: &Session| {
        let st = s.stats();
        st.srtp_packets + st.srtcp_packets
    };
    let before = matches!(classify(datagram), Kind::Rtp | Kind::Rtcp)
        .then(|| router.owner(&tuple))
        .flatten()
        .and_then(|id| router.session(&id).map(|s| (id, authenticated(s))));
    router.handle_datagram(Instant::now(), tuple, datagram);
    if let Some((id, count)) = before
        && router
            .session(&id)
            .is_some_and(|s| authenticated(s) > count)
        && let Some(feed) = feeds.get_mut(&id)
    {
        feed.received += datagram.len();
    }
}

async fn listen(listener: TcpListener, app: Arc<App>, whip: Arc<Whip>) -> std::io::Result<()> {
    let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS));
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            // Out of file descriptors, or a connection reset before it was
            // taken: not the end of WHIP, nor of the server.
            Err(error) => {
                tracing::warn!(%error, "WHIP accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let (app, whip) = (app.clone(), whip.clone());
        tokio::spawn(async move {
            let _slot = slot;
            connection(socket, peer, app, whip).await;
        });
    }
}

/// An answer: status, extra headers, content type and body.
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn refused(r: Refused) -> Self {
        let mut headers = Vec::new();
        if let Some(allow) = r.allow {
            headers.push(("allow", allow.to_owned()));
        }
        Self {
            status: r.status,
            headers,
            content_type: "text/plain; charset=utf-8",
            body: r.reason.as_bytes().to_vec(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut headers: Vec<(&str, &str)> = vec![
            ("cache-control", "no-store"),
            ("content-type", self.content_type),
        ];
        headers.extend(self.headers.iter().map(|(k, v)| (*k, v.as_str())));
        http_lite::response(self.status, &headers, &self.body)
    }
}

async fn connection(mut socket: TcpStream, peer: SocketAddr, app: Arc<App>, whip: Arc<Whip>) {
    let mut buf = Vec::with_capacity(2048);
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut socket, &mut buf)).await {
        Err(_) => Err(Refused::new(408, "read timeout")),
        Ok(Err(None)) => return,
        Ok(Err(Some(why))) => Err(match why {
            HeadRefusal::Malformed => Refused::new(400, "malformed request"),
            HeadRefusal::HeadTooLarge => Refused::new(431, "request head too large"),
            HeadRefusal::TargetTooLong => Refused::new(414, "request target too long"),
        }),
        Ok(Ok(parsed)) => Ok(parsed),
    };
    let reply = match head {
        Err(r) => Reply::refused(r),
        Ok((head, used)) => {
            let origin =
                whip_request::allowed_origin(head.origin.as_deref(), &whip.config.allow_origins)
                    .map(str::to_owned);
            let work = exchange(&mut socket, &buf[used..], &head, peer, &app, &whip);
            let mut reply = match tokio::time::timeout(HANDLER_TIMEOUT, work).await {
                Ok(Some(reply)) => reply,
                Ok(None) => return,
                Err(_) => Reply::refused(Refused::new(503, "request timeout")),
            };
            if let Some(origin) = origin {
                let preflight = head.method == "OPTIONS";
                reply
                    .headers
                    .extend(whip_request::cors_headers(&origin, preflight));
            }
            reply
        }
    };
    let sent = async {
        socket.write_all(&reply.bytes()).await?;
        socket.shutdown().await
    };
    if matches!(tokio::time::timeout(HEAD_TIMEOUT, sent).await, Ok(Ok(()))) {
        drain(&mut socket).await;
    }
}

/// After the answer, an offer still arriving is read and dropped for a
/// moment, so a refusal sent before the whole request came in reaches the
/// client rather than being lost to a TCP reset.
async fn drain(socket: &mut TcpStream) {
    let mut scratch = [0u8; 4096];
    let mut total = 0;
    let _ = tokio::time::timeout(Duration::from_millis(250), async {
        while total < 2 * whip_request::MAX_OFFER {
            match socket.read(&mut scratch).await {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
    })
    .await;
}

/// Reads until a whole head is in `buf`; `Err(None)` if the client left.
async fn read_head(
    socket: &mut TcpStream,
    buf: &mut Vec<u8>,
) -> Result<(http_lite::RequestHead, usize), Option<HeadRefusal>> {
    loop {
        match http_lite::parse_request(buf) {
            ParsedRequest::Complete(head, used) => return Ok((head, used)),
            ParsedRequest::Refused(why) => return Err(Some(why)),
            ParsedRequest::Partial => {}
        }
        let mut chunk = [0u8; 4096];
        let room = http_lite::MAX_HEAD
            .saturating_sub(buf.len())
            .min(chunk.len());
        match socket.read(&mut chunk[..room]).await {
            Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return Err(None),
        }
    }
}

/// The answer to one request; `None` if the client went away.
async fn exchange(
    socket: &mut TcpStream,
    already: &[u8],
    head: &http_lite::RequestHead,
    peer: SocketAddr,
    app: &Arc<App>,
    whip: &Arc<Whip>,
) -> Option<Reply> {
    let action = match whip_request::route(&head.method, &head.path) {
        Ok(action) => action,
        Err(r) => return Some(Reply::refused(r)),
    };
    let authorization = head
        .authorization
        .as_ref()
        .map(http_lite::Credential::value);
    match action {
        Action::Preflight => Some(Reply {
            status: 200,
            headers: vec![("accept-post", "application/sdp".to_owned())],
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
        }),
        Action::Delete(id) => Some(delete(whip, id, authorization).await),
        Action::Publish(path) => {
            let token = match whip_request::token(authorization) {
                Ok(token) => token,
                Err(r) => return Some(Reply::refused(r)),
            };
            let length = match whip_request::offer_length(head) {
                Ok(n) => n,
                Err(r) => return Some(Reply::refused(r)),
            };
            if head.expects_continue && length > already.len() {
                socket
                    .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                    .await
                    .ok()?;
            }
            let mut body = vec![0u8; length];
            let have = already.len().min(length);
            body[..have].copy_from_slice(&already[..have]);
            socket.read_exact(&mut body[have..]).await.ok()?;
            let ip = match whip_request::publisher_ip(
                peer.ip(),
                whip.config.trusted_proxy,
                head.forwarded_for.as_deref(),
            ) {
                Ok(ip) => ip,
                Err(r) => return Some(Reply::refused(r)),
            };
            let bearer = whip_request::bearer(authorization).unwrap_or_default();
            Some(
                publish(app, whip, path, token, bearer, &body, ip)
                    .await
                    .unwrap_or_else(Reply::refused),
            )
        }
    }
}

async fn delete(whip: &Arc<Whip>, id: &str, authorization: Option<&str>) -> Reply {
    let not_found = || Reply::refused(Refused::new(404, "no such session"));
    let Some(id) = SessionId::parse(id) else {
        return not_found();
    };
    let Some(bearer) = whip_request::bearer(authorization) else {
        return Reply::refused(resource::Refusal::Unauthorised.into());
    };
    let found = whip
        .resources()
        .get(&id)
        .map(|r| (r.token, r.released.clone(), r.losses.clone()));
    let Some((token, mut released, losses)) = found else {
        return not_found();
    };
    if !same(&token, &digest(bearer)) {
        return Reply::refused(resource::Refusal::Unauthorised.into());
    }
    if whip.commands.send(Command::Close { id }).await.is_err() {
        return Reply::refused(resource::Refusal::Unavailable.into());
    }
    // Answer once the path is released, so a POST that follows at once is
    // not refused 409 by the publication on its way out.
    let _ = tokio::time::timeout(RELEASE_TIMEOUT, released.wait_for(|r| *r)).await;
    tracing::debug!(%losses, "WHIP session deleted");
    Reply {
        status: 200,
        headers: Vec::new(),
        content_type: "text/plain; charset=utf-8",
        body: Vec::new(),
    }
}

fn admission_refusal(r: Refusal) -> Refused {
    let r = match r {
        Refusal::Request(PublishError::BadPath | PublishError::BadRequest) => {
            return Refused::new(404, "no such stream path");
        }
        Refusal::Request(PublishError::Unauthorized) => resource::Refusal::Unauthorised,
        Refusal::AlreadyLive => resource::Refusal::Conflict,
        Refusal::Throttled => resource::Refusal::Throttled,
        Refusal::PublisherLimit | Refusal::PresentationLimit | Refusal::AuthUnavailable => {
            resource::Refusal::Unavailable
        }
    };
    r.into()
}

async fn publish(
    app: &Arc<App>,
    whip: &Arc<Whip>,
    path: &str,
    token: resource::Token<'_>,
    bearer: &str,
    body: &[u8],
    ip: IpAddr,
) -> Result<Reply, Refused> {
    let offer = Offer::parse(body).map_err(|e| Refused::from(resource::Refusal::Malformed(e)))?;
    let unavailable = || Refused::from(resource::Refusal::Unavailable);
    let credentials = Credentials::generate().map_err(|_| unavailable())?;
    let id = SessionId::generate().map_err(|_| unavailable())?;
    let origin = id
        .as_bytes()
        .iter()
        .take(8)
        .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
        >> 1;
    let (answer, negotiated) = sdp::answer(
        &offer,
        &AnswerConfig {
            credentials: &credentials,
            fingerprint: whip.identity.fingerprint(),
            candidates: &whip.config.candidates,
            opus_max_bitrate: whip.config.opus_max_bitrate,
            origin,
        },
    )
    .map_err(|e| Refused::from(resource::Refusal::OutsideProfile(e)))?;
    let stream = StreamPath::parse(path).ok_or(Refused::new(404, "no such stream path"))?;
    let req = PublishRequest {
        path: stream,
        user: (!token.user.is_empty()).then(|| token.user.to_owned()),
        pass: Some(token.password.to_owned()),
        connection_attempt_id: None,
    };
    let publication = Publication::begin_authorised(app, &req, SourceKind::Es, ip)
        .await
        .map_err(|r| {
            app.log_refusal("webrtc", &r);
            admission_refusal(r)
        })?;
    let setup = Setup {
        video: negotiated.video.is_some(),
        audio: negotiated.audio.is_some(),
    };
    let session = Session::new(
        session_config(&whip.config),
        whip.identity.clone(),
        credentials,
        negotiated,
        Instant::now(),
    )
    .map_err(|_| unavailable())?;
    // The ingest task owns the publication from here, so a cancelled
    // request leaves no session without one.
    let (tx, rx) = mpsc::channel(QUEUE);
    let (released_tx, released) = watch::channel(false);
    let feed = Feed::new(tx, publication.loss_notice());
    let (queued, counts) = (feed.queued(), feed.counts());
    whip.resources().insert(
        id,
        Resource {
            token: digest(bearer),
            losses: counts.clone(),
            released,
        },
    );
    tokio::spawn(ingest(
        whip.clone(),
        id,
        publication,
        (rx, queued, counts),
        released_tx,
        setup,
    ));
    let (reply, registered) = oneshot::channel();
    whip.commands
        .send(Command::Register {
            id,
            session: Box::new(session),
            feed,
            reply,
        })
        .await
        .map_err(|_| unavailable())?;
    match registered.await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return Err(unavailable()),
    }
    Ok(Reply {
        status: 201,
        headers: vec![("location", whip.config.public_url.location(&id))],
        content_type: "application/sdp",
        body: answer.into_bytes(),
    })
}

/// Which tracks the answer accepted.
#[derive(Debug, Clone, Copy)]
struct Setup {
    video: bool,
    audio: bool,
}

/// Configures the ingest for the negotiated tracks: stereo Opus (the
/// answer says `stereo=1`), and whether video waits for audio.
fn configure(source: &mut Source, setup: Setup, now: SystemTime) -> Result<Update, IngestError> {
    let Source::Es(es) = source else {
        return Err(IngestError::TrackConflict);
    };
    if setup.audio {
        es.opus_config(OpusConfig::STEREO)?;
    }
    match (setup.video, setup.audio) {
        (true, audio) => {
            es.set_expect_audio(audio);
            Ok(Update::default())
        }
        (false, true) => es.declare_tracks(TrackSet::AudioOnly, now),
        (false, false) => Err(IngestError::InvalidTracks),
    }
}

fn feed_item(source: &mut Source, item: Item, now: SystemTime) -> Result<Update, IngestError> {
    let Source::Es(es) = source else {
        return Err(IngestError::TrackConflict);
    };
    match item {
        Item::Video { pts, au, .. } => {
            let t = i64::try_from(pts).map_err(|_| IngestError::InvalidTimestamp)?;
            es.video(t, t, au, now)
        }
        Item::Audio { pts, packet, .. } => {
            let t = i64::try_from(pts).map_err(|_| IngestError::InvalidTimestamp)?;
            es.opus_at(t, &packet, now)
        }
        Item::Reanchor => Ok(Update::default()),
    }
}

/// A timeline re-anchor with no loss, for an audio-only stream: end the
/// run here, as a reconnect does (`Muxer::interrupt`), so the jumped
/// packet starts a new one after an HLS discontinuity instead of reading
/// to the muxer as a break, which it would report as loss. Live latency
/// stays bounded either way. A video-led stream is left alone: its muxer
/// takes a timestamp jump without reporting loss, and an interrupt would
/// drop its pending frame and wait for a keyframe.
fn reanchor(source: &mut Source, setup: Setup, now: SystemTime) -> Result<Update, IngestError> {
    let Source::Es(es) = source else {
        return Err(IngestError::TrackConflict);
    };
    if setup.video {
        return Ok(Update::default());
    }
    Ok(es
        .muxer_mut()
        .map_or_else(Update::default, |m| m.interrupt(now)))
}

/// One session's ingest: owns its publication, and releases it when the
/// session ends or the media is refused.
async fn ingest(
    whip: Arc<Whip>,
    id: SessionId,
    mut publication: Publication,
    (mut rx, queued, counts): (mpsc::Receiver<Item>, Arc<AtomicUsize>, Arc<LossCounts>),
    released: watch::Sender<bool>,
    setup: Setup,
) {
    if let Err(error) = publication.push(|source, now| configure(source, setup, now)) {
        tracing::warn!(%error, "WHIP ingest setup refused");
    } else {
        let mut last_audio: Option<(u64, AudioTrace)> = None;
        while let Some(item) = rx.recv().await {
            queued.fetch_sub(item.size(), Ordering::AcqRel);
            let received = item.received();
            if let Err(error) = publication.received(received) {
                tracing::warn!(%error, "dropping WHIP publisher");
                break;
            }
            let audio = item.audio_trace();
            let mut broke = false;
            let pushed = publication.push(|source, now| {
                let update = match item {
                    Item::Reanchor => reanchor(source, setup, now)?,
                    item => feed_item(source, item, now)?,
                };
                if update.media_loss {
                    counts.muxer.fetch_add(1, Ordering::Relaxed);
                    broke = true;
                }
                Ok(update)
            });
            if let Err(error) = pushed {
                tracing::warn!(%error, "dropping WHIP publisher");
                break;
            }
            if let Some((pts, trace)) = audio {
                if broke && let Some(before) = last_audio {
                    let detail = explain_break(before, pts, trace);
                    tracing::debug!(%detail, "WHIP audio muxer reported a break");
                    *counts.muxer_break.lock().unwrap_or_else(|e| e.into_inner()) = Some(detail);
                }
                last_audio = Some((pts, trace));
            }
        }
    }
    rx.close();
    publication.end();
    whip.resources().remove(&id);
    let _ = whip.commands.send(Command::Close { id }).await;
    let _ = released.send(true);
}

#[cfg(test)]
#[path = "whip_tests.rs"]
mod tests;
