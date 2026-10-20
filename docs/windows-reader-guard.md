# Windows bounded process ownership

The explicit Windows `collect::run_standalone_windows_bounded` backend returns
bounded diagnostic output and a typed process receipt. It is intended for a
separate single-spawner helper process. General `run_bounded` collectors retain
their existing Tokio transport on every OS and do not produce owned Job receipts.
Successful general collectors, including capability checks, are not evidence of
native backend acceptance.

The explicit backend creates a fresh unnamed Windows Job for each invocation.
It sets `KILL_ON_JOB_CLOSE`, does not permit
breakaway, and supplies `PROC_THREAD_ATTRIBUTE_JOB_LIST` to `CreateProcessW`.
Assignment happens during creation, before the initial thread can execute. The
child is also created suspended, its identity is read through retained handles,
and only that retained primary thread is resumed. There is no spawn-then-attach
fallback, shared Job attachment, parent Job mutation, or PID-based termination.
This requires Windows 10 or later. Unsupported attributes fail closed.

Only three synchronous child-side standard-I/O handles are inherited through
`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. Parent endpoints are Tokio overlapped named
pipes with explicit directions, one instance, first-instance enforcement, and
remote clients rejected. Pipe names use a fresh system-generated random nonce.
The protected pipe DACL permits the current process token's user and SYSTEM.
Before process creation, the parent opens each client endpoint itself and checks
the connected client's PID. Parent copies of child endpoints close before the
initial thread resumes. Runtime SIDs, pipe names, command arguments, environment
values and executable paths are excluded from receipts and diagnostics.

## Evidence and success

`BoundedResult.process_receipt` records the guard owner separately from the child
root. Each identity has a PID, the complete OS creation FILETIME represented as
decimal text, and an OS session ID. A random `owner_session` binds the invocation;
it is distinct from the OS session ID. ACK shape checks require nonzero PIDs,
canonical nonzero decimal-u64 creation times, distinct owner/root PIDs, unique
observed identities, and root membership in the observed list. A future remote
verifier must additionally bind the receipt to its expected invocation/owner;
deserializing these fields alone does not establish remote authenticity. Observed descendant identities retain
query/synchronize handles and are accepted only after checking membership of the
opened process object in the owned Job.

A cleanup acknowledgement requires all of the following:

- Root process handle has signaled exit.
- The owned Job reports zero active processes.
- All retained, observed process handles have signaled exit.
- Both output readers have observed actual EOF.
- The input writer task has terminated.

The Job's `total_processes` and the list of observed identities are separate
measurements. Polling can miss short-lived descendants; the list is not an
exhaustive process history. Zero Job membership is the descendant-exit evidence.
After observing the retained root exit, accounting is refreshed before deciding
that descendants linger; an earlier active snapshot cannot establish that.
Exit code `259` alone is never used as a liveness test.

A completed input writer is not a delivery acknowledgement. Tokio/mio may have
accepted an asynchronous write before it completes in the OS. Its pipe is dropped
rather than relying on `shutdown()` as a half-close. Process/Job exit and output
EOF remain mandatory. Aborting a reader is never counted as EOF.

Timeout, output overflow, lingering descendants, I/O failure, setup failure,
spawn failure and unconfirmed cleanup cannot release parser payload or code 0.
The explicit API accepts the caller's absolute `std::time::Instant` deadline;
it does not rebase a remaining duration. That same execution deadline is checked
before admitting completed evidence and again at payload release: a completion first observed after its deadline is a
timeout even if subsequent cleanup is fully confirmed. After
timeout/overflow/lingering, only the newly owned Job is terminated, and a
separate three-second cleanup window gathers exit and pipe evidence. Output
readers continue draining without increasing their saved byte budget. Cleanup
that cannot be confirmed is recorded as incomplete. Dropping/cancelling the
invocation aborts its tasks and drops the sole owned Job handle, whose
kill-on-close policy applies to the invocation's process tree.

The mandatory `StandaloneWindowsEnvironment` is a complete map and the sole
child-environment authority. The backend never reads or merges the ambient
process environment. An empty map supplies an actual empty double-NUL block;
omission removes a key. `Command` supplies program, regular argv and current
directory only. Any remaining `Command` environment set/remove entry is rejected
before spawn. `env_clear()` alone is accepted, but does not override the explicit
map. Empty/equal-sign/NUL keys, NUL values and ordinal case-insensitive duplicate
keys fail closed; duplicate values are not merged or given an implicit winner.
The validated block is sorted with Windows ordinal case-insensitive comparison.

Executable resolution searches the complete map's explicit PATH, application
and system directories. It does not fall back to ambient parent PATH. Empty PATH
entries are excluded so they cannot introduce a current-directory search. Short
canonical drive and UNC executable paths use the ordinary path form only when
`GetFullPathNameW` leaves that form exactly unchanged. Long paths,
normalization-sensitive names, and other namespaces retain their verbatim form;
removing a prefix must not select a different file. This follows Rust Command's
short-path compatibility rule for both the application name and argv[0].
Arguments use UTF-16 Windows argv quoting; shell batch files are rejected rather
than reinterpreted.

## Boundary and verification

The backend must run in the helper's early CLI dispatch, before GUI/Tauri,
schedulers, plugins or background process launchers start. A helper handles one
invocation, and no unrelated std/Tokio process launch may overlap its native run.
Rust's ordinary Windows process spawn protects temporary inheritable stdio with
a private mutex. The backend's explicit handle list controls inheritance into
its own child; it cannot prevent concurrent ordinary spawns from inheriting its
clients. A separate application mutex cannot coordinate with Rust's private one.
The API name, a caller flag, and a deserialized receipt do not prove isolation.
No opaque acceptance witness is issued by this diagnostic API, and normal
candidate topology remains disabled.

The GUI/controller must bootstrap the helper through ordinary std/Tokio `Command`
with piped stdio or owned non-inheritable handles passed through `Stdio`; it does
not create native guard clients in the GUI process. Before acceptance, the
controller must bind a fresh invocation/session, the pinned helper artifact and
source policy, retained full helper identity, actual helper exit and pipe EOF,
and a fixed independent outer deadline. Wire receipts alone cannot authorize an
opaque witness. Connecting and verifying that boundary is separate work.

The guard covers the helper's directly created Windows process tree, including
a locally created SSH client's local descendants. It does **not** own
a PowerShell or Python reader on a different SSH destination. A remote-host
native runner and bound exit acknowledgement are still required for that reader.
An independent observer retaining the remote helper's actual process handle
must run on that destination; a local SSH process exit cannot replace it.
The native guard alone does not guarantee a hard wall-clock return: synchronous
filesystem/Win32 setup calls cannot be preempted by Tokio's timeout. The deadline
is checked before process creation/resume and payload admission, but a standalone
helper's outer hard deadline and independent process observer remain required.
Finite host execution windows, scheduler coordination and settings coverage are
separate work. Normal remote topology remains disabled pending all gates and
actual target acceptance.

`tests/windows_guard.rs` is a dedicated process whose native invocations and
standard-spawn controls share one serial lock. It exercises the explicit product
backend with all twelve lifecycle cases, three startup/EOF smoke cases, and
complete-environment admission and isolation regressions,
including real PowerShell, native Rust stdio, and explicit child output-handle
closure. Standard-spawn controls finish before native invocation. Cases cover
input/EOF, timeout, both output limits, unread stdin,
lingering descendants, missing executable, unrelated child survival, cancellation
and argv/environment roundtrips. Windows native execution is required; a cross
compilation type check does not establish runtime acceptance. The environment
negative uses a separate fixture parent with only a public synthetic canary;
that parent invokes the real backend with `env_clear()` and an explicit empty
map. An argv-selected real child must execute and observe exactly an empty
environment. No process-global environment mutation or actual credential is used.

API references: [Rust temporary handle inheritance lock](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/sys/process/windows.rs#L314-L399),
[Job assignment at creation](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812%2F),
[process attributes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute),
[pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[Rust executable path compatibility](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/sys/args/windows.rs#L328-L387),
[Tokio named pipes](https://docs.rs/tokio/1.54.0/tokio/net/windows/named_pipe/struct.ServerOptions.html).
