# Synthetic origin codec-epoch integration gate

`origin-epoch-integration.py` is an opt-in, dependency-free Python 3.9+ harness
for a separately reserved synthetic event on an already running Sapflow origin.
It starts no server, changes no configuration, and never accesses a phone. It
publishes our generated H.264/AAC fixtures as 12 seconds of portrait, 12 seconds
of landscape, then 12 seconds of portrait. Each stage uses a fresh ffmpeg
process, RTMP connection and source clock. `--cycles=2` or `3` repeats this
sequence; adjacent portrait stages share one codec epoch as intended.
`--stage-seconds=12..30` changes every stage (default 12), and
`--reconnect-gap-seconds=0..3` pauses between completed stages (default zero),
including between cycles. There is no pause before the first stage or after
the final stage. For a longer player-observation window with phone-like gaps,
use `--stage-seconds 20 --cycles 2 --reconnect-gap-seconds 2`.

No network qualification is implied by the offline tests. Run it only after the
origin owner reserves a fresh `event-epoch-...` event and supplies the arguments.
It refuses an existing HTTP event or recording directory. That preflight does
not replace an exclusive event reservation against concurrent publishers.

## Invocation

Use a trusted local machine with `ffmpeg` and `ffprobe` in `PATH`, ordinary TLS
trust configured if needed, and read-only access to the origin's recording
root. Supply the actual binary and source manifest corresponding to that
origin. The hash checks verify local artefacts, not the identity of an already
running remote process; retain the owner's separate running-process identity
receipt. The frozen source manifest must include both `epoch-*.ts` fixtures and
must match this checkout. The harness itself has a separate hash in its receipt.

Populate credential environment variables without printing them. Only their
names appear in the command. The following values are placeholders:

```sh
python3 -B tests/origin-epoch-integration.py \
  --http-base 'https://origin.example.invalid' \
  --rtmp-base 'rtmps://origin.example.invalid/live' \
  --event 'event-epoch-reserved-example' \
  --publish-user-env SAPFLOW_EPOCH_USER \
  --publish-pass-env SAPFLOW_EPOCH_PASSWORD \
  --recording-root '/mounted-read-only-origin-recordings' \
  --origin-binary '/verified-origin/sapflow' \
  --expected-origin-sha256 '<64 lowercase hex digits supplied by owner>' \
  --source-manifest 'target/origin-codec-epoch-source-hashes.json' \
  --expected-manifest-sha256 '<64 lowercase hex digits supplied by owner>' \
  --output 'target/origin-epoch-reserved-example'
```

The HTTP base has no `/live` suffix; the RTMP base includes its application
path. Bases must have no embedded credentials, query or fragment. Publishing
credentials enter the child ffmpeg URL and are therefore visible to the local
process owner while it runs; receipts redact raw/encoded credentials and echoed
destination URLs. HTTP reads use normal certificate/hostname validation, refuse
redirects, and request the origin's identity-encoded playlists. A proxy that
forces gzip is outside this narrow fixture contract.

The output parent must exist and the output directory must not exist. It must
be outside the read-only recording root. The harness reads only
`<recording-root>/live/<reserved-event>/*.mp4`; it never truncates, renames or
removes origin files. Allow finalisation after the last disconnect: the default
recording wait is 100 seconds for a 90-second grace. An explicit
`--recording-wait-seconds` accepts 1 through 120 seconds.
Before creating output or making network requests, the harness rejects a plan
whose `3 × cycles × stage-seconds + (3 × cycles − 1) × gap-seconds +
recording-wait-seconds + 60` exceeds 300 seconds. The fixed 60 seconds reserves
time for probes, independent decode and cleanup; it is not an additional wait.
The default plan is 196 seconds; two cycles of 20-second stages with two-second gaps and
the default recording wait plan 290 seconds. Nine 30-second stages are refused
even with the shortest recording wait. The admission estimate does not replace
the absolute deadline if actual publishing, reads or decoding run slowly.

## Assertions and evidence

Each playlist segment is associated with its applicable MAP, including mixed
old/new epoch windows. MAP hints do not bind old segments to a new codec. The
monitor rejects decreasing media sequence, changed URI/MAP/discontinuity for a
published sequence or part, changed targets, missing epoch discontinuities,
external/traversal URIs, excessive responses and malformed timing. When a new
completed codec epoch appears, it re-fetches the previous epoch's newest known
segment and init and compares their bytes. This checks retention at the
transition; it does not claim an arbitrary retention duration or test blocking
reload requests.

For every observed epoch, ffprobe checks exact portrait 720x1280 or landscape
1280x720 H.264 plus 48 kHz AAC. ffmpeg independently decodes both tracks from
an applicable init plus a segment. Finalised, separately copied origin
recordings must decode both tracks at the same dimensions and start within
50 ms of zero, preserving the fixture's small A/V offset. The harness does not
infer upright physical orientation from synthetic dimensions.

`receipt.local.json` records publisher exits, fixture/source/manifest/binary
hashes, epoch/retention checks, decode results and cleanup. The last playlist
per stage is retained. Timing configuration and the planned budget are recorded,
and each publisher records timezone-aware UTC start/completion timestamps plus
its monotonic elapsed duration and process limit. Use UTC to correlate player
freezes with reconnects; wall-clock adjustments do not drive process deadlines.
Media copies are removed, including on failure. A pass
is origin integration evidence only: it does not establish native Safari
recovery, Apple authoring compliance, a mobile-device result, or owner
acceptance. In particular the harness does not resolve the separate static
master `RESOLUTION` policy for dynamic aspect ratios.

## Bounds and cleanup

The main-thread POSIX alarm bounds network/process work to 300 seconds,
including a peer trickling HTTP bytes. Each publisher has a stage-length plus
eight-second deadline (20 seconds by default), capped by global time remaining;
each probe/decode has 15 seconds within the total deadline. Reads have a
two-second socket inactivity timeout. Child stdout/stderr each retain at most
64 KiB; overflow kills only that owned process group. Cleanup requests TERM,
then KILL if needed, and waits for the owned output readers. It never signals
an origin or unrelated publisher. Uninterruptible kernel/filesystem work is
outside a user-space hard-stop guarantee; failed cleanup is reported.

Playlists are at most 128 KiB/4096 lines, resources 4 MiB, all successful HTTP
reads together 64 MiB, parsed segment/part entries 512 per playlist, historical
segments 512, historical parts 2048, epochs 16 and cached resource URIs 512.
One final recording copy is at most 64 MiB and is decoded then deleted before
the next. Only the harness-created output scratch directory is removed. No
long-running process is deliberately retained.

## Offline checks and fuzz entry

```sh
python3 -B -I -m unittest discover -s tests -p test_origin_epoch_integration.py -v
python3 -B -I tests/origin-epoch-playlist-fuzz.py < bounded-playlist-input
```

The single-input fuzz entry accepts at most 128 KiB plus one byte and exercises
parsing, repeated history admission and epoch tracking. Feed bounded regular
files; stdin waiting for an unfinished pipe is outside its input contract.
Unexpected exceptions are findings. Unit tests include deterministic malformed
inputs, 500 seeded byte mutations, MAP transitions, retained-resource mutation,
URI/number bounds, create-new copies, subprocess argv and credential redaction.
Timing tests cover integer/range validation, all nine stages/eight gaps, the
exact 300-second admission boundary, process/global deadline precedence,
gap cancellation at the deadline and UTC/timing receipts using fake publishers
and HTTP responses. They do not sleep for stage/gap durations.
They invoke no network, server, ffmpeg publisher, build or device action.
