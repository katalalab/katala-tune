# Saved hourly baseline and Rust parity

Continue the approved pure baseline contract (`2026-10-26-baseline-pure-design.md`) on the independent baseline branch, normally merged with main3decafb. The user requested saved-data connection and Rust agreement, with safety constraints and parent-only merge preserved.

## Approach and scope

Use a read-only Store adapter in both runtimes: `metricsBaseline(node,metric,now)` / `metrics_baseline(node,metric,now)`. It returns the pure report plus bounded `source` evidence. It does not add an engine finding, IPC, renderer, collector, maintenance, notification or action. Existing absolute alerts remain independent.

Considered alternatives: reading raw alone loses the older half of the 28-day window after retention; reading hour history alone loses recent data; separate queries without a snapshot race with atomic retention. Merge retained sums/counts with unsealed raw minutes in one SQLite read snapshot. No schema or new dependency is needed.

## Snapshot and numeric connection

Use a nested-safe SAVEPOINT; first SELECT pins the read snapshot. Read singleton seal/last_now, matching-node hour rows in `[end-28d,end)`, and matching-node raw minutes in `[max(end-28d,seal),end)`. End is the last complete UTC hour, clipped lower ranges at zero. Read no other node, daily rows, src, names, messages or security data.

Raw n is a sample count: positive safe integer validates the row, never weights an hourly mean. Each aligned unique minute contributes its selected numeric mean once. Select only cpu/mem/commit (caller preselects compatible percentage definitions); `[mean,max]` must be two finite numbers with 0<=mean<=max<=100. Missing/invalid raw values are omitted, never zero. Hour stats use the retained `[sum,valid_minutes,max]` without re-rounding means. Combine sums/counts before division; same-hour seal splits must preserve their unequal weights. Raw rows below seal (including late replacements) never override frozen archived data.

Retained selected stats must be a finite nonnegative sum, integer count1..60, finite max0..100, mean<=max (existing retention tolerance1e-9), and count no larger than the number of minute starts below seal in that hour. History bucket must be aligned and below seal. Missing selected key contributes no minutes; malformed selected archive/state or combined count>60 withholds the whole assessment as `source_invalid`. It is impossible to reconstruct minute placement from archived counts; valid minutes are represented minutes, not continuous coverage. Capacity-evicted history stays missing.

Read bounds: at most672 hour rows and40320 raw rows, with one extra sentinel each. Stream raw rows, normalize/merge immediately, and never accumulate raw JSON payloads. For text JSON payloads of <=65536 bytes for raw and <=2048 bytes for history, SQLite validates the document and projects only the selected metric as JSON before either runtime parses it (unrelated large numbers/deep fields must not change selected numeric availability); select neither raw src nor identifiers into output. A source row limit yields `source_limit`, no baseline observation. Missing source tables/state yields `storage_unavailable`. Valid last_now>now yields `clock_rollback`, no assessment. Busy/I/O/SQL failures propagate as errors, not a healthy/insufficient verdict. Invalid API metric/node/clock throws/errors before reads. Node is a nonempty well-formed UTF8 string<=256 bytes; now uses the pure Date range.

On successful source read, `source={state:'ready',sealed_before,last_now,history_rows,raw_rows,omitted_raw_rows}`. Withholding source state is `'insufficient'`, with available bounded counters and null unknown fields; root reasons contains its stable source reason, observations empty, all assessment evidence/processed coverage null (input_points0). Hour-level normalized points still pass every pure maturity/quality gate. No persistent writes, migrations, pruning or receipt updates happen in this method. Missing tables on an old read-only DB are not created.

## Rust and shared acceptance

Port the pure algorithm to `hourly_baseline::analyze_hours(points,metric,now)`. UTC weekday/slot uses epoch arithmetic, avoiding host timezone/DST and new date dependencies. Keep exact window/maturity/quantile/elevation/trend contracts; malformed data is skipped consistently. Pure API programming errors return an error. JS and Rust report structures, reason ordering, coverage and source counters agree; numeric evidence uses the same floating arithmetic and tests compare numeric values with a stated small tolerance where needed.

Shared fixtures cover weekly/diurnal normal, step, gradual/both, CPU-unassessed trend, global/profile/endpoint insufficiency, null/zero/coarse/current/future/duplicates, interpolation and thresholds. Saved fixtures cover unequal seal-hour weights, no archival/daily-only/capacity gaps, frozen late/raw correction, invalid/blob/unsafe-n data, source limits/corruption/rollback, cross-node filtering, reopen/read-only and unchanged DB contents. In the JS test, a controlled second WAL connection commits compaction after the adapter pins its first read. The Rust test pins an outer read transaction, compacts on a second WAL connection, then calls the nested adapter. Both must return the complete old snapshot and the next independent call the new snapshot. The Rust internal SELECT race is additionally checked by source review against the same SAVEPOINT/first-read ordering; it is not represented as a deterministic Rust mid-method hook test.

No speed, leak, causality, action-effect (#35), UI acceptance, full Issue #34 completion or physical DB/WAL hard cap is claimed by this slice. Parent coordinates heavy test slots, PR review and merge.
