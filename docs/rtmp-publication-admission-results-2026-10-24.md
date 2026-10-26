# RTMP publication admission, 24 October 2026

Historical: the Hyper viewer backend measured here was removed on
26 October (`0fb75e5`). Its figures describe builds before that change.

C9 enforces the configured number of current RTMP publishers, separately from the connection limit. With `SAPFLOW_MAX_PUBLISHERS = 1`, an executed regression on the preceding source accepted two different publication paths. That exact test now passes with the correction.

The configuration promises `N` simultaneous publishers per protocol, defaulting to 16. RTMP previously bounded connections at `4N` without limiting accepted publication owners to `N`. The correction leaves connection admission in place and gives each current RTMP session generation one permit from a runtime-free core quota.

Same-path takeover moves the existing permit, so it succeeds at full publication capacity. An old connection's delayed retirement cannot return its successor's credit. Retirement releases publication credit even while media remains available during reconnect grace. A detached reconnect must acquire available capacity again; another path may have used the released slot. A refused SRT-to-RTMP takeover leaves the existing publisher, recording and callbacks intact. RTMP-to-SRT takeover returns the RTMP permit.

| Check | Observed result |
| --- | --- |
| Existing code with the added regression only | Cargo exit 101; one intended assertion failure, confirming two accepted owners with `N = 1` |
| Corrected final workspace | Cargo exit zero; 884 passed, five ignored; all 16 new tests executed once and passed |
| Clippy, workspace and all targets with warnings denied | Exit zero |
| Formatting | Exit zero |
| Exact input closure | 304 regular files and five explicit licence aliases unchanged during final checks |

The new cases cover exact and zero quota, overflow without large allocations, concurrent admissions, rollback, full-capacity takeover, stale generations, held retirement, cross-protocol refusal, authentication, real recording-prefix retention and RTMP status output. The server emits `NetStream.Publish.Denied` with the description `publisher limit` and records a distinct typed refusal. The existing native RTMP client retains the generic denied code, so this does not add a typed peer-capacity reason to that client API.

The final validation receipt has SHA256 `d7dfb055957d3fff6d69c14ad7bfb39019790be19dae47439b7341bc31bf5137`. The first corrected workspace run passed, then Clippy rejected four test expressions. Its failed receipt remains retained. A separate revision changed three `unwrap_err` calls to reasoned `expect_err` calls and replaced an immediately invoked test closure with a scoped block. The production prefix stayed byte-identical; the final workspace, Clippy and formatting runs above belong to that revised source.

The changes remain uncommitted in the native SDK worktree. Its Git commit alone does not identify C9. Independent readback verified the final source closure, and the final core file has SHA256 `fc3899500266158e1fd90cf4f996d4a41471acc59db0f8a2022af99de1edcc4a`.

The fresh native ARM64 debug server, SHA256 `b1cc12a50a5633d59de8cde3795c63c7cd410d5f354ed0dbd8cd51640d83634a`, also passed one unchanged end-to-end smoke with one own HTTP worker. Authentication/refusal, RTMP takeover, reconnect recording, SRT encryption, decoded media cadence and shutdown checks passed. The shell and controller exited zero. Its receipt is `72d4f4005682a1186d6f1b6218a464a0c01afd18e6373435d8342f0e00d9edf1`. Independent readback found 81 process identities absent and all 21 fixture ports free. All 21 generated evidence files, totalling 6,549,603 bytes, were retained and rehashed; the original temporary work directory is absent. The bounded port reader retained its initial bind rejections and all 45 observations before availability, without inferring their cause. Sources and pinned inputs remained unchanged. This is the local own-backend CI smoke branch; the separate Hyper branch was not rerun for C9.

This establishes admission correctness only. It contains no new CPU, RSS, throughput, sustained-load or device result. The M4 comparison still uses the separately frozen C8 release. Global HLS residency, cached media and slow-consumer ownership remain separate gates in [the retained-media contract](retained-media-contract.md).
