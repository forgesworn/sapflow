# Threat model

What sapflow exposes, who can reach it, what could go wrong and what stops
it. Written at M5 (2026-10-20). Reassess when a protocol or a dependency
changes.

## What is exposed

| Surface | Who reaches it | Before authentication |
|---------|----------------|-----------------------|
| SRT, UDP 8890 | Anyone on the internet | Every datagram is parsed before the stream id is checked: by `srt-protocol` (default), or by our own `sapflow-srt` with `SAPFLOW_SRT_IMPL=own` |
| RTMP, TCP 1935 | Anyone on the internet | Handshake, chunk and AMF0 parsing by our own `sapflow_hls::rtmp`, then the publish request |
| HLS, TCP 8888 | Viewers, normally through Caddy | Everything: HLS is public by design (v4v gates access in front) |
| Callback, outbound HTTP | Only to `SAPFLOW_CALLBACK_URL` | n/a |
| Recordings directory | Local disk, shared read-only with v4v | n/a |

Assets: the stage (availability during a show), the publisher credential,
the callback token, the recordings, and the host.

## Attackers

1. **Anyone on the internet** with no credentials: crash or stall the server,
   guess the publisher password, fill the disk or memory.
2. **A viewer**, or many: exhaust connections or memory through HLS.
3. **The publisher** (credentialed, but an encoder can be compromised or
   buggy): send hostile media to crash the muxer, flood bandwidth, publish
   to someone else's stage path.
4. **The network between sapflow and v4v**: read or forge callbacks.

## Threats and mitigations

| # | Threat | Mitigation | Status |
|---|--------|------------|--------|
| T1 | One crafted UDP datagram crashes the server (panic in `srt-protocol` parsing) | Found by fuzzing. Fixed in `vendor/srt-protocol` (see its `PATCHES.md`), `Packet::parse` turns any remaining panic into an error, release builds unwind so a panic ends one task. Regression inputs in `fuzz/regressions/srt_packet`. Our own `sapflow-srt` is built not to panic: no `unwrap`, `expect`, `panic!` or indexing outside tests (denied by lint), every length bounded by the datagram, stream id at most 512 bytes, at most 16 handshake extensions and 512 NAK entries per packet, timestamps and sequence numbers compared with wrapping arithmetic. Packets for a connection are taken only from the address that made it. | Fixed; packet parser, listener and connections after the handshake fuzzed clean (`srt_packet`, `srt_listener`; ours: `own_srt_packet` 207 million inputs and `own_srt_listener` 4.2 million, 20 minutes each with overflow checks, no crash) |
| T2 | Crafted RTMP bytes crash the server or exhaust memory before authentication (fuzzing found two panics in `rml_rtmp`, the library used until 2026-10-20) | RTMP is now our own publish-only code (`sapflow_hls::rtmp`): no indexing without a bound, no unchecked length arithmetic, nothing allocated from a declared length. Before the publish is accepted a connection may hold 1 MiB (unparsed input plus partial messages); after, 40 MiB. At most 64 chunk streams and 16 created streams; chunk size 1 to 16 MiB; AMF0 nesting 16 deep and 4,096 values per message. The handshake and publish request must arrive within 10 s in total. A panic would end only that connection's task (unwind). | Fuzzed clean (`rtmp_session` 4.7 million inputs and `amf0` 45 million, 20 minutes each, no crash); unit-tested per layer and against a recorded ffmpeg session |
| T3 | Hostile media from a publisher crashes or wedges the muxer | TS, PES, ADTS, Annex B, SPS: maintained crates, fed through code that drops bad packets and resyncs. FLV tags, AudioSpecificConfig, avcC and NAL lengths: our own small bounded parsers (h264-reader's avcC iterator panicked under fuzzing, so it was replaced). All fuzzed (`ts_ingest`, `flv_ingest`, `small_parsers`). A panic while muxing ends that publisher only; its path is released on drop. | Fuzzed clean |
| T4 | Publisher password guessing | 10 failures from one address in 60 s blocks that address for the rest of the window; constant-time comparison. | Done. Weak against many addresses: use a long random password |
| T5 | Taking over a live stage path | A second publisher on a live path is refused; only the configured publisher may publish at all; paths must match `live/event-[a-z0-9-]+`. | Done |
| T6 | Publisher floods bandwidth or memory | Bitrate cap per publisher (default 40 Mbit/s over 5 s); TS input buffer capped at 8 MiB; segment capped at 64 MiB (a stream with no keyframes is dropped); 10 s idle timeout. With `sapflow-srt`: a receive buffer of 8,192 packets (about 12 MB) per connection, filled no further (a packet beyond it delivers the oldest early; a jump past the whole buffer starts afresh rather than reporting everything between as lost); no packet held longer than its latency plus 1 s, whatever its timestamp says; at most 1,024 loss ranges tracked (the list is scanned per packet) and a drop request costs one pass over it; 16,384 payloads queued for the muxer, or the publisher is dropped; 5 s without a packet closes the connection. | Done |
| T7 | Many publishers or idle RTMP sockets | `SAPFLOW_MAX_PUBLISHERS` per protocol (default 16); RTMP connections capped at 4x that, and get 10 s to authenticate. A dropped publisher's stream is held for `SAPFLOW_RECONNECT_GRACE` (default 90 s) with its playlist window (24 segments, a few MB at stage bitrates); only the credentialed publisher can create streams, so held streams are bounded by what it publishes within one grace period. | Done |
| T8 | Slow-loris or connection floods on HLS | At most 20,000 HTTP connections; 60 s to send request headers (hyper applies the same limit to idle keep-alive connections, and a shorter one broke clients' connection reuse); 64 KiB request buffer. Blocking playlist and part requests are held at most 3x the target duration. Every viewer shares the same rendered bytes, so memory does not grow per viewer. The open-file soft limit is raised to the hard limit at startup, with a warning if it stays low. | Done; measured at M4 |
| T9 | Path traversal through chunk names or stream paths | Chunk names must be `<prefix>_init.mp4`, `_seg<N>.mp4` or `_part<N>.mp4`, looked up in memory, never on disk. Stream paths are validated before use; recording paths are built only from validated paths and digits. Expiry deletes only `<digits>.mp4` files under `live/*/`. | Done, unit-tested |
| T10 | Recordings fill the disk | Recording does not start below `SAPFLOW_RECORD_MIN_FREE_GB` free (default 2) and stops, finishing its file, if free space falls below it mid-show; expiry after `SAPFLOW_RECORD_DELETE_AFTER` (default 7d); a recording that falls 50 s behind is abandoned rather than buffered in memory. A full disk stops recording, not streaming. | Done |
| T11 | Callback token exposed or callbacks forged | Plain HTTP by design: the callback must stay on the private network (as with mediamtx). The token is not logged. v4v refuses callbacks whose path does not match the prepared session. | Accepted risk; keep the callback off the public internet |
| T12 | Viewer privacy | No sessions, cookies or query strings in chunk URLs; viewer IPs are not logged. | Done |
| T13 | Log flooding with bad datagrams or refused publishes | `srt_protocol` and `srt_tokio` log errors only; `sapflow-srt` logs nothing (it counts what it drops); refused publishes are logged at most 20 a minute, with a count of those suppressed. | Done |
| T14 | Unmaintained dependencies (`srt-tokio`/`srt-protocol` 2024-05) | Pinned versions; `srt-protocol` vendored with our fixes; fuzz targets exercise it. Our replacement, `sapflow-srt` (no dependencies), serves SRT with `SAPFLOW_SRT_IMPL=own` and becomes the default at M6c, when `srt-tokio` and the vendored copy go. RTMP no longer depends on `rml_rtmp` (2023-04): it is our own code. | Ongoing until M6c: one patched copy, the default |
| T16 | Spoofed-source SRT handshakes (the SYN cookie is meant to prove the client receives at the address it claims) | `srt-protocol` derives the cookie from the peer address and the current second with a fixed-key hash, so it can be computed by anyone; a spoofed handshake can get as far as access control. The stream id check and credentials still apply, and a spoofed source never receives the replies. `sapflow-srt` keys the cookie per process (the standard library's randomly keyed hash, `RandomState`, SipHash-1-3 at present; its algorithm is not guaranteed) over the peer address and port and the minute; it is valid for that minute and the next. A conclusion with a wrong cookie gets no answer, and nothing is allocated before a valid one. Its socket ids come from the same key, so they are not guessable either. At most 100 handshake packets a second are answered (bursts of 200) and at most twice `SAPFLOW_MAX_PUBLISHERS` connections, open or awaiting the policy decision, are held. | Fixed in `sapflow-srt`; accepted for `srt-protocol` until M6c |
| T15 | Supply chain | Few dependencies, all permissive licences (`THIRD_PARTY_LICENSES.txt`), `Cargo.lock` committed, `unsafe` forbidden in our crates. | Done |

## Out of scope

- TLS: HLS is served behind Caddy, which terminates TLS. SRT control
  packets are never encrypted, so the stream id carries the publisher
  credential in clear, as mediamtx's form does, with or without a
  passphrase. `SAPFLOW_SRT_PASSPHRASE` (our own receiver only) encrypts the
  media (AES-CTR, which does not authenticate it); every caller must then
  use it, and a wrong one is refused and counted as a failed login (T4).
  Its salt and wrapped key travel in clear, so a weak passphrase can be
  guessed offline: use a long random one. Hiding the credential itself
  needs a VPN.
- Denial of service by volume (raw bandwidth floods): the network's job.

## Known gaps, next steps

- The fixes stay in our vendored copy: they are not being reported
  upstream, so the copy is ours to keep current.
- Run vMix against sapflow (OBS and Larix have published over both
  protocols; `docs/interop.md`).
- Fuzz srt-tokio's own glue (socket tasks, timers) as well as srt-protocol.
- Run the fuzz targets for hours, ideally in CI (the longest run so far is 20
  minutes per target).
- SRT encryption hardening from the M6b review (2026-10-21), all low:
  rate-limit only failed in-session key messages, so a forged one (which
  needs a spoofed address and the socket id) cannot delay a key refresh;
  reject retransmissions older than the previous key switch once a key
  slot is reused; check whether libsrt expects a reply to a key message
  that only retires the old key.
