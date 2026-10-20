//! srt-protocol's packet parser (every datagram to the SRT port reaches it),
//! and re-encoding whatever it accepts: a panic on encode is a known risk.
#![no_main]
use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use srt_protocol::packet::Packet;

fuzz_target!(|data: &[u8]| {
    let Some((&flag, rest)) = data.split_first() else { return };
    let mut buf = rest;
    if let Ok(p) = Packet::parse(&mut buf, flag & 1 == 1) {
        let mut out = BytesMut::new();
        p.serialize(&mut out);
    }
});
