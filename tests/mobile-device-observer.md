# Android performance observation

`mobile-device-observer.py` collects bounded, read-only counters during a broadcast
that the operator has already arranged. It does not launch, stop, install or
configure an app. It never accesses media, logcat, recordings or app storage.
The full observer has had one short physical smoke run; lightweight-mode device
validation is pending. Neither run is matched performance-comparison evidence.

```sh
python3 -B tests/mobile-device-observer.py \
  --adb adb --serial DEVICE_SERIAL --package com.example.publisher \
  --duration 120 --interval 5 --timeout 3 --output trial.local.jsonl

python3 -B -m unittest discover -s tests -p test_mobile_device_observer.py -v
```

For lighter CPU-tick observation, omit the expensive per-sample diagnostics:

```sh
python3 -B tests/mobile-device-observer.py \
  --adb adb --serial DEVICE_SERIAL --package com.example.publisher \
  --duration 120 --interval 5 --timeout 2 \
  --skip-memory --skip-device-state --output ticks.local.jsonl
```

The flags are independent: `--skip-memory` omits `meminfo`, while
`--skip-device-state` omits battery and thermal queries. With both flags, each
sample issues only `pidof PACKAGE` and one `/proc/PID/stat` read. The second
stat read normally used to bracket memory collection is unnecessary here.
Startup metadata still issues bounded package-version, clock-frequency and build
fingerprint queries. Both options are recorded in metadata; omitted metric
fields remain present with `value: null` and `unavailable_reason:
"skipped_by_option"`. Skipping thermal data means the observer cannot assess
thermal starting state or throttling; arrange that check separately for trials.

Select the connected device explicitly. Use separate local output files for each
trial; an existing file is never overwritten. The observer does not retain the
serial, raw subprocess output, process names, unrelated package information or
thermal sensor names. Package identity, Android build fingerprint and diagnostic
counters may still identify a trial, so review before sharing.

The JSONL file contains a `metadata` row, `sample` rows and an `end` row. Metadata
records UTC start time, duration/interval/timeout, selected package version code
and name, build fingerprint, clock tick frequency and the comparison caveats.
A fingerprint and version are not an APK content hash. Record candidate hashes
and the broadcast configuration separately in the trial evidence.

Each metric has `value` and `unavailable_reason`. Missing, denied, unsupported,
malformed and ambiguous results produce `null`; valid measured zero remains zero.
`command_errors` records fixed error codes without subprocess diagnostics.
Direct command failures also propagate into the corresponding metric's
`unavailable_reason`: for example, a timed-out memory command marks both PSS and
RSS `command_timeout`, rather than suggesting a missing process or malformed
response. A failed clock-frequency query retains its reason for CPU percentage;
raw CPU ticks can still be available.

- `pidof PACKAGE` selects exactly one main process. No general process listing is
  collected. Missing or multiple PIDs make app counters unavailable.
- `/proc/PID/stat` provides cumulative user+system CPU ticks and process start
  ticks. `getconf CLK_TCK` supplies their unit. CPU percentage uses consecutive
  readings with the same PID/start ticks and host monotonic elapsed time; 100%
  means one core, so multithreaded use can exceed 100%. The first reading and a
  restart require a new baseline. No guessed Android clock frequency is used.
- `dumpsys meminfo -s PID` provides labelled `TOTAL PSS` and `TOTAL RSS` in KiB.
  Both PID and package in the response must match. Legacy tables are not guessed.
  Memory can remain available when procfs denies CPU access; a detected restart
  during the memory read invalidates the memory result. Sequential reads cannot
  eliminate every process-reuse race and are not atomic snapshots.
- `dumpsys battery` supplies charging-source flags, level/scale, voltage in mV,
  temperature in degrees C, and Android numeric status/health. Missing source
  flags remain unavailable. `plugged` is true if a reported source is powered;
  false requires all three standard AC/USB/wireless flags explicitly false and
  no reported powered dock. Battery percentage is calculated only for valid
  level/scale. No application energy calculation is attempted.
- `dumpsys thermalservice` supplies the Android numeric thermal status (0 to 6)
  and status-override flag when recognised. Sensor lists are discarded. An
  overridden status must not be treated as natural thermal behaviour.

Limits: duration 1 to 86,400 seconds, interval 1 to 3,600 seconds, subprocess
timeout 0.1 to 10 seconds, captured stdout at most 512 KiB per command. The
observation deadline includes identity collection. Each command is capped by
both its timeout and remaining observation time, then killed/reaped if needed.
No raw output is saved; rows stream to disk. Slow samples skip missed intervals
without catch-up bursts. `collection_duration_s` exposes sampling cost. Ctrl-C
terminates the active child and records an interrupted end row during sampling;
interruption during initial metadata collection can leave an empty file.

For a matched Sapflow/Larix trial, independently fix device, OS, codec,
resolution, frame rate, bitrate, protocol, network, recording state, duration,
power connection, starting battery and thermal state. Repeat and alternate run
order. ADB and especially `meminfo` introduce overhead; use the same observer
settings for both apps. The selected process excludes system encoders and
secondary services. Plugged-in battery readings are not energy measurements.
These counters alone establish neither a Larix win nor broadcast quality,
latency, recording reliability or sustained thermal acceptance.

In the initial 35-second physical smoke run, four samples were collected after
the broadcast had already developed A/V divergence. CPU tick frequency was 100
Hz, with three delta readings of approximately 268.6%, 264.3% and 265.3%
one-core CPU use. Thermal status was 1 without an override. Every memory query
hit its two-second timeout, leaving memory unavailable and costing about 2.7
seconds per sample overall. This intrusive observation is not a baseline or a
matched comparison. The skip options address that measurement overhead; their
actual device cost and CPU continuity still require a separate physical check.

Primary command/field references:
[Android dumpsys documentation](https://developer.android.com/tools/dumpsys) and
[Linux proc_pid_stat field definitions](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html).
