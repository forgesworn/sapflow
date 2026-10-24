//! A recorded ffmpeg publish, through our RTMP connection and into
//! `sapflow-hls`'s FLV ingest, makes LL-HLS segments.
use sapflow_hls::{FlvIngest, MuxerConfig};
use sapflow_rtmp::{Connection, Event};
use std::time::SystemTime;

const FFMPEG_PUBLISH: &[u8] = include_bytes!("fixtures/ffmpeg-rtmp-publish.bin");

#[test]
fn recorded_ffmpeg_publish_makes_segments() {
    let mut conn = Connection::new(1);
    conn.feed(FFMPEG_PUBLISH).expect("feed");
    let mut ing = FlvIngest::new(MuxerConfig::default());
    let mut last_video = 0;
    while let Some(ev) = conn.poll().expect("valid session") {
        match ev {
            Event::Publish { .. } => conn.accept_publish(),
            Event::Metadata { audio } => ing.expect_audio(audio),
            Event::Tracks { tracks } => {
                ing.declare_tracks(tracks, SystemTime::UNIX_EPOCH)
                    .expect("declared tracks");
            }
            Event::Video { timestamp, data } => {
                assert!(timestamp >= last_video, "video timestamps rise");
                last_video = timestamp;
                ing.video(timestamp, &data, SystemTime::UNIX_EPOCH)
                    .expect("video");
            }
            Event::Audio { timestamp, data } => ing.audio(timestamp, &data),
            Event::Finished => {}
        }
    }
    assert!(last_video > 2800, "last video at {last_video} ms");
    let m = ing.muxer().expect("muxer");
    assert!(m.has_content());
    let video = m.video().expect("AV regression has video");
    assert_eq!((video.width, video.height), (160, 90));
    assert_eq!(m.audio().map(|a| a.sample_rate), Some(48000));
}
