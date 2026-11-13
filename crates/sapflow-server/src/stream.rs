//! Live streams: one per publishing encoder, shared by ingest and HTTP.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sapflow_core::StreamPath;
use sapflow_core::limits::{PublicationPermit, PublicationQuota};
use sapflow_core::residency::{PresentationPermit, PresentationQuota, default_presentation_limit};
use sapflow_hls::muxer::Part;
use sapflow_hls::{
    EsIngest, FlvIngest, IngestError, Muxer, MuxerConfig, PartLookup, TsIngest, Update, playlist,
};

use crate::callback::Callbacks;
use crate::recorder::{IncompleteReason, Recorder, RecorderFactory};
use crate::roll_budget::{Roll, RollBudget, RollLimit};
use tokio::sync::watch;

pub struct LiveStream {
    pub path: StreamPath,
    cfg: MuxerConfig,
    state: Mutex<State>,
    session: Mutex<Session>,
    version: watch::Sender<u64>,
    fragments: Budget,
    /// When the stream was first claimed, for [`STARTING_FOR`].
    claimed: Instant,
    // Drop after cached state and session: reservation follows the final stream
    // owner, including publications, grace tasks and parked viewer requests.
    _presentation_permit: PresentationPermit,
}

/// Frozen nonempty track configs for one recorder initialization epoch. The
/// audio track is AAC or Opus.
#[derive(Clone)]
pub struct Tracks {
    video: Option<sapflow_hls::codec::H264Config>,
    audio: Option<sapflow_hls::codec::AudioConfig>,
}

impl Tracks {
    /// AAC-only constructor kept for the existing recorder tests.
    #[cfg(test)]
    pub fn new(
        video: Option<sapflow_hls::codec::H264Config>,
        audio: Option<sapflow_hls::codec::AacConfig>,
    ) -> Option<Self> {
        Self::with_audio(video, audio.map(sapflow_hls::codec::AudioConfig::Aac))
    }

    pub fn with_audio(
        video: Option<sapflow_hls::codec::H264Config>,
        audio: Option<sapflow_hls::codec::AudioConfig>,
    ) -> Option<Self> {
        (video.is_some() || audio.is_some()).then_some(Self { video, audio })
    }

    pub fn video(&self) -> Option<&sapflow_hls::codec::H264Config> {
        self.video.as_ref()
    }

    /// The AAC track, for the existing recorder tests; `None` for Opus.
    #[cfg(test)]
    pub fn audio(&self) -> Option<&sapflow_hls::codec::AacConfig> {
        self.audio
            .as_ref()
            .and_then(sapflow_hls::codec::AudioConfig::aac)
    }

    pub fn audio_config(&self) -> Option<&sapflow_hls::codec::AudioConfig> {
        self.audio.as_ref()
    }

    /// The audio track's timescale, which recording rebases its fragments in.
    pub fn audio_rate(&self) -> Option<u32> {
        self.audio
            .as_ref()
            .map(sapflow_hls::codec::AudioConfig::sample_rate)
    }

    /// The recording init segment for these tracks.
    pub fn recording_init(&self, duration: Duration) -> Option<Bytes> {
        sapflow_hls::fmp4::init_segment_for_audio_tracks(
            self.video(),
            self.audio_config(),
            duration,
        )
    }
}

/// Where the media comes from.
pub enum Source {
    /// SRT carries MPEG-TS.
    Ts(Box<TsIngest>),
    /// RTMP carries FLV tags.
    Flv(Box<FlvIngest>),
    /// WHIP carries H.264 access units and Opus packets, already
    /// depacketised by `sapflow-whip`.
    Es(Box<EsIngest>),
}

#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    Ts,
    Flv,
    Es,
}

impl Source {
    fn new(kind: SourceKind, cfg: MuxerConfig, budget: &Budget) -> Self {
        let (live, kept) = (budget.fragments.clone(), budget.retained.clone());
        match kind {
            SourceKind::Ts => Self::Ts(Box::new(TsIngest::with_fragment_budget(cfg, live, kept))),
            SourceKind::Flv => {
                Self::Flv(Box::new(FlvIngest::with_fragment_budget(cfg, live, kept)))
            }
            SourceKind::Es => Self::Es(Box::new(EsIngest::with_fragment_budget(cfg, live, kept))),
        }
    }

    /// A new connection's ingest, carrying on `muxer` if there is one.
    fn resume(kind: SourceKind, cfg: MuxerConfig, muxer: Option<Muxer>, budget: &Budget) -> Self {
        match (kind, muxer) {
            (_, None) => Self::new(kind, cfg, budget),
            (SourceKind::Ts, Some(m)) => Self::Ts(Box::new(TsIngest::resume(cfg, m))),
            (SourceKind::Flv, Some(m)) => Self::Flv(Box::new(FlvIngest::resume(cfg, m))),
            (SourceKind::Es, Some(m)) => Self::Es(Box::new(EsIngest::resume(cfg, m))),
        }
    }

    pub fn dropped_packets(&self) -> u64 {
        match self {
            Self::Ts(ingest) => ingest.dropped_packets(),
            Self::Flv(_) | Self::Es(_) => 0,
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        match self {
            Self::Ts(t) => t.muxer(),
            Self::Flv(f) => f.muxer(),
            Self::Es(e) => e.muxer(),
        }
    }

    fn take_muxer(&mut self) -> Option<Muxer> {
        match self {
            Self::Ts(t) => t.take_muxer(),
            Self::Flv(f) => f.take_muxer(),
            Self::Es(e) => e.take_muxer(),
        }
    }

    /// Drain only input already accepted by this connection. This is not a
    /// media admission path and must also run after a scoped gate is revoked.
    /// `clean_close` lets TS trim a final audio PES the sender cut short.
    fn finish(&mut self, now: SystemTime, clean_close: bool) -> Result<Update, IngestError> {
        match self {
            Self::Ts(ts) if clean_close => ts.finish_after_clean_close(now),
            Self::Ts(ts) => ts.finish(now),
            Self::Flv(flv) => Ok(flv
                .muxer_mut()
                .map_or_else(Update::default, |m| m.interrupt(now))),
            Self::Es(es) => Ok(es
                .muxer_mut()
                .map_or_else(Update::default, |m| m.interrupt(now))),
        }
    }

    fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        match self {
            Self::Ts(t) => t.muxer_mut(),
            Self::Flv(f) => f.muxer_mut(),
            Self::Es(e) => e.muxer_mut(),
        }
    }

    fn trimmed_audio_tail(&self) -> bool {
        matches!(self, Self::Ts(ts) if ts.trimmed_audio_tail())
    }
}

pub struct State {
    pub ingest: Source,
    /// Chunk-name prefix, unique per presentation so a new
    /// stream never collides with chunks a cache still holds from the last.
    pub prefix: String,
    pub closed: bool,
    pub media: Option<Bytes>,
    /// Rendered only on demand, then shared until the next media update.
    conventional_media: Option<Bytes>,
    pub multivariant: Option<Bytes>,
    /// The tracks of the current codec epoch and the one before, as the
    /// muxer had them after a push, so an epoch's recording can still be
    /// started once the muxer has moved on ([`Stream::feed_outgoing`]).
    epoch_tracks: Vec<(u64, Tracks)>,
}

/// Who owns the stream: the connected publisher, or nobody while it is
/// held for a reconnect. Locked before `state` when both are needed.
pub struct Session {
    /// Bumped by each publisher that attaches, so a late detach or expiry
    /// from an earlier one changes nothing.
    generation: u64,
    attached: bool,
    /// Credit belongs to this exact attached RTMP generation, not its TCP task.
    publication_permit: Option<PublicationPermit>,
    /// All completed parts below this floor belong to the previous connection.
    /// detach seals its pending run before claim can advance this floor.
    fresh_part_floor: u64,
    /// Kept across a reconnect: one recording per show.
    pub recorder: Option<Recorder>,
    /// The recording of the codec epoch before the current one, with that
    /// epoch's id, while audio stamped before the cut may still arrive and
    /// be dropped ([`LiveStream::roll_recording`]). Finished after.
    outgoing: Option<(u64, Recorder)>,
    /// How often the recording may still roll to a new file at a codec
    /// epoch ([`LiveStream::roll_recording`]). Kept across reconnects, like
    /// the recording; set from the configuration when the stream is claimed
    /// fresh.
    pub rolls: RollBudget,
    pub recorder_factory: Option<RecorderFactory>,
    outage_deadline: Option<Instant>,
    /// The generation whose transport reported an orderly, lossless close.
    clean_close: Option<u64>,
    /// The `unpublish` owed to the app by the generation that sent `publish`,
    /// with that publication's callbacks. Whoever ends the publication first
    /// takes it, its `Publication` or a stop, so it is sent once. After a
    /// takeover it stays the replaced generation's until the new one
    /// publishes.
    unpublish: Option<(u64, Callbacks)>,
}

impl Session {
    /// Whether an outgoing recording is held for its late audio.
    #[cfg(test)]
    pub(crate) fn holds_outgoing(&self) -> bool {
        self.outgoing.is_some()
    }

    /// The current generation, and whether a publisher is attached to it.
    #[cfg(test)]
    pub(crate) fn owner(&self) -> (u64, bool) {
        (self.generation, self.attached)
    }

    pub fn owns_connection(&self, generation: u64) -> bool {
        self.generation == generation && self.attached
    }

    /// Ignore delayed notices from an older connection after replacement.
    pub fn mark_recording_incomplete(&self, generation: u64, reason: IncompleteReason) {
        if self.generation == generation
            && let Some(recorder) = &self.recorder
        {
            recorder.mark_incomplete(reason);
        }
    }

    /// The transport of `generation` closed in order with nothing dropped.
    /// This marks nothing complete by itself: it only lets the terminal drain
    /// trim a final audio PES that the sender cut short.
    pub fn mark_clean_close(&mut self, generation: u64) {
        if self.generation == generation {
            self.clean_close = Some(generation);
        }
    }

    pub fn closed_cleanly(&self) -> bool {
        self.clean_close == Some(self.generation)
    }

    pub fn hold_remaining(&mut self, grace: Duration, now: Instant) -> Duration {
        if grace.is_zero() {
            return Duration::ZERO;
        }
        let deadline = *self.outage_deadline.get_or_insert(now + grace);
        deadline.saturating_duration_since(now)
    }

    pub fn recovered(&mut self) {
        self.outage_deadline = None;
    }

    /// `generation` sent `publish`, so it owes the app an `unpublish`.
    pub fn owe_unpublish(&mut self, generation: u64, callbacks: Callbacks) {
        self.unpublish = Some((generation, callbacks));
    }

    /// The `unpublish` `generation` owes, if nothing has sent it yet. A newer
    /// generation that has published since owns the stage instead.
    pub fn take_unpublish(&mut self, generation: u64) -> Option<Callbacks> {
        match &self.unpublish {
            Some((owner, _)) if *owner == generation => self.unpublish.take().map(|(_, c)| c),
            _ => None,
        }
    }
}

/// Encoded availability only: neither fetching nor decoding has occurred.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshMediaPart {
    pub part_id: u64,
    pub initialization_id: u64,
    pub init_path: String,
    pub part_path: String,
    pub has_video: bool,
    pub has_audio: bool,
}

/// One matching attached connection, with optional fresh encoded availability.
pub struct ConnectionMediaSnapshot {
    pub fresh_media: Option<FreshMediaPart>,
}

/// What a push did.
#[derive(Debug, Clone, Copy, Default)]
pub struct Applied {
    /// The stream is servable.
    pub ready: bool,
    /// A new codec initialization epoch: the HLS timeline continues, and a
    /// new recording begins at the epoch's first part.
    pub restarted: bool,
    /// Accepted media crossed a definite input loss or repetition. This must
    /// taint the matching recording before any newly sealed parts are fed.
    pub media_loss: bool,
}

fn fresh_prefix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    prefix_from_nanos(nanos)
}

/// Twelve lowercase hex digits from the clock's low 48 bits. Zero-padded:
/// for about five hours in every 78 the leading digit is zero, and players
/// that accept exactly twelve digits (the TV player and receiver) refused
/// every chunk of a stream started then.
fn prefix_from_nanos(nanos: u128) -> String {
    format!("{:012x}", nanos & 0xffff_ffff_ffff)
}

impl State {
    pub(crate) fn conventional_playlist(&mut self) -> Option<Bytes> {
        if self.conventional_media.is_none() {
            let muxer = self.ingest.muxer()?;
            self.conventional_media = Some(Bytes::from(playlist::media_with_mode(
                muxer,
                &self.prefix,
                false,
            )));
        }
        self.conventional_media.clone()
    }
}

impl LiveStream {
    fn new(
        path: StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
        publication_permit: Option<PublicationPermit>,
        presentation_permit: PresentationPermit,
        fragments: Budget,
    ) -> Self {
        Self {
            path,
            cfg,
            state: Mutex::new(State {
                ingest: Source::new(kind, cfg, &fragments),
                prefix: fresh_prefix(),
                closed: false,
                media: None,
                conventional_media: None,
                multivariant: None,
                epoch_tracks: Vec::new(),
            }),
            session: Mutex::new(Session {
                generation: 0,
                attached: true,
                publication_permit,
                fresh_part_floor: 0,
                recorder: None,
                outgoing: None,
                rolls: RollBudget::new(RollLimit {
                    burst: 0,
                    ..RollLimit::default()
                }),
                recorder_factory: None,
                outage_deadline: None,
                clean_close: None,
                unpublish: None,
            }),
            version: watch::Sender::new(0),
            fragments,
            claimed: Instant::now(),
            _presentation_permit: presentation_permit,
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        // Release builds unwind (panic = "unwind"): a panic while holding
        // the lock ends only the task it happened in, and the next holder
        // takes the state as it was left.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Snapshot one independently decodable completed part from this exact
    /// connection's run. Retained old media and pending first IDRs do not qualify.
    pub fn connection_media(&self, generation: u64) -> Option<ConnectionMediaSnapshot> {
        let session = self.session();
        if session.generation != generation || !session.attached {
            return None;
        }
        let state = self.lock();
        if state.closed {
            return None;
        }
        Some(ConnectionMediaSnapshot {
            fresh_media: self.fresh_media_from_state(&session, &state),
        })
    }

    pub fn fresh_media_locked(&self, session: &Session, generation: u64) -> Option<FreshMediaPart> {
        if session.generation != generation || !session.attached {
            return None;
        }
        let state = self.lock();
        if state.closed {
            return None;
        }
        self.fresh_media_from_state(session, &state)
    }

    fn fresh_media_from_state(&self, session: &Session, state: &State) -> Option<FreshMediaPart> {
        let muxer = state.ingest.muxer()?;
        let init_id = muxer.current_initialization_id();
        const JS_MAX: u64 = (1 << 53) - 1;
        if init_id > JS_MAX {
            return None;
        }
        // Search newest first, stopping at the retained window. No media Arc is
        // stored in a receipt/owner, so status cannot extend media retention.
        for id in (session.fresh_part_floor..muxer.next_part_id()).rev() {
            let part = match muxer.part(id) {
                PartLookup::Found(part) => part,
                _ => break,
            };
            if part.initialization.id != init_id {
                break;
            }
            if part.independent && id <= JS_MAX {
                let root = self.path.as_path();
                return Some(FreshMediaPart {
                    part_id: id,
                    initialization_id: init_id,
                    init_path: format!(
                        "/{root}/{}",
                        playlist::epoch_init_name(&state.prefix, init_id)
                    ),
                    part_path: format!("/{root}/{}", playlist::part_name(&state.prefix, id)),
                    has_video: muxer.video().is_some(),
                    has_audio: muxer.audio_config().is_some(),
                });
            }
        }
        None
    }

    /// A counter that rises on every change a viewer could be waiting for.
    pub fn version(&self) -> u64 {
        *self.version.borrow()
    }

    /// Subscribe before checking state, then wait on `changed()`.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    /// Whether anything has been served (so a reconnect is worth waiting for).
    pub fn has_content(&self) -> bool {
        self.lock().ingest.muxer().is_some_and(|m| m.has_content())
    }

    /// Feeds media through `f`.
    pub fn apply(
        &self,
        f: impl FnOnce(&mut Source, SystemTime) -> Result<Update, IngestError>,
    ) -> Result<Applied, IngestError> {
        let mut st = self.lock();
        if st.closed {
            return Err(IngestError::PresentationExpired);
        }
        let update = match f(&mut st.ingest, SystemTime::now()) {
            Ok(update) => {
                Self::note_epoch(&mut st);
                update
            }
            Err(error) => {
                // A rejected input can still retire accepted muxer state.
                // Conventional requests previously rendered that state live.
                st.conventional_media = None;
                return Err(error);
            }
        };
        if !update.any() {
            return Ok(Applied::default());
        }
        // Codec epochs retain this presentation's prefix and all media IDs.
        // Per-segment immutable initialization identities select each MAP.
        let ready = Self::render(&mut st);
        drop(st);
        self.version.send_modify(|v| *v += 1);
        crate::own_http::notify();
        Ok(Applied {
            ready,
            restarted: update.restarted,
            media_loss: update.media_loss,
        })
    }

    /// Remembers the current epoch's tracks (and keeps the one before).
    fn note_epoch(st: &mut State) {
        let Some(m) = st.ingest.muxer() else {
            return;
        };
        let id = m.current_initialization_id();
        let Some(tracks) = Tracks::with_audio(m.video().cloned(), m.audio_config().copied()) else {
            return;
        };
        match st.epoch_tracks.last_mut() {
            Some((last, current)) if *last == id => *current = tracks,
            _ => {
                if st.epoch_tracks.len() >= 2 {
                    st.epoch_tracks.remove(0);
                }
                st.epoch_tracks.push((id, tracks));
            }
        }
    }

    fn render(st: &mut State) -> bool {
        st.conventional_media = None;
        let rendered = st.ingest.muxer().filter(|m| m.has_content()).map(|m| {
            (
                Bytes::from(playlist::media(m, &st.prefix)),
                Bytes::from(playlist::multivariant(m)),
            )
        });
        let ready = rendered.is_some();
        if let Some((media, mv)) = rendered {
            st.media = Some(media);
            st.multivariant = Some(mv);
        }
        ready
    }

    /// Finalise only this session's accepted parser input, while its caller
    /// holds the session (and registry) lock. No successor can change source or
    /// recording policy before loss attribution, codec rollover and tail feed.
    fn finish_connection(&self, session: &mut Session) {
        let result = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            // Repeated finishes return early; log the trim only once.
            let trimmed_before = state.ingest.trimmed_audio_tail();
            let result = state
                .ingest
                .finish(SystemTime::now(), session.closed_cleanly());
            Self::note_epoch(&mut state);
            if !trimmed_before && state.ingest.trimmed_audio_tail() {
                tracing::info!(
                    path = %self.path.as_path(),
                    "trimmed a final audio packet the sender cut short at a clean close"
                );
            }
            // finish can seal an accepted prefix before returning a semantic
            // error. Its immutable media still needs rendering and feeding.
            Self::render(&mut state);
            result
        };
        let loss = match &result {
            Ok(update) => update.media_loss,
            Err(_) => true,
        };
        if loss {
            session.mark_recording_incomplete(session.generation, IncompleteReason::EndOfInput);
        }
        let restarted = result.is_ok_and(|update| update.restarted);
        let unrecorded = self.roll_recording(session, restarted);
        if restarted {
            if unrecorded {
                session.mark_recording_incomplete(
                    session.generation,
                    IncompleteReason::EpochUnrecorded,
                );
            }
            if loss {
                session.mark_recording_incomplete(session.generation, IncompleteReason::EndOfInput);
            }
        }
        if let Some(recorder) = &mut session.recorder
            && recorder.is_accepting()
            && let Some((tracks, parts)) = self.parts_after(recorder.last_part())
        {
            // Recorder additionally pins the immutable init identity. A
            // failed epoch change cannot append AAC under an old video init.
            recorder.feed(tracks, parts);
        }
        self.version.send_modify(|v| *v += 1);
        crate::own_http::notify();
    }

    /// After every push or finish, with the session held: rolls the
    /// recording to a new codec epoch if the push began one (`restarted`),
    /// and settles late audio. Returns whether the old epoch's served media
    /// went unrecorded ([`LiveStream::feed_outgoing`]); the caller marks the
    /// new recording for that, and for any loss the push reported.
    ///
    /// - On a roll, the old recording is fed the rest of its epoch and a new
    ///   one starts.
    /// - Audio stamped before a cut's keyframe can arrive after the cut
    ///   (FFmpeg's TS muxing leaves it up to 200 ms behind its video in
    ///   file order). What the muxer had to drop of it is the one loss
    ///   marked by its timestamp, not its place in stream order: it is
    ///   charged to the epoch the cut ended ([`Muxer::take_late_audio_loss`])
    ///   and marks that epoch's recording. With audio more than a whole
    ///   epoch behind, the later epoch can be charged with an earlier one's
    ///   audio; the earlier file is then marked as well, as its window
    ///   closed short.
    /// - So a recording whose epoch may still lose late audio
    ///   ([`Muxer::late_audio_epoch`]) is held, not finished, until that
    ///   window closes: at the latest two seconds of video after the cut,
    ///   or at the next cut or the end of the connection.
    pub fn roll_recording(&self, session: &mut Session, restarted: bool) -> bool {
        let (lost, open, now) = {
            let mut st = self.lock();
            st.ingest.muxer_mut().map_or_else(Default::default, |m| {
                (
                    m.take_late_audio_loss(),
                    m.late_audio_epoch(),
                    m.published_media(),
                )
            })
        };
        if let Some((epoch, held)) = &session.outgoing
            && lost.contains(epoch)
        {
            held.mark_incomplete(IncompleteReason::MediaLoss);
        }
        if session
            .outgoing
            .as_ref()
            .is_some_and(|(epoch, _)| open != Some(*epoch))
        {
            // Finished: nothing more of its epoch can come.
            session.outgoing = None;
        }
        if !restarted {
            self.resume_recording(session, now);
            return false;
        }
        let mut outgoing = session.recorder.take();
        let unrecorded = outgoing
            .as_mut()
            .is_some_and(|recorder| !self.feed_outgoing(recorder));
        // A roll from a recording that never started (an epoch never
        // served, as at a start-up change while part timing is learnt)
        // makes no more files: it spends nothing.
        let started = outgoing
            .as_ref()
            .is_some_and(|recorder| recorder.last_part().is_some());
        let roll = if session.recorder_factory.is_some() {
            session.rolls.epoch(now, started)
        } else {
            Roll::Record
        };
        let epoch = outgoing
            .as_ref()
            .and_then(|recorder| self.recorded_epoch(recorder));
        if let (Some(recorder), Some(epoch)) = (&outgoing, epoch)
            && lost.contains(&epoch)
        {
            recorder.mark_incomplete(IncompleteReason::MediaLoss);
        }
        session.recorder = match roll {
            Roll::Record => session
                .recorder_factory
                .as_ref()
                .map(RecorderFactory::start),
            Roll::RanOut => {
                // The file that ran out: epochs after it are not recorded.
                if let Some(recorder) = &outgoing {
                    recorder.mark_incomplete(IncompleteReason::EpochRate);
                }
                tracing::warn!(
                    path = %self.path.as_path(),
                    "the encoder changes its codec configuration too often: codec epochs are served but not recorded until one lasts {} s",
                    session.rolls.settle().as_secs()
                );
                None
            }
            Roll::Skip => None,
        };
        if let (Some(recorder), Some(epoch)) = (outgoing, epoch)
            && open == Some(epoch)
        {
            session.outgoing = Some((epoch, recorder));
        }
        unrecorded
    }

    /// While the recording's budget is spent: once the current epoch has
    /// lasted the settle time and a roll is back, start its file from the
    /// epoch's first retained part, marked `epoch-rate` (codec epochs
    /// before it were served but not recorded). A file must begin with an
    /// independent part, so leading parts that are not are skipped, and
    /// with none yet the resume waits.
    fn resume_recording(&self, session: &mut Session, now: Duration) {
        if session.recorder.is_some() || !session.rolls.may_resume(now) {
            return;
        }
        let Some(factory) = session.recorder_factory.as_ref() else {
            return;
        };
        let Some((tracks, parts)) = self.parts_after(None) else {
            return;
        };
        let Some(from) = self.first_segment_part(&parts) else {
            return;
        };
        session.rolls.resume();
        let mut recorder = factory.start();
        recorder.mark_incomplete(IncompleteReason::EpochRate);
        let skipped = session.rolls.skipped();
        recorder.feed(tracks, parts.into_iter().filter(|p| p.id >= from).collect());
        tracing::info!(
            path = %self.path.as_path(),
            skipped,
            "recording resumes after codec changes that came too often"
        );
        session.recorder = Some(recorder);
    }

    /// Of the current epoch's retained `parts`, the earliest that begins a
    /// segment, so begins with a keyframe (`EXT-X-INDEPENDENT-SEGMENTS`): a
    /// part marked independent may begin mid-GOP with its keyframe later.
    fn first_segment_part(&self, parts: &[Arc<Part>]) -> Option<u64> {
        let st = self.lock();
        let m = st.ingest.muxer()?;
        let starts: Vec<u64> = m
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                sapflow_hls::muxer::Entry::Segment(segment) => {
                    segment.parts.first().map(|part| part.id)
                }
                sapflow_hls::muxer::Entry::Gap { .. } => None,
            })
            .chain(m.open_parts().first().map(|part| part.id))
            .collect();
        parts
            .iter()
            .map(|part| part.id)
            .find(|id| starts.contains(id))
    }

    /// The codec epoch of the parts `recorder` has had, if any.
    fn recorded_epoch(&self, recorder: &Recorder) -> Option<u64> {
        let last = recorder.last_part()?;
        match self.lock().ingest.muxer()?.part(last) {
            PartLookup::Found(part) => Some(part.initialization.id),
            _ => None,
        }
    }

    /// Before a recording rolls to a new codec epoch: gives the outgoing
    /// recorder every part of its epoch that it has not had, so media that
    /// was served is recorded. Returns `false` when some of it cannot be:
    /// mark the next recording incomplete ([`IncompleteReason::EpochUnrecorded`]).
    ///
    /// - The push that starts an epoch also seals the old epoch's last parts
    ///   (the muxer's interrupt), and [`Stream::parts_after`] gives only the
    ///   current epoch's. A recorder that has started is fed the rest of
    ///   its own epoch; it keeps its own init.
    /// - An epoch shorter than one part, once part timing is known, is still
    ///   served: the interrupt seals its open part. Its recorder never
    ///   started, so it is started now with that epoch's tracks, as noted
    ///   after the push that began it, and fed the epoch's parts.
    /// - An epoch begun and ended inside one push left no tracks to start
    ///   its recording with: `false`. So does a recording whose first part
    ///   was refused (no free writer, or the byte budget full), whether it
    ///   started after a push or late, here.
    pub fn feed_outgoing(&self, recorder: &mut Recorder) -> bool {
        if !recorder.is_accepting() {
            // Stopped: it has marked itself, unless it stopped before it had
            // a file, so its epoch was served but has no file or callback.
            return !recorder.refused_start();
        }
        let fed = {
            let st = self.lock();
            let Some(m) = st.ingest.muxer() else {
                return true;
            };
            let first = m.epoch_first_part_id();
            // The outgoing epoch, and the part to feed after.
            let (epoch, after) = match recorder.last_part() {
                Some(last) => match m.part(last) {
                    PartLookup::Found(p) => (p.initialization.id, Some(last)),
                    _ => return true,
                },
                None => match m.current_initialization_id().checked_sub(1) {
                    Some(previous) => (previous, None),
                    None => return true,
                },
            };
            let found = |id: u64| match m.part(id) {
                PartLookup::Found(p) if p.initialization.id == epoch => Some(p),
                _ => None,
            };
            let parts: Vec<Arc<Part>> = match after {
                // Upwards from the last part fed: the file's own tail, even
                // when later epochs sealed parts in the same push.
                Some(last) => (last + 1..first).map_while(found).collect(),
                // Never started: the epoch just before the current one,
                // newest first, back to its start or out of the window.
                None => {
                    let mut parts: Vec<Arc<Part>> = (0..first).rev().map_while(found).collect();
                    parts.reverse();
                    parts
                }
            };
            // An epoch between this one and the current one began and ended
            // inside one push: any parts it left have no recording.
            let current = m.current_initialization_id();
            let skipped = current > epoch + 1
                && (0..first)
                    .rev()
                    .map_while(|id| match m.part(id) {
                        PartLookup::Found(p) => Some(p.initialization.id),
                        _ => None,
                    })
                    .any(|e| e > epoch && e < current);
            if parts.is_empty() {
                return !skipped;
            }
            let tracks = if after.is_some() {
                // Not read by a recorder that has started.
                Tracks::with_audio(m.video().cloned(), m.audio_config().copied())
            } else {
                st.epoch_tracks
                    .iter()
                    .find(|(id, _)| *id == epoch)
                    .map(|(_, tracks)| tracks.clone())
            };
            match tracks {
                Some(tracks) => (tracks, parts, skipped),
                None => return false,
            }
        };
        let (tracks, parts, skipped) = fed;
        recorder.feed(tracks, parts);
        // A late start whose first part was refused (no free writer, or the
        // byte budget full) recorded nothing of the epoch.
        !skipped && recorder.last_part().is_some()
    }

    /// The init segment and every finished part of the current codec epoch
    /// after `after`, for the recorder. Parts are never skipped as long as
    /// this is called after every push and, before a recording rolls to a
    /// new epoch, [`Stream::feed_outgoing`] has fed the old one.
    pub fn parts_after(&self, after: Option<u64>) -> Option<(Tracks, Vec<Arc<Part>>)> {
        let st = self.lock();
        let m = st.ingest.muxer()?;
        let mut id = after.map_or(0, |a| a + 1).max(m.epoch_first_part_id());
        let mut parts = Vec::new();
        while id < m.next_part_id() {
            if let PartLookup::Found(p) = m.part(id) {
                parts.push(p);
            }
            id += 1;
        }
        Some((
            Tracks::with_audio(m.video().cloned(), m.audio_config().copied())?,
            parts,
        ))
    }

    pub fn close(&self) {
        {
            let mut state = self.lock();
            state.closed = true;
            // Terminal drain and recorder feed precede close. Existing response
            // and recorder aliases retain their own immutable accepted bytes.
            let kind = match state.ingest {
                Source::Ts(_) => SourceKind::Ts,
                Source::Flv(_) => SourceKind::Flv,
                Source::Es(_) => SourceKind::Es,
            };
            state.ingest = Source::new(kind, self.cfg, &self.fragments);
            state.media = None;
            state.conventional_media = None;
            state.multivariant = None;
        }
        self.version.send_modify(|v| *v += 1);
        crate::own_http::notify();
    }
}

/// How a publisher came by its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Fresh,
    /// Taken over from a publisher that dropped within the grace period.
    Resumed,
}

/// An atomic end-of-connection decision, captured before a successor claim.
pub enum Retirement {
    Stale,
    Ended,
    Held(Duration),
}

/// A claim refused before ownership, media or recorder policy changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    AlreadyLive,
    PublisherLimit,
    PresentationLimit,
    /// The live fragment budget would be too full for a new stream (see
    /// `Registry::admits_new_stream`), or [`MAX_STARTING_STREAMS`] are
    /// still starting: it would take the room the streams already live need
    /// to fill and slide their windows.
    MemoryLimit,
    /// sapflow is stopping ([`Registry::stop_all`]).
    Stopping,
}

/// The share of the live fragment budget, in percent, that the streams'
/// expected windows may fill before a new stream is refused. Once the
/// budget is full the next part any stream seals is refused, which drops
/// nearly every publisher at once; refusing new streams first leaves the
/// live ones room. A publisher reconnecting to a stream still held for it
/// is not new and is not refused for this.
pub const NEW_STREAM_FRAGMENT_PERCENT: usize = 80;

/// Live streams that may be starting at once (no segment complete yet, so
/// no projection of their window): beyond these a new stream waits, so a
/// crowd of encoders arriving together, as after a restart, is admitted as
/// their windows can be measured rather than all at once.
pub const MAX_STARTING_STREAMS: usize = 8;

/// The longest a live stream counts as starting. One that has sent nothing
/// a window can be projected from by then, such as video with no keyframe,
/// no longer holds a place among [`MAX_STARTING_STREAMS`].
pub const STARTING_FOR: Duration = Duration::from_secs(20);

/// The live streams, by event id, and current RTMP publication admission.
pub struct Registry {
    streams: Mutex<HashMap<String, Arc<LiveStream>>>,
    rtmp_publications: Arc<PublicationQuota>,
    presentations: Arc<PresentationQuota>,
    fragments: Arc<sapflow_core::fragment::FragmentQuota>,
    /// What muxers keep past their windows for RFC 8216bis 6.2.2, charged
    /// apart from `fragments` so that it never refuses live media.
    retained: Arc<sapflow_core::fragment::FragmentQuota>,
    /// Set by [`Registry::stop_all`] with the map locked: no claim after it.
    stopping: AtomicBool,
}

/// What [`Registry::stop_all`] ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stopped {
    /// Streams with a publisher connected.
    pub live: usize,
    /// Streams held for a reconnect, their publisher already gone.
    pub held: usize,
}

/// The fragment budget and retention pool every stream shares.
#[derive(Clone)]
struct Budget {
    fragments: Arc<sapflow_core::fragment::FragmentQuota>,
    retained: Arc<sapflow_core::fragment::FragmentQuota>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(default_presentation_limit(16))
    }
}

impl Registry {
    pub fn new(maximum_presentations: usize) -> Self {
        Self::with_limits(maximum_presentations, 512 * 1024 * 1024)
    }

    pub fn with_limits(maximum_presentations: usize, fragment_bytes: usize) -> Self {
        Self::with_budgets(
            maximum_presentations,
            fragment_bytes,
            sapflow_hls::muxer::RETENTION_POOL_BYTES,
        )
    }

    /// With a fragment budget and, beside it, the retention pool every
    /// stream's muxer charges kept media to: fragment memory is the two
    /// together.
    pub fn with_budgets(
        maximum_presentations: usize,
        fragment_bytes: usize,
        retained_bytes: usize,
    ) -> Self {
        Self {
            streams: Mutex::new(HashMap::new()),
            rtmp_publications: Arc::new(PublicationQuota::new()),
            presentations: Arc::new(PresentationQuota::new(maximum_presentations)),
            fragments: Arc::new(sapflow_core::fragment::FragmentQuota::new(fragment_bytes)),
            retained: Arc::new(sapflow_core::fragment::FragmentQuota::new(retained_bytes)),
            stopping: AtomicBool::new(false),
        }
    }

    /// How many more bytes the live fragment budget may hold before a new
    /// stream is refused; 0 refuses. A stream fills its window over tens of
    /// seconds, so what is in use now says little. The mark,
    /// [`NEW_STREAM_FRAGMENT_PERCENT`] of the budget, must cover what is in
    /// use, plus what each stream with a projection
    /// ([`Muxer::projected_window_bytes`]) is expected to add before its
    /// window is full (a held one too, since its publisher may come back),
    /// plus an assumed window for each stream still starting and for the
    /// newcomer: the median projection, but at least a ninth of the mark, so
    /// that [`MAX_STARTING_STREAMS`] unmeasured streams and a newcomer fill
    /// it on their own. A stream starts until it has a projection, for at
    /// most [`STARTING_FOR`]. Takes each stream's session and media locks
    /// under the map's.
    fn admission_headroom(&self, map: &HashMap<String, Arc<LiveStream>>) -> u128 {
        let mut growth = 0u128;
        let mut projections = Vec::new();
        let mut starting = 0usize;
        for stream in map.values() {
            let attached = stream.session().attached;
            let state = stream.lock();
            let muxer = state.ingest.muxer();
            match muxer.and_then(Muxer::projected_window_bytes) {
                Some(projected) => {
                    projections.push(projected);
                    let held = muxer.map_or(0, Muxer::held_window_bytes);
                    growth += projected.saturating_sub(held) as u128;
                }
                None => {
                    starting += usize::from(attached && stream.claimed.elapsed() < STARTING_FOR);
                }
            }
        }
        if starting >= MAX_STARTING_STREAMS {
            return 0;
        }
        // Wide enough that a 32-bit usize cannot saturate.
        let mark = self.fragments.maximum() as u128 * NEW_STREAM_FRAGMENT_PERCENT as u128 / 100;
        projections.sort_unstable();
        let median = projections.get(projections.len() / 2).copied().unwrap_or(0) as u128;
        let assumed = median.max(mark / (MAX_STARTING_STREAMS as u128 + 1));
        let committed = self.fragments.used() as u128 + growth + assumed * (starting as u128 + 1);
        mark.saturating_sub(committed)
    }

    fn admits_new_stream(&self, map: &HashMap<String, Arc<LiveStream>>) -> bool {
        self.admission_headroom(map) > 0
    }

    /// [`Registry::admission_headroom`] now.
    #[cfg(test)]
    pub(crate) fn headroom(&self) -> u128 {
        self.admission_headroom(&self.map())
    }

    fn budget(&self) -> Budget {
        Budget {
            fragments: self.fragments.clone(),
            retained: self.retained.clone(),
        }
    }
    #[cfg(test)]
    pub(crate) fn active_rtmp_publishers(&self) -> usize {
        self.rtmp_publications.active()
    }

    #[cfg(test)]
    pub(crate) fn fragment_quota(&self) -> Arc<sapflow_core::fragment::FragmentQuota> {
        self.fragments.clone()
    }

    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<LiveStream>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claims a path for a new publisher, with its generation. `None` if a
    /// publisher is connected to it. A stream held for a reconnect is taken
    /// over: same playlist, same recording.
    #[cfg(test)]
    pub fn claim(
        &self,
        path: &StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
    ) -> Option<(Arc<LiveStream>, u64, Claim)> {
        self.claim_configured(path, cfg, kind, false, 16, |_, _| {})
            .ok()
    }

    /// As [`Registry::claim`], but a stream with a publisher attached is taken
    /// over rather than refused, as mediamtx's `overridePublisher` does. An
    /// encoder that changes network reconnects before its old connection
    /// times out; refusing it would stall viewers until the timeout. The old
    /// run is closed as a drop would close it, and the old connection's next
    /// push is refused ([`crate::publish::PushError::Superseded`]).
    #[cfg(test)]
    pub fn claim_or_take_over(
        &self,
        path: &StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
    ) -> Option<(Arc<LiveStream>, u64, Claim)> {
        self.claim_configured(path, cfg, kind, true, 16, |_, _| {})
            .ok()
    }

    /// Configure the accepted connection before another claim can observe it.
    /// The cold setup runs under map -> session ownership; it must not re-enter
    /// the registry or perform file IO. Ingest never calls this callback.
    pub fn claim_configured(
        &self,
        path: &StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
        take_over: bool,
        max_rtmp_publishers: usize,
        configure: impl FnOnce(&mut Session, Claim),
    ) -> Result<(Arc<LiveStream>, u64, Claim), ClaimError> {
        let mut map = self.map();
        // Read under the map lock that stop_all sets it under.
        if self.stopping.load(Ordering::Acquire) {
            return Err(ClaimError::Stopping);
        }
        let mut publication_permit = None;
        if let Some(stream) = map.get(path.event_id()).cloned() {
            let mut session = stream.session();
            if session.attached && !take_over {
                return Err(ClaimError::AlreadyLive);
            }
            // Admission precedes any terminal parsing, generation change or
            // recorder configuration. A refused cross-protocol takeover must
            // leave its current SRT owner completely unchanged.
            if matches!(kind, SourceKind::Flv) {
                publication_permit = Some(match session.publication_permit.take() {
                    Some(permit) => permit,
                    None => self
                        .rtmp_publications
                        .try_acquire(max_rtmp_publishers)
                        .map_err(|_| ClaimError::PublisherLimit)?,
                });
            }
            if session.attached {
                // Preserve accepted terminal parser input and recorder loss
                // attribution before replacing the source under this owner.
                stream.finish_connection(&mut session);
                session.attached = false;
                tracing::info!(path = %path.as_path(), "publisher replaced by a new connection");
            }
            // RTMP-to-SRT replacement retires the old RTMP credit now. A
            // same-protocol replacement moved it above, without a transient N+1.
            session.publication_permit = None;
            // A delayed expiry task cannot extend the original outage window.
            if session
                .outage_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                drop(session);
                Self::end(&mut map, &stream);
            } else {
                session.generation += 1;
                session.attached = true;
                session.publication_permit = publication_permit;
                let mut st = stream.lock();
                let muxer = st.ingest.take_muxer();
                // detach() held the same map/session locks while interrupting
                // the old run. Its final pending part is already below this
                // floor, and cannot be completed by this connection's first IDR.
                session.fresh_part_floor = muxer.as_ref().map_or(0, Muxer::next_part_id);
                st.ingest = Source::resume(kind, stream.cfg, muxer, &self.budget());
                drop(st);
                configure(&mut session, Claim::Resumed);
                return Ok((stream.clone(), session.generation, Claim::Resumed));
            }
        }
        if !self.admits_new_stream(&map) {
            return Err(ClaimError::MemoryLimit);
        }
        if matches!(kind, SourceKind::Flv) && publication_permit.is_none() {
            publication_permit = Some(
                self.rtmp_publications
                    .try_acquire(max_rtmp_publishers)
                    .map_err(|_| ClaimError::PublisherLimit)?,
            );
        }
        let presentation_permit = self
            .presentations
            .try_acquire()
            .map_err(|_| ClaimError::PresentationLimit)?;
        let stream = Arc::new(LiveStream::new(
            path.clone(),
            cfg,
            kind,
            publication_permit,
            presentation_permit,
            self.budget(),
        ));
        configure(&mut stream.session(), Claim::Fresh);
        map.insert(path.event_id().to_owned(), stream.clone());
        Ok((stream, 0, Claim::Fresh))
    }

    pub fn get(&self, event_id: &str) -> Option<Arc<LiveStream>> {
        self.map().get(event_id).cloned()
    }

    /// The publisher of `generation` left. With `hold`, the stream stays up
    /// for a reconnect and this returns true: the caller then calls
    /// [`Registry::expire`] when the grace period is over. Otherwise the
    /// stream ends now. Nothing happens if a newer publisher has taken over.
    #[cfg(test)]
    pub fn detach(&self, stream: &Arc<LiveStream>, generation: u64, hold: bool) -> bool {
        let mut map = self.map();
        if !map
            .get(stream.path.event_id())
            .is_some_and(|s| Arc::ptr_eq(s, stream))
        {
            return false;
        }
        let mut session = stream.session();
        if session.generation != generation || !session.attached {
            return false;
        }
        session.attached = false;
        stream.finish_connection(&mut session);
        session.publication_permit = None;
        if hold {
            // Keep claim() excluded until the previous run and recording tail
            // are closed. A new publisher must not race this interruption.
            return true;
        }
        drop(session);
        Self::end(&mut map, stream);
        false
    }

    /// Retire one accepted publisher under map -> session -> media order.
    /// Already-ended exact ownership can still send its captured Unpublish;
    /// stale ownership must not emit callbacks or mutate a successor's grace.
    pub fn retire(
        &self,
        stream: &Arc<LiveStream>,
        generation: u64,
        grace: Duration,
        can_hold: bool,
    ) -> Retirement {
        let mut map = self.map();
        let mapped = map.get(stream.path.event_id());
        let different = mapped.is_some_and(|current| !Arc::ptr_eq(current, stream));
        let absent = mapped.is_none();
        let mut session = stream.session();
        if different || session.generation != generation {
            return Retirement::Stale;
        }
        let closed = stream.lock().closed;
        if absent && !closed {
            return Retirement::Stale;
        }
        if !session.attached || closed {
            session.attached = false;
            session.publication_permit = None;
            return Retirement::Ended;
        }
        session.hold_remaining(grace, Instant::now());
        session.attached = false;
        stream.finish_connection(&mut session);
        session.publication_permit = None;
        // Terminal parsing cannot extend the original reconnect deadline.
        let remaining = session.hold_remaining(grace, Instant::now());
        if can_hold && !remaining.is_zero() && stream.has_content() {
            return Retirement::Held(remaining);
        }
        drop(session);
        Self::end(&mut map, stream);
        Retirement::Ended
    }

    /// The grace period for `generation`'s drop is over: ends the stream
    /// unless a publisher has taken it over since. Returns whether it ended.
    pub fn expire(&self, stream: &Arc<LiveStream>, generation: u64) -> bool {
        // The map stays locked from the check to the removal, so a
        // reconnect cannot take the stream over in between.
        let mut map = self.map();
        let session = stream.session();
        if session.generation != generation || session.attached || stream.lock().closed {
            return false;
        }
        drop(session);
        Self::end(&mut map, stream);
        true
    }

    /// An explicit scoped revocation has no reconnect grace. Flush only media
    /// already accepted before the authority gate closed, then finalise its
    /// recorder. A delayed old owner cannot terminate a replacement stream.
    pub fn terminate(&self, stream: &Arc<LiveStream>, generation: u64) -> bool {
        let mut map = self.map();
        if !map
            .get(stream.path.event_id())
            .is_some_and(|s| Arc::ptr_eq(s, stream))
        {
            return false;
        }
        let mut session = stream.session();
        if session.generation != generation || stream.lock().closed {
            return false;
        }
        session.attached = false;
        stream.finish_connection(&mut session);
        session.publication_permit = None;
        drop(session);
        Self::end(&mut map, stream);
        true
    }

    /// sapflow is stopping: refuses every claim from now on and ends every
    /// stream at once, with no reconnect grace, under one map lock, so no
    /// publisher can claim a path in between.
    ///
    /// - A live publisher's recording is marked [`IncompleteReason::ServerStopped`]
    ///   (unless it already has a reason).
    /// - A held stream ends as if its grace had passed.
    /// - Any `unpublish` still owed is sent, whichever generation owes it: a
    ///   connection that took the stream over and has not yet published
    ///   leaves its predecessor's `publish` standing, and the stage is ending.
    ///
    /// Each stream's `unpublish` is queued before its recording is released,
    /// so it goes ahead of `recordSegmentComplete`. A connection still running
    /// finds its stream closed; its `Publication` then owes nothing.
    pub fn stop_all(&self) -> Stopped {
        let mut map = self.map();
        self.stopping.store(true, Ordering::Release);
        let streams: Vec<_> = map.values().cloned().collect();
        let mut stopped = Stopped::default();
        for stream in streams {
            {
                let mut session = stream.session();
                if stream.lock().closed {
                    continue;
                }
                let generation = session.generation;
                let live = session.attached;
                if live {
                    // Before the terminal drain, whose loss would otherwise
                    // be the recording's first reason.
                    session.mark_recording_incomplete(generation, IncompleteReason::ServerStopped);
                    stopped.live += 1;
                } else {
                    stopped.held += 1;
                }
                if let Some((_, callbacks)) = session.unpublish.take() {
                    callbacks.send(&stream.path, sapflow_core::StreamEvent::Unpublish);
                }
                if live {
                    // The terminal drain can start a codec epoch and roll the
                    // recording to a new, unmarked file: mark that one too (a
                    // reason it already has is kept). end's drain repeats
                    // nothing.
                    stream.finish_connection(&mut session);
                    session.mark_recording_incomplete(generation, IncompleteReason::ServerStopped);
                }
            }
            Self::end(&mut map, &stream);
        }
        stopped
    }

    /// Frees the path (if still this stream's), closes the stream, and
    /// finishes its recording. Called with the map locked.
    fn end(map: &mut HashMap<String, Arc<LiveStream>>, stream: &Arc<LiveStream>) {
        if map
            .get(stream.path.event_id())
            .is_some_and(|s| Arc::ptr_eq(s, stream))
        {
            map.remove(stream.path.event_id());
        }
        // Repeating a terminal drain is harmless. This also covers expiry
        // after a held connection, without duplicating its final AAC part.
        let recorder = {
            let mut session = stream.session();
            stream.finish_connection(&mut session);
            session.attached = false;
            session.publication_permit = None;
            stream.close();
            // Actual writer completion follows channel closure, not removal
            // from the registry or a requested transport shutdown.
            (session.outgoing.take(), session.recorder.take())
        };
        drop(recorder);
    }
}

#[cfg(test)]
#[path = "stream_retention_tests.rs"]
mod retention_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_prefixes_are_always_twelve_hex_digits() {
        // The leading zero case: 2026-10-29 12:31:16 UTC to 17:24:28 UTC.
        assert_eq!(prefix_from_nanos(0x0175_955d_0500), "0175955d0500");
        assert_eq!(prefix_from_nanos(0), "000000000000");
        assert_eq!(prefix_from_nanos(u128::MAX), "ffffffffffff");
        assert_eq!(prefix_from_nanos(0x1234_fe4b_1084_8000), "fe4b10848000");
        let prefix = fresh_prefix();
        assert_eq!(prefix.len(), 12);
        assert!(
            prefix
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    fn claim(reg: &Registry, p: &StreamPath) -> Option<(Arc<LiveStream>, u64, Claim)> {
        reg.claim(p, MuxerConfig::default(), SourceKind::Ts)
    }

    #[test]
    fn one_publisher_per_path() {
        let reg = Registry::default();
        let p = StreamPath::parse("live/event-a").expect("path");
        let (first, g1, c) = claim(&reg, &p).expect("free");
        assert_eq!(c, Claim::Fresh);
        assert!(claim(&reg, &p).is_none());
        assert!(!reg.detach(&first, g1, false));
        assert!(first.lock().closed);
        assert!(reg.get("event-a").is_none());
        let (second, g2, _) = claim(&reg, &p).expect("free again");
        // A late detach or expiry of the old stream must not evict the new one.
        assert!(!reg.detach(&first, g1, false));
        assert!(!reg.expire(&first, g1));
        assert!(reg.get("event-a").is_some_and(|s| Arc::ptr_eq(&s, &second)));
        assert!(!second.lock().closed);
        let _ = g2;
    }

    #[test]
    fn a_held_stream_is_taken_over_by_a_reconnect() {
        let reg = Registry::default();
        let p = StreamPath::parse("live/event-a").expect("path");
        let (first, g1, _) = claim(&reg, &p).expect("free");
        let prefix = first.lock().prefix.clone();
        assert!(reg.detach(&first, g1, true));
        // Held: still served, not closed, and free to take over.
        assert!(!first.lock().closed);
        assert!(reg.get("event-a").is_some());
        let (again, g2, c) = reg
            .claim(&p, MuxerConfig::default(), SourceKind::Flv)
            .expect("held");
        assert_eq!(c, Claim::Resumed);
        assert!(Arc::ptr_eq(&first, &again));
        assert!(g2 > g1);
        assert_eq!(again.lock().prefix, prefix);
        assert!(matches!(again.lock().ingest, Source::Flv(_)));
        // The first drop's grace running out changes nothing now.
        assert!(!reg.expire(&first, g1));
        assert!(!again.lock().closed);
        assert!(claim(&reg, &p).is_none(), "attached again");
        // The second drop, expired: the stream ends.
        assert!(reg.detach(&again, g2, true));
        assert!(reg.expire(&again, g2));
        assert!(again.lock().closed);
        assert!(reg.get("event-a").is_none());
        assert_eq!(claim(&reg, &p).expect("free").2, Claim::Fresh);
    }

    #[test]
    fn a_live_stream_is_taken_over_only_when_asked() {
        let reg = Registry::default();
        let p = StreamPath::parse("live/event-a").expect("path");
        let (first, g1, _) = claim(&reg, &p).expect("free");
        assert!(claim(&reg, &p).is_none(), "plain claim still refused");
        let (again, g2, c) = reg
            .claim_or_take_over(&p, MuxerConfig::default(), SourceKind::Ts)
            .expect("taken over");
        assert_eq!(c, Claim::Resumed);
        assert!(Arc::ptr_eq(&first, &again));
        assert!(g2 > g1);
        assert!(!first.session().owns_connection(g1));
        assert!(again.session().owns_connection(g2));
        // The replaced connection leaving later changes nothing.
        assert!(!reg.detach(&first, g1, true));
        assert!(!reg.expire(&first, g1));
        assert!(!again.lock().closed);
        assert!(again.session().owns_connection(g2));
    }

    #[tokio::test]
    async fn recorder_setup_is_atomic_with_admission_before_a_real_takeover() {
        use std::sync::mpsc;

        let mut fixture = crate::publish::loss_tests::Fixture::new();
        let path = StreamPath::parse("live/event-a").expect("path");
        let factory = RecorderFactory::new(
            fixture
                .app
                .config
                .recordings
                .as_ref()
                .expect("recording root"),
            &path,
            fixture.app.callbacks.clone(),
            0,
            fixture.app.recording_limits.clone(),
        );
        let registry = fixture.app.streams.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = std::thread::spawn(move || {
            registry
                .claim_configured(
                    &path,
                    MuxerConfig::default(),
                    SourceKind::Ts,
                    true,
                    16,
                    |s, c| {
                        assert_eq!(c, Claim::Fresh);
                        entered_tx.send(()).expect("setup entered");
                        release_rx
                            .recv_timeout(Duration::from_secs(5))
                            .expect("release setup");
                        s.recorder_factory = Some(factory);
                        s.recorder = s.recorder_factory.as_ref().map(RecorderFactory::start);
                    },
                )
                .expect("first claim")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("held setup");
        assert!(matches!(
            fixture.app.streams.streams.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let app = fixture.app.clone();
        let runtime = tokio::runtime::Handle::current();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let _entered = runtime.enter();
            attempt_tx.send(()).expect("takeover attempted");
            let request = sapflow_core::PublishRequest::parse_srt_stream_id(
                "publish:live/event-a:fixture:fixture",
            )
            .expect("request");
            let publication = crate::publish::Publication::begin(
                &app,
                &request,
                SourceKind::Ts,
                "127.0.0.1".parse().expect("loopback"),
            )
            .expect("real takeover");
            done_tx.send(()).expect("takeover completed");
            publication
        });
        attempt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("attempt");
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_tx.send(()).expect("finish first setup");
        let (stream, generation, _) = first.join().expect("first thread");
        let mut publication = second.join().expect("second thread");
        assert!(!stream.session().owns_connection(generation));
        assert!(stream.session().recorder.is_some());
        crate::publish::loss_tests::Fixture::feed(&mut publication);
        publication.end();
        fixture.completion("finalised").await;
        assert_eq!(fixture.scope.observations.counts().recordings_finalised, 1);
        assert_eq!(fixture.scope.observations.counts().recordings_incomplete, 0);
    }
}

#[cfg(test)]
mod grace_tests {
    use super::*;
    #[test]
    fn delayed_expiry_task_cannot_allow_a_claim_past_the_original_deadline() {
        let registry = Registry::default();
        let path = StreamPath::parse("live/event-expired").expect("valid epoch regression fixture");
        let (stream, generation, _) = registry
            .claim(&path, MuxerConfig::default(), SourceKind::Ts)
            .expect("valid epoch regression fixture");
        stream.session().hold_remaining(
            Duration::from_secs(1),
            Instant::now() - Duration::from_secs(2),
        );
        assert!(registry.detach(&stream, generation, true));
        let (fresh, _, claim) = registry
            .claim(&path, MuxerConfig::default(), SourceKind::Ts)
            .expect("valid epoch regression fixture");
        assert_eq!(claim, Claim::Fresh);
        assert!(!Arc::ptr_eq(&stream, &fresh));
        assert!(stream.lock().closed);
    }

    #[test]
    fn failed_connections_keep_original_deadline_but_media_recovery_rearms_it() {
        let now = Instant::now();
        let mut session = Session {
            generation: 0,
            attached: true,
            publication_permit: None,
            fresh_part_floor: 0,
            recorder: None,
            outgoing: None,
            rolls: RollBudget::new(RollLimit::default()),
            recorder_factory: None,
            outage_deadline: None,
            clean_close: None,
            unpublish: None,
        };
        let grace = Duration::from_secs(90);
        assert_eq!(session.hold_remaining(grace, now), grace);
        assert_eq!(
            session.hold_remaining(grace, now + Duration::from_secs(80)),
            Duration::from_secs(10)
        );
        assert_eq!(session.hold_remaining(grace, now + grace), Duration::ZERO);
        session.recovered();
        assert_eq!(
            session.hold_remaining(grace, now + Duration::from_secs(3600)),
            grace
        );
        assert_eq!(session.hold_remaining(Duration::ZERO, now), Duration::ZERO);
    }
}

#[cfg(test)]
mod publication_capacity_tests {
    use super::*;
    use std::cell::Cell;

    fn path(id: &str) -> StreamPath {
        StreamPath::parse(&format!("live/event-{id}")).expect("generated path")
    }

    fn attach(
        registry: &Registry,
        limit: usize,
        id: &str,
        kind: SourceKind,
    ) -> Result<(Arc<LiveStream>, u64, Claim), ClaimError> {
        registry.claim_configured(
            &path(id),
            MuxerConfig::default(),
            kind,
            true,
            limit,
            |_, _| {},
        )
    }

    #[test]
    fn exact_publisher_cap_refuses_before_configuration_and_releases_on_retire() {
        let registry = Registry::default();
        let limit = 2;
        let (first, g1, _) = attach(&registry, limit, "first", SourceKind::Flv).expect("first");
        let (second, _, _) = attach(&registry, limit, "second", SourceKind::Flv).expect("second");
        let configured = Cell::new(false);
        assert!(matches!(
            registry.claim_configured(
                &path("third"),
                MuxerConfig::default(),
                SourceKind::Flv,
                true,
                limit,
                |_, _| configured.set(true)
            ),
            Err(ClaimError::PublisherLimit)
        ));
        assert!(!configured.get());
        assert!(registry.get("event-third").is_none());
        assert!(matches!(
            registry.claim_configured(
                &path("first"),
                MuxerConfig::default(),
                SourceKind::Flv,
                false,
                limit,
                |_, _| configured.set(true)
            ),
            Err(ClaimError::AlreadyLive)
        ));
        assert!(!configured.get());
        assert_eq!(registry.active_rtmp_publishers(), 2);
        assert!(matches!(
            registry.retire(&first, g1, Duration::ZERO, false),
            Retirement::Ended
        ));
        assert!(first.lock().closed, "retained old Arc is still present");
        assert_eq!(registry.active_rtmp_publishers(), 1);
        let _third =
            attach(&registry, limit, "third", SourceKind::Flv).expect("freed owner credit");
        assert_eq!(registry.active_rtmp_publishers(), 2);
        assert!(!second.lock().closed);
    }

    #[test]
    fn same_path_takeover_at_capacity_transfers_credit_and_all_stale_retirements_are_inert() {
        let registry = Registry::default();
        let limit = 1;
        let (old, old_generation, _) =
            attach(&registry, limit, "same", SourceKind::Flv).expect("old");
        let (new, generation, claim) =
            attach(&registry, limit, "same", SourceKind::Flv).expect("full-cap takeover");
        assert_eq!(claim, Claim::Resumed);
        assert!(Arc::ptr_eq(&old, &new));
        assert!(generation > old_generation);
        assert_eq!(registry.active_rtmp_publishers(), 1);
        assert!(matches!(
            registry.retire(&old, old_generation, Duration::ZERO, false),
            Retirement::Stale
        ));
        assert!(!registry.detach(&old, old_generation, false));
        assert!(!registry.terminate(&old, old_generation));
        assert!(!registry.expire(&old, old_generation));
        assert_eq!(registry.active_rtmp_publishers(), 1);
        assert!(new.session().owns_connection(generation));
        assert!(matches!(
            attach(&registry, limit, "other", SourceKind::Flv),
            Err(ClaimError::PublisherLimit)
        ));
        assert!(registry.terminate(&new, generation));
        assert_eq!(registry.active_rtmp_publishers(), 0);
    }

    #[test]
    fn cross_protocol_refusal_is_atomic_and_rtmp_to_srt_releases_credit() {
        let registry = Registry::default();
        let limit = 1;
        let (srt, srt_generation, _) =
            attach(&registry, limit, "srt", SourceKind::Ts).expect("SRT");
        let (rtmp, _, _) = attach(&registry, limit, "rtmp", SourceKind::Flv).expect("RTMP");
        let before_prefix = srt.lock().prefix.clone();
        let before_version = srt.version();
        let configured = Cell::new(false);
        assert!(matches!(
            registry.claim_configured(
                &path("srt"),
                MuxerConfig::default(),
                SourceKind::Flv,
                true,
                limit,
                |_, _| configured.set(true)
            ),
            Err(ClaimError::PublisherLimit)
        ));
        assert!(!configured.get());
        assert!(srt.session().owns_connection(srt_generation));
        assert!(matches!(srt.lock().ingest, Source::Ts(_)));
        assert_eq!(srt.lock().prefix, before_prefix);
        assert_eq!(srt.version(), before_version);
        let (as_srt, _, _) =
            attach(&registry, limit, "rtmp", SourceKind::Ts).expect("SRT replacement");
        assert!(Arc::ptr_eq(&rtmp, &as_srt));
        assert_eq!(registry.active_rtmp_publishers(), 0);
        let (as_rtmp, _, _) =
            attach(&registry, limit, "srt", SourceKind::Flv).expect("now capacity");
        assert!(Arc::ptr_eq(&srt, &as_rtmp));
        assert_eq!(registry.active_rtmp_publishers(), 1);
    }

    #[test]
    fn detached_grace_reconnect_reacquires_only_available_actual_owner_capacity() {
        let registry = Registry::default();
        let limit = 1;
        let (held, old_generation, _) =
            attach(&registry, limit, "held", SourceKind::Flv).expect("first");
        assert!(registry.detach(&held, old_generation, true));
        assert_eq!(registry.active_rtmp_publishers(), 0);
        let (other, generation, _) =
            attach(&registry, limit, "other", SourceKind::Flv).expect("other");
        assert!(matches!(
            attach(&registry, limit, "held", SourceKind::Flv),
            Err(ClaimError::PublisherLimit)
        ));
        assert!(
            !held.lock().closed,
            "refusal does not destroy the held presentation"
        );
        assert!(!held.session().attached);
        registry.retire(&other, generation, Duration::ZERO, false);
        let (resumed, current, claim) =
            attach(&registry, limit, "held", SourceKind::Flv).expect("freed credit");
        assert!(Arc::ptr_eq(&held, &resumed));
        assert_eq!(claim, Claim::Resumed);
        assert!(!registry.expire(&held, old_generation));
        assert!(resumed.session().owns_connection(current));
        assert_eq!(registry.active_rtmp_publishers(), 1);
    }

    #[test]
    fn actual_held_retirement_releases_credit_before_cache_and_expiry_owners() {
        let registry = Registry::default();
        let limit = 1;
        let (stream, generation, _) =
            attach(&registry, limit, "held-media", SourceKind::Flv).expect("owner");
        stream
            .apply(|source, now| {
                let Source::Flv(flv) = source else {
                    panic!("FLV")
                };
                flv.declare_tracks(sapflow_hls::TrackSet::AudioOnly, now)?;
                flv.audio_at(0, &[0xaf, 0, 0x11, 0x88], now)?;
                let mut update = Default::default();
                for frame in 0..64u64 {
                    // Storage/retirement fixture, not decode-quality media.
                    update = flv.audio_at(
                        (frame * 1024 * 1000 / 48_000) as u32,
                        &[0xaf, 1, 0x5a, 0x5a, 0x5a],
                        now,
                    )?;
                }
                Ok(update)
            })
            .expect("accepted bounded audio");
        assert!(stream.has_content());
        assert!(matches!(
            registry.retire(&stream, generation, Duration::from_secs(60), true),
            Retirement::Held(_)
        ));
        assert_eq!(registry.active_rtmp_publishers(), 0);
        assert!(!stream.lock().closed);
        let (_other, _, _) =
            attach(&registry, limit, "other", SourceKind::Flv).expect("credit already freed");
        assert_eq!(registry.active_rtmp_publishers(), 1);
        assert!(registry.expire(&stream, generation));
        assert_eq!(
            registry.active_rtmp_publishers(),
            1,
            "expiry cannot free a different owner"
        );
    }

    #[test]
    fn expired_same_path_takeover_moves_the_full_cap_credit_into_the_fresh_object() {
        let registry = Registry::default();
        let limit = 1;
        let (old, old_generation, _) =
            attach(&registry, limit, "expired", SourceKind::Flv).expect("old");
        old.session().hold_remaining(
            Duration::from_secs(1),
            Instant::now() - Duration::from_secs(2),
        );
        let (fresh, current, claim) =
            attach(&registry, limit, "expired", SourceKind::Flv).expect("full-cap fresh successor");
        assert_eq!(claim, Claim::Fresh);
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert!(old.lock().closed);
        assert_eq!(registry.active_rtmp_publishers(), 1);
        assert!(matches!(
            registry.retire(&old, old_generation, Duration::ZERO, false),
            Retirement::Stale
        ));
        assert!(fresh.session().owns_connection(current));
        assert_eq!(registry.active_rtmp_publishers(), 1);
    }

    #[test]
    fn concurrent_distinct_path_claims_count_only_two_real_owners() {
        let registry = Arc::new(Registry::default());
        let limit = 2;
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut threads = Vec::new();
        for i in 0..4 {
            let (registry, barrier) = (registry.clone(), barrier.clone());
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                attach(&registry, limit, &format!("parallel-{i}"), SourceKind::Flv)
            }));
        }
        barrier.wait();
        let results = threads
            .into_iter()
            .map(|t| t.join().expect("bounded claim worker"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 2);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(ClaimError::PublisherLimit)))
                .count(),
            2
        );
        assert_eq!(registry.active_rtmp_publishers(), 2);
        for (stream, generation, _) in results.into_iter().flatten() {
            registry.retire(&stream, generation, Duration::ZERO, false);
        }
        assert_eq!(registry.active_rtmp_publishers(), 0);
    }

    #[test]
    fn unpublished_fresh_configuration_unwind_rolls_back_owner_credit() {
        let registry = Registry::default();
        let limit = 1;
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = registry.claim_configured(
                &path("unpublished"),
                MuxerConfig::default(),
                SourceKind::Flv,
                true,
                limit,
                |_, _| panic!("test-only fresh setup failure"),
            );
        }));
        assert!(failed.is_err());
        assert!(registry.get("event-unpublished").is_none());
        assert_eq!(registry.active_rtmp_publishers(), 0);
        assert_eq!(registry.presentations.active(), 0);
        assert!(attach(&registry, limit, "accepted", SourceKind::Flv).is_ok());
    }
}
