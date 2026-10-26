# iPhone short functional check, 24 October 2026

The saved local recording passed independent decoding. SRT delivery did not
qualify: the origin received no publication and a 45-second watcher never
observed an HLS master playlist. Larix was not run. This is a separate attempt
from the earlier controls that stopped before Start.

The connected iPhone used the existing C3 benchmark app over local Wi-Fi to
an M1-hosted C8 origin. Its selected configuration was rear camera, 720p30,
Landscape (top right), encrypted SRT and local recording. The review showed
the fresh peer and AES-256 security policy before Apply. Capture preparation,
Start and Stop were observed separately through the bounded UI helper.

| Observation | Result |
| --- | --- |
| App's final snapshot | Stopped; recording finalised; destination end unconfirmed |
| App counters | 10,349 aggregate encoded samples; zero reported capture drops |
| Copied recording | 7,371,329 bytes, matching its phone receipt |
| Stored duration | 135.013334 seconds |
| Video | H.264, 1280 by 720, nominal 30 fps; 4,049 decoded frames |
| Audio | AAC, 48 kHz, mono; 6,326 decoded AAC frames in the probe |
| Decode preserving source time base | Exit zero, zero error output, complete progress marker |
| Origin | No publication or server recording; HLS readiness absent in all 90 watcher polls |

The first default null-output decode exited zero but emitted one timestamp
error, so its zero-error gate failed and remains retained. Independent frame
inspection then found strictly increasing source presentation timestamps at
1/600 second resolution. One pair rounded onto the same 30 Hz output tick.
A separate follow-up used passthrough frame timing and the source encoder time
base; it decoded both stored tracks with zero error output. This verifies the
stored file, not complete capture-source delivery, decoded PCM equality or
physical A/V synchronisation.

The Stop snapshot and the recording's final modification preceded a separate
180-second guard intervention. The guard closed the task's benchmark app
because stop confirmation had not reached it before its deadline. That
intervention is retained. Later readback found the benchmark app and helper
absent. The origin stopped normally, its group was absent and its three ports
were free.

The initial publisher snapshot was waiting for immutable codec configuration;
the final snapshot replaced that status with the stop request. Neither proves
the cause of the missing SRT publication. There was no active-period transport
trace, so no configuration, routing, firewall or runtime cause is assigned.

This check measures no handset CPU, RSS, energy, thermal endurance or relative
performance. The installed app predates the current shared-library changes.
A matched broadcaster comparison still requires proven delivery and retained
transport observations. Camera media, payload hashes, device identifiers,
destinations, credentials, UI trees and raw receipts remain in private lab
storage and must not be staged or published.
