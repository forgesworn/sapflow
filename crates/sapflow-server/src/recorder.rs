//! Fragmented MP4 recording per codec epoch. Scoped files include the callback
//! recording identity after their real Unix start second; legacy names remain.
//!
//! The file is the init segment followed by every LL-HLS part, which is a
//! valid fragmented MP4 that plays in a browser `<video>`. Writes happen on
//! their own task so a slow disk never stalls ingest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sapflow_core::recording::{is_recording_name, recording_file, scoped_recording_file};
use sapflow_core::{RecordingTracks, StreamEvent, StreamPath};
use sapflow_hls::fmp4::{
    FileTimeBase, MAX_FILE_FRAGMENT_BYTES, init_segment_for_tracks, rebase_part_for_file,
};
use sapflow_hls::muxer::Part;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::callback::Callbacks;
use crate::publish_observer::RecordingObservation;
use crate::stream::Tracks;

/// Writes behind ingest by at most this many parts (about 50 s) before
/// recording gives up rather than buffer without bound.
const QUEUE: usize = 256;
/// Includes queued originals and active originals across every epoch and
/// connection generation of one path. File buffers are separate storage.
const RETAINED_PART_BYTES: usize = 64 * 1024 * 1024;
const GLOBAL_RETAINED_PART_BYTES: usize = 512 * 1024 * 1024;
const PATH_WRITERS: usize = 2;
const MAX_WRITERS: usize = 1024;

/// Shared by the App, factories and draining writers. These limits count
/// retained part lengths, not allocations, cache storage or total process RSS.
pub struct RecordingLimits {
    bytes: Arc<Semaphore>,
    writers: Arc<Semaphore>,
    paths: Mutex<HashMap<String, Weak<PathLimits>>>,
    path_bytes: usize,
    path_writers: usize,
    #[cfg(test)]
    test_files: Mutex<Option<Arc<tests::GatedFiles>>>,
}

impl RecordingLimits {
    pub fn new(max_publishers: usize) -> Self {
        // SRT admits N publishers and RTMP admits up to 4N connections. Allow
        // one draining predecessor each, with an absolute file/task ceiling.
        let writers = max_publishers.saturating_mul(10).clamp(1, MAX_WRITERS);
        Self::with_capacities(
            GLOBAL_RETAINED_PART_BYTES,
            RETAINED_PART_BYTES,
            writers,
            PATH_WRITERS,
        )
    }

    fn with_capacities(
        bytes: usize,
        path_bytes: usize,
        writers: usize,
        path_writers: usize,
    ) -> Self {
        Self {
            // The ceiling remains valid when Semaphore's representable count
            // is smaller, notably for a 512 MiB byte count on 32-bit targets.
            bytes: Arc::new(Semaphore::new(bytes.min(Semaphore::MAX_PERMITS))),
            writers: Arc::new(Semaphore::new(writers.min(Semaphore::MAX_PERMITS))),
            paths: Mutex::new(HashMap::new()),
            path_bytes: path_bytes.min(Semaphore::MAX_PERMITS),
            path_writers: path_writers.min(Semaphore::MAX_PERMITS),
            #[cfg(test)]
            test_files: Mutex::new(None),
        }
    }

    fn path(&self, path: &StreamPath) -> Arc<PathLimits> {
        let canonical = path.as_path();
        let mut paths = self.paths.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(limits) = paths.get(canonical.as_str()).and_then(Weak::upgrade) {
            return limits;
        }
        // A succession of distinct, completed paths must not leave an
        // ever-growing index. Only live factories/writers keep a path alive.
        paths.retain(|_, limits| limits.strong_count() != 0);
        let limits = Arc::new(PathLimits {
            bytes: Arc::new(Semaphore::new(self.path_bytes)),
            writers: Arc::new(Semaphore::new(self.path_writers)),
        });
        paths.insert(canonical, Arc::downgrade(&limits));
        limits
    }
}

struct PathLimits {
    bytes: Arc<Semaphore>,
    writers: Arc<Semaphore>,
}

struct WriterReservation {
    _global: OwnedSemaphorePermit,
    _path: OwnedSemaphorePermit,
    // Keep the Weak index upgradeable until the old writer has dropped its
    // file and queue, even after the original factory/connection disappears.
    _limits: Arc<PathLimits>,
}

impl WriterReservation {
    fn acquire(global: &RecordingLimits, path: &Arc<PathLimits>) -> Option<Self> {
        let local = path.writers.clone().try_acquire_owned().ok()?;
        let global = global.writers.clone().try_acquire_owned().ok()?;
        Some(Self {
            _global: global,
            _path: local,
            _limits: path.clone(),
        })
    }
}

#[derive(Default)]
struct RecordingStatus {
    state: AtomicU8,
}

impl RecordingStatus {
    const INCOMPLETE: u8 = 1;
    const FATAL: u8 = 2;
    const COMMITTED: u8 = 4;

    fn mark(&self, flag: u8) -> bool {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & Self::COMMITTED == 0).then_some(state | flag)
            })
            .is_ok()
    }

    fn mark_incomplete(&self) -> bool {
        self.mark(Self::INCOMPLETE)
    }

    fn mark_fatal(&self) -> bool {
        self.mark(Self::FATAL)
    }

    fn is_fatal(&self) -> bool {
        self.state.load(Ordering::Acquire) & Self::FATAL != 0
    }

    fn accepts_input(&self) -> bool {
        self.state.load(Ordering::Acquire) & (Self::FATAL | Self::COMMITTED) == 0
    }

    /// The writer claims publication only after sync. This same CAS freezes
    /// incomplete/fatal input decisions, including when it closed the receiver
    /// itself (for example low disk) while the producer still has a sender.
    fn try_commit(&self) -> Option<bool> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & (Self::FATAL | Self::COMMITTED) == 0).then_some(state | Self::COMMITTED)
            })
            .ok()
            .map(|state| state & Self::INCOMPLETE == 0)
    }
}

struct PartBudget {
    bytes: Arc<Semaphore>,
    global_bytes: Option<Arc<Semaphore>>,
    fragment_limit: usize,
}

impl PartBudget {
    #[cfg(test)]
    fn new(retained_bytes: usize, fragment_limit: usize) -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(retained_bytes)),
            global_bytes: None,
            fragment_limit,
        }
    }

    fn retain(&self, part: Arc<Part>) -> Result<RetainedPart, PartRefusal> {
        if part.data.len() > self.fragment_limit {
            return Err(PartRefusal::Oversized);
        }
        let bytes = u32::try_from(part.data.len()).map_err(|_| PartRefusal::Oversized)?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| PartRefusal::Full)?;
        let global = self
            .global_bytes
            .as_ref()
            .map(|pool| pool.clone().try_acquire_many_owned(bytes))
            .transpose()
            .map_err(|_| PartRefusal::Full)?;
        Ok(RetainedPart {
            part,
            _bytes: permit,
            _global_bytes: global,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PartRefusal {
    Oversized,
    Full,
}

struct RetainedPart {
    // Fields drop in declaration order, returning credit after the original.
    part: Arc<Part>,
    _bytes: OwnedSemaphorePermit,
    _global_bytes: Option<OwnedSemaphorePermit>,
}

struct WritingPart {
    // Drop the rebased copy, then the original, then its byte reservation.
    output: bytes::Bytes,
    retained: RetainedPart,
}

enum Msg {
    Init(Tracks),
    Part(RetainedPart),
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
    limits: Arc<RecordingLimits>,
    path_limits: Arc<PathLimits>,
}

impl RecorderFactory {
    pub fn new(
        dir: &Path,
        path: &StreamPath,
        callbacks: Callbacks,
        min_free: u64,
        limits: Arc<RecordingLimits>,
    ) -> Self {
        let path_limits = limits.path(path);
        Self {
            dir: dir.to_owned(),
            path: path.clone(),
            callbacks,
            min_free,
            runtime: tokio::runtime::Handle::current(),
            limits,
            path_limits,
        }
    }

    pub fn start(&self) -> Recorder {
        Recorder::from_plan(WriterPlan {
            runtime: self.runtime.clone(),
            dir: self.dir.clone(),
            path: self.path.clone(),
            callbacks: self.callbacks.clone(),
            min_free: self.min_free,
            limits: self.limits.clone(),
            path_limits: self.path_limits.clone(),
        })
    }
}

/// No task, file or elementary media exists until the first part is admitted.
struct WriterPlan {
    runtime: tokio::runtime::Handle,
    dir: PathBuf,
    path: StreamPath,
    callbacks: Callbacks,
    min_free: u64,
    limits: Arc<RecordingLimits>,
    path_limits: Arc<PathLimits>,
}

impl WriterPlan {
    fn spawn(
        self,
        rx: mpsc::Receiver<Msg>,
        observation: Option<RecordingObservation>,
        status: Arc<RecordingStatus>,
        reservation: WriterReservation,
    ) {
        // Terminal connection Drop can run outside an ambient Tokio context.
        // Keep using the originating runtime and hold the reservation until
        // run has dropped its queue, file and file buffer after finalisation.
        #[cfg(test)]
        let test_files = self
            .limits
            .test_files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        #[cfg(test)]
        let task_files = test_files.clone();
        let runtime = self.runtime;
        let _task = runtime.spawn(async move {
            // The writer quiesces and drops its file before returning this
            // reservation. Production does not expose a per-writer abort;
            // runtime shutdown can still detach Tokio's blocking file work.
            let writer_guard = reservation;
            RecordingWriter {
                dir: self.dir,
                path: self.path,
                callbacks: self.callbacks,
                min_free: self.min_free,
                observation,
                status,
                #[cfg(test)]
                test_files: test_files.clone(),
            }
            .run(rx)
            .await;
            drop(writer_guard);
            #[cfg(test)]
            if let Some(files) = test_files {
                files.writer_finished();
            }
        });
        #[cfg(test)]
        if let Some(files) = task_files {
            files.track(_task.abort_handle());
        }
    }
}

pub struct Recorder {
    tx: Option<mpsc::Sender<Msg>>,
    pending: Option<WriterPlan>,
    started: bool,
    initialization_id: Option<u64>,
    last_part: Option<u64>,
    observation: Option<RecordingObservation>,
    status: Arc<RecordingStatus>,
    budget: PartBudget,
}

impl Recorder {
    #[cfg(test)]
    pub fn start(dir: &Path, path: &StreamPath, callbacks: Callbacks, min_free: u64) -> Self {
        RecorderFactory::new(
            dir,
            path,
            callbacks,
            min_free,
            Arc::new(RecordingLimits::new(1)),
        )
        .start()
    }

    #[cfg(test)]
    fn start_on_with_budget(
        runtime: &tokio::runtime::Handle,
        dir: &Path,
        path: &StreamPath,
        callbacks: Callbacks,
        min_free: u64,
        budget: PartBudget,
    ) -> Self {
        let limits = Arc::new(RecordingLimits::new(1));
        let path_limits = limits.path(path);
        let mut recorder = Self::from_plan(WriterPlan {
            runtime: runtime.clone(),
            dir: dir.to_owned(),
            path: path.clone(),
            callbacks,
            min_free,
            limits,
            path_limits,
        });
        recorder.budget = budget;
        recorder
    }

    fn from_plan(mut plan: WriterPlan) -> Self {
        let budget = PartBudget {
            bytes: plan.path_limits.bytes.clone(),
            global_bytes: Some(plan.limits.bytes.clone()),
            fragment_limit: MAX_FILE_FRAGMENT_BYTES,
        };
        let status = Arc::new(RecordingStatus::default());
        let Ok((callbacks, observation)) = plan.callbacks.for_recording() else {
            return Self {
                tx: None,
                pending: None,
                started: false,
                initialization_id: None,
                last_part: None,
                observation: None,
                status,
                budget,
            };
        };
        plan.callbacks = callbacks;
        Self {
            tx: None,
            pending: Some(plan),
            started: false,
            initialization_id: None,
            last_part: None,
            observation,
            status,
            budget,
        }
    }

    pub fn last_part(&self) -> Option<u64> {
        self.last_part
    }

    /// Whether another sealed part can attempt admission. An incomplete file
    /// can keep recording; a full queue is decided by feed, never silently skipped.
    pub fn is_accepting(&self) -> bool {
        self.status.accepts_input()
            && (self.pending.is_some()
                || self.tx.as_ref().is_some_and(|sender| !sender.is_closed()))
    }

    /// Preserve and sync the accepted prefix, but never describe it as a
    /// complete recording after a definite upstream media gap.
    pub fn mark_incomplete(&self) {
        if !self.status.mark_incomplete() {
            return;
        }
        // A rejected connection before its first Init creates no recording.
        if self.started
            && let Some(observation) = &self.observation
        {
            observation.incomplete();
        }
    }

    /// Queues the init segment (once) and new parts.
    pub fn feed(&mut self, tracks: Tracks, parts: Vec<Arc<Part>>) {
        if !self.status.accepts_input() {
            self.tx = None;
            self.pending = None;
            return;
        }
        let mut parts = parts.into_iter();
        let Some(first) = parts.next() else {
            return;
        };
        let mut first = Some(first);
        if let Some(plan) = self.pending.take() {
            // Retain before Init/spawn. Refusing the first part must not
            // create an empty file or claim a synced incomplete recording.
            let retained = match self.budget.retain(first.take().expect("first part")) {
                Ok(retained) => retained,
                Err(_) => {
                    self.stop_accepting(true);
                    return;
                }
            };
            let Some(reservation) = WriterReservation::acquire(&plan.limits, &plan.path_limits)
            else {
                self.stop_accepting(true);
                return;
            };
            let id = retained.part.id;
            self.initialization_id = Some(retained.part.initialization.id);
            let (tx, rx) = mpsc::channel(QUEUE);
            if tx.try_send(Msg::Init(tracks.clone())).is_err()
                || tx.try_send(Msg::Part(retained)).is_err()
            {
                self.stop_accepting(true);
                return;
            }
            self.started = true;
            self.last_part = Some(id);
            self.tx = Some(tx);
            plan.spawn(
                rx,
                self.observation.clone(),
                self.status.clone(),
                reservation,
            );
        }
        let Some(tx) = &self.tx else { return };
        let mut ok = true;
        if !self.started {
            self.started = true;
            self.initialization_id = first.as_ref().map(|part| part.initialization.id);
            ok = tx.try_send(Msg::Init(tracks)).is_ok();
        }
        for p in first.into_iter().chain(parts) {
            // Ingest can seal a changed epoch and then fail later in the
            // same batch, before its restart update reaches Publication.
            // Never append that codec's samples under this file's old init.
            if self.initialization_id != Some(p.initialization.id) {
                ok = false;
                break;
            }
            let id = p.id;
            let retained = match self.budget.retain(p) {
                Ok(retained) => retained,
                Err(PartRefusal::Oversized) => {
                    // A separate flag cannot be refused by a full/closed queue.
                    // Preserve the writer's fatal/no-callback size semantics.
                    if self.status.mark_fatal()
                        && let Some(observation) = &self.observation
                    {
                        observation.failed();
                    }
                    ok = false;
                    break;
                }
                Err(PartRefusal::Full) => {
                    ok = false;
                    break;
                }
            };
            if !ok {
                break;
            }
            ok = tx.try_send(Msg::Part(retained)).is_ok();
            if ok {
                self.last_part = Some(id);
            }
        }
        if !ok {
            self.stop_accepting(false);
        }
    }

    fn stop_accepting(&mut self, fatal: bool) {
        if fatal {
            if self.status.mark_fatal()
                && let Some(observation) = &self.observation
            {
                observation.failed();
            }
        } else {
            self.mark_incomplete();
        }
        tracing::error!("recording cannot accept another part; stopping this recording");
        self.tx = None;
        self.pending = None;
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

async fn write_part(
    file: &mut (impl tokio::io::AsyncWrite + Unpin),
    retained: RetainedPart,
    audio_rate: Option<u32>,
    origin: &mut Option<FileTimeBase>,
) -> std::io::Result<Duration> {
    let output = rebase_part_for_file(&retained.part.data, audio_rate, origin).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid recording part")
    })?;
    let part = WritingPart { output, retained };
    file.write_all(&part.output).await?;
    let span = part.retained.part.span;
    drop(part);
    Ok(span)
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

fn publish_recording(
    receipt: &mut WriteReceipt,
    complete: bool,
    callbacks: &Callbacks,
    path: &StreamPath,
    file_path: &Path,
    duration: Duration,
    recording_tracks: RecordingTracks,
) {
    if let Some(observation) = &receipt.observation {
        if complete {
            observation.finalised();
        } else {
            observation.synced_incomplete();
        }
    }
    receipt.completed = true;
    tracing::info!(file = %file_path.display(), secs = duration.as_secs_f64(), complete, "recording finished");
    // Scoped callbacks distinguish a complete file from a synced prefix. The
    // legacy event continues to mean only that a file has finished.
    callbacks.send(
        path,
        StreamEvent::RecordSegmentComplete {
            segment_path: file_path.display().to_string(),
            duration_secs: (duration.as_secs_f64() * 1000.0).round() / 1000.0,
            recording_tracks,
        },
    );
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

#[cfg(test)]
async fn write(
    dir: PathBuf,
    path: StreamPath,
    rx: mpsc::Receiver<Msg>,
    callbacks: Callbacks,
    min_free: u64,
    observation: Option<RecordingObservation>,
    status: Arc<RecordingStatus>,
) {
    RecordingWriter {
        dir,
        path,
        callbacks,
        min_free,
        observation,
        status,
        test_files: None,
    }
    .run(rx)
    .await;
}

struct RecordingWriter {
    dir: PathBuf,
    path: StreamPath,
    callbacks: Callbacks,
    min_free: u64,
    observation: Option<RecordingObservation>,
    status: Arc<RecordingStatus>,
    #[cfg(test)]
    test_files: Option<Arc<tests::GatedFiles>>,
}

impl RecordingWriter {
    async fn run(self, mut rx: mpsc::Receiver<Msg>) {
        // Wait for the first part before creating anything on disk.
        let Some(Msg::Init(tracks)) = rx.recv().await else {
            return;
        };
        let receipt = WriteReceipt {
            observation: self.observation.clone(),
            completed: false,
        };
        let Some(init) = init_segment_for_tracks(tracks.video(), tracks.audio(), Duration::ZERO)
        else {
            return;
        };
        #[cfg(test)]
        if let Some(files) = &self.test_files {
            let (path, file) = files.open(&self.path, self.observation.as_ref().map(|o| o.id()));
            self.write_file(file, path, rx, tracks, init, receipt).await;
            return;
        }
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if tokio::fs::create_dir_all(&self.dir).await.is_err() {
            return;
        }
        if !space_ok(&self.dir, self.min_free).await {
            tracing::error!(path = %self.path.as_path(), "not recording: recordings disk is nearly full");
            return;
        }
        let created = if let Some(observation) = &self.observation {
            create_scoped(&self.dir, &self.path, start, observation.id()).await
        } else {
            create(&self.dir, &self.path, start).await
        };
        let Some((file_path, file)) = created else {
            return;
        };
        self.write_file(file, file_path, rx, tracks, init, receipt)
            .await;
    }

    async fn write_file(
        self,
        mut file: impl RecordingFile,
        file_path: PathBuf,
        rx: mpsc::Receiver<Msg>,
        tracks: Tracks,
        init: bytes::Bytes,
        receipt: WriteReceipt,
    ) {
        self.write_open_file(&mut file, &file_path, rx, tracks, init, receipt)
            .await;
        // Tokio can return from write_all after copying into an owned buffer,
        // while blocking I/O still holds that buffer and the underlying file.
        // Every post-open exit, including fatal input and rebase/header errors,
        // must wait for that work before file drop and writer-credit refund.
        // This is an I/O barrier, not fsync or permission to publish a failure.
        if let Err(error) = file.flush().await {
            tracing::error!(file = %file_path.display(), %error, "recording pending write failed");
        }
    }

    async fn write_open_file(
        self,
        file: &mut impl RecordingFile,
        file_path: &Path,
        mut rx: mpsc::Receiver<Msg>,
        tracks: Tracks,
        init: bytes::Bytes,
        mut receipt: WriteReceipt,
    ) {
        let Some(recording_tracks) =
            RecordingTracks::new(tracks.video().is_some(), tracks.audio().is_some())
        else {
            return;
        };
        if let Some(observation) = &self.observation {
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
                if parts.is_multiple_of(50) && !space_ok(&self.dir, self.min_free).await {
                    tracing::error!(file = %file_path.display(), "recordings disk nearly full; recording stops here");
                    self.status.mark_incomplete();
                    if let Some(observation) = &self.observation {
                        observation.incomplete();
                    }
                    rx.close();
                    break;
                }
                match write_part(file, p, tracks.audio().map(|a| a.sample_rate), &mut origin).await
                {
                    Ok(span) => duration = duration.saturating_add(span),
                    Err(_) => {
                        failed = true;
                        rx.close();
                        break;
                    }
                }
            }
        }
        failed |= self.status.is_fatal();
        if let Some(observation) = &self.observation {
            observation.finalising();
        }
        // Now the length is known: rewrite the init in place (same size).
        let Some(final_init) = init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
        else {
            tracing::error!(file = %file_path.display(), "recording header is invalid");
            return;
        };
        if failed || finalise_file(file, init.len(), &final_init).await.is_err() {
            tracing::error!(file = %file_path.display(), "recording write failed");
            return;
        }
        let Some(complete) = self.status.try_commit() else {
            tracing::error!(file = %file_path.display(), "recording input failed");
            return;
        };
        publish_recording(
            &mut receipt,
            complete,
            &self.callbacks,
            &self.path,
            file_path,
            duration,
            recording_tracks,
        );
    }
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[derive(Default)]
    struct GatedState {
        data: std::io::Cursor<Vec<u8>>,
        write_blocked: bool,
        writes_released: bool,
        fail_write: bool,
        sync_blocked: bool,
        sync_released: bool,
        synced: bool,
        dropped: bool,
        buffered_writes: bool,
        pending_write: Option<Vec<u8>>,
        flush_blocked: bool,
        waker: Option<std::task::Waker>,
    }

    /// Only file creation is substituted. Admission, fragment writes, header
    /// rewrite, sync gating, receipts and writer reservations use the real path.
    #[derive(Default)]
    pub(super) struct GatedFiles {
        files: Mutex<Vec<Arc<Mutex<GatedState>>>>,
        live: Arc<AtomicUsize>,
        peak: AtomicUsize,
        changed: Arc<tokio::sync::Notify>,
        tasks: Mutex<Vec<tokio::task::AbortHandle>>,
        buffered_writes: AtomicBool,
    }

    impl GatedFiles {
        pub(super) fn track(&self, task: tokio::task::AbortHandle) {
            self.tasks.lock().expect("test writer tasks").push(task);
        }

        pub(super) fn writer_finished(&self) {
            self.changed.notify_one();
        }

        pub(super) fn open(&self, path: &StreamPath, id: Option<&str>) -> (PathBuf, GatedFile) {
            let state = Arc::new(Mutex::new(GatedState {
                buffered_writes: self.buffered_writes.load(Ordering::SeqCst),
                ..GatedState::default()
            }));
            let mut files = self.files.lock().expect("test file index");
            let index = files.len();
            files.push(state.clone());
            let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(live, Ordering::SeqCst);
            self.changed.notify_one();
            (
                PathBuf::from("generated-recording")
                    .join(path.as_path())
                    .join(format!("{}-{index}.mp4", id.unwrap_or("legacy"))),
                GatedFile {
                    state,
                    live: self.live.clone(),
                    changed: self.changed.clone(),
                    header_written: false,
                    prefix_written: false,
                },
            )
        }

        fn state(&self, index: usize) -> Arc<Mutex<GatedState>> {
            self.files.lock().expect("test file index")[index].clone()
        }

        fn release(&self, index: usize, sync: bool, fail_write: bool) {
            let state = self.state(index);
            let mut state = state.lock().expect("gated file state");
            state.writes_released = true;
            state.sync_released |= sync;
            state.fail_write = fail_write;
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
        }

        async fn until(&self, predicate: impl Fn() -> bool) {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let changed = self.changed.notified();
                    if predicate() {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .expect("bounded real writer progress");
        }

        async fn blocked(&self, index: usize, sync: bool) {
            self.until(|| {
                let files = self.files.lock().expect("test file index");
                files.get(index).is_some_and(|state| {
                    let state = state.lock().expect("gated file state");
                    if sync {
                        state.sync_blocked
                    } else {
                        state.write_blocked
                    }
                })
            })
            .await;
        }
    }

    pub(super) struct GatedFile {
        state: Arc<Mutex<GatedState>>,
        live: Arc<AtomicUsize>,
        changed: Arc<tokio::sync::Notify>,
        header_written: bool,
        prefix_written: bool,
    }

    impl Drop for GatedFile {
        fn drop(&mut self) {
            self.state.lock().expect("gated file state").dropped = true;
            self.live.fetch_sub(1, Ordering::SeqCst);
            self.changed.notify_one();
        }
    }

    impl tokio::io::AsyncWrite for GatedFile {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            let mut state = this.state.lock().expect("gated file state");
            if state.buffered_writes && this.header_written {
                if state.pending_write.is_some() {
                    if !state.writes_released {
                        state.write_blocked = true;
                        state.waker = Some(cx.waker().clone());
                        this.changed.notify_one();
                        return std::task::Poll::Pending;
                    }
                    let pending = state.pending_write.take().expect("pending owned write");
                    if let Err(error) = std::io::Write::write_all(&mut state.data, &pending) {
                        return std::task::Poll::Ready(Err(error));
                    }
                }
                if state.fail_write {
                    return std::task::Poll::Ready(Err(std::io::Error::other("gated write fault")));
                }
                // Match Tokio File: accepted bytes now belong to independent
                // pending I/O, even though write_all is allowed to finish.
                state.pending_write = Some(bytes.to_vec());
                this.prefix_written = true;
                this.changed.notify_one();
                return std::task::Poll::Ready(Ok(bytes.len()));
            }
            if this.header_written && this.prefix_written && !state.writes_released {
                state.write_blocked = true;
                state.waker = Some(cx.waker().clone());
                this.changed.notify_one();
                return std::task::Poll::Pending;
            }
            if state.fail_write {
                return std::task::Poll::Ready(Err(std::io::Error::other("gated write fault")));
            }
            let count = if !this.header_written {
                this.header_written = true;
                bytes.len()
            } else {
                this.prefix_written = true;
                bytes.len().min(17)
            };
            std::task::Poll::Ready(std::io::Write::write(&mut state.data, &bytes[..count]))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let mut state = this.state.lock().expect("gated file state");
            if state.pending_write.is_some() {
                if !state.writes_released {
                    state.flush_blocked = true;
                    state.waker = Some(cx.waker().clone());
                    this.changed.notify_one();
                    return std::task::Poll::Pending;
                }
                let pending = state.pending_write.take().expect("pending owned write");
                if let Err(error) = std::io::Write::write_all(&mut state.data, &pending) {
                    return std::task::Poll::Ready(Err(error));
                }
                this.changed.notify_one();
                if state.fail_write {
                    return std::task::Poll::Ready(Err(std::io::Error::other("gated write fault")));
                }
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl tokio::io::AsyncSeek for GatedFile {
        fn start_seek(
            self: std::pin::Pin<&mut Self>,
            position: std::io::SeekFrom,
        ) -> std::io::Result<()> {
            std::io::Seek::seek(
                &mut self.state.lock().expect("gated file state").data,
                position,
            )
            .map(|_| ())
        }

        fn poll_complete(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<u64>> {
            std::task::Poll::Ready(Ok(self
                .state
                .lock()
                .expect("gated file state")
                .data
                .position()))
        }
    }

    impl RecordingFile for GatedFile {
        fn sync_recording(
            &mut self,
        ) -> impl std::future::Future<Output = std::io::Result<()>> + Send {
            std::future::poll_fn(|cx| {
                let mut state = self.state.lock().expect("gated file state");
                state.sync_blocked = true;
                if state.sync_released {
                    state.synced = true;
                    std::task::Poll::Ready(Ok(()))
                } else {
                    state.waker = Some(cx.waker().clone());
                    self.changed.notify_one();
                    std::task::Poll::Pending
                }
            })
        }
    }

    fn gated_limits(
        bytes: usize,
        path_bytes: usize,
        writers: usize,
    ) -> (Arc<RecordingLimits>, Arc<GatedFiles>) {
        let limits = Arc::new(RecordingLimits::with_capacities(
            bytes, path_bytes, writers, 2,
        ));
        let files = Arc::new(GatedFiles::default());
        *limits.test_files.lock().expect("test file factory") = Some(files.clone());
        (limits, files)
    }

    fn owned_part(part: &Part, epoch: u64) -> (Arc<Part>, Weak<[u8]>) {
        let payload: Arc<[u8]> = Arc::from(part.data.as_ref());
        let owner = Arc::downgrade(&payload);
        (
            Arc::new(Part {
                initialization: Arc::new(sapflow_hls::muxer::Initialization {
                    id: epoch,
                    data: part.initialization.data.clone(),
                }),
                id: part.id,
                duration: part.duration,
                span: part.span,
                independent: part.independent,
                data: bytes::Bytes::from_owner(payload),
            }),
            owner,
        )
    }

    fn expected_file(tracks: &Tracks, parts: &[Arc<Part>]) -> Vec<u8> {
        let duration = parts
            .iter()
            .fold(Duration::ZERO, |sum, part| sum + part.span);
        let mut bytes = init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
            .expect("generated recording init")
            .to_vec();
        let mut origin = None;
        for part in parts {
            bytes.extend_from_slice(
                &rebase_part_for_file(
                    &part.data,
                    tracks.audio().map(|audio| audio.sample_rate),
                    &mut origin,
                )
                .expect("generated recording part"),
            );
        }
        bytes
    }

    #[test]
    fn writer_capacity_saturates_and_all_semaphore_counts_are_representable() {
        for (publishers, writers) in [(0, 1), (16, 160), (1000, 1024), (usize::MAX, 1024)] {
            let limits = RecordingLimits::new(publishers);
            assert_eq!(limits.writers.available_permits(), writers);
            assert_eq!(
                limits.bytes.available_permits(),
                GLOBAL_RETAINED_PART_BYTES.min(Semaphore::MAX_PERMITS)
            );
            let path = limits.path(&StreamPath::parse("live/event-capacity").expect("path"));
            assert_eq!(path.writers.available_permits(), PATH_WRITERS);
            assert_eq!(
                path.bytes.available_permits(),
                RETAINED_PART_BYTES.min(Semaphore::MAX_PERMITS)
            );
        }
    }

    #[tokio::test]
    async fn stalled_epochs_and_new_connection_factories_share_path_limits_until_file_drop() {
        let scratch = ScopedScratch::new("shared-generations");
        let (tracks, source) = fixture_parts();
        let size = source[0].data.len();
        let (limits, files) = gated_limits(size * 6, size * 4, 4);
        let mut receivers = Vec::new();
        let mut scopes = Vec::new();
        let mut owners = Vec::new();
        let mut expected = Vec::new();
        let mut path_owner = None;
        for generation in 1..=2 {
            let scope =
                crate::publish_observer::fixture_scope("a", generation, generation, generation);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
            let factory =
                RecorderFactory::new(&scratch.0, &scope.key().path, callbacks, 0, limits.clone());
            if let Some(previous) = &path_owner {
                assert!(Arc::ptr_eq(
                    &Weak::upgrade(previous).expect("old writer retains path"),
                    &factory.path_limits
                ));
            }
            path_owner = Some(Arc::downgrade(&factory.path_limits));
            let (part, owner) = owned_part(&source[0], generation);
            expected.push(expected_file(&tracks, std::slice::from_ref(&part)));
            let mut recorder = factory.start();
            recorder.feed(tracks.clone(), vec![part]);
            if generation == 2 {
                recorder.mark_incomplete();
            }
            drop(recorder);
            drop(factory);
            files.blocked(generation as usize - 1, false).await;
            assert!(
                owner.upgrade().is_some(),
                "active original survives connection Drop"
            );
            receivers.push(receiver);
            scopes.push(scope);
            owners.push(owner);
        }
        let path = path_owner
            .as_ref()
            .expect("path owner")
            .upgrade()
            .expect("draining path");
        assert_eq!(path.writers.available_permits(), 0);
        assert_eq!(path.bytes.available_permits(), size * 2);
        assert_eq!(limits.bytes.available_permits(), size * 4);
        assert_eq!(limits.writers.available_permits(), 2);

        // A cold epoch does not spend writer capacity or invent a failure,
        // even while both real writers are blocked and their factories gone.
        let unused = crate::publish_observer::fixture_scope("a", 3, 3, 3);
        let (callbacks, unused_events) = crate::callback::test_queue(unused.clone(), 2);
        let factory =
            RecorderFactory::new(&scratch.0, &unused.key().path, callbacks, 0, limits.clone());
        let mut cold = factory.start();
        assert!(cold.is_accepting());
        cold.feed(tracks.clone(), Vec::new());
        drop(cold);
        assert_eq!(unused.observations.recording_state(), "never-started");
        assert_eq!(unused_events.len(), 0);
        assert_eq!(files.tasks.lock().expect("test writer tasks").len(), 2);

        let (third, refused_owner) = owned_part(&source[0], 3);
        let mut refused = factory.start();
        refused.feed(tracks.clone(), vec![third]);
        assert!(!refused.is_accepting());
        assert!(refused.status.is_fatal());
        assert_eq!(refused.last_part(), None);
        assert!(refused_owner.upgrade().is_none());
        assert_eq!(unused.observations.recording_state(), "failed");
        assert_eq!(unused_events.len(), 0);
        assert_eq!(files.live.load(Ordering::SeqCst), 2);
        assert_eq!(files.peak.load(Ordering::SeqCst), 2);
        assert_eq!(
            path.bytes.available_permits(),
            size * 2,
            "refusal returns its byte reservation"
        );
        drop(refused);
        drop(factory);

        for index in 0..2 {
            files.release(index, false, false);
            files.blocked(index, true).await;
        }
        assert!(owners.iter().all(|owner| owner.upgrade().is_none()));
        assert_eq!(limits.bytes.available_permits(), size * 6);
        assert_eq!(path.bytes.available_permits(), size * 4);
        assert_eq!(
            path.writers.available_permits(),
            0,
            "sync still owns both file slots"
        );
        assert_eq!(limits.writers.available_permits(), 2);
        assert!(receivers.iter().all(|receiver| receiver.is_empty()));
        for index in 0..2 {
            files.release(index, true, false);
        }
        files
            .until(|| limits.writers.available_permits() == 4)
            .await;
        assert_eq!(files.live.load(Ordering::SeqCst), 0);
        assert_eq!(path.writers.available_permits(), 2);
        for index in 0..2 {
            let event = receivers[index]
                .try_recv()
                .expect("synced writer completion");
            let body: serde_json::Value = serde_json::from_slice(&event.body).expect("JSON");
            let state = if index == 0 {
                "finalised"
            } else {
                "incomplete"
            };
            assert_eq!(body["recordingState"], state);
            assert_eq!(body["eventGeneration"], index + 1);
            assert_eq!(scopes[index].observations.recording_state(), state);
            let file = files.state(index);
            let file = file.lock().expect("file state");
            assert!(file.synced && file.dropped);
            assert_eq!(file.data.get_ref(), &expected[index]);
        }
        drop(path);
        assert!(path_owner.expect("path owner").upgrade().is_none());
    }

    #[tokio::test]
    async fn global_admission_bounds_distinct_paths_and_releases_failed_or_cancelled_writers() {
        let scratch = ScopedScratch::new("global-writers");
        let (tracks, source) = fixture_parts();
        let size = source[0].data.len();
        for mode in ["bytes", "path-bytes", "writers", "cancel"] {
            let bytes = if mode == "bytes" { size } else { size * 4 };
            let path_bytes = if mode == "path-bytes" { size } else { size * 4 };
            let writers = if matches!(mode, "writers" | "cancel") {
                1
            } else {
                4
            };
            let (limits, files) = gated_limits(bytes, path_bytes, writers);
            let first = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, first_events) = crate::callback::test_queue(first.clone(), 2);
            let factory =
                RecorderFactory::new(&scratch.0, &first.key().path, callbacks, 0, limits.clone());
            let (part, owner) = owned_part(&source[0], 1);
            let mut recorder = factory.start();
            recorder.feed(tracks.clone(), vec![part]);
            drop(recorder);
            drop(factory);
            files.blocked(0, false).await;

            let event = if mode == "path-bytes" { "a" } else { "b" };
            let second = crate::publish_observer::fixture_scope(event, 2, 2, 2);
            let (callbacks, second_events) = crate::callback::test_queue(second.clone(), 2);
            let factory =
                RecorderFactory::new(&scratch.0, &second.key().path, callbacks, 0, limits.clone());
            let mut refused = factory.start();
            refused.feed(tracks.clone(), vec![source[0].clone()]);
            assert!(!refused.is_accepting());
            assert_eq!(second.observations.recording_state(), "failed");
            assert_eq!(second_events.len(), 0);
            assert_eq!(limits.bytes.available_permits(), bytes - size);
            assert_eq!(files.files.lock().expect("file index").len(), 1);
            assert_eq!(files.tasks.lock().expect("writer tasks").len(), 1);
            drop(refused);
            drop(factory);

            // Distinct empty connections neither spawn tasks nor leave an
            // ever-growing Weak index after their factories disappear.
            for index in 0..100 {
                let path = StreamPath::parse(&format!("live/event-unused-{index}")).expect("path");
                let factory = RecorderFactory::new(
                    &scratch.0,
                    &path,
                    Callbacks::start(None, None),
                    0,
                    limits.clone(),
                );
                let mut empty = factory.start();
                empty.feed(tracks.clone(), Vec::new());
                drop(empty);
                drop(factory);
            }
            assert!(limits.paths.lock().expect("path index").len() <= 2);
            assert_eq!(files.tasks.lock().expect("writer tasks").len(), 1);
            if mode == "cancel" {
                files.tasks.lock().expect("writer tasks")[0].abort();
            } else {
                files.release(0, false, true);
            }
            files.until(|| files.live.load(Ordering::SeqCst) == 0).await;
            assert_eq!(limits.bytes.available_permits(), bytes);
            assert_eq!(limits.writers.available_permits(), writers);
            assert!(owner.upgrade().is_none());
            assert_eq!(first_events.len(), 0);
            assert_eq!(first.observations.recording_state(), "failed");

            // A fresh generation can record after real failure/cancellation;
            // the refused generation itself remains failed and stopped.
            let (callbacks, mut events) = crate::callback::test_queue(second.clone(), 2);
            let factory =
                RecorderFactory::new(&scratch.0, &second.key().path, callbacks, 0, limits.clone());
            let mut next = factory.start();
            next.feed(tracks.clone(), vec![source[0].clone()]);
            drop(next);
            drop(factory);
            files.blocked(1, false).await;
            files.release(1, true, false);
            files
                .until(|| limits.writers.available_permits() == writers)
                .await;
            assert_eq!(limits.bytes.available_permits(), bytes);
            assert_eq!(files.live.load(Ordering::SeqCst), 0);
            let event = events.try_recv().expect("new writer completes");
            let body: serde_json::Value = serde_json::from_slice(&event.body).expect("JSON");
            assert_eq!(body["recordingState"], "finalised");
            assert_eq!(
                second.observations.counts().recordings_failed,
                1,
                "prior failure is retained"
            );
        }
    }

    #[tokio::test]
    async fn global_pool_admits_multiple_paths_and_bounds_syncing_file_count() {
        let scratch = ScopedScratch::new("concurrent-paths");
        let (tracks, parts) = fixture_parts();
        let size = parts[0].data.len();
        let (limits, files) = gated_limits(size * 8, size * 2, 4);
        let mut receivers = Vec::new();
        for index in 0..4 {
            let scope =
                crate::publish_observer::fixture_scope(&format!("parallel-{index}"), 1, 1, 1);
            let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
            let factory =
                RecorderFactory::new(&scratch.0, &scope.key().path, callbacks, 0, limits.clone());
            let mut recorder = factory.start();
            recorder.feed(tracks.clone(), vec![parts[0].clone()]);
            drop(recorder);
            drop(factory);
            receivers.push(receiver);
            files.blocked(index, false).await;
        }
        assert_eq!(files.live.load(Ordering::SeqCst), 4);
        assert_eq!(limits.writers.available_permits(), 0);
        assert_eq!(limits.bytes.available_permits(), size * 4);
        for index in 0..4 {
            files.release(index, false, false);
            files.blocked(index, true).await;
        }
        assert_eq!(limits.bytes.available_permits(), size * 8);
        assert_eq!(limits.writers.available_permits(), 0);
        let refused = crate::publish_observer::fixture_scope("parallel-refused", 1, 1, 1);
        let (callbacks, events) = crate::callback::test_queue(refused.clone(), 2);
        let factory = RecorderFactory::new(
            &scratch.0,
            &refused.key().path,
            callbacks,
            0,
            limits.clone(),
        );
        let mut recorder = factory.start();
        recorder.feed(tracks, vec![parts[0].clone()]);
        assert_eq!(refused.observations.recording_state(), "failed");
        assert_eq!(events.len(), 0);
        assert_eq!(files.peak.load(Ordering::SeqCst), 4);
        assert_eq!(limits.bytes.available_permits(), size * 8);
        for index in 0..4 {
            files.release(index, true, false);
        }
        files
            .until(|| limits.writers.available_permits() == 4)
            .await;
        assert_eq!(files.live.load(Ordering::SeqCst), 0);
        assert!(receivers.iter().all(|receiver| receiver.len() == 1));
    }

    #[tokio::test]
    async fn fatal_and_rebase_exits_keep_writer_slots_until_buffered_io_quiesces() {
        let scratch = ScopedScratch::new("buffered-failure");
        let (tracks, source) = fixture_parts();
        let size = source[0].data.len();
        for mode in ["fatal", "rebase"] {
            let limits = Arc::new(RecordingLimits::with_capacities(size * 8, size * 4, 2, 1));
            let files = Arc::new(GatedFiles::default());
            files.buffered_writes.store(true, Ordering::SeqCst);
            *limits.test_files.lock().expect("test file factory") = Some(files.clone());
            let mut paths = Vec::new();
            let mut scopes = Vec::new();
            let mut events = Vec::new();
            for index in 0..2 {
                let scope =
                    crate::publish_observer::fixture_scope(&format!("buffered-{index}"), 1, 1, 1);
                let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
                let factory = RecorderFactory::new(
                    &scratch.0,
                    &scope.key().path,
                    callbacks,
                    0,
                    limits.clone(),
                );
                paths.push(factory.path_limits.clone());
                let mut recorder = factory.start();
                recorder.budget.fragment_limit = size;
                recorder.feed(tracks.clone(), vec![source[0].clone()]);
                files
                    .until(|| {
                        files
                            .files
                            .lock()
                            .expect("file index")
                            .get(index)
                            .is_some_and(|state| {
                                state.lock().expect("file state").pending_write.is_some()
                            })
                    })
                    .await;
                let bad = Arc::new(Part {
                    initialization: source[0].initialization.clone(),
                    id: source[0].id + 1,
                    duration: source[0].duration,
                    span: source[0].span,
                    independent: source[0].independent,
                    data: if mode == "fatal" {
                        bytes::Bytes::from(vec![0; size + 1])
                    } else {
                        bytes::Bytes::from_static(b"invalid fragment")
                    },
                });
                recorder.feed(tracks.clone(), vec![bad]);
                drop(recorder);
                drop(factory);
                files
                    .until(|| files.state(index).lock().expect("file state").flush_blocked)
                    .await;
                scopes.push(scope);
                events.push(receiver);
            }
            assert_eq!(files.live.load(Ordering::SeqCst), 2);
            assert_eq!(limits.writers.available_permits(), 0);
            assert_eq!(limits.bytes.available_permits(), size * 8);
            for index in 0..2 {
                assert_eq!(paths[index].writers.available_permits(), 0);
                let state = files.state(index);
                let state = state.lock().expect("file state");
                assert!(
                    state
                        .pending_write
                        .as_ref()
                        .is_some_and(|data| !data.is_empty())
                );
                assert!(!state.dropped && !state.synced);
                assert_eq!(scopes[index].observations.recording_state(), "failed");
                assert_eq!(events[index].len(), 0, "fatal output is never published");
            }
            // Both a successor of an occupied path and a distinct path must
            // refuse admission while copied I/O still owns the original files.
            for event in ["buffered-0", "buffered-new"] {
                let scope = crate::publish_observer::fixture_scope(event, 2, 2, 2);
                let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
                let factory = RecorderFactory::new(
                    &scratch.0,
                    &scope.key().path,
                    callbacks,
                    0,
                    limits.clone(),
                );
                let mut recorder = factory.start();
                recorder.feed(tracks.clone(), vec![source[0].clone()]);
                assert!(!recorder.is_accepting());
                assert_eq!(scope.observations.recording_state(), "failed");
                assert_eq!(receiver.len(), 0);
            }
            assert_eq!(files.files.lock().expect("file index").len(), 2);
            for index in 0..2 {
                // A failed completion of the copied write also has to settle
                // before its file and writer credits may be released.
                files.release(index, false, mode == "fatal");
            }
            files
                .until(|| limits.writers.available_permits() == 2)
                .await;
            assert_eq!(files.live.load(Ordering::SeqCst), 0);
            assert_eq!(files.peak.load(Ordering::SeqCst), 2);
            for index in 0..2 {
                assert_eq!(paths[index].writers.available_permits(), 1);
                let state = files.state(index);
                let state = state.lock().expect("file state");
                assert!(state.pending_write.is_none() && state.dropped && !state.synced);
                assert_eq!(scopes[index].observations.recording_state(), "failed");
                assert_eq!(events[index].len(), 0);
            }
        }
    }

    #[tokio::test]
    async fn header_refusal_keeps_writer_slots_until_buffered_io_quiesces() {
        let scratch = ScopedScratch::new("buffered-header-refusal");
        let (tracks, source) = fixture_parts();
        let size = source[0].data.len();
        let limits = Arc::new(RecordingLimits::with_capacities(size * 2, size * 2, 1, 1));
        let files = Arc::new(GatedFiles::default());
        files.buffered_writes.store(true, Ordering::SeqCst);
        let scope = crate::publish_observer::fixture_scope("buffered-header", 1, 1, 1);
        let (callbacks, receiver) = crate::callback::test_queue(scope.clone(), 2);
        let (callbacks, observation) = callbacks.for_recording().expect("recording");
        let path = limits.path(&scope.key().path);
        let reservation = WriterReservation::acquire(&limits, &path).expect("writer reservation");
        let budget = PartBudget {
            bytes: path.bytes.clone(),
            global_bytes: Some(limits.bytes.clone()),
            fragment_limit: MAX_FILE_FRAGMENT_BYTES,
        };
        let (sender, input) = mpsc::channel(1);
        assert!(
            sender
                .try_send(Msg::Part(
                    budget.retain(source[0].clone()).expect("retained part")
                ))
                .is_ok()
        );
        drop(sender);
        let (file_path, file) = files.open(&scope.key().path, observation.as_ref().map(|o| o.id()));
        // Force finalise_file's size-refusal branch after a real accepted part.
        // Normal header construction uses the same size for both generations.
        let mut init = init_segment_for_tracks(tracks.video(), tracks.audio(), Duration::ZERO)
            .expect("initial header")
            .to_vec();
        init.push(0);
        let writer = RecordingWriter {
            dir: scratch.0.clone(),
            path: scope.key().path.clone(),
            callbacks,
            min_free: 0,
            observation: observation.clone(),
            status: Arc::new(RecordingStatus::default()),
            test_files: None,
        };
        let receipt = WriteReceipt {
            observation,
            completed: false,
        };
        let task = tokio::spawn(async move {
            let writer_guard = reservation;
            writer
                .write_file(file, file_path, input, tracks, init.into(), receipt)
                .await;
            drop(writer_guard);
        });
        files
            .until(|| files.state(0).lock().expect("file state").flush_blocked)
            .await;
        assert_eq!(limits.writers.available_permits(), 0);
        assert_eq!(path.writers.available_permits(), 0);
        assert_eq!(scope.observations.recording_state(), "failed");
        assert_eq!(receiver.len(), 0);
        {
            let state = files.state(0);
            let state = state.lock().expect("file state");
            assert!(
                state
                    .pending_write
                    .as_ref()
                    .is_some_and(|data| !data.is_empty())
            );
            assert!(!state.dropped && !state.synced);
        }
        files.release(0, false, false);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("bounded I/O barrier")
            .expect("writer task");
        assert_eq!(limits.writers.available_permits(), 1);
        assert_eq!(path.writers.available_permits(), 1);
        assert_eq!(files.live.load(Ordering::SeqCst), 0);
        let state = files.state(0);
        let state = state.lock().expect("file state");
        assert!(state.pending_write.is_none() && state.dropped && !state.synced);
        assert_eq!(receiver.len(), 0);
    }

    #[tokio::test]
    async fn shared_global_byte_refusal_syncs_exactly_the_admitted_prefix() {
        let scratch = ScopedScratch::new("global-prefix");
        let (tracks, parts) = fixture_parts();
        let prefix = &parts[..2];
        let bytes = prefix.iter().map(|part| part.data.len()).sum();
        let (limits, files) = gated_limits(bytes, bytes * 2, 4);
        let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
        let (callbacks, mut events) = crate::callback::test_queue(scope.clone(), 2);
        let factory =
            RecorderFactory::new(&scratch.0, &scope.key().path, callbacks, 0, limits.clone());
        let mut recorder = factory.start();
        // No await: all three admissions happen before the writer can run.
        recorder.feed(tracks.clone(), parts[..3].to_vec());
        assert_eq!(recorder.last_part(), Some(prefix[1].id));
        assert!(!recorder.is_accepting());
        assert!(!recorder.status.is_fatal());
        assert_eq!(scope.observations.recording_state(), "incomplete");
        assert_eq!(limits.bytes.available_permits(), 0);
        drop(recorder);
        drop(factory);
        files.blocked(0, false).await;
        files.release(0, false, false);
        files.blocked(0, true).await;
        assert_eq!(events.len(), 0, "no callback before sync");
        assert_eq!(limits.bytes.available_permits(), bytes);
        assert_eq!(limits.writers.available_permits(), 3);
        files.release(0, true, false);
        files
            .until(|| limits.writers.available_permits() == 4)
            .await;
        let event = events.try_recv().expect("synced prefix completion");
        let body: serde_json::Value = serde_json::from_slice(&event.body).expect("JSON");
        assert_eq!(body["recordingState"], "incomplete");
        assert_eq!(
            files.state(0).lock().expect("file state").data.get_ref(),
            &expected_file(&tracks, prefix)
        );
        assert_eq!(scope.observations.counts().recordings_finalised, 0);
    }

    fn recording_status(incomplete: bool) -> Arc<RecordingStatus> {
        let status = Arc::new(RecordingStatus::default());
        if incomplete {
            status.mark_incomplete();
        }
        status
    }

    fn part_message(part: Arc<Part>) -> Msg {
        Msg::Part(
            PartBudget::new(RETAINED_PART_BYTES, MAX_FILE_FRAGMENT_BYTES)
                .retain(part)
                .expect("fixture part fits the recording budget"),
        )
    }

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

    #[test]
    fn publication_commit_freezes_complete_incomplete_and_fatal_decisions() {
        for incomplete in [false, true] {
            let status = recording_status(incomplete);
            assert_eq!(status.try_commit(), Some(!incomplete));
            assert!(!status.accepts_input());
            assert!(!status.mark_fatal());
            assert!(!status.mark_incomplete());
            assert!(!status.is_fatal());
            assert_eq!(status.try_commit(), None, "publication is claimed once");
        }
        let fatal = recording_status(false);
        assert!(fatal.mark_fatal());
        assert!(fatal.is_fatal());
        assert!(!fatal.accepts_input());
        assert_eq!(fatal.try_commit(), None);
    }

    #[test]
    fn acceptance_guard_keeps_incomplete_recording_but_stops_closed_or_terminal_work() {
        let (tracks, parts) = fixture_parts();
        let part = parts[0].clone();
        let size = part.data.len();
        let make_recorder = |capacity, retained_bytes| {
            let (sender, receiver) = mpsc::channel(capacity);
            (
                Recorder {
                    tx: Some(sender),
                    pending: None,
                    started: false,
                    initialization_id: None,
                    last_part: None,
                    observation: None,
                    status: recording_status(false),
                    budget: PartBudget::new(retained_bytes, MAX_FILE_FRAGMENT_BYTES),
                },
                receiver,
            )
        };

        let (mut count_limited, count_rx) = make_recorder(2, size * 3);
        assert!(count_limited.is_accepting());
        count_limited.mark_incomplete();
        assert!(
            count_limited.is_accepting(),
            "upstream gaps can keep recording"
        );
        count_limited.feed(tracks.clone(), vec![part.clone()]);
        assert_eq!(count_rx.len(), 2);
        assert!(
            count_limited.is_accepting(),
            "a full queue must fail explicitly in feed"
        );
        count_limited.feed(tracks.clone(), vec![part.clone()]);
        assert!(!count_limited.is_accepting());

        let (mut byte_limited, _byte_rx) = make_recorder(QUEUE, size);
        byte_limited.feed(tracks.clone(), vec![part.clone()]);
        assert!(byte_limited.is_accepting());
        byte_limited.feed(tracks.clone(), vec![part.clone()]);
        assert!(!byte_limited.is_accepting());

        let (closed, mut receiver) = make_recorder(2, size);
        receiver.close();
        assert!(!closed.is_accepting());

        for fatal in [false, true] {
            let (mut terminal, receiver) = make_recorder(2, size);
            if fatal {
                assert!(terminal.status.mark_fatal());
            } else {
                assert_eq!(terminal.status.try_commit(), Some(true));
            }
            assert!(!terminal.is_accepting());
            terminal.feed(tracks.clone(), vec![part.clone()]);
            assert_eq!(receiver.len(), 0, "terminal input is never queued");
        }
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
                tx.send(part_message(parts[0].clone()))
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
                recording_status(false),
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
            pending: None,
            started: false,
            initialization_id: None,
            last_part: None,
            observation,
            status: recording_status(false),
            budget: PartBudget::new(RETAINED_PART_BYTES, MAX_FILE_FRAGMENT_BYTES),
        };
        let (tracks, parts) = fixture_parts();
        let first_id = parts[0].id;
        recorder.feed(tracks, parts);
        assert!(recorder.tx.is_none());
        assert!(recorder.status.state.load(Ordering::Acquire) & RecordingStatus::INCOMPLETE != 0);
        assert_eq!(receiver.len(), 2);
        assert_eq!(recorder.last_part(), Some(first_id));
        assert_eq!(scope.observations.recording_state(), "incomplete");
        assert_eq!(scope.observations.counts().recordings_finalised, 0);
    }

    #[test]
    fn byte_budget_counts_shared_references_through_dequeue_and_receiver_drop() {
        let (_, parts) = fixture_parts();
        let part = parts[0].clone();
        let size = part.data.len();
        let budget = PartBudget::new(size * 2, MAX_FILE_FRAGMENT_BYTES);
        let (sender, mut receiver) = mpsc::channel(2);
        for _ in 0..2 {
            let retained = budget.retain(part.clone()).expect("exact byte boundary");
            assert!(sender.try_send(Msg::Part(retained)).is_ok());
        }
        assert_eq!(budget.bytes.available_permits(), 0);
        assert!(matches!(budget.retain(part), Err(PartRefusal::Full)));
        let active = receiver.try_recv().expect("one active original");
        assert_eq!(
            budget.bytes.available_permits(),
            0,
            "dequeue keeps its charge"
        );
        drop(active);
        assert_eq!(budget.bytes.available_permits(), size);
        drop(receiver);
        assert_eq!(budget.bytes.available_permits(), size * 2);
    }

    #[test]
    fn failed_count_admission_returns_its_byte_reservation() {
        let (tracks, parts) = fixture_parts();
        let part = parts[0].clone();
        let size = part.data.len();
        for closed in [false, true] {
            let budget = PartBudget::new(size, MAX_FILE_FRAGMENT_BYTES);
            let (sender, receiver) = mpsc::channel(1);
            let mut receiver = Some(receiver);
            if closed {
                drop(receiver.take());
            } else {
                assert!(sender.try_send(Msg::Init(tracks.clone())).is_ok());
            }
            let retained = budget.retain(part.clone()).expect("reserved original");
            let refused = sender.try_send(Msg::Part(retained)).expect_err("refusal");
            assert_eq!(
                budget.bytes.available_permits(),
                0,
                "error still owns the part"
            );
            drop(refused);
            assert_eq!(budget.bytes.available_permits(), size);
        }
    }

    #[tokio::test]
    async fn active_write_keeps_byte_credit_until_completion_error_or_cancellation() {
        use tokio::io::AsyncReadExt;

        let (tracks, parts) = fixture_parts();
        let part = parts[0].clone();
        let size = part.data.len();
        assert!(size > 128);
        for mode in ["complete", "error", "cancel"] {
            let budget = PartBudget::new(size, MAX_FILE_FRAGMENT_BYTES);
            let (sender, mut receiver) = mpsc::channel(1);
            let retained = budget.retain(part.clone()).expect("retained original");
            assert!(sender.try_send(Msg::Part(retained)).is_ok());
            let Msg::Part(retained) = receiver.recv().await.expect("dequeued part") else {
                panic!("part");
            };
            assert_eq!(budget.bytes.available_permits(), 0);
            let (mut writer, mut reader) = tokio::io::duplex(64);
            let audio_rate = tracks.audio().map(|audio| audio.sample_rate);
            let task = tokio::spawn(async move {
                write_part(&mut writer, retained, audio_rate, &mut None).await
            });
            let mut first = [0; 1];
            tokio::time::timeout(Duration::from_secs(3), reader.read_exact(&mut first))
                .await
                .expect("writer enters real backpressure")
                .expect("first output byte");
            assert!(!task.is_finished());
            assert_eq!(
                budget.bytes.available_permits(),
                0,
                "active write stays charged"
            );
            match mode {
                "complete" => {
                    let mut rest = Vec::new();
                    tokio::time::timeout(Duration::from_secs(3), reader.read_to_end(&mut rest))
                        .await
                        .expect("bounded output drain")
                        .expect("output");
                    assert_eq!(rest.len() + 1, size);
                    assert_eq!(task.await.expect("writer task").expect("write"), part.span);
                }
                "error" => {
                    drop(reader);
                    assert!(task.await.expect("writer task").is_err());
                }
                "cancel" => {
                    task.abort();
                    assert!(task.await.expect_err("cancelled write").is_cancelled());
                }
                _ => unreachable!(),
            }
            assert_eq!(budget.bytes.available_permits(), size);
        }
    }

    #[tokio::test]
    async fn shared_backing_charge_follows_queued_and_active_writes_through_all_exits() {
        use tokio::io::AsyncReadExt;
        for mode in ["complete", "error", "cancel"] {
            let quota = Arc::new(sapflow_core::fragment::FragmentQuota::new(8 * 1024 * 1024));
            let mut ingest = sapflow_hls::TsIngest::with_fragment_quota(
                sapflow_hls::MuxerConfig::default(),
                quota.clone(),
            );
            for chunk in
                include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts").chunks(1316)
            {
                ingest
                    .push(chunk, SystemTime::UNIX_EPOCH)
                    .expect("generated bounded media");
            }
            let muxer = ingest.take_muxer().expect("bounded muxer");
            let audio_rate = muxer.audio().map(|audio| audio.sample_rate);
            let sapflow_hls::PartLookup::Found(part) = muxer.part(0) else {
                panic!("accepted part")
            };
            let size = part.data.len();
            let span = part.span;
            drop(muxer);
            drop(ingest);
            let charge = quota.used();
            assert!(
                charge >= size,
                "whole backing and initialization stay charged"
            );
            let budget = PartBudget::new(size, MAX_FILE_FRAGMENT_BYTES);
            let (sender, mut receiver) = mpsc::channel(1);
            assert!(
                sender
                    .try_send(Msg::Part(budget.retain(part).expect("recorder alias")))
                    .is_ok(),
                "queue part"
            );
            assert_eq!(quota.used(), charge, "queued original retains its charge");
            let Msg::Part(retained) = receiver.recv().await.expect("dequeue") else {
                panic!("part")
            };
            let (mut writer, mut reader) = tokio::io::duplex(64);
            let task = tokio::spawn(async move {
                write_part(&mut writer, retained, audio_rate, &mut None).await
            });
            let mut first = [0];
            tokio::time::timeout(Duration::from_secs(3), reader.read_exact(&mut first))
                .await
                .expect("writer backpressure")
                .expect("first byte");
            assert!(!task.is_finished());
            assert_eq!(
                quota.used(),
                charge,
                "active write retains original despite its rebased copy"
            );
            match mode {
                "complete" => {
                    let mut rest = Vec::new();
                    tokio::time::timeout(Duration::from_secs(3), reader.read_to_end(&mut rest))
                        .await
                        .expect("bounded drain")
                        .expect("remaining bytes");
                    assert_eq!(rest.len() + 1, size);
                    assert_eq!(task.await.expect("writer").expect("write"), span);
                }
                "error" => {
                    drop(reader);
                    assert!(task.await.expect("writer").is_err());
                }
                "cancel" => {
                    task.abort();
                    assert!(task.await.expect_err("cancelled").is_cancelled());
                }
                _ => unreachable!(),
            }
            assert_eq!(
                quota.used(),
                0,
                "final original owner releases once on {mode}"
            );
        }
    }

    #[tokio::test]
    async fn byte_refusal_drains_and_syncs_only_the_accepted_prefix() {
        for scoped in [false, true] {
            let scratch = ScopedScratch::new("byte-prefix");
            let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
            let (callbacks, mut receiver) = if scoped {
                crate::callback::test_queue(scope.clone(), 2)
            } else {
                crate::callback::legacy_test_queue(2)
            };
            let (tracks, parts) = fixture_parts();
            let prefix = &parts[..2];
            let limit = prefix.iter().map(|part| part.data.len()).sum();
            let budget = PartBudget::new(limit, MAX_FILE_FRAGMENT_BYTES);
            let bytes = budget.bytes.clone();
            let mut recorder = Recorder::start_on_with_budget(
                &tokio::runtime::Handle::current(),
                &scratch.0,
                &scope.key().path,
                callbacks,
                0,
                budget,
            );
            // No await lets the producer fill the injected byte budget first.
            recorder.feed(tracks.clone(), parts[..3].to_vec());
            assert!(recorder.tx.is_none());
            assert_eq!(recorder.last_part(), Some(prefix[1].id));
            assert_eq!(bytes.available_permits(), 0);
            recorder.feed(tracks.clone(), vec![parts[2].clone()]);
            assert_eq!(recorder.last_part(), Some(prefix[1].id));
            drop(recorder);
            let queued = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
                .await
                .expect("actual incomplete-prefix sync")
                .expect("one completion");
            let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
            if scoped {
                assert_eq!(body["recordingState"], "incomplete");
                assert_eq!(scope.observations.counts().recordings_finalised, 0);
            } else {
                assert!(body.get("recordingState").is_none());
            }
            let duration = prefix
                .iter()
                .fold(Duration::ZERO, |sum, part| sum + part.span);
            let mut expected = init_segment_for_tracks(tracks.video(), tracks.audio(), duration)
                .expect("init")
                .to_vec();
            let mut origin = None;
            for part in prefix {
                expected.extend_from_slice(
                    &rebase_part_for_file(
                        &part.data,
                        tracks.audio().map(|audio| audio.sample_rate),
                        &mut origin,
                    )
                    .expect("accepted part"),
                );
            }
            let written = tokio::fs::read(body["segmentPath"].as_str().expect("path"))
                .await
                .expect("synced prefix");
            assert_eq!(written, expected);
            assert_eq!(bytes.available_permits(), limit);
            assert_eq!(receiver.len(), 0);
        }
    }

    #[tokio::test]
    async fn oversized_input_is_fatal_even_with_a_full_or_closed_count_queue() {
        for scoped in [false, true] {
            for closed in [false, true] {
                let scratch = ScopedScratch::new("fatal-size");
                let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
                let (callbacks, callbacks_rx) = if scoped {
                    crate::callback::test_queue(scope.clone(), 2)
                } else {
                    crate::callback::legacy_test_queue(2)
                };
                let (callbacks, observation) = callbacks.for_recording().expect("writer");
                let (tracks, parts) = fixture_parts();
                let first = &parts[0];
                let size = first.data.len();
                let budget = PartBudget::new(size * 3, size);
                let bytes = budget.bytes.clone();
                let status = recording_status(false);
                let (sender, mut input) = mpsc::channel(2);
                let mut recorder = Recorder {
                    tx: Some(sender),
                    pending: None,
                    started: false,
                    initialization_id: None,
                    last_part: None,
                    observation: observation.clone(),
                    status: status.clone(),
                    budget,
                };
                recorder.feed(tracks.clone(), vec![first.clone()]);
                assert_eq!(input.len(), 2, "count queue is full");
                if closed {
                    input.close();
                }
                let oversized = Arc::new(Part {
                    initialization: first.initialization.clone(),
                    id: first.id + 1,
                    duration: first.duration,
                    span: first.span,
                    independent: first.independent,
                    data: bytes::Bytes::from(vec![0; size + 1]),
                });
                recorder.feed(tracks, vec![oversized.clone()]);
                assert!(recorder.tx.is_none());
                assert!(status.is_fatal());
                assert_eq!(recorder.last_part(), Some(first.id));
                assert_eq!(
                    Arc::strong_count(&oversized),
                    1,
                    "oversized bytes are not retained"
                );
                assert_eq!(bytes.available_permits(), size * 2);
                drop(recorder);
                write(
                    scratch.0.clone(),
                    scope.key().path.clone(),
                    input,
                    callbacks,
                    0,
                    observation,
                    status,
                )
                .await;
                assert_eq!(bytes.available_permits(), size * 3);
                assert_eq!(callbacks_rx.len(), 0, "fatal files are never published");
                if scoped {
                    assert_eq!(scope.observations.recording_state(), "failed");
                    assert_eq!(scope.observations.counts().recordings_failed, 1);
                    assert_eq!(scope.observations.counts().recordings_finalised, 0);
                }
            }
        }
    }

    #[tokio::test]
    async fn a_legacy_recording_with_a_gap_still_reports_it_finished() {
        let scratch = ScopedScratch::new("legacy-gap");
        let path = StreamPath::parse("live/event-a").expect("path");
        let (callbacks, mut receiver) = crate::callback::legacy_test_queue(2);
        let (sender, input) = mpsc::channel(2);
        let (tracks, mut parts) = fixture_parts();
        assert!(sender.try_send(Msg::Init(tracks)).is_ok());
        assert!(sender.try_send(part_message(parts.remove(0))).is_ok());
        drop(sender);
        // An SRT timeout or dropped packets mark the recording incomplete.
        let status = recording_status(true);
        write(scratch.0.clone(), path, input, callbacks, 0, None, status).await;
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
            assert!(sender.try_send(part_message(part)).is_ok());
            drop(sender);
            let status = recording_status(!invalid);
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
                status,
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
    async fn closed_receiver_orders_fatal_input_before_or_after_synced_publication() {
        for scoped in [false, true] {
            for fatal_first in [false, true] {
                let scope = crate::publish_observer::fixture_scope("a", 1, 1, 1);
                let (callbacks, mut callback_rx) = if scoped {
                    crate::callback::test_queue(scope.clone(), 2)
                } else {
                    crate::callback::legacy_test_queue(2)
                };
                let (callbacks, observation) = callbacks.for_recording().expect("writer");
                let (tracks, parts) = fixture_parts();
                let first = &parts[0];
                let size = first.data.len();
                let status = recording_status(false);
                let (sender, mut input) = mpsc::channel(2);
                let mut recorder = Recorder {
                    tx: Some(sender),
                    pending: None,
                    started: false,
                    initialization_id: None,
                    last_part: None,
                    observation: observation.clone(),
                    status: status.clone(),
                    budget: PartBudget::new(size * 3, size),
                };
                recorder.feed(tracks.clone(), vec![first.clone()]);
                assert!(matches!(input.try_recv(), Ok(Msg::Init(_))));
                let Ok(Msg::Part(retained)) = input.try_recv() else {
                    panic!("accepted prefix");
                };
                let init = init_segment_for_tracks(tracks.video(), tracks.audio(), Duration::ZERO)
                    .expect("init");
                let final_init =
                    init_segment_for_tracks(tracks.video(), tracks.audio(), first.span)
                        .expect("final init");
                let mut file = FaultFile {
                    data: std::io::Cursor::new(Vec::new()),
                    fault: "none",
                    calls: Vec::new(),
                };
                file.write_all(&init).await.expect("init write");
                write_part(
                    &mut file,
                    retained,
                    tracks.audio().map(|audio| audio.sample_rate),
                    &mut None,
                )
                .await
                .expect("accepted prefix write");
                // Model low-space shutdown: the writer closes its receiver
                // while the producer can still hold a sender and call feed.
                input.close();
                assert!(status.mark_incomplete());
                if let Some(observation) = &observation {
                    observation.incomplete();
                }
                finalise_file(&mut file, init.len(), &final_init)
                    .await
                    .expect("header, flush and sync precede publication");
                assert!(file.calls.ends_with(&["seek", "write", "flush", "sync"]));
                let oversized = Arc::new(Part {
                    initialization: first.initialization.clone(),
                    id: first.id + 1,
                    duration: first.duration,
                    span: first.span,
                    independent: first.independent,
                    data: bytes::Bytes::from(vec![0; size + 1]),
                });
                if fatal_first {
                    recorder.feed(tracks.clone(), vec![oversized.clone()]);
                }
                let decision = status.try_commit();
                if !fatal_first {
                    recorder.feed(tracks.clone(), vec![oversized]);
                    recorder.mark_incomplete();
                    assert!(!status.is_fatal(), "committed input decisions are frozen");
                }
                let mut receipt = WriteReceipt {
                    observation,
                    completed: false,
                };
                if let Some(complete) = decision {
                    assert!(!complete, "the synced low-space prefix stays incomplete");
                    publish_recording(
                        &mut receipt,
                        complete,
                        &callbacks,
                        &scope.key().path,
                        Path::new("recording.mp4"),
                        first.span,
                        RecordingTracks::AudioVideo,
                    );
                }
                drop(receipt);
                if fatal_first {
                    assert_eq!(callback_rx.len(), 0);
                    if scoped {
                        assert_eq!(scope.observations.recording_state(), "failed");
                    }
                } else {
                    let queued = callback_rx.try_recv().expect("committed completion");
                    let body: serde_json::Value =
                        serde_json::from_slice(&queued.body).expect("JSON");
                    if scoped {
                        assert_eq!(body["recordingState"], "incomplete");
                        assert_eq!(scope.observations.recording_state(), "incomplete");
                        assert_eq!(scope.observations.counts().recordings_failed, 0);
                    } else {
                        assert!(body.get("recordingState").is_none());
                    }
                }
            }
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
