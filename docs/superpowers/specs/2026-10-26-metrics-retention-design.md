# Issue #34: bounded metrics retention, first slice

The user explicitly delegated acceptance definition and sequential implementation to this chat, coordinated with the parent. Defaults below are engineering decisions, not pre-existing user requirements. Parent requests dedicated retention tables/APIs and light fixtures first; common DB init/migrations wait for issue19, and Rust builds/full suites/native work wait for the assigned slot. No release/merge/deployment, notification, startup, security-setting or action changes.

## Approaches and scope

1. Keep full probes longer: duplicates private/raw data and does not bound capacity well.
2. Add numeric hourly/daily rollups in the existing SQLite store: selected. Existing minute averages/maxima are the authoritative input, dependency-free and independent of action audit schema.
3. Compute only when drawing: cannot recover deleted data.

Implement monitor numeric history first, preserving minute values for 14 days, hourly aggregates for 90 days, daily aggregates for 400 days. Snapshot long-term summary follows issue19 schema coordination; baseline and event overlays follow in separate slices. This slice alone does not close #34.

## Data and statistics contract

New tables use dedicated names `metrics_history` and `metrics_retention_state`, without a global schema version. Both Rust and JS consume one SQL schema file. Source minute table stays compatible. History PK is (node_id, resolution, bucket), resolution is hour/day, buckets use UTC epoch milliseconds. Node IDs are at most 256 UTF-8 bytes for this optional history; longer IDs remain in existing data and count as omitted history.

Allow only the existing 15 numeric monitor keys (cpu, mem, swap, commit, dr, dw, rx, tx, power_cpu_w, power_soc_w, power_platform_w, power_gpu_w, gpu, gmem, gtemp). Exclude procs, destinations, arbitrary keys, paths and raw logs. Each metric stores [sum_of_minute_means, valid_minute_count, maximum_of_minute_maxima]. A valid entry is exactly two finite nonnegative numbers with mean <= max <= 1e12. Counts are per metric: a missing metric is not zero. Unusable source rows (including unsafe sample counts, wholly missing numeric values, oversized node IDs or source JSON above 64 KiB) increment omitted_rows. Unknown/non-numeric keys are never copied. Source iteration processes batches of 5,000 rows. Daily merge adds sums/counts and takes maxima, preserving minute-weighted statistics across unequal hourly coverage. API derives mean from sum/count; it never claims this is a raw-sample-weighted mean or a median.

Compaction accepts explicit `before` and `now` clocks, 0 <= before <= max(0, now - 14 days), both safe integers (<= 2^53-1). Only minutes in [sealed_before, before) are added to hours. After materialization, hours strictly before the UTC hour containing (now - 90 days) merge incrementally into their UTC daily bucket before those hours are deleted. Daily buckets before the UTC day containing (now - 400 days) expire. A cutoff day can contain both daily (expired-hour contributions) and hourly (newer-hour contributions) partitions; do not count either partition twice in a future combined view. The caller cannot seal the most recent 14 days. Crossing cutoff buckets may remain, so at most 2161 hourly/401 daily buckets per continuously observed node, before global capacity eviction.

Compaction is one transaction including archive writes, state update, raw minute deletion, daily conversion and pruning. A failure rolls everything back. The monotonic sealed_before watermark prevents replay after raw deletion from double-counting; updates before that watermark are too late for historical correction, are discarded once the caller deletion cutoff reaches them, and increment a visible `late_rows` counter. This freeze contract must be disclosed. Current, unsealed minutes remain replaceable through existing add_metrics. A backdated/repeated maintenance call never lowers the watermark. The raw DELETE cutoff stays the requested before, independently of the monotonic seal: a clock rollback cannot delete currently recent raw observations. late_rows counts only rows actually eligible for deletion (< min(before, previous seal)). No scheduler/collector is started by retention.

## Capacity and read API

Default history budget: 50,000 rows and 16 MiB logical bytes globally, whichever is reached first. Logical bytes = UTF-8 node_id bytes + UTF-8 stats JSON bytes + 80 fixed bytes per row; v <= 2048 bytes. Drop oldest bucket first, then resolution/node deterministically. Store bounded cumulative expired/capacity/late/omitted counters and watermark in the singleton state. Report retained first/last buckets, rows, logical bytes, capacity limits and dropped counters. Capacity loss is explicitly visible and cannot mean a complete historical interval.

This is the capacity of this feature's retained logical payload, not a hard limit on the shared SQLite file, its WAL or other feature tables. A standalone long-run fixture records page_count/page_size, freelist and WAL size to show physical effects; no VACUUM, file deletion or forced checkpoint runs against user data.

Module interfaces: JS `initialize(db)`, `compact(db, before, now, budget?)`, `query(db, nodeId, since, until, resolution)`, `status(db)`, pure `aggregateMinutes(rows)`/`mergeStats(values)`. Rust equivalents use rusqlite Connection and Result; no Store init registration before parent's issue19 schema clearance. `compact` owns BEGIN/COMMIT/ROLLBACK and deletes old metrics_minute rows itself. Later Store::prune_metrics replaces its standalone DELETE with this transaction and preserves the deleted-raw-row count. No changes to action tables or shared migration numbering.

Read queries accept hour/day, use [since, until), return sorted rows and per-metric {mean,max,minutes}. They enforce finite safe clocks and a bounded 50,000-row result. They do not mutate/compact or issue notifications. The module API is the first slice's deliverable; app/CLI wiring waits for schema clearance and a subsequent reviewed integration commit.

## Acceptance tests for this slice

- Actual SQLite fixture verifies 0/2/4 minute values -> hourly mean 2/max 4, missing metric preserved, unequal-hour daily mean 3 from minute values 0/2/4/6 (not mean of means 4).
- Duplicate maintenance/reopen produces identical numerical stats; replacement before seal takes effect; replay before seal watermark does not change stats and increments late_rows.
- UTC hour/day and 14/90/400-day cutoff boundaries, gaps and missing metrics; timestamp rollback never reopens sealed history.
- Trigger-injected archive failure leaves raw minutes, history and watermark unchanged; reopening an old minute-only DB leaves its rows intact until explicit maintenance.
- Hundreds of synthetic days through the real module, including capacity pressure: counts, bytes and oldest/latest retained buckets within the published contract; default-budget long fixture and tiny-budget eviction boundary test. Physical page/WAL values recorded separately.
- JS/Rust same fixtures and schema/results when Rust slot is allocated; Electron embedded Node compatibility also waits for slot. No real ledger/host data or probes.

## Remaining #34 acceptance

Baseline slice: deterministic normal/step/trend fixtures; initial proposal is per-node/per-metric hourly data over 28 days, require at least 7 distinct days and 72 valid hours, compare a disjoint recent window, return insufficient-data reason and no baseline findings when immature. Thresholds and configuration-change boundaries must be fixed before implementation, not inferred from the present fixed-threshold alerts.

Event slice: overlay existing action and tool observations on the same time axis with observation/execution/effective time clearly distinguished; synthetic UI verification. No automatic undo, external notification or startup. These remain open after the retention module, as does long-term snapshot integration.

## Coordinated integration (2026-10-26)

The parent cleared common table registration after verifying issue19 adds only nullable target TEXT with unchanged method signatures. Rust Store now registers the dedicated schema; its existing prune_metrics call archives/deletes atomically. The unchanged existing monitor caller reaches that path, with a fixed retry correction below. JS openDb registers the same tables and both Stores expose numeric history/status readers. No action/add_columns/engine or collector startup changes were needed. Snapshot and UI/CLI history remain later slices.

Maintenance retries after five monotonic minutes on archive or receipt failure; successful archive plus receipt uses24hours. Bounded constant meta metricsRetentionMaintenance records UTC at/ok/category/last_success_at/next_attempt_at and is returned by read-only Store status. Existing typed journal Checkpoint fields record committed/category/rows/delay; no new event schema enum, notifications or daemon. A restarted run hydrates the previous success; receipt failure never acknowledges success. If the DB also refuses the failure receipt, the primary outcome remains false and the existing journal writer's bounded failure reporting applies.
