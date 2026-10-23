//! Fragmented MP4 recording per codec epoch. Scoped files include the callback
//! recording identity after their real Unix start second; legacy names remain.
//!
//! The file is the init segment followed by every LL-HLS part, which is a
//! valid fragmented MP4 that plays in a browser `<video>`. Writes happen on
//! their own task so a slow disk never stalls ingest.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sapflow_core::recording::{is_recording_name, recording_file, scoped_recording_file};
use sapflow_core::{RecordingTracks, StreamEvent, StreamPath};
use sapflow_hls::fmp4::{init_segment_for_tracks, rebase_part_for_file};
use sapflow_hls::muxer::Part;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::callback::Callbacks;
use crate::publish_observer::RecordingObservation;
use crate::stream::Tracks;

/// Writes behind ingest by at most this many parts (about 50 s) before
/// recording gives up rather than buffer without bound.
const QUEUE: usize = 256;

enum Msg {
    Init(Tracks),
    Part(Arc<Part>),
}

/// Cold construction policy for the currently accepted connection. Keeping it
/// on the session lets terminal-only codec changes start the right scoped
/// recording without retaining an App or any elementary media.
pub struct RecorderFactory {
    dir: PathBuf,
    path: StreamPath,
    callbacks: Callbacks,
    min_free: u64,
    runtime: tokio::runtime::Handle,
}

impl RecorderFactory {
    pub fn new(dir: &Path, path: &StreamPath, callbacks: Callbacks, min_free: u64) -> Self {
        Self {
            dir: dir.to_owned(),
            path: path.clone(),
            callbacks,
            min_free,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    pub fn start(&self) -> Recorder {
        Recorder::start_on(
            &self.runtime,
            &self.dir,
            &self.path,
            self.callbacks.clone(),
            self.min_free,
        )
    }
}

pub struct Recorder {
    tx: Option<mpsc::Sender<Msg>>,
    started: bool,
    initialization_id: Option<u64>,
    last_part: Option<u64>,
    observation: Option<RecordingObservation>,
    incomplete: Arc<AtomicBool>,
}

impl Recorder {
    pub fn start(dir: &Path, path: &StreamPath, callbacks: Callbacks, min_free: u64) -> Self {
        Self::start_on(
            &tokio::runtime::Handle::current(),
            dir,
            path,
            callbacks,
            min_free,
        )
    }

    /// The factory retains the originating runtime handle. Terminal connection
    /// Drop may run outside an ambient Tokio context; spawning on this handle
    /// keeps writer ownership on that executor without creating another one.
    /// The runtime itself must remain alive through actual writer completion.
    pub fn start_on(
        runtime: &tokio::runtime::Handle,
        dir: &Path,
        path: &StreamPath,
        callbacks: Callbacks,
        min_free: u64,
    ) -> Self {
        let incomplete = Arc::new(AtomicBool::new(false));
        let Ok((callbacks, observation)) = callbacks.for_recording() else {
            return Self {
                tx: None,
                started: false,
                initialization_id: None,
                last_part: None,
                observation: None,
                incomplete,
            };
        };
        let (tx, rx) = mpsc::channel(QUEUE);
        runtime.spawn(write(
            dir.to_owned(),
            path.clone(),
            rx,
            callbacks,
            min_free,
            observation.clone(),
            incomplete.clone(),
        ));
        Self {
            tx: Some(tx),
            started: false,
            initialization_id: None,
            last_part: None,
            observation,
            incomplete,
        }
    }

    pub fn last_part(&self) -> Option<u64> {
        self.last_part
    }

    /// Preserve and sync the accepted prefix, but never describe it as a
    /// complete recording after a definite upstream media gap.
    pub fn mark_incomplete(&self) {
        self.incomplete.store(true, Ordering::Release);
        // A rejected connection before its first Init creates no recording.
        if self.started
            && let Some(observation) = &self.observation
        {
            observation.incomplete();
        }
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
            self.initialization_id = parts.first().map(|part| part.initialization.id);
            ok = tx.try_send(Msg::Init(tracks)).is_ok();
        }
        for p in parts {
            if !ok {
                break;
            }
            // Ingest can seal a changed epoch and then fail later in the
            // same batch, before its restart update reaches Publication.
            // Never append that codec's samples under this file's old init.
            if self.initialization_id != Some(p.initialization.id) {
                ok = false;
                break;
            }
            let id = p.id;
            ok = tx.try_send(Msg::Part(p)).is_ok();
            if ok {
                self.last_part = Some(id);
            }
        }
        if !ok {
            self.mark_incomplete();
            tracing::error!("recording cannot accept another part; stopping this recording");
            self.tx = None;
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Channel closure requests finalisation; it never reports success.
        self.tx = None;
        if let Some(observation) = &self.observation {
            observation.finalising();
        }
    }
}

struct WriteReceipt {
    observation: Option<RecordingObservation>,
    completed: bool,
}
impl Drop for WriteReceipt {
    fn drop(&mut self) {
        if !self.completed
            && let Some(observation) = &self.observation
        {
            observation.failed();
        }
    }
}

trait RecordingFile: tokio::io::AsyncWrite + tokio::io::AsyncSeek + Unpin + Send {
    fn sync_recording(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;
}
impl RecordingFile for tokio::fs::File {
    fn sync_recording(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send {
        self.sync_all()
    }
}

async fn finalise_file(
    file: &mut impl RecordingFile,
    initial_len: usize,
    final_init: &[u8],
) -> std::io::Result<()> {
    if final_init.len() != initial_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recording header size changed",
        ));
    }
    file.seek(std::io::SeekFrom::Start(0)).await?;
    file.write_all(final_init).await?;
    file.flush().await?;
    file.sync_recording().await
}

/// Whether the recordings disk has at least `min_free` bytes available.
async fn space_ok(dir: &Path, min_free: u64) -> bool {
    if min_free == 0 {
        return true;
    }
    let dir = dir.to_owned();
    match tokio::task::spawn_blocking(move || fs4::available_space(&dir)).await {
        Ok(Ok(free)) => free >= min_free,
        // A configured free-space floor cannot be established on an error.
        _ => false,
    }
}

async fn write(
    dir: PathBuf,
    path: StreamPath,
    mut rx: mpsc::Receiver<Msg>,
    callbacks: Callbacks,
    min_free: u64,
    observation: Option<RecordingObservation>,
    incomplete: Arc<AtomicBool>,
) {
    // Wait for the first part before creating anything on disk.
    let Some(Msg::Init(tracks)) = rx.recv().await else {
        return;
    };
    let Some(recording_tracks) =
        RecordingTracks::new(tracks.video().is_some(), tracks.audio().is_some())
    else {
        return;
    };
    let mut receipt = WriteReceipt {
        observation: observation.clone(),
        completed: false,
    };
    let Some(init) = init_segment_for_tracks(tracks.video(), tracks.audio(), Duration::ZERO) else {
        return;
    };
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    if !space_ok(&dir, min_free).await {
        tracing::error!(path = %path.as_path(), "not recording: recordings disk is nearly full");
        return;
    }
    let created = if let Some(observation) = &observation {
        create_scoped(&dir, &path, start, observation.id()).await
    } else {
        create(&dir, &path, start).await
    };
    let Some((file_path, mut file)) = created else {
        return;
    };
    if let Some(observation) = &observation {
        observation.active();
    }
    let mut duration = Duration::ZERO;
    let mut origin = None;
    let mut failed = file.write_all(&init).await.is_err();
    let mut parts = 0u64;
    if failed {
        rx.close();
    }
    while let Some(msg) = rx.recv().await {
        if failed {
            break;
        }
        if let Msg::Part(p) = msg {
            // About every 10 s of media.
            parts += 1;
            if parts.is_multiple_of(50) && !space_ok(&dir, min_free).await {
                tracing::error!(file = %file_path.display(), "recordings disk nearly full; recording stops here");
                incomplete.store(true, Ordering::Release);
                if let Some(observation) = &observation {
                    observation.incomplete();
                }
                rx.close();
                break;
            }
            let output = match rebase_part_for_file(
                &p.data,
                tracks.audio().map(|a| a.sample_rate),
                &mut origin,
            ) {
                Ok(output) => output,
                Err(_) => {
                    failed = true;
                    rx.close();
                    break;
                }
            };
            failed = file.write_all(&output).await.is_err();
            if failed {
                rx.close();
                break;
            }
            duration = duration.saturating_add(p.span);
        }
    }
    if let Some(observation) = &observation {
        observation.finalising();
    }
    // Now the length is known: rewrite the init in place (same size).
    let Some(final_init) = init_segment_for_tracks(tracks.video(), tracks.audio(), duration) else {
        tracing::error!(file = %file_path.display(), "recording header is invalid");
        return;
    };
    if failed
        || finalise_file(&mut file, init.len(), &final_init)
            .await
            .is_err()
    {
        tracing::error!(file = %file_path.display(), "recording write failed");
        return;
    }
    let complete = !incomplete.load(Ordering::Acquire);
    if let Some(observation) = &observation {
        if complete {
            observation.finalised();
        } else {
            observation.synced_incomplete();
        }
    }
    receipt.completed = true;
    tracing::info!(file = %file_path.display(), secs = duration.as_secs_f64(), complete, "recording finished");
    // Scoped callbacks say whether the file is complete (`recordingState`).
    // The legacy event means only that the file is finished, as mediamtx's
    // does: an encoder drop leaves a gap, and v4v still takes the file as the
    // replay, judging it by its duration.
    callbacks.send(
        &path,
        StreamEvent::RecordSegmentComplete {
            segment_path: file_path.display().to_string(),
            duration_secs: (duration.as_secs_f64() * 1000.0).round() / 1000.0,
            recording_tracks,
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

async fn create_scoped(
    dir: &Path,
    path: &StreamPath,
    start: u64,
    recording_id: &str,
) -> Option<(PathBuf, tokio::fs::File)> {
    let file_path = dir.join(scoped_recording_file(path, start, recording_id)?);
    if let Some(parent) = file_path.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        tracing::error!(%error, "cannot create recording directory");
        return None;
    }
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
        .await
    {
        Ok(file) => Some((file_path, file)),
        Err(error) => {
            // A duplicate identity must neither overwrite the existing file
            // nor invent a later wall-clock start to hide the collision.
            tracing::error!(%error, "cannot create scoped recording");
            None
        }
    }
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

    struct ScopedScratch(PathBuf);
    impl ScopedScratch {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "sapflow-observer-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create unique owned scratch");
            Self(path)
        }
    }
    impl Drop for ScopedScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture_parts() -> (Tracks, Vec<Arc<Part>>) {
        parts_from_generated(include_bytes!(
            "../../sapflow-hls/tests/fixtures/testsrc-3s.ts"
        ))
    }
    fn parts_from_generated(data: &[u8]) -> (Tracks, Vec<Arc<Part>>) {
        let mut ingest = sapflow_hls::TsIngest::new(sapflow_hls::MuxerConfig::default());
        for bytes in data.chunks(1316) {
            ingest
                .push(bytes, SystemTime::UNIX_EPOCH)
                .expect("generated fixture");
        }
        let mut muxer = ingest.take_muxer().expect("muxer");
        muxer.interrupt(SystemTime::UNIX_EPOCH);
        let tracks = Tracks::new(muxer.video().cloned(), muxer.audio().copied())
            .expect("generated nonempty tracks");
        let parts = (0..muxer.next_part_id())
            .map(|id| match muxer.part(id) {
                sapflow_hls::PartLookup::Found(part) => part,
                _ => panic!("generated part retained"),
            })
            .collect();
        (tracks, parts)
    }

    #[test]
    fn recording_track_identity_refuses_an_empty_initialization() {
        assert!(Tracks::new(None, None).is_none());
    }

    #[tokio::test]
    async fn audio_only_recordings_sync_without_video_and_preserve_incomplete_status() {
        const MONO: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/audio-48k-mono.ts");
        const STEREO: &[u8] =
            include_bytes!("../../sapflow-hls/tests/fixtures/audio-44k1-stereo.ts");
        for (data, rate, channels) in [(MONO, 48_000, 1), (STEREO, 44_100, 2)] {
            for incomplete in [false, true] {
                let scratch = ScopedScratch::new("generated-audio");
                let scope = crate::publish_observer::fixture_scope("audio", 1, 1, 1);
                let (callbacks, mut receiver) = crate::callback::test_queue(scope.clone(), 2);
                let (tracks, parts) = parts_from_generated(data);
                assert!(tracks.video().is_none());
                let audio = tracks.audio().expect("AAC track");
                assert_eq!((audio.sample_rate, audio.channels), (rate, channels));
                assert!(parts.len() > 10, "real independently written audio parts");
                let duration = parts.iter().fold(Duration::ZERO, |d, part| d + part.span);
                assert!(duration > Duration::from_secs(5));
                let header = init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
                    .expect("nonempty audio initialization");
                let mut recorder = Recorder::start(&scratch.0, &scope.key().path, callbacks, 0);
                recorder.feed(tracks, parts);
                if incomplete {
                    recorder.mark_incomplete();
                }
                drop(recorder);
                let queued = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
                    .await
                    .expect("actual writer completion deadline")
                    .expect("completion callback");
                let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
                let expected = if incomplete {
                    "incomplete"
                } else {
                    "finalised"
                };
                assert_eq!(body["recordingState"], expected);
                assert_eq!(
                    body["recordingTracks"],
                    serde_json::json!({"hasVideo": false, "hasAudio": true})
                );
                assert_eq!(scope.observations.recording_state(), expected);
                let bytes = tokio::fs::read(body["segmentPath"].as_str().expect("file path"))
                    .await
                    .expect("synced generated file");
                assert!(bytes.len() > header.len());
                assert_eq!(&bytes[..header.len()], header.as_ref());
                assert_eq!(receiver.len(), 0, "one completion per actual writer");
            }
        }
    }

    async fn await_recording_state(scope: &crate::publish_observer::CallbackScope, expected: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while scope.observations.recording_state() != expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bounded recording completion");
    }

    #[tokio::test]
    async fn unused_recorder_has_no_file_completion_or_failure_receipt() {
        let scratch = ScopedScratch::new("unused");
        let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
        let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 1);
        let recorder = Recorder::start(&scratch.0, &scope.key().path, callbacks, 0);
        recorder.mark_incomplete();
        drop(recorder);
        tokio::task::yield_now().await;
        assert_eq!(scope.observations.recording_state(), "never-started");
        assert_eq!(receiver.len(), 0);
        assert!(!scratch.0.join("live").exists());
    }

    #[tokio::test]
    async fn writer_refuses_missing_or_out_of_order_initialization_without_completion() {
        for part_before_init in [false, true] {
            let scratch = ScopedScratch::new("missing-init");
            let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 1);
            let (callbacks, observation) = callbacks.for_recording().expect("writer");
            let (tx, rx) = mpsc::channel(2);
            if part_before_init {
                let (_, parts) = fixture_parts();
                tx.send(Msg::Part(parts[0].clone()))
                    .await
                    .expect("first part");
            }
            drop(tx);
            write(
                scratch.0.clone(),
                scope.key().path.clone(),
                rx,
                callbacks,
                0,
                observation,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            assert_eq!(receiver.len(), 0);
            assert_eq!(scope.observations.recording_state(), "never-started");
            assert!(!scratch.0.join("live").exists());
        }
    }

    #[tokio::test]
    async fn real_file_completion_is_synced_before_scoped_callback_and_keeps_old_identity() {
        let scratch = ScopedScratch::new("complete");
        let scope = crate::publish_observer::fixture_scope("a", 1, 11, 3);
        let (callbacks, mut receiver) = crate::callback::test_queue(scope.clone(), 4);
        let (tracks, parts) = fixture_parts();
        let duration = parts
            .iter()
            .fold(Duration::ZERO, |duration, part| duration + part.span);
        let expected_header = init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
            .expect("generated recording header");
        let mut recorder = Recorder::start(&scratch.0, &scope.key().path, callbacks.clone(), 0);
        recorder.feed(tracks, parts);
        assert_ne!(scope.observations.recording_state(), "finalised");
        drop(recorder); // Closes admission to the writer, not a success receipt.
        let replacement = crate::publish_observer::fixture_scope("a", 2, 22, 4);
        let _new_callbacks = callbacks.scoped(replacement);
        let queued = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .expect("callback deadline")
            .expect("completion");
        let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
        assert_eq!(scope.observations.recording_state(), "finalised");
        assert_eq!(scope.observations.callback_state(), "pending");
        assert_eq!(body["event"], "recordSegmentComplete");
        assert_eq!(body["recordingState"], "finalised");
        assert_eq!(
            body["recordingTracks"],
            serde_json::json!({"hasVideo": true, "hasAudio": true})
        );
        assert_eq!(body["sessionId"], scope.key().session_id.as_str());
        assert_eq!(body["eventGeneration"], 1);
        assert_eq!(body["connectionEpoch"], 3);
        assert_eq!(body["recordingId"].as_str().expect("ID").len(), 32);
        let name = Path::new(body["segmentPath"].as_str().expect("path"))
            .file_name()
            .expect("recording name")
            .to_str()
            .expect("UTF-8");
        assert!(is_recording_name(name));
        assert!(name.ends_with(&format!(
            "-{}.mp4",
            body["recordingId"].as_str().expect("ID")
        )));
        let bytes = tokio::fs::read(body["segmentPath"].as_str().expect("path"))
            .await
            .expect("read synced file");
        assert!(bytes.len() > expected_header.len());
        assert_eq!(&bytes[..expected_header.len()], expected_header.as_ref());
        drop(queued); // Queue consumption alone is not HTTP acknowledgement.
        assert_eq!(scope.observations.callback_state(), "failed");
    }

    #[tokio::test]
    async fn real_low_space_and_create_failure_report_failure_without_file_publication() {
        for low_space in [true, false] {
            let scratch = ScopedScratch::new(if low_space {
                "low-space"
            } else {
                "create-failure"
            });
            let path = scratch.0.join("recordings");
            if !low_space {
                std::fs::write(&path, b"directory creation must fail").expect("file barrier");
            }
            let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
            let mut recorder = Recorder::start(
                &path,
                &scope.key().path,
                callbacks,
                if low_space { u64::MAX } else { 0 },
            );
            let (tracks, parts) = fixture_parts();
            recorder.feed(tracks, parts);
            drop(recorder);
            await_recording_state(&scope, "failed").await;
            assert_eq!(receiver.len(), 0);
            assert_eq!(scope.observations.counts().recordings_finalised, 0);
            assert!(!path.join("live/event-a").exists());
        }
    }

    #[test]
    fn actual_feed_queue_overflow_stops_admission_and_marks_incomplete() {
        let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
        let (callbacks, _callbacks) = crate::callback::test_queue(scope.clone(), 2);
        let (_, observation) = callbacks.for_recording().expect("recording");
        let (sender, receiver) = mpsc::channel(2);
        let mut recorder = Recorder {
            tx: Some(sender),
            started: false,
            initialization_id: None,
            last_part: None,
            observation,
            incomplete: Arc::new(AtomicBool::new(false)),
        };
        let (tracks, parts) = fixture_parts();
        let first_id = parts[0].id;
        recorder.feed(tracks, parts);
        assert!(recorder.tx.is_none());
        assert!(recorder.incomplete.load(Ordering::Acquire));
        assert_eq!(receiver.len(), 2);
        assert_eq!(recorder.last_part(), Some(first_id));
        assert_eq!(scope.observations.recording_state(), "incomplete");
        assert_eq!(scope.observations.counts().recordings_finalised, 0);
    }

    #[tokio::test]
    async fn a_legacy_recording_with_a_gap_still_reports_it_finished() {
        let scratch = ScopedScratch::new("legacy-gap");
        let path = StreamPath::parse("live/event-a").expect("path");
        let (callbacks, mut receiver) = crate::callback::legacy_test_queue(2);
        let (sender, input) = mpsc::channel(2);
        let (tracks, mut parts) = fixture_parts();
        assert!(sender.try_send(Msg::Init(tracks)).is_ok());
        assert!(sender.try_send(Msg::Part(parts.remove(0))).is_ok());
        drop(sender);
        // An SRT timeout or dropped packets mark the recording incomplete.
        let incomplete = Arc::new(AtomicBool::new(true));
        write(
            scratch.0.clone(),
            path,
            input,
            callbacks,
            0,
            None,
            incomplete,
        )
        .await;
        let queued = receiver.try_recv().expect("recordSegmentComplete");
        let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
        assert_eq!(body["event"], "recordSegmentComplete");
        assert!(body.get("recordingState").is_none());
        assert!(
            std::fs::metadata(body["segmentPath"].as_str().expect("path"))
                .expect("file")
                .len()
                > 0
        );
    }

    #[tokio::test]
    async fn synced_incomplete_prefix_is_distinguished_and_rebase_failure_publishes_nothing() {
        for invalid in [false, true] {
            let scratch = ScopedScratch::new(if invalid {
                "bad-fragment"
            } else {
                "partial-prefix"
            });
            let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, mut receiver) = crate::callback::test_queue(scope.clone(), 2);
            let (callbacks, observation) = callbacks.for_recording().expect("recording");
            let (sender, input) = mpsc::channel(2);
            let (tracks, mut parts) = fixture_parts();
            let part = parts.remove(0);
            let part = if invalid {
                Arc::new(Part {
                    initialization: part.initialization.clone(),
                    id: part.id,
                    duration: part.duration,
                    span: part.span,
                    independent: part.independent,
                    data: bytes::Bytes::from_static(b"invalid-moof"),
                })
            } else {
                part
            };
            assert!(sender.try_send(Msg::Init(tracks)).is_ok());
            assert!(sender.try_send(Msg::Part(part)).is_ok());
            drop(sender);
            let incomplete = Arc::new(AtomicBool::new(!invalid));
            if !invalid {
                observation.as_ref().expect("observation").incomplete();
            }
            write(
                scratch.0.clone(),
                scope.key().path.clone(),
                input,
                callbacks,
                0,
                observation,
                incomplete,
            )
            .await;
            if invalid {
                assert_eq!(scope.observations.recording_state(), "failed");
                assert!(receiver.try_recv().is_err());
            } else {
                let body: serde_json::Value =
                    serde_json::from_slice(&receiver.try_recv().expect("partial completion").body)
                        .expect("JSON");
                assert_eq!(
                    body["recordingTracks"],
                    serde_json::json!({"hasVideo": true, "hasAudio": true})
                );
                assert_eq!(body["recordingState"], "incomplete");
                assert_eq!(scope.observations.recording_state(), "incomplete");
                assert_eq!(scope.observations.counts().recordings_finalised, 0);
                assert!(
                    std::fs::metadata(body["segmentPath"].as_str().expect("path"))
                        .expect("synced prefix")
                        .len()
                        > 0
                );
            }
        }
    }

    struct FaultFile {
        data: std::io::Cursor<Vec<u8>>,
        fault: &'static str,
        calls: Vec<&'static str>,
    }
    impl tokio::io::AsyncWrite for FaultFile {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            this.calls.push("write");
            std::task::Poll::Ready(if this.fault == "write" {
                Err(std::io::Error::other("write fault"))
            } else {
                std::io::Write::write(&mut this.data, bytes)
            })
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            this.calls.push("flush");
            std::task::Poll::Ready(if this.fault == "flush" {
                Err(std::io::Error::other("flush fault"))
            } else {
                Ok(())
            })
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    impl tokio::io::AsyncSeek for FaultFile {
        fn start_seek(
            self: std::pin::Pin<&mut Self>,
            position: std::io::SeekFrom,
        ) -> std::io::Result<()> {
            let this = self.get_mut();
            this.calls.push("seek");
            if this.fault == "seek" {
                Err(std::io::Error::other("seek fault"))
            } else {
                std::io::Seek::seek(&mut this.data, position).map(|_| ())
            }
        }
        fn poll_complete(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<u64>> {
            std::task::Poll::Ready(Ok(self.get_mut().data.position()))
        }
    }
    impl RecordingFile for FaultFile {
        fn sync_recording(
            &mut self,
        ) -> impl std::future::Future<Output = std::io::Result<()>> + Send {
            self.calls.push("sync");
            std::future::ready(if self.fault == "sync" {
                Err(std::io::Error::other("sync fault"))
            } else {
                Ok(())
            })
        }
    }
    #[tokio::test]
    async fn finalisation_requires_header_write_flush_and_sync_and_faults_never_publish() {
        for fault in ["seek", "write", "flush", "sync", "header", "none"] {
            let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
            let (callbacks, observation) = callbacks.for_recording().expect("recording");
            let observation = observation.expect("scoped");
            observation.active();
            observation.finalising();
            let mut guard = WriteReceipt {
                observation: Some(observation.clone()),
                completed: false,
            };
            let mut file = FaultFile {
                data: std::io::Cursor::new(b"old-headmedia".to_vec()),
                fault,
                calls: Vec::new(),
            };
            let result = finalise_file(
                &mut file,
                if fault == "header" { 99 } else { 8 },
                b"new-head",
            )
            .await;
            if result.is_ok() {
                observation.finalised();
                guard.completed = true;
                assert_eq!(file.calls, ["seek", "write", "flush", "sync"]);
                assert_eq!(file.data.into_inner().as_slice(), b"new-headmedia");
            }
            drop(guard);
            callbacks.send(
                &scope.key().path,
                StreamEvent::RecordSegmentComplete {
                    segment_path: "/fixture/recording.mp4".into(),
                    duration_secs: 1.0,
                    recording_tracks: RecordingTracks::AudioVideo,
                },
            );
            if fault == "none" {
                assert_eq!(scope.observations.recording_state(), "finalised");
                assert_eq!(receiver.len(), 1);
            } else {
                assert!(result.is_err());
                assert_eq!(scope.observations.recording_state(), "failed");
                assert_eq!(scope.observations.counts().recordings_finalised, 0);
                assert_eq!(receiver.len(), 0);
            }
        }
    }

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
        let scoped = "100-0123456789abcdef0123456789abcdef.mp4";
        let near_match = "100-0123456789abcdef0123456789abcdeg.mp4";
        for f in ["100.mp4", scoped, near_match, "notes.txt", "200.mp4.tmp"] {
            std::fs::write(ev.join(f), b"x").expect("write");
        }
        let later = SystemTime::now() + Duration::from_secs(3600);
        assert_eq!(expire(&dir, Duration::from_secs(7200), later).await, 0);
        assert_eq!(expire(&dir, Duration::from_secs(60), later).await, 2);
        assert!(!ev.join("100.mp4").exists() && !ev.join(scoped).exists());
        assert!(ev.join(near_match).exists());
        assert!(ev.join("notes.txt").exists() && ev.join("200.mp4.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn space_check() {
        let dir = std::env::temp_dir();
        assert!(space_ok(&dir, 0).await);
        assert!(space_ok(&dir, 1).await);
        assert!(!space_ok(&dir, u64::MAX).await);
        let scratch = ScopedScratch::new("unknown-space");
        assert!(!space_ok(&scratch.0.join("missing"), 1).await);
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

    #[tokio::test]
    async fn scoped_epochs_share_the_real_start_without_overwrite_or_sixty_file_limit() {
        let scratch = ScopedScratch::new("scoped-create");
        let path = StreamPath::parse("live/event-a").expect("path");
        let first_id = format!("{:032x}", 1);
        let (first, mut file) = create_scoped(&scratch.0, &path, 1000, &first_id)
            .await
            .expect("first recording");
        file.write_all(b"generated-prefix").await.expect("write");
        file.flush().await.expect("flush");
        drop(file);
        assert!(
            create_scoped(&scratch.0, &path, 1000, &first_id)
                .await
                .is_none(),
            "a repeated callback identity cannot replace its file"
        );
        assert_eq!(
            tokio::fs::read(&first).await.expect("existing bytes"),
            b"generated-prefix"
        );
        for value in 2..=64 {
            let id = format!("{value:032x}");
            let (file_path, file) = create_scoped(&scratch.0, &path, 1000, &id)
                .await
                .expect("distinct epoch in the same second");
            drop(file);
            assert!(file_path.ends_with(format!("live/event-a/1000-{id}.mp4")));
        }
        assert!(
            create_scoped(&scratch.0, &path, 1000, "../invalid")
                .await
                .is_none()
        );
    }
}
