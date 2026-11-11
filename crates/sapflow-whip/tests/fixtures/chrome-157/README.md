# Chrome 157, gate 2 record

One headless Chrome 157 run against `examples/whip_harness.rs` on
loopback (W3a gate 2), recorded with `--record` and replayed by
`chrome_157_handshake_and_srtp_replay` in `src/tests.rs`.

| File | What |
|---|---|
| `offer.sdp` | Chrome's offer. Candidates removed, documentation addresses on the `c=`, `o=` and `a=rtcp:` lines, and the discard port 9 on the `m=` lines |
| `dtls.txt` | Every DTLS datagram in order: `C` from Chrome, `S` from us, `T` a timer resend |
| `leaf.der.hex` | Chrome's DTLS leaf certificate (self-signed, generated per browser profile) |
| `media.txt` | Chrome's first 8 SRTP and 2 SRTCP datagrams, from its built-in fake camera and microphone (generated, not a camera) |
| `summary.txt` | The milestones the harness logged |

No address or port is kept. The `m=` ports were Chrome's ephemeral
loopback ports and appeared nowhere else: the record keeps no STUN
response (so no XOR-MAPPED-ADDRESS) and no tuple, and DTLS, SRTP and
SRTCP carry none. The replay test validates a documentation-range tuple
with its own STUN client and feeds everything from it.
