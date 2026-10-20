//! Our SRT packet codec (`sapflow_srt::packet`): every datagram to the SRT
//! port is parsed by it. Whatever parses must encode, parse again to the
//! same packet, and encode to the same bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_srt::packet::Packet;

fuzz_target!(|data: &[u8]| {
    let Ok(p) = Packet::parse(data) else {
        return;
    };
    let mut once = Vec::new();
    p.encode(&mut once);
    let again = Packet::parse(&once).expect("an encoded packet parses");
    assert_eq!(again, p, "parse, encode, parse changes nothing");
    let mut twice = Vec::new();
    again.encode(&mut twice);
    assert_eq!(once, twice, "encoding is stable");
});
