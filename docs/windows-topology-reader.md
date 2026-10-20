# Finite remote Windows topology reader

Windows topology transport requires an explicit `windows_reader_program` pointing to an existing native `tune.exe`, an explicit `git_program`, target identity hash, and the per-request source allowlist. Program paths use a drive letter, forward slashes, safe ASCII components and an `.exe` suffix. No helper is installed, no PowerShell fallback exists, and no normal remote configuration is enabled by this change.

The remote command is a fixed Git Bash `exec` of `tune topology-reader --budget-ms N`. This entry runs before any Engine, ledger or database initialization; other platforms reject it silently. It accepts only the embedded Windows topology probe, never an arbitrary program or script.

## Wire contract

Each frame starts with eight ASCII bytes `KTRDR001`, a little-endian u32 header length and a little-endian u32 capture length, followed by those exact byte ranges. The request capture length must be zero. Request JSON is capped at 2 MiB and the configured file limit. Response JSON is capped at 1 MiB, enough for the guard's maximum 4096 retained identities; capture bytes use the configured stdout limit. Lengths are checked before allocation. JSON rejects unknown fields in typed objects, duplicate keys at every depth and trailing JSON values. Cursor shape and source/limit admission use the existing product rules.

The reader duplicates its standard-input handle into an unbuffered Rust File and consumes one declared request frame without waiting for EOF or prefetching. Subsequent stdin bytes remain unread and cannot become another request, probe or command. No helper stdin-EOF claim is made. The guard's stdin writer and stdout/stderr EOF fields describe only its owned PowerShell process.

The response binds a fresh controller nonce, exact request JSON byte SHA-256, scope/time, expected target identity, embedded probe SHA-256 and budget. The request itself has no self-referential digest. The guard independently creates its owner session. Process identity carries PID, complete decimal-string FILETIME and OS session; no hostname, path, SID, argv, environment or raw stderr is added to receipts.

Controller admission order is SSH success, complete bounded frame, typed header/request binding, actual completed guard receipt with root exit code zero and confirmed cleanup, capture digest, capture JSON/runtime identity, source allowlist validation, then semantic manifest parsing/cache/database. Missing or uncertain evidence returns an empty failed capture with no continuation. The embedded probe checks allowed HEAD/path/bytes/hash before returning content; the helper and controller both recheck the source report. Configuration collection is still rejected in this source-only stage.

## Deadline and exit

The standalone CLI starts its monotonic hard budget before argument parsing. A request may shorten that budget and cannot extend it. Initial input admission is additionally limited to two seconds. Work reserves time for the guard's termination observations and response output. A separate main-thread supervisor does not read/write standard streams, join worker threads, or open product state.

At deadline or failure, the supervisor requests termination of only its own process through `TerminateProcess(GetCurrentProcess(), nonzero)`, without logging, flushing, a success acknowledgement, Rust runtime cleanup or DLL detach. Successful workers drop guard resources and their private runtime, write and flush the complete response, then notify the supervisor to request exit zero. No other PID or inherited parent Job is terminated. A defensive `abort()` is used only if the self-termination API unexpectedly returns. The sole non-inherited owned Job handle has kill-on-close semantics.

Partial output or even a complete response followed by a nonzero SSH exit cannot be accepted. OS pending I/O can delay final exit after the termination request. The helper cannot attest to its own post-response final process exit: an independent observer must retain the helper's process handle and bind PID/FILETIME/session during native acceptance. A local controller Job owns local SSH descendants, not the remote reader.

## Evidence boundaries

Protocol/adapter tests reject altered acknowledgements, request digests, frames, identities and budgets. CLI tests cover unsupported/malformed entry and open/partial stdin. Windows-only tests run the embedded probe against a temporary committed fixture and keep capture output unread past pipe capacity to exercise self-termination. Cross-target Rust checking is not Windows execution or linkage.

Normal recurring scheduler coordination, finite per-host execution slots and their independent exit observer remain Gate 3. Configuration coverage remains a separate requirement. Normal remote topology must stay disabled until those gates and native product acceptance pass.

Sources: [Rust cleanup](https://doc.rust-lang.org/src/std/rt.rs.html), [Windows ExitProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-exitprocess), [Windows TerminateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess), [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).
