# Saved baseline / Rust parity acceptance receipt

Scope: Issue #34 saved numeric Store reader and pure hourly classifier. Main baseline is3decafb; source750266d. The earlier retention backend is already merged via PR #98. This slice does not close Issue #34.

## Delivered behavior

- `Store::metrics_baseline(node,metric,now)` / JS `metricsBaseline` reads the existing connection only; it never opens a normal DB, initializes schemas, migrates, repairs or compacts. A normal `Store::open` / `openDb` still has its existing initialization/migration writes; use an already-open read-only connection for inspection.
- A SAVEPOINT pins one snapshot across seal/last_now, selected-node hour history and raw minutes at/after seal. Partial seal-hour sum/counts are combined before division. Raw sample count validates a row but does not weight it. Late raw below seal cannot overwrite frozen archived minutes.
- cpu/mem/commit percentage hourly means only. UTC weekday/four-hour profiles, disjoint28-day training /24-hour observation windows, explicit maturity/quality gates. No daily-to-hour inference or missing-value zero fill. `within_baseline` describes only assessed hourly-use windows.
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
| Rust baseline integration |5 pass,0 fail (`cargo test -p tune-core --test baseline --locked`) |
| Existing Rust retention regression |9 pass,0 fail (`cargo test -p tune-core --test retention --locked`) |
| Shared expected/parity cases |21 pure +18 saved; complete JS/Rust reports exactly equal, plus independent literal state/coverage/quantile/trend assertions |
| Static checks |`cargo fmt --all --check`; focused `cargo clippy -p tune-core --lib --test baseline --test retention --locked -- -D warnings`; diffcheck/gitleaks/candidate privacy passed |

All Cargo work used this clone's `target/issue34-baseline`, build jobs2 and test threads1. No shared warm target was modified.

Cases include weekday/diurnal normal use, step/rising/both, CPU trend unassessed, sparse profiles/endpoints, zero/null/coarse/duplicates/current/future, actual gaps, interpolated quantiles and exact thresholds. Saved cases include30minutes at10 plus20minutes at50 yielding26, mixed336 archived+336 recent hours,40320 valid raw minutes accepted and40321 source rows rejected, corrupt/unsafe/blob data, daily-only/capacity gaps, rollback, same-node filtering and old/read-only/nested connections.

The JS WAL test compacts on a second connection between the adapter's SELECTs and proves one old snapshot followed by one new snapshot. Rust verifies the corresponding nested reader against an outer pinned transaction and a second-connection compaction; its internal SELECT ordering is additionally reviewed statically. Tests confirm unchanged total_changes and read-only reopen; old missing tables are not created. Busy reads are actual exclusive-lock errors, followed by a successful retry on the same connection.

The owned Node parity fixture child has a15s execution deadline polled every10ms, stdout4MiB/stderr64KiB caps (one extra byte detects excess), and kill/wait cleanup guarded by RAII. It reads a test-owned local file and spawns no descendants, so no inherited grandchild pipe is expected. This is not a strict15s wall-clock bound: native kill/wait, pipe-reader joins and OS scheduling have no independent cleanup deadline. The code does not claim a stronger guarantee; an OS-level stuck cleanup remains outside the measured execution deadline. The preexisting timezone test's first run timed out after5s during Node spawn; isolated and subsequent focused suites passed without source/timeout/ignore changes. Its cause remains unresolved; successful retries are not evidence of a fix. Original error evidence and subsequent logs are retained privately in this plan's ignored workspace/conversation.

## Remaining acceptance

Independent whole-branch read-only review of3decafb..750266d found no blockers. It inspected snapshot ordering, projection/bounds, failure modes, pure arithmetic and registrations; it did not independently rerun tests. Parent integration review governs subsequent steps. Full npm check, Electron embedded Node, GUI, Windows and SSH tests have not been allocated/run for this slice. Focused success does not establish those results.

No renderer/IPC/automatic finding/collector/notification/action/release changes. Long-term UI, event overlay/before-after display, benchmark regression and final Issue #34 screen acceptance remain follow-up. No boot/window/effective-state provenance is invented; hourly pressure observations are not leak, slowness, causality or action-effect (#35) proof. Parent alone reviews integration and merges; release/fleet/#57/#101/Defender holds remain in force.
