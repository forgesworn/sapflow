//! Live streams: one per publishing encoder, shared by ingest and HTTP.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use sapflow_core::StreamPath;
use sapflow_hls::muxer::Part;
use sapflow_hls::{
    FlvIngest, IngestError, Muxer, MuxerConfig, PartLookup, TsIngest, Update, playlist,
};

use crate::recorder::Recorder;
use tokio::sync::watch;

pub struct LiveStream {
    pub path: StreamPath,
    cfg: MuxerConfig,
    state: Mutex<State>,
    session: Mutex<Session>,
    version: watch::Sender<u64>,
}

/// The track configs, for a recorder's own init segment.
pub type Tracks = (
    sapflow_hls::codec::H264Config,
    Option<sapflow_hls::codec::AacConfig>,
);

/// Where the media comes from.
pub enum Source {
    /// SRT carries MPEG-TS.
    Ts(TsIngest),
    /// RTMP carries FLV tags.
    Flv(FlvIngest),
}

#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    Ts,
    Flv,
}

impl Source {
    fn new(kind: SourceKind, cfg: MuxerConfig) -> Self {
        match kind {
            SourceKind::Ts => Self::Ts(TsIngest::new(cfg)),
            SourceKind::Flv => Self::Flv(FlvIngest::new(cfg)),
        }
    }

    /// A new connection's ingest, carrying on `muxer` if there is one.
    fn resume(kind: SourceKind, cfg: MuxerConfig, muxer: Option<Muxer>) -> Self {
        match (kind, muxer) {
            (_, None) => Self::new(kind, cfg),
            (SourceKind::Ts, Some(m)) => Self::Ts(TsIngest::resume(cfg, m)),
            (SourceKind::Flv, Some(m)) => Self::Flv(FlvIngest::resume(cfg, m)),
        }
    }

    pub fn dropped_packets(&self) -> u64 {
        match self {
            Self::Ts(ingest) => ingest.dropped_packets(),
            Self::Flv(_) => 0,
        }
    }

    pub fn muxer(&self) -> Option<&Muxer> {
        match self {
            Self::Ts(t) => t.muxer(),
            Self::Flv(f) => f.muxer(),
        }
    }

    fn muxer_mut(&mut self) -> Option<&mut Muxer> {
        match self {
            Self::Ts(t) => t.muxer_mut(),
            Self::Flv(f) => f.muxer_mut(),
        }
    }

    fn take_muxer(&mut self) -> Option<Muxer> {
        match self {
            Self::Ts(t) => t.take_muxer(),
            Self::Flv(f) => f.take_muxer(),
        }
    }
}

pub struct State {
    pub ingest: Source,
    /// Chunk-name prefix, unique per presentation so a new
    /// stream never collides with chunks a cache still holds from the last.
    pub prefix: String,
    pub closed: bool,
    pub media: Option<Bytes>,
    pub multivariant: Option<Bytes>,
}

/// Who owns the stream: the connected publisher, or nobody while it is
/// held for a reconnect. Locked before `state` when both are needed.
pub struct Session {
    /// Bumped by each publisher that attaches, so a late detach or expiry
    /// from an earlier one changes nothing.
    generation: u64,
    attached: bool,
    /// Kept across a reconnect: one recording per show.
    pub recorder: Option<Recorder>,
    outage_deadline: Option<Instant>,
}

impl Session {
    /// Ignore delayed notices from an older connection after replacement.
    pub fn mark_recording_incomplete(&self, generation: u64) {
        if self.generation == generation
            && let Some(recorder) = &self.recorder
        {
            recorder.mark_incomplete();
        }
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
}

/// What a push did.
#[derive(Debug, Clone, Copy, Default)]
pub struct Applied {
    /// The stream is servable.
    pub ready: bool,
    /// A new codec initialization epoch: the HLS timeline continues, and a
    /// new recording begins at the epoch's first part.
    pub restarted: bool,
}

fn fresh_prefix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", nanos & 0xffff_ffff_ffff)
}

impl LiveStream {
    fn new(path: StreamPath, cfg: MuxerConfig, kind: SourceKind) -> Self {
        Self {
            path,
            cfg,
            state: Mutex::new(State {
                ingest: Source::new(kind, cfg),
                prefix: fresh_prefix(),
                closed: false,
                media: None,
                multivariant: None,
            }),
            session: Mutex::new(Session {
                generation: 0,
                attached: true,
                recorder: None,
                outage_deadline: None,
            }),
            version: watch::Sender::new(0),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock aborts the process (panic = abort
        // in release), so poisoning only matters in tests.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
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
        let update = f(&mut st.ingest, SystemTime::now())?;
        if !update.any() {
            return Ok(Applied::default());
        }
        // Codec epochs retain this presentation's prefix and all media IDs.
        // Per-segment immutable initialization identities select each MAP.
        let ready = Self::render(&mut st);
        drop(st);
        self.version.send_modify(|v| *v += 1);
        Ok(Applied {
            ready,
            restarted: update.restarted,
        })
    }

    fn render(st: &mut State) -> bool {
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

    /// The publisher left: end the current run so a reconnect starts a new
    /// segment after a discontinuity, and publish what was closed.
    fn interrupt(&self) {
        let mut st = self.lock();
        let changed = st
            .ingest
            .muxer_mut()
            .is_some_and(|m| m.interrupt(SystemTime::now()).any());
        if changed {
            Self::render(&mut st);
        }
        drop(st);
        self.version.send_modify(|v| *v += 1);
    }

    /// The init segment and every finished part after `after`, for the
    /// recorder. Parts are never skipped as long as this is called after
    /// every push.
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
        Some(((m.video().clone(), m.audio().copied()), parts))
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.version.send_modify(|v| *v += 1);
    }
}

/// How a publisher came by its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Fresh,
    /// Taken over from a publisher that dropped within the grace period.
    Resumed,
}

/// The live streams, by event id.
#[derive(Default)]
pub struct Registry {
    streams: Mutex<HashMap<String, Arc<LiveStream>>>,
}

impl Registry {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<LiveStream>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claims a path for a new publisher, with its generation. `None` if a
    /// publisher is connected to it. A stream held for a reconnect is taken
    /// over: same playlist, same recording.
    pub fn claim(
        &self,
        path: &StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
    ) -> Option<(Arc<LiveStream>, u64, Claim)> {
        let mut map = self.map();
        if let Some(stream) = map.get(path.event_id()).cloned() {
            let mut session = stream.session();
            if session.attached {
                return None;
            }
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
                let mut st = stream.lock();
                let muxer = st.ingest.take_muxer();
                st.ingest = Source::resume(kind, stream.cfg, muxer);
                return Some((stream.clone(), session.generation, Claim::Resumed));
            }
        }
        let stream = Arc::new(LiveStream::new(path.clone(), cfg, kind));
        map.insert(path.event_id().to_owned(), stream.clone());
        Some((stream, 0, Claim::Fresh))
    }

    pub fn get(&self, event_id: &str) -> Option<Arc<LiveStream>> {
        self.map().get(event_id).cloned()
    }

    /// The publisher of `generation` left. With `hold`, the stream stays up
    /// for a reconnect and this returns true: the caller then calls
    /// [`Registry::expire`] when the grace period is over. Otherwise the
    /// stream ends now. Nothing happens if a newer publisher has taken over.
    pub fn detach(&self, stream: &Arc<LiveStream>, generation: u64, hold: bool) -> bool {
        let mut map = self.map();
        let mut session = stream.session();
        if session.generation != generation || !session.attached {
            return false;
        }
        session.attached = false;
        if hold {
            // Keep claim() excluded until the previous run and recording tail
            // are closed. A new publisher must not race this interruption.
            stream.interrupt();
            if let Some(recorder) = &mut session.recorder
                && let Some((tracks, parts)) = stream.parts_after(recorder.last_part())
            {
                recorder.feed(tracks, parts);
            }
            return true;
        }
        drop(session);
        Self::end(&mut map, stream);
        false
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
        stream.interrupt();
        if let Some(recorder) = &mut session.recorder
            && let Some((tracks, parts)) = stream.parts_after(recorder.last_part())
        {
            recorder.feed(tracks, parts);
        }
        drop(session);
        Self::end(&mut map, stream);
        true
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
        stream.close();
        // Dropping the recorder closes its queue; the writer then finishes
        // the file and sends recordSegmentComplete.
        let recorder = stream.session().recorder.take();
        drop(recorder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            recorder: None,
            outage_deadline: None,
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
