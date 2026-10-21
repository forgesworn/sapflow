//! Live streams: one per publishing encoder, shared by ingest and HTTP.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Chunk-name prefix, unique per stream (and per restart) so a new
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
}

/// What a push did.
#[derive(Debug, Clone, Copy, Default)]
pub struct Applied {
    /// The stream is servable.
    pub ready: bool,
    /// A reconnected encoder's settings differed, so the stream started
    /// afresh (new chunk names, new init): a new recording too.
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
        let update = f(&mut st.ingest, SystemTime::now())?;
        if !update.any() {
            return Ok(Applied::default());
        }
        if update.restarted {
            st.prefix = fresh_prefix();
            st.media = None;
            st.multivariant = None;
        }
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
        let mut id = after.map_or(0, |a| a + 1);
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
        if let Some(stream) = map.get(path.event_id()) {
            let mut session = stream.session();
            if session.attached {
                return None;
            }
            session.generation += 1;
            session.attached = true;
            let mut st = stream.lock();
            let muxer = st.ingest.take_muxer();
            st.ingest = Source::resume(kind, stream.cfg, muxer);
            return Some((stream.clone(), session.generation, Claim::Resumed));
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
        drop(session);
        if hold {
            drop(map);
            stream.interrupt();
            return true;
        }
        Self::end(&mut map, stream);
        false
    }

    /// The grace period for `generation`'s drop is over: ends the stream
    /// unless a publisher has taken it over since.
    pub fn expire(&self, stream: &Arc<LiveStream>, generation: u64) {
        // The map stays locked from the check to the removal, so a
        // reconnect cannot take the stream over in between.
        let mut map = self.map();
        let session = stream.session();
        if session.generation != generation || session.attached {
            return;
        }
        drop(session);
        Self::end(&mut map, stream);
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
        reg.expire(&first, g1);
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
        reg.expire(&first, g1);
        assert!(!again.lock().closed);
        assert!(claim(&reg, &p).is_none(), "attached again");
        // The second drop, expired: the stream ends.
        assert!(reg.detach(&again, g2, true));
        reg.expire(&again, g2);
        assert!(again.lock().closed);
        assert!(reg.get("event-a").is_none());
        assert_eq!(claim(&reg, &p).expect("free").2, Claim::Fresh);
    }
}
