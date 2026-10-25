# Issue #34: numeric monitor retention component receipt

Date: 2026-10-26. Base: a63540024babb2f1e0284b9d94f8116e3cfa2f35. Component: 4f5a747. This receipt covers the standalone SQLite/JS module only; the application does not yet call it.

## Accepted engineering defaults

The user delegated acceptance definition and implementation. The coordinating parent adopted hourly90/day400 as engineering defaults, with snapshot200 unchanged. Preserve raw minutes14days, then numerical hour/day rollups; UTC buckets, missing metrics omitted, valid-minute weighted mean and maximum. Global capacity is 50,000 rows or16MiB logical payload, node_id<=256 UTF-8 bytes and statsJSON<=2048bytes. Capacity loss and too-late source rows are counted. Shared DB/WAL physical size is not hard-capped by this feature.

## Verification

`node --test test/metrics-retention.test.js`: 13 passed,0 failed,0 skipped (about2.4seconds). Fresh independent reviewer reproduced/fixed-review the unsafe SQLite integer blocker and independently re-ran13/13 with no remaining findings in this component.

The suite exercises real SQLite and the real module: unequal hourly coverage, genuine zero and missing values, allowlist, duplicate compaction, pre-seal timestamp replacement, frozen replay counter, monotonic watermark, UTC hour/day boundaries, fractional values, injected archive rollback preserving raw rows, old DB initialization/reopen and read-only queries.

Default row and byte capacity limits are each pressured by a synthetic archive. A separate500-day stream across6 fictional nodes inserts72,000 minute rows through SQLite, then runs periodic compaction. At the final clock it retains12,804 hour/day rows and1,473,312 logical bytes, with the expected first/last buckets and14days of raw rows. Allocated DB bytes2,371,584; WAL bytes5,248,912; free pages211. These physical observations are fixture results, not a shared user-DB bound.

Syntax check, staged gitleaks pre-commit scan, candidate privacy scan, HEAD OSS and HEAD-history OSS(122commits) passed. No real ledger contents, hosts, probes or raw logs were copied into fixtures.

## Deferred integration and original acceptance

Parent sequences issue19's shared DB/action changes first. Common DB initialization/migration numbering and monitor prune remain untouched. Rust parity/build, full project suite, Electron embedded Node, application wiring and native verification await the parent slot; none is claimed passing. No release/tag/feed/deployment, external notification, startup registration, host setting/action/process changes, push, PR status change or merge was performed.

The original#34 acceptance remains open: the capacity condition has component-level synthetic evidence only; end-to-end retention is not integrated. Snapshot long-term summaries, mature baseline/normal/degradation/trend and insufficient-data fixtures, event overlays and GUI evidence are still outstanding. The parent alone decides integration/merge order.

## Connected backend follow-up (verification in progress)

Common table registration and actual Store::prune_metrics are now connected; the pre-existing monitor maintenance reaches transactional archiving. JS compatible DB initialization and read APIs coexist with the additive action target column. Snapshot200, GUI/CLI history, baseline and event overlays remain unchanged/unimplemented in this slice.

Focused follow-up:9Rust integration tests (shared7scenarios including logical byte limits/UTF8 ordering/clock rollback and actual Store metadata reader parity),2Rust retry tests,17Node tests passed. Existing DB with action target survives initialization, compaction and reopen. A BLOB source mismatch and a recent-raw clock rollback deletion were each reproduced and repaired. Silent24h suppression after maintenance failure was corrected to monotonic5minute retry, success24h; archive-only and receipt-only failures remain false, old success survives restart and recovery is idempotent. Independent review reports no remaining blockers.

The prior Electron all-suite467pass/22skip and468pass/22skip runs were successful pre-final-stage checks; they do not verify the final clock rollback patch. Final verification follows normal merge of parent main26b86e4 (PR95), then final Electron and full CI. No ordinary app or host has been operated or deployed.
