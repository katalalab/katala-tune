# Windows CPU sampling design

## Approved scope

Fix CPU interval alignment and missing readings in [win_probe.ps1](../../../probes/win_probe.ps1), without changing rules, transports, NoNetwork, benchmarks, dependencies, or machine state. The operator supplied and approved this design before implementation.

## Cause and choice

The old numerator includes provider waits outside its global stopwatch interval. Null host samples are cast to zero before averaging. A clamp would hide the first defect; merely moving the stopwatch would still ignore per-process read timing and PID reuse. Use immutable per-process snapshots instead: PID, UTC start ticks, cumulative CPU seconds, and an adjacent monotonic timestamp. Divide CPU delta by the matching timestamp delta and logical processor count; do not clamp.

Capture each snapshot before unrelated CIM work. PID/start mismatch, missing/non-finite reads, arithmetic overflow, negative delta, or nonpositive elapsed/count yields null, not idle. Reuse the second captured values for lifetime average and process identity. App totals are unknown if any constituent CPU is unknown.

Host CPU retains the numeric-or-null cpu_busy field. Average only present samples; add cpu_busy_availability (available/partial/unavailable) and cpu_busy_samples (2/1/0). Real zero is present. This adds metadata without changing existing renderers.

## Verification and limits

Run existing baseline, failing Node source contracts, and then the minimal implementation. A standalone PS5.1 payload imports only named helper AST definitions, never executes the probe, and tests synthetic timing/CIM-delay, identity, missing, negative, zero, and partial cases. Local Node checks do not prove PowerShell execution; the parent runs that payload and live read-only Windows verification. Keep UTF-8 BOM plus ASCII body, and new PowerShell code uses double quotes. Rollback is a scoped revert of this commit, not a machine change.
