# Saved baseline / Rust parity acceptance receipt

Scope: Issue #34 saved numeric Store reader and pure hourly classifier. Main baseline is3decafb; product source750266d. The later test-only child-capture correction is isolated from the product reader. The earlier retention backend is already merged via PR #98. This slice does not close Issue #34.

## Delivered behavior

- `Store::metrics_baseline(node,metric,now)` / JS `metricsBaseline` reads the existing connection only; it never opens a normal DB, initializes schemas, migrates, repairs or compacts. A normal `Store::open` / `openDb` still has its existing initialization/migration writes; use an already-open read-only connection for inspection.
- A SAVEPOINT pins one snapshot across seal/last_now, selected-node hour history and raw minutes at/after seal. Partial seal-hour sum/counts are combined before division. Raw sample count validates a row but does not weight it. Late raw below seal cannot overwrite frozen archived minutes.
- cpu/mem/commit percentage hourly means only. UTC weekday/four-hour profiles, a28-day horizon with recent24hours excluded from training (training27days, observation24hours), explicit maturity/quality gates. No daily-to-hour inference or missing-value zero fill. `within_baseline` describes only assessed hourly-use windows.
- Bounded source reads:672 hour rows /40320 minute rows plus one sentinel; text payload caps2048/65536bytes. SQL projects only the selected metric JSON before parsing, including when unrelated JSON has huge exponents or deep structure. Output contains no node/src/raw payload.
- `source_limit`, `source_invalid`, `storage_unavailable`, `clock_rollback` withhold the entire assessment; genuine SQLite busy/I/O/SQL errors propagate. Ordinary incomplete data returns the pure coverage/maturity reasons with no observation.

## Deterministic evidence

Fixed clock:2026-10-26 12:30 UTC, synthetic identities/values, temporary/in-memory databases only.

| Check | Result |
| --- | --- |
| Initial saved JS stub |8 intended failing tests, then GREEN |
| Initial Rust stubs |2 intended failures, then GREEN |
| Additional parity failures |Fractional archive timestamp, unrelated JSON exponent/depth, and nonfinite archived sum each RED→GREEN |
| Node pure/saved/shared/retention |45 pass,0 fail (`node --test test/hourly-baseline.test.js test/saved-baseline.test.js test/baseline-shared.test.js test/metrics-retention.test.js`) |
| Rust baseline, capture and Scratch boundary tests |14 pass,0 fail (`cargo test -p tune-core --test baseline --locked`) |
| Existing Rust retention regression |9 pass,0 fail (`cargo test -p tune-core --test retention --locked`) |
| Shared expected/parity cases |21 pure +18 saved; complete JS/Rust reports exactly equal, plus independent literal state/coverage/quantile/trend assertions |
| Static checks |`cargo fmt --all --check`; focused `cargo clippy -p tune-core --lib --test baseline --test retention --locked -- -D warnings`; diffcheck/gitleaks/candidate privacy passed |

All assigned Cargo work used this clone's `target/issue34-baseline` and build jobs2. Initial runs used test threads1; the later Scratch correction explicitly removes RUST_TEST_THREADS and uses Cargo's default parallelism. No shared warm target was modified.

Cases include weekday/diurnal normal use, step/rising/both, CPU trend unassessed, sparse profiles/endpoints, zero/null/coarse/duplicates/current/future, actual gaps, interpolated quantiles and exact thresholds. Saved cases include30minutes at10 plus20minutes at50 yielding26, mixed336 archived+336 recent hours,40320 valid raw minutes accepted and40321 source rows rejected, corrupt/unsafe/blob data, daily-only/capacity gaps, rollback, same-node filtering and old/read-only/nested connections.

The JS WAL test compacts on a second connection between the adapter's SELECTs and proves one old snapshot followed by one new snapshot. Rust verifies the corresponding nested reader against an outer pinned transaction and a second-connection compaction; its internal SELECT ordering is additionally reviewed statically. Tests confirm unchanged total_changes and read-only reopen; old missing tables are not created. Busy reads are actual exclusive-lock errors, followed by a successful retry on the same connection.

The original capture helper had unbounded `Child::wait` and reader-thread `join` paths. Parent review raised P2; a documentation limitation did not resolve it. The replacement common test support (`tests/support/owned_node.rs`, helper-only commit e288f06) uses literal Node with stdin closed and no shell, existing Tokio only, and no process-name/PID lookup. Success requires successful root exit and both naturally completed, within-cap output reads; caller JSON parsing separately rejects incomplete output.

The execution deadline15s and final cleanup deadline17s are set before runtime creation/spawn and passed unchanged. Root observation uses owned `try_wait`; kill uses owned `start_kill`/kill-on-drop. Reader tasks are harvested only after completion. Failure gives pipes time for natural completion until the original cleanup deadline, then cancels unresolved tasks without calling that EOF or successful cleanup. `RuntimeOwner::drop` calls `shutdown_timeout` with the original remaining budget on normal return and panic; no default unbounded runtime destruction, child wait or unfinished join remains. Caller panic/JSON failure retains its owned scratch folder.

Three stub assertions and the runtime-unwind800ms wait regression were observed RED before their fixes. Positive output, owned child hang, stdout/stderr overflow, nonzero exit, root exit with a deliberately held duplex reader, incomplete JSON rejection and runtime unwinding now pass. The duplex case proves async EOF/cancellation handling, while synthetic blocking-pool work proves bounded runtime teardown. Neither is Windows pipe execution evidence. Windows I/O analysis comes from the locked Tokio dependency source; cancellation may leave underlying blocking work unresolved. Such a capture fails and retains scratch; it does not certify OS I/O cancellation. Output caps bound retained bytes (4MiB stdout/64KiB stderr), not I/O lifetime. Native spawn/filesystem calls and OS scheduling are not hard realtime operations; the explicit supervisor waits/shutdown share finite budgets.

The preexisting timezone test's first run timed out after5s during Node spawn; isolated and subsequent focused suites passed without source/timeout/ignore changes. Its cause remains unresolved; successful retries are not evidence of a fix. Original error evidence and subsequent logs are retained privately in this plan's ignored workspace/conversation.

## Remaining acceptance

Independent whole-branch read-only review of3decafb..750266d found no product blockers; parent subsequently identified the capture P2. Independent delta review confirmed the test-only wait/join/runtime correction has no additional blockers. It inspected snapshot ordering, projection/bounds, failure modes, pure arithmetic and registrations; it did not independently rerun tests. Parent integration review governs subsequent steps. Final-head full checks/CI remain under parent coordination. GUI/normal DB/action acceptance is outside this slice. Focused success does not establish broader results; historical parent integration checks below do not certify a later head.

No renderer/IPC/automatic finding/collector/notification/action/release changes. Long-term UI, event overlay/before-after display, benchmark regression and final Issue #34 screen acceptance remain follow-up. No boot/window/effective-state provenance is invented; hourly pressure observations are not leak, slowness, causality or action-effect (#35) proof. Parent alone reviews integration and merges; release/fleet/#57/#101/Defender holds remain in force.


## CI-discovered Scratch collision correction

PR #103 CI run40261570723/job121374879723 on old4877400 failed three macOS baseline tests withOs17 AlreadyExists at the exclusive temporary-directory create. Timestamp/PID was treated as unique without collision handling. The actual clock precision and colliding paths were not recorded, so those causal details remain uncertain; the create collision itself was measured. Parent saved the failure logs. Old local successes do not cancel this failure.

The test-only Scratch constructor now adds a process-local AtomicU64 serial and tries at most32 candidates. Only AlreadyExists advances the candidate; all other errors propagate. It neither deletes nor opens an existing candidate, and the guard owns a path only after exclusive create succeeds. Product and shared Node helper blobs are unchanged.

A fixed-clock controlled collision first failed as expected, then passed after the correction. Tests cover16 concurrent same-clock constructors, existing marker preservation,32candidate exhaustion without deleting existing owners, and noncollision error propagation with the original file unchanged. Final focused Rust baseline14/14 +retention9/9, default parallelism (RUST_TEST_THREADS unset), focusedclippy/fmt/gitleaks/privacy are recorded for the correction. No serial-test setting, ignore or timeout extension was used to repair it. Parent delta review and final-head CI/full checks are required before integration.

Historical parent evidence on old4877400: Mac fullcheck exit0 (JS572/27skip, Rust724/8ignored across50summaries, fmt/workspaceclippy), Electron RUN_AS_NODE571/28skip exit0; authorized isolated Windows clone Node4files45pass and Rustbaseline10/retention9 exit0. Windows helper cases exercised real pipes, while held-duplex and synthetic blockingpool cases retain their narrower meanings. These are old-head results, recorded alongside the CI3fail rather than used to erase it. No normal DB/app/probe/action or security-setting changes were involved.

The new fixed-clock parallel test initially used a16-party Barrier. Independent review identified a possible wait during partial OS thread-creation failure; the barrier was removed before the final verification. Fixed-clock/sharedAtomic constructors and retained guards still exercise distinct ownership without waiting for missing participants. No OS thread-exhaustion experiment was performed. Parent delta source review found no further required issues.

Old4877400 Windows CI also failed in an existing tune-cli topology_reader embedded-probe test with backend=missing /stop=io_failure; parent preserved and classified that separately. Its relationship to Issue #101/earlier I/O is unproven. This Scratch correction does not claim to remediate Windows I/O.
