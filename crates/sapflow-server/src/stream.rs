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
use tokio::sync::watch;

pub struct LiveStream {
    pub path: StreamPath,
    /// Chunk-name prefix, unique per publish so a republish never collides
    /// with chunks a cache still holds from the last one.
    pub prefix: String,
    state: Mutex<State>,
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
    pub fn muxer(&self) -> Option<&Muxer> {
        match self {
            Self::Ts(t) => t.muxer(),
            Self::Flv(f) => f.muxer(),
        }
    }
}

pub struct State {
    pub ingest: Source,
    pub closed: bool,
    pub media: Option<Bytes>,
    pub multivariant: Option<Bytes>,
}

impl LiveStream {
    fn new(path: StreamPath, cfg: MuxerConfig, kind: SourceKind) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            path,
            prefix: format!("{:x}", nanos & 0xffff_ffff_ffff),
            state: Mutex::new(State {
                ingest: match kind {
                    SourceKind::Ts => Source::Ts(TsIngest::new(cfg)),
                    SourceKind::Flv => Source::Flv(FlvIngest::new(cfg)),
                },
                closed: false,
                media: None,
                multivariant: None,
            }),
            version: watch::Sender::new(0),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock aborts the process (panic = abort
        // in release), so poisoning only matters in tests.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Subscribe before checking state, then wait on `changed()`.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    /// Feeds media through `f`. Returns whether the stream is servable.
    pub fn apply(
        &self,
        f: impl FnOnce(&mut Source, SystemTime) -> Result<Update, IngestError>,
    ) -> Result<bool, IngestError> {
        let mut st = self.lock();
        let update = f(&mut st.ingest, SystemTime::now())?;
        if !update.any() {
            return Ok(false);
        }
        let rendered = st.ingest.muxer().filter(|m| m.has_content()).map(|m| {
            (
                Bytes::from(playlist::media(m, &self.prefix)),
                Bytes::from(playlist::multivariant(m)),
            )
        });
        let ready = rendered.is_some();
        if let Some((media, mv)) = rendered {
            st.media = Some(media);
            st.multivariant = Some(mv);
        }
        drop(st);
        self.version.send_modify(|v| *v += 1);
        Ok(ready)
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

/// The live streams, by event id.
#[derive(Default)]
pub struct Registry {
    streams: Mutex<HashMap<String, Arc<LiveStream>>>,
}

impl Registry {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<LiveStream>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claims a path for a new publisher. `None` if it is already live.
    pub fn claim(
        &self,
        path: &StreamPath,
        cfg: MuxerConfig,
        kind: SourceKind,
    ) -> Option<Arc<LiveStream>> {
        let mut map = self.map();
        if map.contains_key(path.event_id()) {
            return None;
        }
        let stream = Arc::new(LiveStream::new(path.clone(), cfg, kind));
        map.insert(path.event_id().to_owned(), stream.clone());
        Some(stream)
    }

    pub fn get(&self, event_id: &str) -> Option<Arc<LiveStream>> {
        self.map().get(event_id).cloned()
    }

    /// Releases a path, if it is still held by this stream.
    pub fn release(&self, stream: &Arc<LiveStream>) {
        let mut map = self.map();
        if map
            .get(stream.path.event_id())
            .is_some_and(|s| Arc::ptr_eq(s, stream))
        {
            map.remove(stream.path.event_id());
        }
        drop(map);
        stream.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_publisher_per_path() {
        let reg = Registry::default();
        let p = StreamPath::parse("live/event-a").expect("path");
        let first = reg
            .claim(&p, MuxerConfig::default(), SourceKind::Ts)
            .expect("free");
        assert!(
            reg.claim(&p, MuxerConfig::default(), SourceKind::Ts)
                .is_none()
        );
        reg.release(&first);
        assert!(first.lock().closed);
        assert!(reg.get("event-a").is_none());
        let second = reg
            .claim(&p, MuxerConfig::default(), SourceKind::Ts)
            .expect("free again");
        // A late release of the old stream must not evict the new one.
        reg.release(&first);
        assert!(reg.get("event-a").is_some_and(|s| Arc::ptr_eq(&s, &second)));
    }
}
