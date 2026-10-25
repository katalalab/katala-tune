# Native Windows exit race — 0.6.5-specs.3

## Reproduced cause

The intermittent `native_three_entries_use_real_git_and_process_one_frame_without_eof` failure was reproduced using the installed Git for Windows wrapper. Its direct reader returned exit 2 and withheld all output. Isolated core diagnostics then reproduced `LingeringDescendants` during a fixed Git metadata query. The retained root and Git child had finished, while a retained console-host member reported terminal exit code 0 before its wait became signaled. A later diagnostic sample already saw all members signaled, but the earlier rejection was irreversible.

This was a runtime classification race, not an insufficient test timeout. Private pre-fix captures include the original native-role failure, diagnostic iteration 13, member iteration 27 (nonsignaled handle with terminal 0) and cached-name iteration 40 (`conhost.exe`). Temporary image-name instrumentation was removed from the patch. No machine identifiers or raw source captures are stored here.

## Correction and safety boundary

The owned Windows backend queries only already-retained nonsignaled descendants. A terminal code permits another normal observation within the original execution deadline and existing 3-second cleanup window. Code 259 or a failed query cannot grant this hint. A signaled process that legitimately exited with 259 remains exited. There is no executable-name exception, process rerun, arbitrary added sleep or extended deadline.

The hint never sets `observed_exited` or `job_empty` and cannot create an OS witness or expose output. Every retained handle must actually signal, the Job must report empty, both output pipes must reach EOF, the writer must finish, and the existing artifact/ownership/exit-policy checks must pass. Unknown/running children still fail closed. Cleanup expiry still requests termination once and withholds payloads.

The [Git for Windows wrapper documentation](https://gitforwindows.org/git-wrapper.html) describes the wrapper's child launch. Microsoft documents [exit status](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getexitcodeprocess) separately from [process termination and signaled state](https://learn.microsoft.com/en-us/windows/win32/procthread/terminating-a-process). The specific timing above was observed on the test host.

## Verification

- New regression failed before the correction and passed afterwards: a terminal-but-pending descendant does not trigger immediate rejection, cannot expose output, and cannot restart the cleanup deadline. Unconfirmed cleanup still expires and terminates.
- Status decision coverage: terminal 0/1, nonsignaled 259, query error, and signaled 259 with no exit-code query.
- Independent source review approved the boundary: no actionable correctness/security findings.
- Real Git wrapper diagnostic: 60 consecutive successful runs, 300 bounded metadata queries. Ordinary native CLI integration: 20 consecutive successful runs, each checking reader/supervisor/verifier roles with real Git and open stdin. No failure was retried or skipped inside these runs; the repetition driver stops on the first failure.
- `npm run check` passed: JavaScript 437 passed / 13 platform skips; Rust 626 passed / 4 ignored, including doc tests. Core unit tests: 293 passed / 2 ignored. Windows process-boundary integration: all 24 passed, including actual lingering-child rejection, timeout/cancellation cleanup and withheld output. Formatting, workspace Clippy and HEAD OSS scan passed.

## Delivery

The runtime correction was built from commit `24647386a3064b2be28127d04f2b150dd069abfe`. Windows desktop and both existing Mac app bundles were updated to specs.3 with backups. All six authorized machines' standalone CLIs report specs.3; SHA-256 values match their platform artifacts. Both Mac installations retain configuration and login items, pass strict signature checks, and have consistent live/backup databases. Windows live/backup databases also pass `quick_check`.

All three consoles successfully collected and stored six current specs.3 reports. Real Windows Tauri IPC rendered six selected machines and 145 comparison rows. The temporary Windows debug listener was removed after verification. Shared-machine read opt-in remains intact and its shared status was preserved. Private reports, deployment hashes, diagnostic logs and screenshots remain outside Git.

## Separate test-artifact publication race — specs.7

A later full specifications check reproduced reader success followed by supervisor/verifier exit 2 with empty output. This occurrence had a separate cause: a concurrently queued Cargo build published a different `target/debug/tune.exe` while the integration executable was running. Cargo's build-directory lock does not remain held throughout integration execution. The fixture approved one digest before its three roles, then reopened the shared mutable build path. Production artifact pinning correctly rejected the changed executable.

The overlap was established from the integration duration, executable write time and build completion time. A deterministic regression uses a disposable executable copy to model publication between roles: the reader succeeds, different valid PE overlay bytes are published, and approval with the previous hash is rejected with exit 2 and no output. The real Cargo output is never modified by this regression. An isolated diagnostic also completed 40 native-reader captures and 200 Git metadata queries without an owned-process failure.

The integration fixture now creates one private helper copy, approves its hash and path together, and retains a read-only Windows lease denying writes and deletion through every role. Requests and launches use that copy. The lease is released before fixture cleanup. The regression additionally proves that stale approval at the changed build path remains rejected while the fixture's approved helper continues to work. No production process, source, artifact, hash, deadline, output or ownership rule was relaxed. There are no retries or skips. Application builds are run after verification instead of concurrently against the same target directory.
