# Issue #34: numeric monitor retention backend receipt

Date:2026-10-26. Source verified:9c1f8c2, after normal merge of main26b86e4 (PR95). Later receipt-only edits do not change that source.

## Resulting behavior and scope

The existing monitor maintenance archives numerical minute rows before deletion in one transaction. Raw minutes remain14days; hourly summaries remain90days, daily summaries400days. Fifteen existing monitor keys are allowlisted. Each metric preserves sum/valid-minute-count/maximum, so unequal coverage is weighted correctly and missing values remain missing. No raw probe/log/process payload is copied into history.

The dedicated additive tables coexist with issue19's nullable action target TEXT column. Rust/JS initializers register the same schema; Store readers return bounded history, capacity state and saved maintenance outcome. Existing snapshot retention remains200. Long-history GUI/CLI access, snapshot long-term summaries, baselines/trends and event overlays remain subsequent slices; #34 stays open.

Capacity is50,000 rows or16MiB logical payload globally, whichever comes first. Each node ID is<=256 UTF-8 bytes and stats JSON<=2048bytes. Eviction uses oldest bucket then deterministic resolution/node order and reports loss. This is not a hard cap on the shared DB/WAL file. Sealed historical updates are counted as late when eligible for deletion. A clock rollback retains recent raw observations without reopening the frozen archive.

Maintenance uses a monotonic five-minute retry after archive or receipt failure; archive-plus-receipt success uses24hours. Existing typed Checkpoint fields and bounded constant metadata expose the result without raw exception text. Only complete success updates the success timestamp; it survives a monitor restart. Read-only status calls do not start collection or update state.

## Verification

- `cargo test -p tune-core --locked -- --test-threads=4`:527 passed,0 failed,4 existing ignored; dedicated target, build jobs2.
- Retention-specific coverage included in that run:9integration/parity tests plus2monitor retry tests. Shared7scenarios exercise numerical bytes, row/byte capacity, UTF-8 ordering, malformed BLOB/integer omission, replay and clock rollback. Real Store initialization/compaction/reopen preserves pre-existing action target bytes. Actual JS/Rust Store readers agree on saved maintenance metadata.
- Electron embedded Node all-suite, test-concurrency1:529 tests,503 passed,0 failed,26 platform skips, about46.1seconds. Git signing disabled only in the fixture subprocess environment. This includes17retention tests and SQLite migration compatibility.
- `cargo clippy -p tune-core --all-targets --locked -- -D warnings`, workspace formatting, diff check, staged pre-commit gitleaks and HEAD/141-commit history privacy checks passed.
- Independent reviews found and rechecked repairs for unsafe SQLite integers, BLOB conversion, silent maintenance failure, restart/receipt-only failure and raw deletion after clock rollback. No remaining blockers in the backend slice.

A500-day synthetic six-node stream inserts72,000 minute rows with periodic real SQLite compaction. At the final clock it retains12,804 hour/day rows and1,473,312 logical bytes, with the expected ranges and14days of raw data. Allocated DB2,371,584bytes; WAL5,248,912bytes; free pages211. Separate fleet-sized fixtures pressure each default capacity limit. These are controlled fixture observations, not a user-DB physical bound.

## Remaining verification and integration

Workspace/all-OS validation uses the Draft PR's normal CI; it is not claimed successful before results arrive. No native app GUI, real host workload, release/tag/feed, ordinary deployment or setting/action/process change was performed for this slice. Parent owns merge order and further #34 acceptance. Baseline/trend, insufficient-baseline findings, event overlays and GUI evidence remain outstanding.
