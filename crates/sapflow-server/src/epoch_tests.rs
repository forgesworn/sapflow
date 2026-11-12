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
    let (callbacks, _receiver) = crate::callback::test_queue(scope, 32);
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
