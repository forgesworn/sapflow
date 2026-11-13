//! Recordings left unreported by a killed writer, an undelivered callback or
//! a stop, reported at the next start ([`crate::recording_recovery`]).
//!
//! A "kill" is the writer's runtime shut down under it, with the recorder
//! still open, so the writer never reaches its finish path; a "restart" is a
//! new outbox and callback sender on the same directory.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sapflow_core::StreamPath;
use sapflow_hls::muxer::Part;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::callback::Callbacks;
use crate::recorder::{Recorder, duration_ms};
use crate::recording_outbox::tests::Scratch;
use crate::recording_outbox::{Entry, Finished, Outbox};
use crate::recording_recovery::{self, MAX_PER_START, Pacing, Pending, Summary};
use crate::stream::Tracks;

const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

/// Production pacing, shortened.
fn pacing(interval: Duration) -> Pacing {
    Pacing {
        interval,
        retry_first: Duration::from_millis(50),
        retry_max: Duration::from_millis(200),
    }
}

/// A stand-in for v4v: each request's arrival and JSON body, answered with
/// the status `answer` gives its index, or never when it gives None.
struct Receiver {
    addr: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<(Instant, serde_json::Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Receiver {
    async fn start(answer: impl Fn(usize) -> Option<u16> + Send + Sync + 'static) -> Self {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(answer);
        let log = seen.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (log, answer) = (log.clone(), answer.clone());
                tokio::spawn(async move {
                    let Some(body) = read_body(&mut socket).await else {
                        return;
                    };
                    let index = {
                        let mut log = log.lock().expect("log");
                        log.push((Instant::now(), body));
                        log.len() - 1
                    };
                    match answer(index) {
                        Some(status) => {
                            let response = crate::http_lite::response(status, &[], b"");
                            let _ = socket.write_all(&response).await;
                            let _ = socket.shutdown().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                });
            }
        });
        Self { addr, seen, task }
    }

    fn url(&self) -> String {
        format!("http://{}/callback", self.addr)
    }

    fn bodies(&self) -> Vec<serde_json::Value> {
        let seen = self.seen.lock().expect("log");
        seen.iter().map(|(_, body)| body.clone()).collect()
    }

    fn completions(&self) -> Vec<serde_json::Value> {
        self.bodies()
            .into_iter()
            .filter(|body| body["event"] == "recordSegmentComplete")
            .collect()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_body(socket: &mut tokio::net::TcpStream) -> Option<serde_json::Value> {
    let mut buf = Vec::new();
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        if let Ok(httparse::Status::Complete(used)) = request.parse(&buf) {
            let length: usize = request
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-length"))
                .and_then(|h| std::str::from_utf8(h.value).ok()?.parse().ok())?;
            let mut body = buf[used..].to_vec();
            while body.len() < length {
                let mut chunk = [0u8; 4096];
                let n = socket.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            return serde_json::from_slice(&body).ok();
        }
        let mut chunk = [0u8; 4096];
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn fixture_parts() -> (Tracks, Vec<Arc<Part>>) {
    let mut ingest = sapflow_hls::TsIngest::new(sapflow_hls::MuxerConfig::default());
    for bytes in include_bytes!("../../sapflow-hls/tests/fixtures/testsrc-3s.ts").chunks(1316) {
        ingest
            .push(bytes, SystemTime::UNIX_EPOCH)
            .expect("generated fixture");
    }
    let mut muxer = ingest.take_muxer().expect("muxer");
    muxer.interrupt(SystemTime::UNIX_EPOCH);
    let tracks =
        Tracks::new(muxer.video().cloned(), muxer.audio().copied()).expect("nonempty tracks");
    let parts = (0..muxer.next_part_id())
        .map(|id| match muxer.part(id) {
            sapflow_hls::PartLookup::Found(part) => part,
            _ => panic!("generated part retained"),
        })
        .collect();
    (tracks, parts)
}

/// The duration the writer gives a file of `parts`, and where they end in it.
fn written(tracks: &Tracks, parts: &[Arc<Part>]) -> (Duration, u64) {
    let mut origin = None;
    let mut duration = Duration::ZERO;
    let mut end = tracks.recording_init(Duration::ZERO).expect("init").len() as u64;
    for part in parts {
        let (output, part_end) = sapflow_hls::fmp4::rebase_part_for_file_timed(
            &part.data,
            tracks.audio_rate(),
            &mut origin,
        )
        .expect("rebase");
        duration = duration.max(part_end);
        end += output.len() as u64;
    }
    (duration, end)
}

fn path() -> StreamPath {
    StreamPath::parse("live/event-a").expect("path")
}

fn recordings_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(files) = std::fs::read_dir(dir.join("live/event-a")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = files.filter_map(|f| Some(f.ok()?.path())).collect();
    files.sort();
    files
}

fn prefix_of(file: &Path) -> sapflow_hls::fmp4::RecordedPrefix {
    let mut opened = std::fs::File::open(file).expect("recording");
    let len = opened.metadata().expect("meta").len();
    sapflow_hls::fmp4::recorded_prefix(&mut opened, len, Some(48_000)).expect("read")
}

fn digest(file: &Path) -> Vec<u8> {
    use sha2::Digest;
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .expect("recording")
        .read_to_end(&mut bytes)
        .expect("read");
    sha2::Sha256::digest(&bytes).to_vec()
}

/// What a killed writer leaves: `keep` whole fragments, then, with `cut`,
/// half of the next one, as a disk that had not written all of it would.
struct Interrupted {
    scratch: Scratch,
    file: PathBuf,
    duration: Duration,
    /// The scoped recording's identity, if scoped.
    scope: Option<Arc<crate::publish_observer::CallbackScope>>,
}

fn interrupted(scoped: bool, keep: usize, cut: bool) -> Interrupted {
    let scratch = Scratch::new("interrupted");
    let (tracks, parts) = fixture_parts();
    assert!(parts.len() > keep + 1, "{} parts", parts.len());
    let fed = keep + usize::from(cut);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let scope = scoped.then(|| crate::publish_observer::fixture_scope("a", 1, 11, 3));
    let recorder;
    let queue;
    {
        let _context = runtime.enter();
        let outbox = Arc::new(Outbox::open(&scratch.0).expect("outbox"));
        let (callbacks, rx) = match &scope {
            Some(scope) => crate::callback::test_queue(scope.clone(), 16),
            None => crate::callback::legacy_test_queue(16),
        };
        queue = rx;
        let callbacks = callbacks.with_outbox(outbox);
        let mut started = Recorder::start(&scratch.0, &path(), callbacks, 0);
        started.feed(tracks.clone(), parts[..fed].to_vec());
        recorder = started;
    }
    // The writer is still waiting for more when it is killed.
    let deadline = Instant::now() + Duration::from_secs(5);
    let file = loop {
        if let Some(file) = recordings_in(&scratch.0).pop()
            && prefix_of(&file).fragments == fed as u64
        {
            break file;
        }
        assert!(Instant::now() < deadline, "writer wrote every fed part");
        std::thread::sleep(Duration::from_millis(10));
    };
    runtime.shutdown_timeout(Duration::from_secs(2));
    drop(recorder);
    assert!(queue.is_empty(), "a killed writer sends nothing");
    drop(queue);
    let (duration, end) = written(&tracks, &parts[..keep]);
    if cut {
        let (_, next) = written(&tracks, &parts[..fed]);
        let handle = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("recording");
        handle.set_len(end + (next - end) / 2).expect("cut");
    }
    assert_eq!(prefix_of(&file).fragments, keep as u64);
    Interrupted {
        scratch,
        file,
        duration,
        scope,
    }
}

/// A restart: the outbox opened afresh, and everything it lists sent to a
/// receiver answering `status`, `interval` apart.
async fn restart(dir: &Path, receiver: &Receiver, interval: Duration) -> Summary {
    let outbox = Arc::new(Outbox::open(dir).expect("reopen outbox"));
    let pending = {
        let outbox = outbox.clone();
        tokio::task::spawn_blocking(move || Pending::list(&outbox, MAX_PER_START))
            .await
            .expect("list")
    };
    let callbacks = Callbacks::start(Some(receiver.url()), Some("fixture-token".into()));
    let summary =
        recording_recovery::run(outbox, pending, callbacks.clone(), WEEK, pacing(interval)).await;
    tokio::time::timeout(Duration::from_secs(5), callbacks.drained())
        .await
        .expect("drained");
    summary
}

fn entries(dir: &Path) -> Vec<Entry> {
    let outbox = Outbox::open(dir).expect("outbox");
    let (names, _) = outbox.list(usize::MAX).expect("list");
    names
        .iter()
        .map(|name| outbox.read(name).expect("entry").0)
        .collect()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[test]
fn a_killed_writer_is_reported_once_incomplete_with_its_decodable_duration() {
    for scoped in [false, true] {
        for cut in [false, true] {
            let interrupted = interrupted(scoped, 4, cut);
            let dir = interrupted.scratch.0.clone();
            let stored = entries(&dir);
            assert_eq!(stored.len(), 1);
            assert!(stored[0].finished.is_none(), "the writer never finished");
            let before = digest(&interrupted.file);
            block_on(async {
                let receiver = Receiver::start(|_| Some(204)).await;
                let summary = restart(&dir, &receiver, Duration::from_millis(10)).await;
                assert_eq!(summary.delivered, 1, "{summary:?}");
                let bodies = receiver.bodies();
                assert_eq!(bodies.len(), 1);
                let body = &bodies[0];
                assert_eq!(body["event"], "recordSegmentComplete");
                assert_eq!(body["path"], "live/event-a");
                assert_eq!(body["recordingState"], "incomplete");
                assert_eq!(body["recordingReason"], "server-interrupted");
                assert_eq!(
                    body["segmentPath"],
                    interrupted.file.display().to_string(),
                    "named as the writer named it"
                );
                let expected = duration_ms(interrupted.duration) as f64 / 1000.0;
                assert!(expected > 0.5, "{expected}");
                assert_eq!(
                    body["segmentDuration"]
                        .as_str()
                        .expect("duration")
                        .parse::<f64>()
                        .expect("number"),
                    expected
                );
                if let Some(scope) = &interrupted.scope {
                    let name = interrupted
                        .file
                        .file_name()
                        .expect("name")
                        .to_string_lossy();
                    let id = body["recordingId"].as_str().expect("recording ID");
                    assert!(name.ends_with(&format!("-{id}.mp4")));
                    assert_eq!(body["sessionId"], scope.key().session_id.as_str());
                    assert_eq!(body["eventGeneration"], 1);
                    assert_eq!(body["connectionEpoch"], 3);
                    assert_eq!(body["eventSequence"], 1, "the sequence it reserved");
                    assert_eq!(body["contractVersion"], 1);
                    assert_eq!(
                        body["recordingTracks"],
                        serde_json::json!({"hasVideo": true, "hasAudio": true})
                    );
                } else {
                    assert!(body.get("sessionId").is_none());
                }
                // Reported once: a second restart has nothing to send.
                let again = restart(&dir, &receiver, Duration::from_millis(10)).await;
                assert_eq!(again, Summary::default());
                assert_eq!(receiver.bodies().len(), 1);
            });
            assert!(entries(&dir).is_empty());
            assert_eq!(
                digest(&interrupted.file),
                before,
                "the file is never rewritten"
            );
        }
    }
}

/// The same, checked against an independent demuxer. Run it explicitly:
/// `cargo test -p sapflow recovered_duration_matches_ffprobe -- --ignored`.
#[test]
#[ignore = "requires installed ffprobe and ffmpeg; run explicitly"]
fn recovered_duration_matches_ffprobe() {
    let (_, parts) = fixture_parts();
    let longest_part = parts
        .iter()
        .map(|p| p.span)
        .max()
        .expect("parts")
        .as_secs_f64();
    for cut in [false, true] {
        let interrupted = interrupted(false, 6, cut);
        let dir = interrupted.scratch.0.clone();
        let reported = block_on(async {
            let receiver = Receiver::start(|_| Some(204)).await;
            restart(&dir, &receiver, Duration::from_millis(10)).await;
            receiver.bodies()[0]["segmentDuration"]
                .as_str()
                .expect("duration")
                .parse::<f64>()
                .expect("number")
        });
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=dts_time,pts_time,duration_time",
                "-of",
                "csv=p=0",
            ])
            .arg(&interrupted.file)
            .output()
            .expect("ffprobe runs");
        assert!(probe.status.success(), "ffprobe opens the interrupted file");
        let text = String::from_utf8(probe.stdout).expect("UTF-8");
        // The writer measures decode time; presentation adds the muxer's
        // composition offset, so compare decode times.
        let packet = |line: &str| -> Option<(f64, f64, f64)> {
            let mut fields = line.split(',');
            let pts: f64 = fields.next()?.parse().ok()?;
            let dts: f64 = fields.next()?.parse().ok()?;
            let duration: f64 = fields.next()?.parse().ok()?;
            Some((pts, dts, duration))
        };
        let end = text
            .lines()
            .filter_map(packet)
            .map(|(_, dts, duration)| dts + duration)
            .fold(0.0, f64::max);
        let presented = text
            .lines()
            .filter_map(packet)
            .map(|(pts, _, duration)| pts + duration)
            .fold(0.0, f64::max);
        let packets = text.lines().count();
        eprintln!(
            "cut={cut} reported={reported:.3}s ffprobe_video_decode_end={end:.3}s presentation_end={presented:.3}s packets={packets} longest_part={longest_part:.3}s"
        );
        // A whole file decodes to where it was reported; a cut one may give
        // a demuxer part of its last fragment's frames, never more than it.
        assert!(end + 0.05 >= reported, "{end} < {reported}");
        assert!(
            end <= reported + if cut { longest_part } else { 0.05 } + 0.001,
            "{end} > {reported}"
        );
        let decode = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&interrupted.file)
            .args(["-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(
            decode.status.success(),
            "ffmpeg decodes the interrupted file"
        );
    }
}

#[test]
fn a_finished_recording_whose_callback_was_lost_is_reported_once_as_it_was() {
    for scoped in [false, true] {
        let scratch = Scratch::new("undelivered");
        let dir = scratch.0.clone();
        let live = block_on(async {
            let outbox = Arc::new(Outbox::open(&dir).expect("outbox"));
            let scope = scoped.then(|| crate::publish_observer::fixture_scope("a", 1, 11, 3));
            let (callbacks, mut rx) = match &scope {
                Some(scope) => crate::callback::test_queue(scope.clone(), 16),
                None => crate::callback::legacy_test_queue(16),
            };
            let callbacks = callbacks.with_outbox(outbox);
            let (tracks, parts) = fixture_parts();
            let mut recorder = Recorder::start(&dir, &path(), callbacks, 0);
            recorder.feed(tracks, parts);
            recorder.mark_incomplete(crate::recorder::IncompleteReason::MediaLoss);
            drop(recorder);
            let queued = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .expect("completion deadline")
                .expect("completion");
            // Taken from the queue and never delivered: as if the process
            // stopped while v4v was down.
            let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
            drop(queued);
            body
        });
        assert_eq!(live["recordingState"], "incomplete");
        assert_eq!(live["recordingReason"], "media-loss");
        let stored = entries(&dir);
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].finished,
            Some(Finished {
                complete: false,
                reason: Some("media-loss".into()),
                duration_ms: (live["segmentDuration"]
                    .as_str()
                    .expect("duration")
                    .parse::<f64>()
                    .expect("number")
                    * 1000.0)
                    .round() as u64,
            })
        );
        block_on(async {
            let receiver = Receiver::start(|_| Some(204)).await;
            let summary = restart(&dir, &receiver, Duration::from_millis(10)).await;
            assert_eq!(summary.delivered, 1);
            let bodies = receiver.bodies();
            assert_eq!(bodies.len(), 1);
            let mut recovered = bodies[0].clone();
            if scoped {
                // The only difference: the sequence the recording reserved
                // at its start, below the one its live callback took.
                let reserved = recovered["eventSequence"].as_u64().expect("sequence");
                assert!(reserved < live["eventSequence"].as_u64().expect("sequence"));
                recovered["eventSequence"] = live["eventSequence"].clone();
            }
            assert_eq!(
                recovered, live,
                "the same state, reason, duration and identity"
            );
            assert_eq!(
                restart(&dir, &receiver, Duration::from_millis(10)).await,
                Summary::default()
            );
            assert_eq!(receiver.bodies().len(), 1);
        });
        assert!(entries(&dir).is_empty());
    }
}

#[test]
fn a_delivered_callback_leaves_nothing_to_send_again() {
    let scratch = Scratch::new("delivered");
    let dir = scratch.0.clone();
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let outbox = Arc::new(Outbox::open(&dir).expect("outbox"));
        let callbacks = Callbacks::start(Some(receiver.url()), None).with_outbox(outbox.clone());
        let (tracks, parts) = fixture_parts();
        let mut recorder = Recorder::start(&dir, &path(), callbacks.clone(), 0);
        recorder.feed(tracks, parts);
        drop(recorder);
        tokio::time::timeout(Duration::from_secs(5), async {
            while receiver.completions().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            callbacks.drained().await;
        })
        .await
        .expect("delivered");
        assert_eq!(receiver.completions()[0]["recordingState"], "finalised");
        assert!(outbox.list(10).expect("list").0.is_empty());
        drop(callbacks);
        drop(outbox);
        assert_eq!(
            restart(&dir, &receiver, Duration::from_millis(10)).await,
            Summary::default()
        );
        assert_eq!(receiver.completions().len(), 1);
    });
}

/// Hand-written `finished` entries for small files under `live/event-a/`.
fn pending_files(dir: &Path, count: u64) -> Vec<String> {
    let outbox = Outbox::open(dir).expect("outbox");
    std::fs::create_dir_all(dir.join("live/event-a")).expect("event dir");
    (0..count)
        .map(|i| {
            let file = format!("live/event-a/{}.mp4", 1000 + i);
            std::fs::write(dir.join(&file), b"generated").expect("recording");
            let mut entry = crate::recording_outbox::tests::legacy(&file);
            entry.finished = Some(Finished {
                complete: true,
                reason: None,
                duration_ms: 1000 + i,
            });
            outbox.write(&entry).expect("entry");
            file
        })
        .collect()
}

fn reported_files(receiver: &Receiver) -> Vec<String> {
    receiver
        .completions()
        .iter()
        .map(|body| {
            body["segmentPath"]
                .as_str()
                .expect("path")
                .rsplit('/')
                .next()
                .expect("name")
                .to_owned()
        })
        .collect()
}

#[test]
fn a_crash_during_recovery_neither_loses_nor_repeats_an_acknowledged_report() {
    let scratch = Scratch::new("crash-mid-recovery");
    let dir = scratch.0.clone();
    pending_files(&dir, 3);
    // The first run is killed while the app has the second report and has
    // not answered it.
    let first = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let receiver = first.block_on(async {
        let receiver = Receiver::start(|index| (index == 0).then_some(204)).await;
        let outbox = Arc::new(Outbox::open(&dir).expect("outbox"));
        let pending = Pending::list(&outbox, MAX_PER_START);
        let callbacks = Callbacks::start(Some(receiver.url()), None);
        tokio::spawn(recording_recovery::run(
            outbox,
            pending,
            callbacks,
            WEEK,
            pacing(Duration::from_millis(10)),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while receiver.bodies().len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("second report in flight");
        receiver
    });
    let first_run = reported_files(&receiver);
    drop(receiver);
    first.shutdown_timeout(Duration::from_secs(2));
    let (acknowledged, unanswered) = (first_run[0].clone(), first_run[1].clone());
    assert_eq!(
        entries(&dir).len(),
        2,
        "only the acknowledged entry is gone"
    );
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let summary = restart(&dir, &receiver, Duration::from_millis(10)).await;
        assert_eq!(summary.delivered, 2);
        // The unanswered report is sent again; the acknowledged one is not.
        let mut second_run = reported_files(&receiver);
        assert!(second_run.contains(&unanswered));
        assert!(!second_run.contains(&acknowledged));
        second_run.push(acknowledged);
        second_run.sort();
        assert_eq!(second_run, ["1000.mp4", "1001.mp4", "1002.mp4"]);
    });
    assert!(entries(&dir).is_empty());
}

#[test]
fn expired_or_missing_recordings_are_not_reported_and_their_entries_go() {
    let scratch = Scratch::new("expired");
    let dir = scratch.0.clone();
    let files = pending_files(&dir, 3);
    let old = std::fs::File::options()
        .write(true)
        .open(dir.join(&files[0]))
        .expect("recording");
    old.set_modified(SystemTime::now() - 2 * WEEK)
        .expect("mtime");
    drop(old);
    std::fs::remove_file(dir.join(&files[1])).expect("expired by an earlier run");
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let summary = restart(&dir, &receiver, Duration::from_millis(10)).await;
        assert_eq!((summary.delivered, summary.dropped), (1, 2), "{summary:?}");
        assert_eq!(reported_files(&receiver), ["1002.mp4"]);
    });
    assert!(entries(&dir).is_empty());
    assert!(
        dir.join(&files[0]).exists(),
        "recovery never deletes recordings"
    );
}

#[test]
fn many_recordings_are_reported_one_at_a_time_and_live_events_still_go() {
    assert!(
        recording_recovery::INTERVAL >= Duration::from_secs(1),
        "at most 60 a minute, half of v4v's 120 from one address"
    );
    let scratch = Scratch::new("many");
    let dir = scratch.0.clone();
    pending_files(&dir, 12);
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let interval = Duration::from_millis(100);
        let outbox = Arc::new(Outbox::open(&dir).expect("outbox"));
        let pending = Pending::list(&outbox, MAX_PER_START);
        assert_eq!(pending.len(), 12);
        let callbacks = Callbacks::start(Some(receiver.url()), None).with_outbox(outbox.clone());
        let recovery = tokio::spawn(recording_recovery::run(
            outbox.clone(),
            pending,
            callbacks.clone(),
            WEEK,
            pacing(interval),
        ));
        // A publisher that connects now has its events sent between the
        // recovered ones, not after them all.
        tokio::time::sleep(Duration::from_millis(250)).await;
        callbacks.send(&path(), sapflow_core::StreamEvent::Publish);
        let summary = recovery.await.expect("recovery");
        assert_eq!(summary.delivered, 12);
        let seen: Vec<(Instant, serde_json::Value)> = receiver.seen.lock().expect("log").clone();
        let recovered: Vec<Instant> = seen
            .iter()
            .filter(|(_, body)| body["event"] == "recordSegmentComplete")
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(recovered.len(), 12);
        // Sent one at a time, each at least an interval after the last was
        // queued; arrival jitter can shorten a single gap, not the span.
        let span = recovered[11] - recovered[0];
        assert!(
            span >= interval * 11 - Duration::from_millis(50),
            "{span:?}"
        );
        let publish = seen
            .iter()
            .position(|(_, body)| body["event"] == "publish")
            .expect("publish delivered");
        let last_recovered = seen
            .iter()
            .rposition(|(_, body)| body["event"] == "recordSegmentComplete")
            .expect("recovered");
        assert!(
            publish < last_recovered,
            "publish waited for every recovered report"
        );
        assert!(outbox.list(20).expect("list").0.is_empty());
    });
}

#[test]
fn an_app_that_is_down_at_start_gets_every_report_once_it_is_back() {
    let scratch = Scratch::new("down");
    let dir = scratch.0.clone();
    pending_files(&dir, 5);
    block_on(async {
        // Down for three reports' four attempts each (a power cut that
        // restarted it with sapflow), then back, with no restart of sapflow.
        let receiver = Receiver::start(|index| Some(if index < 12 { 503 } else { 204 })).await;
        let summary = restart(&dir, &receiver, Duration::from_millis(10)).await;
        assert_eq!(summary.failed, recording_recovery::STOP_AFTER_FAILURES);
        assert!(summary.stopped, "the round ended after three failures");
        assert_eq!(
            (summary.retry_rounds, summary.delivered),
            (1, 5),
            "{summary:?}"
        );
        assert_eq!(receiver.bodies().len(), 4 * 3 + 5);
        let mut files = reported_files(&receiver)[12..].to_vec();
        files.sort();
        assert_eq!(
            files,
            ["1000.mp4", "1001.mp4", "1002.mp4", "1003.mp4", "1004.mp4"]
        );
    });
    assert!(entries(&dir).is_empty());
}

#[test]
fn a_refusal_keeps_every_entry_for_the_next_start() {
    let scratch = Scratch::new("refused");
    let dir = scratch.0.clone();
    pending_files(&dir, 3);
    block_on(async {
        // A wrong token after a redeploy, say: not retried in this run,
        // not erased either.
        let refusing = Receiver::start(|_| Some(401)).await;
        let summary = restart(&dir, &refusing, Duration::from_millis(10)).await;
        assert_eq!(
            (summary.refused, summary.retry_rounds, summary.stopped),
            (3, 0, false)
        );
        assert_eq!(refusing.bodies().len(), 3);
    });
    assert_eq!(entries(&dir).len(), 3);
}

#[test]
fn recording_expiry_leaves_the_outbox_alone() {
    let scratch = Scratch::new("expiry");
    let dir = scratch.0.clone();
    let files = pending_files(&dir, 1);
    block_on(async {
        let later = SystemTime::now() + 2 * WEEK;
        assert_eq!(crate::recorder::expire(&dir, WEEK, later).await, 1);
    });
    assert!(!dir.join(&files[0]).exists());
    assert_eq!(entries(&dir).len(), 1, "expire only touches live/*/");
    let outbox = Outbox::open(&dir).expect("outbox");
    assert_eq!(outbox.sweep(WEEK, SystemTime::now()), 1);
}

#[test]
fn an_outbox_failure_never_stops_recording() {
    let scratch = Scratch::new("outbox-failure");
    let dir = scratch.0.clone();
    block_on(async {
        let outbox = Arc::new(Outbox::open(&dir).expect("outbox"));
        outbox
            .fail_writes
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (callbacks, mut rx) = crate::callback::legacy_test_queue(4);
        let callbacks = callbacks.with_outbox(outbox.clone());
        let (tracks, parts) = fixture_parts();
        let mut recorder = Recorder::start(&dir, &path(), callbacks, 0);
        recorder.feed(tracks, parts);
        drop(recorder);
        let queued = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("completion deadline")
            .expect("completion");
        let body: serde_json::Value = serde_json::from_slice(&queued.body).expect("JSON");
        assert_eq!(body["recordingState"], "finalised");
        assert!(outbox.list(10).expect("list").0.is_empty());
    });
}

fn begin_live(app: &Arc<crate::App>) -> crate::publish::Publication {
    let request = sapflow_core::PublishRequest::parse_srt_stream_id(
        "publish:live/event-live:fixture:fixture",
    )
    .expect("request");
    crate::publish::Publication::begin(
        app,
        &request,
        crate::stream::SourceKind::Ts,
        "127.0.0.1".parse().expect("loopback"),
    )
    .expect("publication")
}

/// The loss fixture with a real callback sender to `url` and an outbox.
fn stopping_fixture(url: String) -> (crate::publish::loss_tests::Fixture, Arc<Outbox>) {
    let mut fixture = crate::publish::loss_tests::Fixture::new();
    let outbox = Arc::new(Outbox::open(&fixture.root).expect("outbox"));
    let app = Arc::get_mut(&mut fixture.app).expect("unshared fixture app");
    app.callbacks = Callbacks::start(Some(url), None).with_outbox(outbox.clone());
    (fixture, outbox)
}

#[test]
fn a_stop_delivers_as_before_and_leaves_nothing_behind() {
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let (fixture, outbox) = stopping_fixture(receiver.url());
        let mut live = begin_live(&fixture.app);
        crate::publish::loss_tests::Fixture::feed(&mut live);
        let outcome = crate::shutdown::stop(&fixture.app, &[], crate::shutdown::DEADLINE).await;
        assert!(outcome.in_time, "{outcome:?}");
        assert_eq!((outcome.writers, outcome.callbacks), (0, 0));
        drop(live);
        let completions = receiver.completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0]["recordingState"], "incomplete");
        assert_eq!(completions[0]["recordingReason"], "server-stopped");
        let events: Vec<serde_json::Value> = receiver
            .bodies()
            .into_iter()
            .filter(|body| body["event"] != "health")
            .map(|body| body["event"].clone())
            .collect();
        assert_eq!(events, ["publish", "unpublish", "recordSegmentComplete"]);
        assert!(
            outbox.list(10).expect("list").0.is_empty(),
            "a delivered report leaves no entry"
        );
    });
}

#[test]
fn a_report_undelivered_at_the_stop_deadline_is_sent_at_the_next_start_as_it_was() {
    let first = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    // The app takes `publish` and `unpublish`, then is down for the rest
    // of the stop.
    let (fixture, outbox, down) = first.block_on(async {
        let down = Receiver::start(|index| Some(if index < 2 { 204 } else { 503 })).await;
        let (fixture, outbox) = stopping_fixture(down.url());
        let mut live = begin_live(&fixture.app);
        crate::publish::loss_tests::Fixture::feed(&mut live);
        let outcome = crate::shutdown::stop(&fixture.app, &[], Duration::from_millis(300)).await;
        assert!(!outcome.in_time);
        drop(live);
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.app.recording_limits.writers_running() > 0
                || down.completions().is_empty()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the completion was attempted");
        (fixture, outbox, down)
    });
    let attempted = down.completions()[0].clone();
    drop(down);
    // The process exits with the callback still being retried.
    first.shutdown_timeout(Duration::from_secs(2));
    let stored = {
        let (names, _) = outbox.list(10).expect("list");
        names
            .iter()
            .map(|name| outbox.read(name).expect("entry").0)
            .collect::<Vec<_>>()
    };
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].finished.as_ref().map(|f| f.reason.as_deref()),
        Some(Some("server-stopped"))
    );
    block_on(async {
        let receiver = Receiver::start(|_| Some(204)).await;
        let pending = Pending::list(&outbox, MAX_PER_START);
        let callbacks = Callbacks::start(Some(receiver.url()), None);
        let summary = recording_recovery::run(
            outbox.clone(),
            pending,
            callbacks,
            WEEK,
            pacing(Duration::from_millis(10)),
        )
        .await;
        assert_eq!(summary.delivered, 1);
        assert_eq!(receiver.completions(), [attempted]);
    });
    assert!(outbox.list(10).expect("list").0.is_empty());
    drop(fixture);
}
