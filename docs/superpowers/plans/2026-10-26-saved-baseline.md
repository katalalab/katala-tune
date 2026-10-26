# Saved baseline and Rust parity implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans inline. Parent controls heavy slots and merge.

**Goal:** Read existing numeric history into the hourly baseline and prove JS/Rust agreement without starting monitoring or actions.

**Architecture:** A snapshot Store reader merges archived sums/counts with unsealed raw minute means. Existing JS pure classifier gets a Rust counterpart and shared fixtures. No schema/UI/runtime automation changes.

**Tech Stack:** Existing Node24/node:sqlite, Rust/rusqlite/serde_json; no new dependencies.

**Spec:** ../specs/2026-10-26-saved-baseline-design.md

## Global constraints

- Dedicated baseline clone/branch, main3decafb merged normally. Parent alone merges; no force/main push, release or deployment.
- No renderer/IPC/engine findings/collector/notifications/actions/host settings. Synthetic temp DB only; no normal user DB or raw security data.
- Pure baseline thresholds remain unchanged. Percent means only; no speed/leak/causality/effect claims.
- SAVEPOINT snapshot; selected-node hour+unsealed minutes,672/40320 row bounds+sentinel; omit invalid raw and withhold corrupt archive/clock rollback.
- Stream bounded payloads; no raw identifiers echoed. Busy/I/O errors propagate. No persistent reader writes/migration/prune.
- Focused Node first. Cargo uses dedicated target/jobs2/test1 only after parent allocation; full core/Node/Electron checks in assigned slot before PR readiness.

## Review focus

- Seal mid-hour losing or double-counting weighted contributions: Task1 unequal30/20minutes and late raw fixture.
- Maintenance between reads mixing snapshots: Task1 two-connection WAL controlled compaction fixture.
- Corrupt/oversized/bool/unsafe stored values becoming zero or trusted: Task1 malformed source fixtures and Task2 shared parity.
- Read limits hiding missing history as normal: Task1 limit/capacity/missing tables/rollback fixtures.
- UTC/quantile/actual-gap arithmetic differences: Task2 shared literal expectations and pure report parity.

### Task 1: Saved JS reader

**Files:** create lib/saved-baseline.js,test/saved-baseline.test.js; modify lib/db.js registration only.
**Interfaces:** consumes analyzeHours(points,{metric,now}); produces readBaseline(db,node,metric,now) and Store.metricsBaseline(...), report+source per spec.

- [x] Write fixtures asserting a seal-hour sum1300/count50 =>mean26 (30minutes10 +20minutes50), frozen late raw excluded and raw sample counts do not weight; empty source gives insufficient. Stub then run focused test: intended assertions fail.
- [x] Implement snapshot reader, scalar normalization/counts and registered Store method. Run focused pure+saved tests: pass.
- [x] Add RED source/quality/row-limit/rollback/read-only/atomic-WAL fixtures; implement minimal guards and verify pass. No source mutation.
- [x] Candidate privacy/gitleaks and local commit after focused tests.

### Task 2: Rust reader and pure/shared parity

**Files:** create crates/tune-core/src/hourly_baseline.rs,src/db/baseline.rs,tests/baseline.rs,tests/js/hourly_baseline.js,tests/fixtures/hourly-baseline.json; modify src/lib.rs and src/db/mod.rs registrations.
**Interfaces:** pure analyze_hours(&Value,&str,i64)->Result<Value,...>; Store.metrics_baseline(&str,&str,i64)->rusqlite::Result<Value> consumes same stored rows/limits as Task1.

- [x] Write shared fixture driver and Rust tests with literal state/coverage/quantile/weighted-source expectations; stub then assigned focused cargo test: expected assertion failures.
- [x] Port pure algorithm/reader without host date calls or dependencies; run shared pure and saved tests: agreement and expected assertions pass.
- [x] Add Rust read-only/reopen/error/snapshot coverage at boundary, verify both readers preserve content.
- [x] Run assigned focused baseline+retention Rust/Node suites,fmt and focused clippy,diff/privacy/gitleaks. Full npm check/Electron/GUI/Windows/SSH are not allocated; request parent allocation before broader verification. Record failures by name; no unrelated repairs without coordination. Commit after checks.

### Task 3: Review and parent handoff

- [x] Independent whole-branch read-only review against both specs/fixtures, fix required findings via RED→GREEN; record limits.
- [x] Write docs/acceptance receipt and update architecture for the Store read API. No Issue34 close reference.
- [ ] Send parent concrete source/validation/residual scope. Prepare draft PR if parent coordination allows; attach created PR. Parent owns merge, full Issue34 stays OPEN.
