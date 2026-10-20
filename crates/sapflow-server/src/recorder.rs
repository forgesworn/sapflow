//! Recording: one fragmented MP4 per show, `<dir>/live/<eventId>/<unix-start>.mp4`.
//!
//! The file is the init segment followed by every LL-HLS part, which is a
//! valid fragmented MP4 that plays in a browser `<video>`. Writes happen on
//! their own task so a slow disk never stalls ingest.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sapflow_core::recording::{is_recording_name, recording_file};
use sapflow_core::{StreamEvent, StreamPath};
use sapflow_hls::fmp4::init_segment_for_file;
use sapflow_hls::muxer::Part;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::callback::Callbacks;
use crate::stream::Tracks;

/// Writes behind ingest by at most this many parts (about 50 s) before
/// recording gives up rather than buffer without bound.
const QUEUE: usize = 256;

enum Msg {
    Init(Tracks),
    Part(Arc<Part>),
}

pub struct Recorder {
    tx: Option<mpsc::Sender<Msg>>,
    started: bool,
    last_part: Option<u64>,
}

impl Recorder {
    pub fn start(dir: &Path, path: &StreamPath, callbacks: Callbacks, min_free: u64) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(write(dir.to_owned(), path.clone(), rx, callbacks, min_free));
        Self {
            tx: Some(tx),
            started: false,
            last_part: None,
        }
    }

    pub fn last_part(&self) -> Option<u64> {
        self.last_part
    }

    /// Queues the init segment (once) and new parts.
    pub fn feed(&mut self, tracks: Tracks, parts: Vec<Arc<Part>>) {
        let Some(tx) = &self.tx else { return };
        if parts.is_empty() {
            return;
        }
        let mut ok = true;
        if !self.started {
            self.started = true;
            ok &= tx.try_send(Msg::Init(tracks)).is_ok();
        }
        for p in parts {
            self.last_part = Some(p.id);
            ok &= tx.try_send(Msg::Part(p)).is_ok();
        }
        if !ok {
            tracing::error!("recording fell behind the stream; stopping this recording");
            self.tx = None;
        }
    }
}

/// Whether the recordings disk has at least `min_free` bytes available.
async fn space_ok(dir: &Path, min_free: u64) -> bool {
    if min_free == 0 {
        return true;
    }
    let dir = dir.to_owned();
    match tokio::task::spawn_blocking(move || fs4::available_space(&dir)).await {
        Ok(Ok(free)) => free >= min_free,
        // Cannot tell: carry on, a write error will stop the recording.
        _ => true,
    }
}

async fn write(
    dir: PathBuf,
    path: StreamPath,
    mut rx: mpsc::Receiver<Msg>,
    callbacks: Callbacks,
    min_free: u64,
) {
    // Wait for the first part before creating anything on disk.
    let Some(Msg::Init((video, audio))) = rx.recv().await else {
        return;
    };
    let init = init_segment_for_file(&video, audio.as_ref(), Duration::ZERO);
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = tokio::fs::create_dir_all(&dir).await;
    if !space_ok(&dir, min_free).await {
        tracing::error!(path = %path.as_path(), "not recording: recordings disk is nearly full");
        return;
    }
    let Some((file_path, mut file)) = create(&dir, &path, start).await else {
        return;
    };
    let mut duration = Duration::ZERO;
    let mut failed = file.write_all(&init).await.is_err();
    let mut full = false;
    let mut parts = 0u64;
    while let Some(msg) = rx.recv().await {
        if failed || full {
            continue;
        }
        if let Msg::Part(p) = msg {
            // About every 10 s of media.
            parts += 1;
            if parts.is_multiple_of(50) && !space_ok(&dir, min_free).await {
                tracing::error!(file = %file_path.display(), "recordings disk nearly full; recording stops here");
                full = true;
                continue;
            }
            failed = file.write_all(&p.data).await.is_err();
            duration += p.duration;
        }
    }
    // Now the length is known: rewrite the init in place (same size).
    let final_init = init_segment_for_file(&video, audio.as_ref(), duration);
    if !failed && final_init.len() == init.len() {
        failed = file.seek(std::io::SeekFrom::Start(0)).await.is_err()
            || file.write_all(&final_init).await.is_err();
    }
    if failed || file.flush().await.is_err() || file.sync_all().await.is_err() {
        tracing::error!(file = %file_path.display(), "recording write failed");
        return;
    }
    tracing::info!(file = %file_path.display(), secs = duration.as_secs_f64(), "recording finished");
    callbacks.send(
        &path,
        StreamEvent::RecordSegmentComplete {
            segment_path: file_path.display().to_string(),
            duration_secs: (duration.as_secs_f64() * 1000.0).round() / 1000.0,
        },
    );
}

async fn create(dir: &Path, path: &StreamPath, start: u64) -> Option<(PathBuf, tokio::fs::File)> {
    // A reconnect within the same second gets the next free second.
    for t in start..start + 60 {
        let file_path = dir.join(recording_file(path, t));
        if let Some(parent) = file_path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            tracing::error!(error = %e, "cannot create recording directory");
            return None;
        }
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file_path)
            .await
        {
            Ok(f) => return Some((file_path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                tracing::error!(error = %e, "cannot create recording");
                return None;
            }
        }
    }
    None
}

/// Deletes recordings older than `max_age`, every ten minutes. Only files
/// named like ours, under `live/<eventId>/`, are touched.
pub async fn expire_forever(dir: PathBuf, max_age: Duration) {
    loop {
        let removed = expire(&dir, max_age, SystemTime::now()).await;
        if removed > 0 {
            tracing::info!(removed, "expired old recordings");
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

pub async fn expire(dir: &Path, max_age: Duration, now: SystemTime) -> usize {
    let mut removed = 0;
    let Ok(mut events) = tokio::fs::read_dir(dir.join("live")).await else {
        return 0;
    };
    while let Ok(Some(event)) = events.next_entry().await {
        let Ok(mut files) = tokio::fs::read_dir(event.path()).await else {
            continue;
        };
        while let Ok(Some(f)) = files.next_entry().await {
            let name = f.file_name();
            if !name.to_str().is_some_and(is_recording_name) {
                continue;
            }
            let Ok(meta) = f.metadata().await else {
                continue;
            };
            let old = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age > max_age);
            if meta.is_file() && old && tokio::fs::remove_file(f.path()).await.is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sapflow-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn expiry_only_touches_old_recordings() {
        let dir = scratch("expire");
        let ev = dir.join("live/event-a");
        std::fs::create_dir_all(&ev).expect("mkdir");
        for f in ["100.mp4", "notes.txt", "200.mp4.tmp"] {
            std::fs::write(ev.join(f), b"x").expect("write");
        }
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(expire(&dir, Duration::from_secs(7200), later).await, 0);
        assert_eq!(expire(&dir, Duration::from_secs(60), later).await, 1);
        assert!(!ev.join("100.mp4").exists());
        assert!(ev.join("notes.txt").exists() && ev.join("200.mp4.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn space_check() {
        let dir = std::env::temp_dir();
        assert!(space_ok(&dir, 0).await);
        assert!(space_ok(&dir, 1).await);
        assert!(!space_ok(&dir, u64::MAX).await);
    }

    #[tokio::test]
    async fn create_never_overwrites() {
        let dir = scratch("create");
        let p = StreamPath::parse("live/event-a").expect("path");
        let (a, _) = create(&dir, &p, 1000).await.expect("first");
        let (b, _) = create(&dir, &p, 1000).await.expect("second");
        assert!(a.ends_with("live/event-a/1000.mp4"));
        assert!(b.ends_with("live/event-a/1001.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
