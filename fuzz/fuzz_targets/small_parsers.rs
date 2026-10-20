//! Every small parser that sees input from outside.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_core::{PublishRequest, recording};
use sapflow_hls::codec::{self, AacConfig};
use sapflow_hls::playlist;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let _ = PublishRequest::parse_srt_stream_id(&s);
    if let Some((app, key)) = s.split_once('\n') {
        let _ = PublishRequest::parse_rtmp(app, key);
    }
    let _ = playlist::parse_chunk("abc", &s);
    let _ = playlist::parse_blocking_query(Some(&s));
    let _ = recording::parse_age(&s);
    let _ = recording::is_recording_name(&s);
    let _ = AacConfig::from_audio_specific_config(data);
    let _ = codec::avc_decoder_config(data);
    let _ = codec::adts_frames(data);
    let _ = codec::access_unit(data);
    for n in 1..=4 {
        let _ = codec::access_unit_avcc(data, n);
    }
    if data.len() > 4 {
        let _ = codec::H264Config::new(data, &data[..2]);
    }
});
