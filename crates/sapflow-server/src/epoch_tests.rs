//! Delivery-mode and codec-epoch behaviour of the viewer path, over real
//! sockets: the checks that used to call the Hyper handlers directly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sapflow_core::StreamPath;
use sapflow_hls::{PartLookup, Readiness, playlist};

use crate::App;
use crate::stream::SourceKind;
use crate::test_support::{EVENT, LANDSCAPE, PORTRAIT, Pending, app, begin, feed, viewer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_mode_serves_completed_segments_without_waiting_for_old_part_queries() {
    let (mut app, _scratch) = app(false);
    Arc::get_mut(&mut app)
        .expect("unshared fixture")
        .config
        .low_latency = false;
    let address = viewer(&app);
    let mut publisher = begin(&app);
    feed(&mut publisher, LANDSCAPE);
    let started = Instant::now();
    let response = Pending::new(
        address,
        "GET",
        &format!("/live/{EVENT}/stream.m3u8?_HLS_msn=999999&_HLS_part=99"),
    )
    .response(false);
    // A blocking request for a missing part would park for three target
    // durations, longer than the client's read deadline.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "whole playlists do not wait for parts"
    );
    assert_eq!(response.status, 200);
    let text = std::str::from_utf8(&response.body).expect("UTF-8 playlist");
    assert!(!text.contains("#EXT-X-PART"));
    assert!(!text.contains("#EXT-X-SERVER-CONTROL"));
    assert!(!text.contains("#EXT-X-PRELOAD-HINT"));
    let file = text
        .lines()
        .find(|line| !line.starts_with('#') && line.contains("_seg"))
        .expect("completed segment");
    let segment = Pending::new(address, "GET", &format!("/live/{EVENT}/{file}")).response(false);
    assert_eq!(segment.status, 200);
    assert!(!segment.body.is_empty());
}

async fn exercise(
    app: &Arc<App>,
    mut recording_callbacks: Option<&mut tokio::sync::mpsc::Receiver<crate::callback::Queued>>,
) {
    let address = viewer(app);
    let get = |file: &str| Pending::new(address, "GET", &format!("/live/{EVENT}/{file}"));
    let mut check = playlist::RuleCheck::default();
    let mut pending = None;
    let mut preloaded = None;
    let mut original = None;
    let mut prefix = String::new();
    for (epoch, data) in [PORTRAIT, LANDSCAPE, PORTRAIT].into_iter().enumerate() {
        let mut publisher = begin(app);
        feed(&mut publisher, data);
        if epoch > 0
            && let Some(callbacks) = &mut recording_callbacks
        {
            // Three offline epochs can arrive before the first writer drains.
            // Honour the two-writer path bound: wait for actual synced
            // completion of the previous epoch before sending the next one.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let callback = callbacks.recv().await.expect("recording callback queue");
                    let body: serde_json::Value =
                        serde_json::from_slice(&callback.body).expect("recording callback JSON");
                    if body["event"] == "recordSegmentComplete" {
                        break;
                    }
                }
            })
            .await
            .expect("previous codec epoch writer must finish");
        }
        let stream = publisher.stream.clone();
        let mut reload_msn = None;
        {
            let st = stream.lock();
            let m = st.ingest.muxer().expect("valid epoch regression fixture");
            let video = m.video().expect("AV regression has video");
            assert_eq!(
                (video.width, video.height),
                if epoch == 1 { (1280, 720) } else { (720, 1280) }
            );
            assert_eq!(
                m.audio()
                    .expect("valid epoch regression fixture")
                    .sample_rate,
                48_000
            );
            let text =
                std::str::from_utf8(st.media.as_ref().expect("valid epoch regression fixture"))
                    .expect("valid epoch regression fixture");
            check.check(text).expect("valid epoch regression fixture");
            assert_eq!(m.target_duration(), 2);
            assert_eq!(m.current_initialization_id(), epoch as u64);
            assert_eq!(text.matches("#EXT-X-MAP:").count(), epoch + 1);
            if epoch == 0 {
                prefix = st.prefix.clone();
                original = Some((
                    m.init().clone(),
                    m.segment(7)
                        .expect("valid epoch regression fixture")
                        .data
                        .clone(),
                ));
                let msn = m.media_sequence() + m.entries().len() as u64 + 1;
                assert_eq!(m.readiness(Some(msn), Some(0)), Readiness::Wait);
                reload_msn = Some(msn);
            } else {
                assert_eq!(st.prefix, prefix);
                let (init, segment) = original.as_ref().expect("valid epoch regression fixture");
                assert_eq!(m.initialization(0), Some(init));
                assert_eq!(
                    &m.segment(7).expect("valid epoch regression fixture").data,
                    segment
                );
                let new_map = format!(
                    "#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"{}\"",
                    playlist::epoch_init_name(&prefix, epoch as u64)
                );
                assert!(
                    text.contains(&new_map),
                    "new MAP must precede all new-epoch parts"
                );
            }
            // Do not hold a state lock while a viewer is served or a
            // publication is cleaned up.
        }
        let (_, parts) = stream
            .parts_after(None)
            .expect("valid epoch regression fixture");
        assert!(!parts.is_empty());
        assert!(
            parts
                .iter()
                .all(|part| part.initialization.id == epoch as u64)
        );
        if let Some(msn) = reload_msn {
            // A blocking reload for the next segment parks across the epochs.
            let mut request = get(&format!("stream.m3u8?_HLS_msn={msn}&_HLS_part=0"));
            request.assert_waiting();
            pending = Some(request);
        }
        publisher.end();
        if epoch == 0 {
            let before = stream.lock().media.clone();
            // A short intermediate phone pose may connect and leave before
            // any frame. It must not advertise an empty epoch or new MAP.
            begin(app).end();
            assert_eq!(stream.lock().media, before);
            let hint = stream
                .lock()
                .ingest
                .muxer()
                .expect("valid epoch regression fixture")
                .next_part_id();
            let mut request = get(&playlist::part_name(&prefix, hint));
            request.assert_waiting();
            preloaded = Some((hint, request));
        }
        if epoch == 1 {
            let response = pending
                .take()
                .expect("valid epoch regression fixture")
                .response(false);
            assert_eq!(response.status, 200);
            assert!(
                std::str::from_utf8(&response.body)
                    .expect("valid epoch regression fixture")
                    .contains(&prefix)
            );
            let (hint, request) = preloaded.take().expect("valid epoch regression fixture");
            let response = request.response(false);
            assert_eq!(response.status, 200);
            let PartLookup::Found(part) = stream
                .lock()
                .ingest
                .muxer()
                .expect("valid epoch regression fixture")
                .part(hint)
            else {
                panic!("hint must resolve")
            };
            assert_eq!(part.initialization.id, 1);
            assert_eq!(response.body.as_slice(), part.data.as_ref());
        }
    }
    // Old cached initialization URI remains servable across the window.
    let response = get(&playlist::init_name(&prefix)).response(false);
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body.as_slice(),
        original.expect("valid epoch regression fixture").0.as_ref()
    );
    let path = StreamPath::parse("live/event-epochs").expect("valid epoch regression fixture");
    let (stream, generation, _) = app
        .streams
        .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Ts)
        .expect("valid epoch regression fixture");
    app.streams.detach(&stream, generation, false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn portrait_landscape_portrait_preserves_http_identity_and_pending_reload() {
    let (app, _scratch) = app(false);
    exercise(&app, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed ffmpeg and ffprobe; run explicitly for origin acceptance"]
async fn each_resolution_epoch_recording_decodes_independently_with_audio() {
    let (mut app, scratch) = app(true);
    let scope = crate::publish_observer::fixture_scope("epochs", 1, 1, 1);
    let (callbacks, mut receiver) = crate::callback::test_queue(scope, 32);
    Arc::get_mut(&mut app).expect("sole fixture App").callbacks = callbacks;
    exercise(&app, Some(&mut receiver)).await;
    let directory = scratch.0.join("live/event-epochs");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let files = loop {
        let mut files: Vec<_> = std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        files.sort();
        if files.len() == 3
            && files.iter().all(|file| {
                let output = std::process::Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-show_entries",
                        "format=duration",
                        "-of",
                        "csv=p=0",
                    ])
                    .arg(file)
                    .output()
                    .expect("valid epoch regression fixture");
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<f64>()
                    .is_ok_and(|seconds| seconds > 1.5 && seconds < 3.0)
            })
        {
            break files;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "three finalised epoch files required"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    // Packets per track, as (video, audio).
    let packets = |file: &std::path::Path| {
        let count = |track: &str| {
            let output = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", track])
                .args(["-show_entries", "packet=pts", "-of", "csv=p=0"])
                .arg(file)
                .output()
                .expect("ffprobe runs");
            String::from_utf8_lossy(&output.stdout).lines().count()
        };
        (count("v:0"), count("a:0"))
    };
    let mut widths = Vec::new();
    let mut recorded = Vec::new();
    for file in &files {
        recorded.push(packets(file));
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,width,height,start_time",
                "-of",
                "json",
            ])
            .arg(file)
            .output()
            .expect("valid epoch regression fixture");
        let text = String::from_utf8(output.stdout).expect("valid epoch regression fixture");
        assert!(output.status.success());
        assert!(
            text.contains("h264") && text.contains("aac"),
            "both tracks required: {text}"
        );
        let metadata: serde_json::Value = serde_json::from_str(&text).expect("ffprobe metadata");
        let video = metadata["streams"]
            .as_array()
            .expect("recorded tracks")
            .iter()
            .find(|track| track["codec_name"] == "h264")
            .expect("recorded video");
        widths.push(video["width"].as_u64().expect("recorded width"));
        assert!(
            text.contains("0.000000") || text.contains("0.000011"),
            "file origin near zero: {text}"
        );
        let status = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
            .arg(file)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
            .status()
            .expect("valid epoch regression fixture");
        assert!(
            status.success(),
            "independent audio+video decode failed: {}",
            file.display()
        );
    }
    // Writer tasks can create their files in a different order from codec
    // arrival. HTTP identities test epoch order; recordings must preserve the
    // expected track configurations and decode independently of their names.
    widths.sort_unstable();
    assert_eq!(widths, [720, 720, 1280]);
}

/// A resolution change mid-stream on one connection, as an encoder under
/// load makes: the ingest starts a new codec epoch (`restarted`), the
/// recording rolls to a new file there, and each file decodes on its own,
/// audio and video, with no error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed ffmpeg and ffprobe; run explicitly for origin acceptance"]
async fn a_mid_stream_resolution_change_rolls_the_recording_and_both_files_decode() {
    const CHANGE: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/resolution-change.ts");
    let (mut app, scratch) = app(true);
    let scope = crate::publish_observer::fixture_scope("epochs", 1, 1, 1);
    let (callbacks, mut receiver) = crate::callback::test_queue(scope, 32);
    Arc::get_mut(&mut app).expect("sole fixture App").callbacks = callbacks;
    let mut publisher = begin(&app);
    // As SRT delivers it: one datagram's worth at a time.
    for chunk in CHANGE.chunks(1316) {
        feed(&mut publisher, chunk);
    }
    {
        let stream = publisher.stream.clone();
        let st = stream.lock();
        let m = st.ingest.muxer().expect("muxer");
        assert_eq!(m.current_initialization_id(), 1, "one new epoch");
        // Each epoch's video and audio as the muxer holds them, to compare
        // with the recordings.
        for epoch in 0..=1u64 {
            let mut file = m.initialization(epoch).expect("init").to_vec();
            for id in 0..m.next_part_id() {
                if let sapflow_hls::PartLookup::Found(p) = m.part(id)
                    && p.initialization.id == epoch
                {
                    file.extend_from_slice(&p.data);
                }
            }
            std::fs::write(scratch.0.join(format!("muxer-epoch{epoch}.mp4")), file)
                .expect("scratch");
        }
    }
    publisher.end();
    // End the held stream, so the last file is finished.
    let path = StreamPath::parse("live/event-epochs").expect("path");
    let (stream, generation, _) = app
        .streams
        .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Ts)
        .expect("claim");
    app.streams.detach(&stream, generation, false);
    let directory = scratch.0.join("live/event-epochs");
    let duration = |file: &std::path::Path| {
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(file)
            .output()
            .expect("ffprobe runs");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let files = loop {
        let mut files: Vec<_> = std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        files.sort();
        if files.len() == 2 && files.iter().all(|f| duration(f) > 1.0) {
            break files;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "two finished recordings required, found {files:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    // Packets per track, as (video, audio).
    let packets = |file: &std::path::Path| {
        let count = |track: &str| {
            let output = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", track])
                .args(["-show_entries", "packet=pts", "-of", "csv=p=0"])
                .arg(file)
                .output()
                .expect("ffprobe runs");
            String::from_utf8_lossy(&output.stdout).lines().count()
        };
        (count("v:0"), count("a:0"))
    };
    let mut widths = Vec::new();
    let mut recorded = Vec::new();
    for file in &files {
        recorded.push(packets(file));
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "stream=width", "-of", "csv=p=0"])
            .arg(file)
            .output()
            .expect("ffprobe runs");
        widths.push(
            String::from_utf8_lossy(&probe.stdout)
                .trim()
                .parse::<u32>()
                .expect("a width"),
        );
        let decode = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-xerror", "-i"])
            .arg(file)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "{}: {}",
            file.display(),
            String::from_utf8_lossy(&decode.stderr)
        );
    }
    widths.sort_unstable();
    assert_eq!(widths, [128, 160], "one file per resolution");
    // The file of the first resolution holds every packet the muxer cut
    // in its epoch, the last parts sealed by the change included.
    let first = files
        .iter()
        .zip(&widths_of(&files))
        .position(|(_, w)| *w == 160)
        .expect("the 160x90 file");
    assert_eq!(
        recorded[first],
        packets(&scratch.0.join("muxer-epoch0.mp4")),
        "the old recording keeps its epoch's tail: files {:?}, widths {:?}, packets {recorded:?}",
        files,
        widths_of(&files)
    );
    // The new file holds at least what the muxer had cut in its epoch when
    // the feed ended; the end of the stream adds the tail.
    let (video, audio) = recorded[1 - first];
    let (cut_video, cut_audio) = packets(&scratch.0.join("muxer-epoch1.mp4"));
    assert!(
        video >= cut_video && audio >= cut_audio && cut_video > 0,
        "the new recording: {recorded:?} for ({cut_video}, {cut_audio}) cut"
    );
    // Every input frame, counted: the fixture has 60 video frames each side
    // of the change. The first file has all 60, the held frame at the
    // change included; the second all but the last, whose PES never ended
    // (TS finish leaves an unbounded final PES out).
    assert_eq!((recorded[first].0, video), (60, 59), "video frames");
    // The fixture's AAC: one frame before the first keyframe (no run to
    // join), 95 from it to the change and 94 after. The first file has all
    // 95 but the late one below; the second all 94.
    assert_eq!((recorded[first].1, audio), (94, 94), "AAC frames");
    // The fixture's AAC PES arrive up to 200 ms behind their video, so the
    // frame stamped just before the change arrives after the new run's
    // first part: it is dropped. The file it belonged to, the first, says
    // so; the second has all of its own.
    let mut done = Vec::new();
    while let Ok(callback) = receiver.try_recv() {
        let body: serde_json::Value =
            serde_json::from_slice(&callback.body).expect("recording callback JSON");
        if body["event"] == "recordSegmentComplete" {
            done.push(body);
        }
    }
    let state = |file: &std::path::Path| {
        done.iter()
            .find(|b| b["segmentPath"].as_str() == file.to_str())
            .map(|b| (b["recordingState"].clone(), b["recordingReason"].clone()))
            .expect("a callback for the file")
    };
    assert_eq!(
        state(&files[first]),
        ("incomplete".into(), "media-loss".into())
    );
    assert_eq!(state(&files[1 - first]).0, "finalised");
}

fn widths_of(files: &[std::path::PathBuf]) -> Vec<u32> {
    files
        .iter()
        .map(|file| {
            let probe = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", "v:0"])
                .args(["-show_entries", "stream=width", "-of", "csv=p=0"])
                .arg(file)
                .output()
                .expect("ffprobe runs");
            String::from_utf8_lossy(&probe.stdout)
                .trim()
                .parse::<u32>()
                .expect("a width")
        })
        .collect()
}

const BLIP: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/resolution-blip.ts");

/// Feeds `resolution-blip.ts` (160x90, three frames at 128x72, 160x90) as
/// SRT delivers it, ends the stream, and returns the recording callbacks
/// and the files. The middle epoch is shorter than a part, but part timing
/// is fixed by then, so the change seals its open part: it is served.
async fn short_epoch_run() -> (
    Vec<serde_json::Value>,
    Vec<std::path::PathBuf>,
    crate::test_support::Scratch,
) {
    let (mut app, scratch) = app(true);
    let scope = crate::publish_observer::fixture_scope("epochs", 1, 1, 1);
    let (callbacks, mut receiver) = crate::callback::test_queue(scope, 32);
    Arc::get_mut(&mut app).expect("sole fixture App").callbacks = callbacks;
    let mut publisher = begin(&app);
    let mut done = Vec::new();
    let next = |receiver: &mut tokio::sync::mpsc::Receiver<crate::callback::Queued>| {
        let callback = receiver.try_recv().ok()?;
        let body: serde_json::Value =
            serde_json::from_slice(&callback.body).expect("recording callback JSON");
        (body["event"] == "recordSegmentComplete").then_some(body)
    };
    let mut waited = false;
    for chunk in BLIP.chunks(1316) {
        feed(&mut publisher, chunk);
        let epoch = publisher
            .stream
            .lock()
            .ingest
            .muxer()
            .map(|m| m.current_initialization_id());
        if epoch == Some(2) && !waited {
            // As in real time, where the next part gives the first file's
            // writer time to finish: two writers a path. The first file is
            // finished at the second change, which ends its late audio.
            waited = true;
            tokio::time::timeout(Duration::from_secs(5), async {
                while done.is_empty() {
                    match next(&mut receiver) {
                        Some(body) => done.push(body),
                        None => tokio::time::sleep(Duration::from_millis(5)).await,
                    }
                }
            })
            .await
            .expect("the first file finishes");
        }
    }
    {
        let stream = publisher.stream.clone();
        let st = stream.lock();
        let m = st.ingest.muxer().expect("muxer");
        assert_eq!(m.current_initialization_id(), 2, "two changes");
        let text = std::str::from_utf8(st.media.as_ref().expect("playlist")).expect("UTF-8");
        let short = format!(
            "#EXT-X-MAP:URI=\"{}\"\n",
            playlist::epoch_init_name(&st.prefix, 1)
        );
        let at = text
            .find(&short)
            .expect("the short epoch is in the playlist");
        assert!(
            text[at..]
                .lines()
                .nth(1)
                .is_some_and(|l| !l.starts_with("#EXT-X-DISCONTINUITY")),
            "with media under it: {text}"
        );
    }
    publisher.end();
    let path = StreamPath::parse("live/event-epochs").expect("path");
    let (stream, generation, _) = app
        .streams
        .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Ts)
        .expect("claim");
    app.streams.detach(&stream, generation, false);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while done.len() < 3 {
            let callback = receiver.recv().await.expect("recording callback queue");
            let body: serde_json::Value =
                serde_json::from_slice(&callback.body).expect("recording callback JSON");
            if body["event"] == "recordSegmentComplete" {
                done.push(body);
            }
        }
    })
    .await;
    let mut files: Vec<_> = std::fs::read_dir(scratch.0.join("live/event-epochs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    files.sort();
    (done, files, scratch)
}

/// Served media is recorded: the short epoch gets its own file, and every
/// file is finalised.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_short_epoch_that_was_served_is_recorded() {
    let (done, files, _scratch) = short_epoch_run().await;
    assert_eq!(done.len(), 3, "one recording per served epoch: {done:?}");
    assert!(
        done.iter().all(|b| b["recordingState"] == "finalised"),
        "{done:?}"
    );
    assert_eq!(files.len(), 3);
}

/// Each of the three files decodes alone, at its epoch's size, and the
/// short epoch's file holds video.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed ffmpeg and ffprobe; run explicitly for origin acceptance"]
async fn a_short_epoch_recording_decodes() {
    let (_, files, _scratch) = short_epoch_run().await;
    let mut widths = widths_of(&files);
    for file in &files {
        let decode = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-xerror", "-i"])
            .arg(file)
            .args(["-map", "0:v:0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg runs");
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "{}: {}",
            file.display(),
            String::from_utf8_lossy(&decode.stderr)
        );
    }
    widths.sort_unstable();
    assert_eq!(widths, [128, 160, 160]);
}

/// A recording's first part refused (here every part: a per-path budget of
/// one byte) leaves a late-started epoch unrecorded, and the next file must
/// say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_late_start_marks_the_next_recording() {
    let (mut app, _scratch) = app(true);
    Arc::get_mut(&mut app)
        .expect("sole fixture App")
        .recording_limits = Arc::new(crate::recorder::RecordingLimits::with_path_bytes(1, 1));
    let mut publisher = begin(&app);
    for chunk in BLIP.chunks(1316) {
        feed(&mut publisher, chunk);
    }
    let session = publisher.stream.session();
    let recorder = session
        .recorder
        .as_ref()
        .expect("the third epoch's recorder");
    assert_eq!(
        recorder.status().reason(),
        Some(crate::recorder::IncompleteReason::EpochUnrecorded)
    );
}

/// A PES start in one TS packet: its stream id and PTS (90 kHz), from the
/// MPEG-TS packet and PES header layouts.
fn pes_start(p: &[u8]) -> Option<(u8, u64)> {
    if p.len() < 188 || p[0] != 0x47 || p[1] & 0x40 == 0 {
        return None;
    }
    let control = (p[3] >> 4) & 3;
    let mut at = 4;
    if control & 2 != 0 {
        at += 1 + usize::from(p[4]);
    }
    if control & 1 == 0 {
        return None;
    }
    let pes = p.get(at..)?;
    if pes.len() < 14 || pes[..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
        return None;
    }
    let b = &pes[9..14];
    let pts = ((u64::from(b[0]) >> 1) & 7) << 30
        | u64::from(b[1]) << 22
        | (u64::from(b[2]) >> 1) << 15
        | u64::from(b[3]) << 7
        | u64::from(b[4]) >> 1;
    Some((pes[3], pts))
}

/// Video PES starts in a transport stream, with their PTS (90 kHz).
fn video_pes_starts(ts: &[u8]) -> Vec<(usize, u64)> {
    ts.chunks(188)
        .enumerate()
        .filter_map(|(i, p)| match pes_start(p)? {
            (0xe0, pts) => Some((i * 188, pts)),
            _ => None,
        })
        .collect()
}

/// One elementary frame of a transport stream: video (DTS, PTS, Annex B) or
/// one AAC frame (PTS, payload), times 90 kHz.
enum Frame {
    Video(i64, i64, Vec<u8>),
    Aac(i64, sapflow_hls::codec::AacConfig, Vec<u8>),
}

impl Frame {
    fn at(&self) -> i64 {
        match self {
            Self::Video(dts, ..) | Self::Aac(dts, ..) => *dts,
        }
    }
}

/// The elementary frames of a clean, single-programme TS in file order,
/// from the MPEG-TS packet and PES header layouts (no clock wrap).
fn elementary_frames(ts: &[u8]) -> Vec<Frame> {
    let times = |pes: &[u8]| -> (i64, i64, usize) {
        let ts = |b: &[u8]| {
            (((u64::from(b[0]) >> 1) & 7) << 30
                | u64::from(b[1]) << 22
                | (u64::from(b[2]) >> 1) << 15
                | u64::from(b[3]) << 7
                | u64::from(b[4]) >> 1) as i64
        };
        let pts = ts(&pes[9..14]);
        let dts = if pes[7] & 0x40 != 0 {
            ts(&pes[14..19])
        } else {
            pts
        };
        (dts, pts, 9 + usize::from(pes[8]))
    };
    let mut out = Vec::new();
    let mut open: std::collections::HashMap<u16, Vec<u8>> = Default::default();
    let emit = |pes: Vec<u8>, out: &mut Vec<Frame>| {
        if pes.len() < 14 || pes[7] & 0x80 == 0 {
            return;
        }
        let (dts, pts, body) = times(&pes);
        match pes[3] {
            0xe0 => out.push(Frame::Video(dts, pts, pes[body..].to_vec())),
            0xc0..=0xdf => {
                for (i, (config, payload)) in sapflow_hls::codec::adts_frames(&pes[body..])
                    .into_iter()
                    .enumerate()
                {
                    let step = 1024 * 90_000 / i64::from(config.sample_rate);
                    out.push(Frame::Aac(pts + step * i as i64, config, payload.to_vec()));
                }
            }
            _ => {}
        }
    };
    for p in ts.chunks(188) {
        if p.len() < 188 || p[0] != 0x47 {
            continue;
        }
        let pid = (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
        let control = (p[3] >> 4) & 3;
        let mut at = 4;
        if control & 2 != 0 {
            at += 1 + usize::from(p[4]);
        }
        if control & 1 == 0 || at >= 188 || pid == 0 {
            continue;
        }
        let payload = &p[at..];
        if p[1] & 0x40 != 0 {
            if payload.starts_with(&[0, 0, 1]) {
                if let Some(done) = open.insert(pid, payload.to_vec()) {
                    emit(done, &mut out);
                }
            } else {
                // A table (PAT, PMT): not a PES.
                open.remove(&pid);
            }
        } else if let Some(pes) = open.get_mut(&pid) {
            pes.extend_from_slice(payload);
        }
    }
    let mut rest: Vec<_> = open.into_values().collect();
    rest.sort_by_key(|pes| pes.len());
    for pes in rest {
        emit(pes, &mut out);
    }
    out
}

/// The frames in time order, but each AAC frame arriving just before the
/// first video frame `lag` (90 kHz) past it, or at the end: audio that far
/// behind its video, as from an encoder or a muxer with that much delay.
fn audio_behind(frames: Vec<Frame>, lag: i64) -> Vec<Frame> {
    let (mut video, audio): (Vec<Frame>, Vec<Frame>) = frames
        .into_iter()
        .partition(|f| matches!(f, Frame::Video(..)));
    video.sort_by_key(Frame::at);
    let mut audio = audio;
    audio.sort_by_key(Frame::at);
    let mut out = Vec::new();
    let mut audio = audio.into_iter().peekable();
    for v in video {
        while audio.peek().is_some_and(|a| a.at() + lag <= v.at()) {
            out.push(audio.next().expect("peeked"));
        }
        out.push(v);
    }
    out.extend(audio);
    out
}

/// The resolution change with its audio arriving 100, 300, 500 and 1000 ms
/// behind its video, through the elementary-stream path (WHIP's). Audio
/// stamped before the change that arrives while the new run's first part is
/// open joins it, in the new file; after that it is dropped. A drop marks
/// the file whose audio it was, the first, and only then; the second file
/// is never marked for it. Every frame is in a file or counted as dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed ffmpeg and ffprobe; run explicitly for origin acceptance"]
async fn late_audio_at_a_resolution_change_marks_only_the_file_it_belonged_to() {
    const CHANGE: &[u8] = include_bytes!("../../sapflow-hls/tests/fixtures/resolution-change.ts");
    let mut table = Vec::new();
    for lag_ms in [0i64, 100, 300, 500, 1000] {
        let frames = audio_behind(elementary_frames(CHANGE), lag_ms * 90);
        let first_idr = frames
            .iter()
            .find_map(|f| match f {
                Frame::Video(dts, _, annexb) if sapflow_hls::codec::access_unit(annexb).idr => {
                    Some(*dts)
                }
                _ => None,
            })
            .expect("an IDR");
        // The AAC from the first keyframe on (one frame before it has no
        // run to join): 95 before the change and 94 after. As at any
        // stream's start, of those that arrive before the second video
        // frame opens the first part, only the last is kept.
        let second = frames
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f, Frame::Video(..)))
            .nth(1)
            .map_or(0, |(i, _)| i);
        let early = frames[..second]
            .iter()
            .filter(|f| matches!(f, Frame::Aac(pts, ..) if *pts >= first_idr))
            .count();
        let audio = frames
            .iter()
            .filter(|f| matches!(f, Frame::Aac(pts, ..) if *pts >= first_idr))
            .count()
            - early.saturating_sub(1);
        let (mut app, scratch) = app(true);
        let scope = crate::publish_observer::fixture_scope("epochs", 1, 1, 1);
        let (callbacks, mut receiver) = crate::callback::test_queue(scope, 32);
        Arc::get_mut(&mut app).expect("sole fixture App").callbacks = callbacks;
        let req = sapflow_core::PublishRequest::parse_srt_stream_id(
            "publish:live/event-epochs:fixture:fixture",
        )
        .expect("request");
        let mut publisher = crate::publish::Publication::begin(
            &app,
            &req,
            SourceKind::Es,
            "127.0.0.1".parse().expect("loopback"),
        )
        .expect("begin");
        let config = frames
            .iter()
            .find_map(|f| match f {
                Frame::Aac(_, config, _) => Some(*config),
                Frame::Video(..) => None,
            })
            .expect("AAC");
        for frame in &frames {
            publisher
                .push(|source, now| {
                    let crate::stream::Source::Es(es) = source else {
                        unreachable!()
                    };
                    es.audio_config(config);
                    match frame {
                        Frame::Video(dts, pts, annexb) => {
                            es.video(*dts, *pts, sapflow_hls::codec::access_unit(annexb), now)
                        }
                        Frame::Aac(pts, config, payload) => {
                            es.audio_at(*pts, *config, payload, now)
                        }
                    }
                })
                .expect("fixture frames");
        }
        publisher.end();
        let path = StreamPath::parse("live/event-epochs").expect("path");
        let (stream, generation, _) = app
            .streams
            .claim(&path, sapflow_hls::MuxerConfig::default(), SourceKind::Es)
            .expect("claim");
        app.streams.detach(&stream, generation, false);
        let mut done = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while done.len() < 2 {
                let callback = receiver.recv().await.expect("recording callback queue");
                let body: serde_json::Value =
                    serde_json::from_slice(&callback.body).expect("recording callback JSON");
                if body["event"] == "recordSegmentComplete" {
                    done.push(body);
                }
            }
        })
        .await
        .expect("two finished recordings");
        let count = |file: &std::path::Path, track: &str| {
            let output = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", track])
                .args(["-show_entries", "packet=pts", "-of", "csv=p=0"])
                .arg(file)
                .output()
                .expect("ffprobe runs");
            String::from_utf8_lossy(&output.stdout).lines().count()
        };
        // Per file: its width, (video, AAC) frames, and (state, reason).
        type Recorded = (u32, (usize, usize), (String, String));
        let mut files: Vec<Recorded> = done
            .iter()
            .map(|b| {
                let file = std::path::PathBuf::from(b["segmentPath"].as_str().expect("a path"));
                let width = widths_of(std::slice::from_ref(&file))[0];
                let decode = std::process::Command::new("ffmpeg")
                    .args(["-hide_banner", "-v", "error", "-xerror", "-i"])
                    .arg(&file)
                    .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
                    .output()
                    .expect("ffmpeg runs");
                assert!(
                    decode.status.success() && decode.stderr.is_empty(),
                    "{lag_ms} ms: {}: {}",
                    file.display(),
                    String::from_utf8_lossy(&decode.stderr)
                );
                let state = (
                    b["recordingState"].as_str().unwrap_or_default().to_owned(),
                    b["recordingReason"].as_str().unwrap_or_default().to_owned(),
                );
                (width, (count(&file, "v:0"), count(&file, "a:0")), state)
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        let [(160, old, old_state), (128, new, new_state)] = &files[..] else {
            panic!("{lag_ms} ms: one file per resolution: {files:?}");
        };
        let dropped = audio - old.1 - new.1;
        table.push((lag_ms, *old, *new, dropped, old_state.clone()));
        assert_eq!((old.0, new.0), (60, 60), "{lag_ms} ms: every video frame");
        assert_eq!(new_state.0, "finalised", "{lag_ms} ms: the second file");
        if dropped > 0 {
            assert_eq!(
                (old_state.0.as_str(), old_state.1.as_str()),
                ("incomplete", "media-loss"),
                "{lag_ms} ms: {dropped} dropped"
            );
        } else {
            assert_eq!(old_state.0, "finalised", "{lag_ms} ms: nothing dropped");
        }
        drop(scratch);
    }
    eprintln!("lag ms, (video, AAC) first file, second file, AAC dropped, first file: {table:?}");
    // The first part of the new run holds about 200 ms of video: audio
    // later than that behind is what is dropped.
    let dropped: Vec<usize> = table.iter().map(|row| row.3).collect();
    assert_eq!(dropped, [0, 0, 5, 15, 38], "AAC dropped at 0 to 1000 ms");
}

/// A whole short epoch and the next one's first IDR inside one push: the
/// started recording still gets its own epoch's tail, and the short epoch,
/// which left no tracks, marks the next file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed ffprobe; run explicitly for origin acceptance"]
async fn a_started_recording_keeps_its_tail_when_an_epoch_passes_in_one_push() {
    let (mut app, scratch) = app(true);
    let scope = crate::publish_observer::fixture_scope("epochs", 1, 1, 1);
    let (callbacks, mut receiver) = crate::callback::test_queue(scope, 32);
    Arc::get_mut(&mut app).expect("sole fixture App").callbacks = callbacks;
    let starts = video_pes_starts(BLIP);
    // The short epoch's IDR at 3.442667 s, the next one's at 3.564 s.
    let at = |pts: u64| {
        starts
            .iter()
            .position(|(_, p)| p.abs_diff(pts) < 10)
            .expect("an IDR at that time")
    };
    let (short, next) = (at(309_840), at(320_760));
    let begin_short = starts[short].0;
    // Through the start of the PES after the next IDR, so that IDR is whole.
    let end = starts.get(next + 1).map_or(BLIP.len(), |(o, _)| o + 188);
    let mut publisher = begin(&app);
    for chunk in BLIP[..begin_short].chunks(1316) {
        feed(&mut publisher, chunk);
    }
    feed(&mut publisher, &BLIP[begin_short..end]);
    {
        let stream = publisher.stream.clone();
        let st = stream.lock();
        let m = st.ingest.muxer().expect("muxer");
        assert_eq!(m.current_initialization_id(), 2, "both changes in one push");
        let mut file = m.initialization(0).expect("init").to_vec();
        for id in 0..m.next_part_id() {
            if let sapflow_hls::PartLookup::Found(p) = m.part(id)
                && p.initialization.id == 0
            {
                file.extend_from_slice(&p.data);
            }
        }
        std::fs::write(scratch.0.join("muxer-epoch0.mp4"), file).expect("scratch");
        drop(st);
        let session = stream.session();
        assert_eq!(
            session
                .recorder
                .as_ref()
                .expect("recorder")
                .status()
                .reason(),
            Some(crate::recorder::IncompleteReason::EpochUnrecorded),
            "the short epoch had no recording"
        );
    }
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let callback = receiver.recv().await.expect("recording callback queue");
            let body: serde_json::Value =
                serde_json::from_slice(&callback.body).expect("recording callback JSON");
            if body["event"] == "recordSegmentComplete" {
                break body;
            }
        }
    })
    .await
    .expect("the first file finishes");
    assert_eq!(first["recordingState"], "finalised");
    let file = std::path::PathBuf::from(first["segmentPath"].as_str().expect("a path"));
    let packets = |file: &std::path::Path| {
        let output = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "packet=pts", "-of", "csv=p=0"])
            .arg(file)
            .output()
            .expect("ffprobe runs");
        String::from_utf8_lossy(&output.stdout).lines().count()
    };
    assert_eq!(
        packets(&file),
        packets(&scratch.0.join("muxer-epoch0.mp4")),
        "the first file keeps its epoch's tail"
    );
    publisher.end();
}
