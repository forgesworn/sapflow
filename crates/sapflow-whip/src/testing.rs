//! Test helpers, for tests and the fuzz harness only: the W0 fixtures and
//! a synthetic offer.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs
)]

use crate::sdp;
use sapflow_dtls::Fingerprint;

/// A W0 fixture under `fixtures/whip/w0/`, as text.
pub fn fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/whip/w0")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p:?}: {e}"))
}

/// The synthetic offer's ICE password.
pub const OFFER_PWD: &str = "0123456789abcdefghijklmn";
/// The synthetic offer's SSRCs: audio, video, video RTX.
pub const AUDIO_SSRC: u32 = 0x1001;
pub const VIDEO_SSRC: u32 = 0x2001;
pub const RTX_SSRC: u32 = 0x2002;
/// Its payload types and extension IDs.
pub const OPUS_PT: u8 = 111;
pub const H264_PT: u8 = 96;
pub const RTX_PT: u8 = 97;
pub const MID_ID: u8 = 4;
pub const TWCC_ID: u8 = 3;

/// A Chrome-shaped offer: audio (mid 0) then video (mid 1), BUNDLE, per
/// section credentials, `actpass`, the MID and transport-cc extensions,
/// H.264 Constrained Baseline with RTX, and SSRCs.
pub fn synthetic_offer(fingerprint: &Fingerprint, ufrag: &str) -> String {
    let fp = fingerprint.to_sdp();
    let transport = format!(
        "a=ice-ufrag:{ufrag}\r\na=ice-pwd:{OFFER_PWD}\r\na=fingerprint:sha-256 {fp}\r\na=setup:actpass\r\n"
    );
    let ext = format!(
        "a=extmap:{MID_ID} {}\r\na=extmap:{TWCC_ID} {}\r\n",
        sdp::MID_URI,
        sdp::TRANSPORT_CC_URI
    );
    format!(
        "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\n\
         a=msid-semantic: WMS\r\n\
         m=audio 9 UDP/TLS/RTP/SAVPF {OPUS_PT} 0\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:0\r\n{ext}\
         a=sendonly\r\na=rtcp-mux\r\na=rtpmap:{OPUS_PT} opus/48000/2\r\na=rtcp-fb:{OPUS_PT} transport-cc\r\n\
         a=fmtp:{OPUS_PT} minptime=10;useinbandfec=1\r\na=ssrc:{AUDIO_SSRC} cname:x\r\n\
         m=video 9 UDP/TLS/RTP/SAVPF {H264_PT} {RTX_PT}\r\nc=IN IP4 0.0.0.0\r\n{transport}a=mid:1\r\n{ext}\
         a=sendonly\r\na=rtcp-mux\r\na=rtpmap:{H264_PT} H264/90000\r\na=rtcp-fb:{H264_PT} nack\r\n\
         a=rtcp-fb:{H264_PT} nack pli\r\na=rtcp-fb:{H264_PT} ccm fir\r\na=rtcp-fb:{H264_PT} transport-cc\r\n\
         a=fmtp:{H264_PT} level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n\
         a=rtpmap:{RTX_PT} rtx/90000\r\na=fmtp:{RTX_PT} apt={H264_PT}\r\n\
         a=ssrc-group:FID {VIDEO_SSRC} {RTX_SSRC}\r\na=ssrc:{VIDEO_SSRC} cname:x\r\na=ssrc:{RTX_SSRC} cname:x\r\n"
    )
}
