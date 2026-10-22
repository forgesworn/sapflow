//! Our RTMP connection (handshake, chunk stream, AMF0, session) on
//! arbitrary bytes, as every RTMP client reaches it.
//!
//! First byte: bit 0 picks the mode, the rest set the read size.
//! - Mode 0: the input is everything the client sends, from C0. Publish
//!   requests are accepted or refused by bit 1 of the second byte.
//! - Mode 1: a recorded ffmpeg session up to and including its publish
//!   request is replayed and accepted first, then the input continues
//!   the chunk stream, so media parsing (and FlvIngest) is reached.
#![no_main]
use std::sync::OnceLock;
use std::time::SystemTime;

use libfuzzer_sys::fuzz_target;
use sapflow_rtmp::{Connection, Event};
use sapflow_hls::{FlvIngest, MuxerConfig};

const RECORDED: &[u8] =
    include_bytes!("../../crates/sapflow-rtmp/tests/fixtures/ffmpeg-rtmp-publish.bin");

/// How many bytes of the recording end with its publish request.
fn publish_prefix() -> &'static [u8] {
    static LEN: OnceLock<usize> = OnceLock::new();
    let len = *LEN.get_or_init(|| {
        let mut conn = Connection::new(0);
        for (i, b) in RECORDED.iter().enumerate() {
            if conn.feed(&[*b]).is_err() {
                break;
            }
            if let Ok(Some(Event::Publish { .. })) = conn.poll() {
                return i + 1;
            }
        }
        0
    });
    &RECORDED[..len]
}

fuzz_target!(|data: &[u8]| {
    let Some((&head, rest)) = data.split_first() else {
        return;
    };
    let read = usize::from(head >> 1).max(1) * 37;
    let mut conn = Connection::new(u64::from(head));
    let mut ingest = FlvIngest::new(MuxerConfig::default());
    let accept = rest.first().is_some_and(|b| b & 2 != 0);
    let input = if head & 1 == 1 {
        let prefix = publish_prefix();
        if prefix.is_empty() || conn.feed(prefix).is_err() {
            return;
        }
        while let Ok(Some(_)) = conn.poll() {}
        conn.accept_publish();
        rest
    } else {
        rest
    };
    for part in input.chunks(read) {
        if conn.feed(part).is_err() {
            return;
        }
        loop {
            match conn.poll() {
                Ok(Some(Event::Publish { .. })) => {
                    if accept {
                        conn.accept_publish();
                    } else {
                        conn.refuse_publish("NetStream.Publish.Denied", "refused");
                    }
                }
                Ok(Some(Event::Video { timestamp, data })) => {
                    let _ = ingest.video(timestamp, &data, SystemTime::UNIX_EPOCH);
                }
                Ok(Some(Event::Audio { timestamp, data })) => ingest.audio(timestamp, &data),
                Ok(Some(Event::Metadata { audio })) => ingest.expect_audio(audio),
                Ok(Some(Event::Finished)) | Ok(None) => break,
                Err(_) => return,
            }
        }
        let _ = conn.take_output();
    }
});
