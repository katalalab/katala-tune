# Windows log transport: worker boundary investigation

This records an investigation, not a product fix. The branch starts at
`8313b1a`; the existing uncommitted experimental worktree is independent and
must not be integrated as a fix. The live Windows log transport launches `ssh`.
Node and mise below are synthetic regression inputs, not production SSH.

## Cause and available boundary

`logs::transport::run` currently starts a Tokio child with piped standard I/O
and kills only that immediate child on timeout. A shim can leave descendants
holding a pipe after that child exits. In the earlier synthetic run the test
did not return after the direct child exited, but the exact place it waited
was not established; Tokio runtime shutdown waiting for blocking pipe work is
a hypothesis, not an observed cause. A successful direct-Node test does not
establish SSH or shim tree cleanup.

The existing `collect::run_standalone_windows_bounded` provides a fresh,
non-breakaway Job with kill-on-last-handle-close, `PROC_THREAD_ATTRIBUTE_JOB_LIST`
at `CreateProcessW`, a restricted standard-handle list, bounded named-pipe I/O,
and explicit observations of process exit and pipe EOF. It is only for an
already isolated, single-spawner worker. Its `HANDLE_LIST` limits what its own
child inherits, but its three client handles must be temporarily inheritable.
Other concurrent native or Rust spawns in the same process can still inherit
them; a mutex private to this code cannot coordinate every native library.
The GUI must therefore not call this guard directly.

Microsoft documents `JOB_LIST` for Windows 10 and later, and documents that
`HANDLE_LIST` requires inheritable handles and `bInheritHandles=TRUE`. The
[Windows process-creation guidance](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812)
describes using the creation-time Job attribute to avoid the suspended-child
assignment window. The [attribute reference](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)
and [Job lifetime rules](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
are separate from proof of the whole controller protocol. In particular,
synchronous filesystem and Win32 setup cannot be preempted by Tokio timeout.

## Synthetic evidence on Windows, 2026-11-01

- The final `owner_abrupt_exit_closes_its_job_and_pipe_holding_descendants`
  passed three times (0.37, 0.37, and 0.36 s, including a deliberate 250 ms observation
  interval). It exercises abrupt owner exit inside a separate owned Job.
  An outer owned Job supervised a fixture owner; the owner created its own Job
  containing a child and a pipe-holding grandchild. Once all three were live,
  the fixture used `TerminateProcess` only on its own pseudo-handle. All three
  retained process handles signaled, and the outer guard reported completed
  cleanup without a lingering-descendant stop. Ready markers are published by
  rename after complete writes. The retained owner identity matches the
  creation-time Job root; the retained child and grandchild identities match
  receipt entries admitted after actual `IsProcessInJob` checks. A separately
  started fixture remained live after the owned Job completed and was then
  stopped through its own retained process handle. Its 60-second lifetime
  exceeds the test's readiness and completion budgets; it also observes its
  exact parent identity and exits on parent death. That last self-exit path was
  not separately forced. A synthetic panic confirmed that unwind cleanup
  stopped the separately owned fixture. Cleanup polls for a bounded interval,
  but cannot preempt a blocked synchronous process API call. The guard also requests
  termination when its already-empty Job is dropped. The observation is
  consistent with inner kill-on-close; it does not exclude every race in which
  the inner guard requested termination before the owner exited. The outer
  receipt's EOFs are for its pipes to the owner, not direct inner-pipe EOFs.
- The existing guard cases for timeout cleanup, a lingering descendant, an
  unrelated child's survival, and EOF while the root remains live each passed.
  They exercised fixture processes and their dedicated Jobs, not fleet machines.
- The original Node regression test, run inside an owned Job with the direct
  Node directory prepended only to that child's `PATH`, passed in 1.27–1.28 s.
  With a mise shim and an intentionally minimal child environment, this run
  instead failed quickly: the shim did not resolve a Node version. That is not
  a reproduction of the earlier hang. In the earlier separate investigation,
  the unchanged test with the then-current shim environment remained running
  after 14.08 s; the direct-Node control passed in 1.32 s. These are separate
  observations, not a claim of a current production SSH incident.

The new owner-exit test is in `crates/tune-core/tests/windows_guard.rs` and
creates and removes only its own directory inside the worktree's `target`.
The environment-dependent baseline runner was temporary and is not part of the
product tree. It used only the freshly built, trusted synthetic `tune-core`
libtest executable. Cargo's hashed binary name is not an immutable source
identity and must be obtained again after any rebuild.

## What remains before a product change

The parent-to-worker boundary does not yet exist. A candidate should start a
fixed, authenticated worker before GUI/runtime initialization, atomically put
it into a fresh Job with `bInheritHandles=FALSE`, and communicate through
bounded, non-inherited named pipes with a restrictive DACL and verified peer
process identity. It must pin or otherwise bind the exact worker image and its
early-dispatch protocol at both CLI and Tauri entry points. The worker may then
reuse the existing standalone guard to launch the intended SSH transport.
An unconstrained arbitrary-command worker or a claimed isolation guarantee based
only on `HANDLE_LIST` would not meet this boundary.

An independent owner must enforce one original wall-clock deadline across
worker setup, handshake, execution, termination, actual process exit and EOF;
failure or unconfirmed cleanup must withhold output. Handling a parent-side
`CreateProcessW` call that itself stalls or a parent crash during that call
remains unresolved. A timer that only drops an async future does not cancel a
synchronous Win32 call. Atomic Job creation, handle ownership and a deadline
handshake need to be evaluated together rather than assuming eventual cleanup.

The owner-exit test's Tokio timeout and abort are not an independent watchdog
against a synchronously blocked Win32 setup call. No test here proves parent
death *during* the creation syscall, concurrent
native spawns in one GUI process, protection from unintended handle inheritance,
real SSH behavior, or a hard wall-clock return during blocked setup. Before a
product integration, use a separately owned synthetic Job to test those cases,
including a pipe-holding grandchild, parent death, independent bystanders,
parallel native spawns, failure injection and actual EOF/exit observations.
If containment or exit cannot be established, keep the transport unchanged and
report failure rather than claiming successful cleanup.
