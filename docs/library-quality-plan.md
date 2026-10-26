# Reusable media libraries and OBS integration

The owner explicitly asked on 21 October 2026 to preserve the OBS discussion
and the ambition to make our libraries world class. This is a continuing
library goal alongside the [mobile broadcaster plan](mobile-streaming-plan.md).
Shipping a phone app is one consumer of that work. The libraries must also be
useful, documented and measurable outside our own applications.

An earlier retained user prompt, dated 8 September 2026, asks whether V4V uses
RelaySwarm, whether an OBS Studio user could use our libraries, and what the
pros and cons would be. The associated reply and full discussion were not
located. The prompt and today's library ambition are confirmed; the workstreams
and ordering below are proposed implementation work. This record is not a
complete reconstruction or approval for a particular OBS plugin or desktop app.

## What OBS contributes to the plan

OBS is a useful production reference for capture, mixing, encoding, recording,
streaming and extension points. Its [backend documentation](https://docs.obsproject.com/backend-design)
separates sources, outputs, encoders and services, with a reusable `libobs`
under the application. Its [plugin documentation](https://docs.obsproject.com/plugins)
describes these extension points. Its
[output API](https://docs.obsproject.com/reference-outputs) includes encoded
packet delivery, stop/error reporting and reconnect signals.

Our proposed response is to make the existing ForgeSworn media components
credible building blocks for several frontends. Evaluate these separately:

| OBS relationship | Proposed purpose | Evidence required |
| --- | --- | --- |
| Protocol interoperability | OBS publishes to Sapflow using ordinary RTMP and encrypted SRT | Versioned black-box runs, both tracks independently decoded, reconnect and normal Stop checked |
| Optional output or destination adapter | OBS uses a specific capability of our libraries or the V4V creator flow | A named benefit over the existing output, narrow boundary, supported OBS versions, correct lifecycle and separately reviewed distribution |
| Additional application consumer | Reuse the libraries in a later desktop production workflow | A concrete creator need, scoped product decision and independent consumer proof |

The first row extends the existing origin work. The other rows remain open
proposals. Scenes, transitions, compositing, multitrack production and a desktop
studio need their own scope and performance evidence. Keep the phone's release
milestones explicit while retaining these ideas for later work.

## What an OBS user could gain

The lowest-friction route is ordinary OBS publishing into Sapflow. That can
use its existing SRT/RTMP interoperability without embedding our SDK in OBS.
An integrated V4V creator destination still needs the actual authenticated
preparation, refresh and End workflow; protocol ingest alone does not deliver
that integration. Current source and device gates are tracked in the
[delivery status](mobile-delivery-status.md).

| Proposed benefit | Tradeoff or evidence needed |
| --- | --- |
| A compatible origin for OBS, mobile publishers and viewers | Operating and securing an origin remains a responsibility; demonstrate the named OBS version and stream profile |
| V4V event preparation and visible destination health from an OBS workflow | Needs a scoped connector, real creator authentication and truthful media observation; no wallet or settlement authority moves into OBS |
| Reusing our transport/session engine in a custom OBS output | Extra plugin/API/distribution maintenance must earn a measured capability or efficiency benefit over ordinary OBS outputs |
| Later RelaySwarm audience delivery from the origin | Must qualify the signed-media pipeline and viewer fallback; audience delivery does not automatically change the OBS publishing protocol |
| Later recording transfer or repair through Link/Shelter | Adds dependencies and background work; prove bounded resources, explicit access and no interference with a show |

For an OBS user whose current publishing already meets their needs, an extra
plugin may add little value. A shared implementation can improve consistency,
maintenance and diagnostics, but speed, reduced bandwidth and lower operating
cost are hypotheses until the relevant complete-path measurements exist.

Answer the V4V/RelaySwarm deployment question from the current V4V and RelaySwarm
checkout and runtime evidence when that work starts. The agreed mobile order is
reliable origin playback first, then qualified RelaySwarm. A roadmap or browser
integration elsewhere does not establish native peer playback acceptance.

The OBS repository [identifies its licence as GPL v2 or later](https://github.com/obsproject/obs-studio/blob/master/README.rst).
Our chosen library licence remains MIT. This plan adds no OBS code or `libobs`
dependency to the MIT core. Record a separate dependency and licence assessment
for any actual OBS adapter before selecting its packaging. Preserve Sapflow's
existing clean-room SRT rule: protocol specifications and black-box behaviour,
without inspecting libsrt or srt-rs source.

## Library responsibilities

Use the existing [ForgeSworn adoption map](native-performance-contract.md#forgesworn-library-adoption-map)
as the starting inventory. Verify each component at its actual source version
before making a readiness claim.

| Component | Intended reusable responsibility | Qualification still needed |
| --- | --- | --- |
| `sapflow-core` and `sapflow-client` | Media/session types, compressed sample validation, bounded queues, clocks and cancellation policy | Documented invariants, caller ownership and failure semantics; deterministic tests and external consumer examples |
| `sapflow-srt` | Our encrypted SRT receiver and caller/sender protocol machinery | Sustained sender/receiver interoperability, loss/reordering, pacing, rekeying, shutdown and resource bounds on each advertised profile |
| `sapflow-hls` | Media packaging and compatible origin output (RTMP moved to `sapflow-rtmp` on 2026-10-22) | Independent publishers/decoders/players, format changes, timestamp boundaries and bounded assembly costs |
| `sapflow-ffi` and native packages | Typed Swift/Kotlin access to the reusable media/session library | Real packaged consumers, ownership/cancellation guarantees, API compatibility, symbols, licences and measured binding overhead |
| Native platform adapters | Hardware capture/codecs, preview, audio routing and application lifecycle | Physical device support matrix and behaviour under orientation, interruption, low storage, heat and network loss |
| V4V connector and Signet adapter | Scoped creator authentication and destination lifecycle | Real signer consent, durable operations, refresh/End/expiry and current-attempt decoded-media confirmation; no V4V account requirement in generic publishing |
| RelaySwarm and RelaySwarmKit | Verified peer delivery with origin fallback | Existing signed-media contracts, native/browser compatibility and measured deadline/fallback behaviour after origin playback is reliable |
| Link, Shelter and suitable related libraries | Later recording transfer, streaming storage and repair | Explicit adoption record, authority boundary, streaming memory bounds and proof that transfer cannot starve broadcasting |

An application convenience does not automatically belong in the shared library.
Keep capture surfaces and raw frames in native platform adapters; keep billing,
creator identity, payment authority and product UI outside the protocol core.
Extract shared code when its contract fits actual consumers.

## What qualifies a library for release

Each advertised library needs evidence in all these areas. Passing unit tests
or having a Rust implementation alone does not establish this quality bar.

| Area | Required release evidence |
| --- | --- |
| Correctness and interoperability | Published supported protocols/profiles, deterministic edge-case tests, independent implementations and decoded media checks |
| Reliability | Repeated outage/reconnect, Stop/End races, expiry, stale callbacks, bounded failure cleanup and recording survival; no false remote-Live state |
| Efficiency | Copy/allocation and thread ownership map; bounded byte/time queues; CPU, memory, latency, wake-ups and energy measurements at matched media quality |
| Sustained behaviour | Physical show-length runs on the supported matrix, including thermal and storage conditions; finite desktop fixtures remain separately labelled |
| API quality | Explicit ownership, threading, monotonic clock, cancellation, errors and unsupported-capability contracts; versioning and migration policy before promising stability |
| Consumer experience | A small working example outside the producer workspace, reproducible install/build steps and documentation of supported platforms and known limits |
| Security and maintenance | Fuzzing for exposed parsers, malformed-input/resource tests, reviewed dependencies/notices, regression coverage and a documented issue/reporting route |
| Reproducibility | Exact source and package hashes, compiler/profile/device/network settings, commands, scalar receipts and cleanup results |

The existing [native performance budgets](native-performance-contract.md#initial-native-performance-budgets)
remain the starting contract: accounted compressed-sample ownership, single
encode for publishing and recording, bounded scheduling and measured hardware
use. Freeze device-specific baselines before using them as release gates.
Retain the existing matched Larix/Moblin/IRLPro comparison method. For OBS,
compare a common desktop workload on the same machine and encoder settings;
report the workload and feature coverage with each result.

All superiority claims require measurements. Record confidence/spread and
tradeoffs rather than reducing resolution, security, recording or other enabled
features to obtain a favourable comparison. Public evidence uses generated
media and sanitised scalar data. Real camera footage, phone recordings and
extracted frames stay out of commits and publication.

## Work queue

All items start open. Source implementation, local tests, packaged consumers,
physical acceptance and published releases remain separate gates.

| ID | Work item | Concrete completion evidence | Order |
| --- | --- | --- | --- |
| LQ1 | Inventory the library surface | Exact versions, licences, maintained APIs, consumers, missing contracts and adoption decisions for each component above | Start with existing SDK and adoption map |
| LQ2 | Publish an SDK consumer contract | Ownership/thread/cancellation/error documentation, compiling Swift/Kotlin examples and a compatibility policy | Alongside current mobile integration |
| LQ3 | Make interoperability repeatable | Generated OBS/FFmpeg protocol profiles, encrypted own SRT, RTMP/RTMPS, independent decode and retained safe receipts | Extend existing origin/sender gates |
| LQ4 | Establish the performance scorecard | Frozen baseline workloads, complete-path attribution and automated regression comparison, plus physical sustained receipts | Required before performance superiority claims |
| LQ5 | Qualify packages as products | Fresh external package installs, supported platform/toolchain matrix, notices, symbols, tagged versions and migration guidance | Before public library release |
| LQ6 | Decide the OBS adapter | Written choice of standard interoperability, service integration or custom output; named benefit, API shape, lifecycle and distribution assessment | After LQ1/LQ3 expose a justified seam |
| LQ7 | Prove reuse beyond the phone UI | A second independently built real consumer using the public library interface, with no copied session/transport engine | After that consumer is selected |
| LQ8 | Review further production tools | Scoped decisions for scenes, mixing, transitions, remote production and desktop UI, with costs and acceptance criteria | After creator need and shared-library readiness justify them |

The immediate delivery priority stays reliable iOS/Android broadcasting,
recording, current-attempt origin observation and a complete V4V show. Track
library gaps discovered there against LQ1 to LQ5 instead of hiding them in app
workarounds. Preserve LQ6 to LQ8 as visible follow-on work.

The first [encoded publishing admission benchmark](sdk-publishing-benchmark.md)
measures one synchronous SDK boundary on generated host AAC. Its paired Release
measurements and queue correctness controls contribute to LQ4; they do not close
the complete-path, allocation, physical power or competitor comparison gates.

## Decisions to recover or settle

- Append any additional earlier OBS requests if their discussion is recovered;
  the retained benefits/tradeoffs question is recorded above.
- Choose whether the first OBS deliverable is interoperability tooling,
  V4V service integration or a transport output adapter.
- Decide whether a later desktop studio is a desired product, with its own
  user workflow and release scope.
- Identify which libraries need public standalone releases first, and their
  supported platforms and initial API stability promises.

These open questions preserve the wider ambition without converting proposals
into completed work or blocking the currently authorised mobile work.

## Reproduce the current-source Android package and app gate

With full JDK 21, Kotlin 2.2.20, Node 18+, the repository's pinned Rust toolchain,
Android platform 36, Build Tools 35.0.0/36.1.0 and NDK 27.1.12297006 selected in
`JAVA_HOME`, `KOTLIN_HOME`, `ANDROID_HOME` and `ANDROID_NDK_HOME`, run:

```sh
python3 -B mobile/android/check-sdk-app.py
```

Use a clean checkout without ignored `local.properties` or global Gradle
properties/init scripts; the command reuses the existing Gradle cache and pins
the selected JDK explicitly. Extra Gradle project environment properties are
refused rather than silently changing the build profile.

The same command runs on standard Ubuntu 24.04 in CI. It builds a fresh ARM64
AAR from current Rust sources in the checkout's existing `target/`, runs its
installed Kotlin host consumer and supplies that exact package to both JVM
variants and all three app builds. The gate requires the full JVM suites, all
84 named AAC profile/generation/run/transfer controls in each variant, clean lint, qualification
and release isolation, exact packaged JNI identity and 16 KiB ELF/ZIP alignment.
First-run Gradle dependency acquisition is online; all tests execute freshly.

The same command also compiles the 13 recording-owner host sources and executes
all 20 controls against that fresh host SDK library, the exact AAR classes and
the already resolved JVM dependencies. These use synthetic compressed bytes and
supplied native-event facts with real host filesystem operations. No private
witness, additional SDK build or camera input is required. Eleven owned command
stages must close; generated control binaries and synthetic data are removed
with the existing scratch after process closure is known.

Only redacted scalar receipts, command/group observations, JUnit XML and lint XML
are retained under `target/android-sdk-app/` and uploaded by CI. Generated SDKs
and development APKs remain ignored build output and are not store releases.
The package is ARM64-only; other SDK ABIs retain their own qualification gates.
This checks the library/app build boundary, not an Android encoder component,
H/T/EOF or waveform authority, device behaviour, recorder adoption or performance.
