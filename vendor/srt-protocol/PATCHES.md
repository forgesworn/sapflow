# srt-protocol 0.4.4, patched for sapflow

Upstream: https://github.com/russelltg/srt-rs (crate `srt-protocol` 0.4.4,
Apache-2.0, last release 2024-05). Copied from the published crate; the
licence text is in `LICENSE-APACHE`. Every change is marked
`sapflow patch` in the source.

Why: fuzzing (`fuzz/fuzz_targets/srt_packet.rs`) found panics in packet
parsing. The listener parses every datagram that reaches the SRT port before
any access control, so one crafted datagram crashed the whole server.

| File | Change |
|------|--------|
| `src/packet/control/srt.rs` | `le_bytes_to_string`: refuse an empty extension instead of reading 4 bytes from nothing |
| `src/lib.rs` | allow upstream lint warnings (vendored code) |
| `src/packet/control/srt.rs` | group extension: check its 4 bytes are there |
| `src/packet/control/mod.rs` | a handshake config extension other than the stream id was `unimplemented!()`; now ignored |
| `src/packet/mod.rs` | `Packet::parse` turns any remaining panic into a parse error (needs `panic = "unwind"`) |
| `src/packet/control/srt.rs` | advertise `TLPKTDROP` and `NAKREPORT` in the handshake, which the receiver implements; without them libsrt senders retransmit on their own timer (2.3x the stream at 20% loss, against 1.2x for libsrt's receiver) |
| `src/protocol/receiver/buffer.rs` | re-request a lost packet every RTT instead of backing off (2, 3, 4... RTTs), as libsrt does; ship with the flags above, never the flags alone, or packets arrive too late |
| `src/protocol/receiver/buffer.rs` | ask for a lost packet again every quarter RTT once its delivery deadline is under 3 RTTs away (it was every RTT), at most 8 times, and not once a retransmission could no longer arrive in time; the deadline is that of the packet that revealed the loss |

After the fixes, `srt_packet` (parse then re-encode) ran 6.2 million inputs
without a panic. Not yet fuzzed: the listener and connection state machines
behind the parser.

Drop this copy once upstream ships the fixes.
