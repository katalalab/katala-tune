# Metrics retention implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement task-by-task in this dedicated clone. Parent controls integration and heavy-test slots.

**Goal:** Preserve bounded numeric monitor history beyond the existing 14-day minute retention without changing collectors or action safety.

**Architecture:** Standalone Rust/JS modules share a dedicated additive SQL schema. Atomic maintenance converts sealed minutes into hours, old hours into days, then enforces TTL/capacity; a monotonic watermark prevents replay inflation.

**Tech Stack:** Existing rusqlite, node:sqlite, serde_json, node:test. No dependencies.

**Spec:** ../specs/2026-10-26-metrics-retention-design.md

## Global constraints

- 14-day minute / 90-day hour / 400-day day history, UTC epoch millisecond buckets.
- 50,000 rows / 16 MiB logical history bytes; 2048 JSON bytes and 256 UTF-8 node bytes per row.
- No common DB init/global migration edits until issue19 schema clearance.
- No Rust build/full suite/Electron/native test until parent slot; light Node fixtures allowed.
- No user DB, real ledger, probes, actions, external notifications, startup, release, merge or deployment.

## Review focus

- Missing and unequal coverage must not become zero or biased means: Task 1 literal expected stats.
- Replacement/replay/clock rollback must not inflate frozen stats: Task 2 watermark tests.
- Failed archive must not delete source data: Task 2 rollback trigger test.
- Byte pressure must evict enough rows and report loss: Task 2 tiny-budget and default-budget long simulation.
- Shared old databases must survive initialization: Task 2 reopen and Task 3 cross-runtime fixture.

### Task 1: Pure numerical aggregation

Files: create lib/metrics-retention.js, test/metrics-retention.test.js.
Interfaces: aggregateMinutes(rows) -> hour rows with node_id/bucket/v; mergeStats(stats) -> per-key [sum,count,max].

- [x] Write literal SQLite-independent fixtures for sparse/malformed/missing metrics, UTC bucketing and unequal-count merge; name the wrong-mean/zero-fill breaks.
- [x] Run `node --test test/metrics-retention.test.js`; observe missing module behavior, then the intended assertions once module skeleton exists.
- [x] Implement allowlisted, finite arithmetic and grouping only.
- [x] Run the focused file to green; record command/output.

### Task 2: Atomic SQLite retention

Files: create config/metrics-retention.sql; extend module and tests.
Interfaces: initialize(db), compact(db,before,now,budget?), query(db,node,since,until,resolution), status(db).

- [x] Write failing behavior tests for hourly/daily persistence, idempotence/replacement/replay, rollback and old DB reopen.
- [x] Run focused tests and confirm expected missing behavior.
- [x] Implement dedicated schema, compaction transaction, scalar singleton counters and bounds. Apply budget only after source archive and daily conversion in the same transaction.
- [x] Test UTC cutoff/gap/clock reversal and tiny-byte/row cap fixture; implement minimal repairs.
- [x] Run 500-day synthetic stream plus capacity pressure against the actual module, recording logical/physical storage and coverage.
- [ ] Run focused Node suite, syntax and current/history OSS checks; gitleaks before any commit. Focused 13/13 and syntax/candidate OSS have passed; staged gitleaks/commit HEAD/history checks below remain before handoff.

### Task 3: Rust parity module and parent integration gate

Files: create crates/tune-core/src/db/retention.rs and crates/tune-core/tests/retention.rs; later register in db/mod.rs only after parent clears issue19 schema. Test fixture helper under crates/tune-core/tests/js/.
Interfaces: equivalent Connection-based functions; shared SQL file; Store::prune_metrics retains its deleted-row result while calling atomic maintenance.

- [ ] Prepare shared literal fixtures and Rust real-SQLite tests before implementation.
- [ ] Obtain parent schema clearance and Rust test slot; watch focused Rust failure, then implement parity and integration.
- [ ] Run targeted Rust/parity, Electron compatibility and required full check only in assigned slot; report any baseline failures by name.
- [ ] Obtain fresh independent whole-diff review; fix important findings with RED/GREEN.
- [ ] Commit small verified changes, provide SHA and evidence to parent. Parent decides PR/merge order. Keep #34 open until baseline/events/UI remaining acceptance is satisfied.

## Execution ledger

- 2026-10-26: user authorized acceptance definition/implementation; parent approved dedicated retention work first, deferred shared init/migrations and heavy verification. Work in feat/issue34-long-history-20261026 on a635400.
- Ruling: use monitor minute data first rather than adding snapshot-save schema changes while issue19 owns that area. Snapshot integration remains explicitly unfinished.
- Builder check: fixed a cutoff-day data-loss contradiction by merging expired hours incrementally into daily buckets before deletion; enforced before <= max(0, now - 14 days) to preserve the raw guarantee. Independent reviewer reported no other material replay/transaction contradiction.
- 2026-10-26 Task 1: stub RED 3 assertions -> GREEN; numeric allowlist/count/max implementation verified.
- 2026-10-26 Task 2: SQLite API missing RED -> GREEN; archive/replay/rollback/TTL/capacity/reopen verified. A fractional 0.1 series exposed round-off in stats validation; relative tolerance repair passed its regression.
- Independent code review: one P2 unsafe SQLite sample-count integer blocked all maintenance. Mixed valid/unsafe count fixture reproduced RangeError; SQL normalization now omits/counts that row before JS conversion. Fresh reviewer rechecked 13/13 with no remaining findings in this slice.
- Latest focused verification: 13 pass / 0 fail / 0 skip; default row+byte pressure and 500-day six-node fixture run in about 2.4 seconds. Retained 12,804 rows / 1,473,312 logical bytes, allocated DB 2,371,584 bytes, WAL 5,248,912 bytes. These are component fixtures, not application or native acceptance.
- Task 3 remains gated on parent issue19 schema clearance and heavy-test slot. No common init, monitor, Rust or action changes have been made.
