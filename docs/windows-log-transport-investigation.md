# Windows log transport: isolated investigation

This is an investigation record, not an accepted implementation. The working
tree contains an uncommitted experiment that must not be integrated as a fix.
The baseline was commit `8313b1ace3bb06aea88987aa0e9e1bffbd568def`.

## Observations (2026-11-01, Windows, JST)

- The original `windows_log_runner_requires_write_both_eofs_and_zero_exit` test
  did not finish after 14.08 seconds when `node` resolved to a mise shim. The
  test was run inside a newly created, kill-on-close Job; its real Node and
  shim descendants remained until that Job alone was terminated.
- The same unchanged test binary, inside an owned Job and with only that
  process's `PATH` prefixed by the direct Node directory, passed: test 1.32 s,
  supervised elapsed 1.35 s. No persistent `PATH` or system setting changed.
- The baseline test binary was rechecked after the experiment: with the default
  shim PATH it was still running at 4 s; only the owned Job was terminated, and
  its active process count reached zero. A Cargo binary filename does not bind
  its source contents and can be overwritten by a subsequent build.
- Tokio's Windows child stdio uses blocking I/O. Cancelling the async read does
  not cancel that underlying blocking operation, and killing the immediate shim
  does not close a pipe still held by a descendant. This explains the observed
  test hang; it does not establish a production incident on another machine.
- An isolated regression fixture independently reproduced a descendant holding
  pipes after its parent exited. Before the experiment it timed out at 8 s;
  its owner Job then confirmed root exit, Job emptiness and both pipe EOFs.
  The experiment passed the initial regression in 0.33 s and the stronger
  readiness/bystander variant in 1.21 s. The original transport test and all
  seven transport tests also passed under an outer owned Job in the experiment.

## Experiment limitations

The experimental concurrent-spawn approach is **not a safe product fix**:

- A parent crash between creating the suspended root and assigning it to the
  Job can leave that root suspended and outside the Job.
- Standard Rust process spawning does not coordinate inheritable handles with
  arbitrary native libraries; a native spawn can observe a temporary handle.
- Spawn and thread enumeration are synchronous and can outlive the async
  deadline. If Job assignment fails, root termination is only requested and
  its actual exit is not confirmed.

The existing standalone guard uses an atomic Job creation attribute, explicit
handle inheritance and cancellable named pipes, but it is only valid in a
dedicated single-spawner worker. It must not be called directly from the GUI.

The `windows_probe` and `windows_action` early-dispatch workers provide useful
patterns for a future bounded log-specific worker, but their closed protocols
are not a generic log transport. A complete design must bind the worker artifact
and peer identity, avoid unintended handle inheritance, preserve an independent
deadline, contain only its own process tree on parent death, and fail without
exposing output when termination is unconfirmed. Parent-crash windows, native
concurrent spawns, failed termination and slow setup require Windows tests.

## Reproducing the temporary Node workaround

From a normal worktree containing the unmodified baseline transport:

```powershell
powershell.exe -NoProfile -NonInteractive -Command '$env:PATH = "C:\Program Files\nodejs;" + $env:PATH; & rustup run 1.99.0 cargo test --locked -p tune-core --lib logs::transport::tests::windows_log_runner_requires_write_both_eofs_and_zero_exit -- --exact --nocapture; exit $LASTEXITCODE'
```

This convenience command scopes `PATH` to its child PowerShell, but does not by
itself create the outer owned Job used for the measurements above. The direct
Node executable must exist at that path.
The workaround does not repair process-tree cleanup or other test environments.
