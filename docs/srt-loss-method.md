# SRT loss collection qualification

`scripts/srt-loss.sh` keeps its generated 720p30 H.264/AAC clip, 30-second
duration, loss/latency settings, five seeds and three concurrent trials by
default. Set `JOBS=1` explicitly for a sequential performance collection.
This script compares receiver delivery under impairment; it does not measure
receiver or sender CPU, allocations, full-ACK capacity or energy.

The generated TS source must contain whole, sync-aligned188-byte packets.
The trial starts `srt-live-transmit -a:no -c:1316` with a UDP input, verifies
that exact child PID owns its input port, then starts the paced FFmpeg
copy/remux feeder with `udp://127.0.0.1:PORT?pkt_size=1316`. This supplies
datagram boundaries rather than relying on stdin/file-input framing. The
source encoder's actual `PIPESTATUS` and the separate sender's actual wait
status are retained. The original latency-plus-four-second drain, sender
stop and one-second delay before stopping the proxy remain.

Each planned trial has a separate five-port block, checked to fit within the
port range. Occupied ports fail the trial without signalling their owners.
UDP ownership uses the child's socket inode on Linux or its PID-filtered
`lsof` result on macOS. These checks do not certify SRT delivery; ordinary
receiver/proxy/decode qualification still applies.

Before creating the clip, it saves every planned trial in `OUT/plan.tsv`.
`results.tsv` and `qualification.json` retain every planned row in that order.
A started trial with a crash, missing receipt or missing exit status fails.
A planned trial that never started is `not_attempted`, which also makes the
collection incomplete and its exit nonzero. The harness does not silently
skip conditions. Existing output evidence is refused rather than overwritten.

For this audio/video fixture, a passing trial requires the source FFmpeg exit
to be zero, positive video and audio packet counts, and both probe exits and
the decoder exit to be zero. A clean run (`--loss none`) also needs zero
lines from `ffmpeg -v error`: there an error message means the clip or the
harness is broken, and it fails even if FFmpeg exits zero. Under loss, error
messages are a result and are reported in the decode messages column. libsrt
writes the bytes it was given, holes included, while sapflow drops what it
cannot use, so failing every trial with an error message hid libsrt's 200 ms
rows entirely; where the two receivers' decode messages differ, their packets
delivered count different things. Receiver/proxy crashes and source
timeouts fail. Requested child termination is recorded separately from a
natural nonzero exit; SIGKILL never qualifies as a clean shutdown.
The original 15 TSV columns keep their positions; encryption, actual tool
exits, attempt state and failure reasons follow them. Failed and encrypted
trials retain private diagnostics under `OUT/logs/`. Keep these logs private
until reviewed for local paths and addresses. The shell exit is nonzero if
any attempted trial fails or any planned trial remains unattempted.

Useful packet counts are relative to each receiver's qualified clean run.
Positive counts and clean decoding do not prove every original sample arrived.
The proxy's `in_time` counter records survival and admission to its delay
queue, before actual target send. Its admission-loss percentage is not the
receiver's true delivery deadline, TSBPD loss or player stall rate. A fixed
seed also does not imply identical packet faults when implementations emit
different packet/control counts. Keep these limits with any comparison.

Run the finite offline regressions with:

```sh
python3 -B tests/test_srt_loss_results.py
```

They execute the shell with fake tools, including decoder errors in a
clean and a loss trial, source/probe/decoder/receiver exits, a killed trial shell, missing
receipts and setup failure. Tiny loopback UDP fixtures verify ownership and
sender-before-feeder order, including an occupied port whose owner survives.
The fixtures contain generated packet bytes, use no SRT connection or codec,
and do not build Rust. Passing them qualifies collection logic, not SRT delivery
or a performance improvement; a new native run must retain its own evidence.
