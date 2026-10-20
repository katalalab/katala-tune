# Windows bounded process ownership

The Rust bounded transport used by topology adapters creates a fresh unnamed
Windows Job for each invocation. It sets `KILL_ON_JOB_CLOSE`, does not permit
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
Exit code `259` alone is never used as a liveness test.

A completed input writer is not a delivery acknowledgement. Tokio/mio may have
accepted an asynchronous write before it completes in the OS. Its pipe is dropped
rather than relying on `shutdown()` as a half-close. Process/Job exit and output
EOF remain mandatory. Aborting a reader is never counted as EOF.

Timeout, output overflow, lingering descendants, I/O failure, setup failure,
spawn failure and unconfirmed cleanup cannot release parser payload or code 0.
The fixed execution deadline is checked before admitting completed evidence and
again at payload release: a completion first observed after its deadline is a
timeout even if subsequent cleanup is fully confirmed. After
timeout/overflow/lingering, only the newly owned Job is terminated, and a
separate three-second cleanup window gathers exit and pipe evidence. Output
readers continue draining without increasing their saved byte budget. Cleanup
that cannot be confirmed is recorded as incomplete. Dropping/cancelling the
invocation aborts its tasks and drops the sole owned Job handle, whose
kill-on-close policy applies to the invocation's process tree.

Executable resolution preserves the explicit child PATH precedence, application
and system directories, then parent PATH used by Rust's Windows `Command`.
Empty PATH entries are excluded so they cannot introduce a current-directory
search. Arguments use UTF-16 Windows argv quoting; shell batch files are rejected rather
than reinterpreted. The bounded API's explicit environment values and removals
are preserved with Windows ordinal case-insensitive name comparison.

## Boundary and verification

This guard covers processes created directly by the Rust product on Windows,
including a locally created SSH client's local descendants. It does **not** own
a PowerShell or Python reader on a different SSH destination. A remote-host
native runner and bound exit acknowledgement are still required for that reader.
The native guard alone does not guarantee a hard wall-clock return: synchronous
filesystem/Win32 setup calls cannot be preempted by Tokio's timeout. The deadline
is checked before process creation/resume and payload admission, but a standalone
helper's outer hard deadline and independent process observer remain required.
Finite host execution windows, scheduler coordination and settings coverage are
separate work. Normal remote topology remains disabled pending all gates and
actual target acceptance.

`tests/windows_guard.rs` exercises the product bounded entry point with owned
Windows test children: input/EOF, timeout, both output limits, unread stdin,
lingering descendants, missing executable, unrelated child survival, cancellation
and argv/environment roundtrips. Windows native execution is required; a cross
compilation type check does not establish runtime acceptance.

API references: [Job assignment at creation](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812%2F),
[process attributes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute),
[pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[Tokio named pipes](https://docs.rs/tokio/1.54.0/tokio/net/windows/named_pipe/struct.ServerOptions.html).
