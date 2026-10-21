# Docker Partial Candidates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development. One worker owns all source edits; the parent integrates and an independent reviewer reads the exact frozen diff.

**Goal:** Show admitted Docker Partial positive facts privately without replacing accepted graph/cache/cursor.

**Architecture:** Keep candidate snapshots independent by scope/attempt and expose a separate candidates array. Gate2 supplies source/cleanup admission bound to the exact payload; DB validates lease and bounds transactionally. Public exports use accepted facts only.

**Tech Stack:** Existing Rust/rusqlite, plain JavaScript, node:test; no new dependency.

## 1. Independent internal display

Files: renderer/topology.js; test/topology-view.test.js.

- [ ] Add RED tests for empty accepted graph plus candidate, old good plus candidate with same IDs/different attributes, stale candidate after latest failure, escaping and the 200-row cap.
- [ ] Use this internal view shape, without merging its entity arrays into accepted arrays:

```json
{"candidates":[{"scope_id":"fixture:docker:engine:opaque","attempt_id":"attempt-1","status":"partial","facts_observed_at_ms":10,"attempt_started_at_ms":11,"attempt_finished_at_ms":12,"is_latest_attempt":true,"entities":[],"edges":[]}]}
```

- [ ] Render each candidate independently under 部分観測の候補, showing facts/attempt times and older-candidate status. Keep the accepted table separate. Render positive edges within the same candidate only; filter/kind/tab and escaping apply to both.
- [ ] Run `node --test test/topology-view.test.js`; require all assertions pass. These control fixtures do not establish browser/native acceptance.

## 2. Admission/store boundary

Files: new crates/tune-core/src/topology/candidates.rs; crates/tune-core/src/topology/mod.rs; crates/tune-core/src/db/topology.rs; crates/tune-core/tests/topology_store.rs.

- [ ] Receive Gate2 source API from its owner before selecting constructor/input type. Do not edit collect.rs or engine_topology.rs or invent Verified from snapshot status.
- [ ] Write RED admission/store tests for first Partial, existing good unchanged, later failure no candidate write, next Complete hides old candidate, source/attempt/payload mismatch, expired writer, duplicate IDs/missing endpoints, 8MiB/4096entities/16384edges candidate, 64MiB/64scope limits and 24-hour facts TTL (inclusive expiry).
- [ ] Add independent candidate table and lease-fenced store method, enforcing the shared design. Byte counts use serialized UTF-8 length. Reject over-limit payload atomically and evict only old candidates for global limits.
- [ ] Keep record_topology Complete-only and source cache/cursor/change semantics untouched. Add separate candidates to topology_view.
- [ ] Run `cargo test --locked -p tune-core --test topology_store`; require all new and existing tests pass.

## 3. Facts timestamps and public privacy

Files: crates/tune-core/src/db/topology.rs; crates/tune-core/src/topology/export.rs; corresponding Rust tests.

- [ ] Add RED old-good→new-Partial test: accepted facts time stays at the original complete observation while last attempt time advances. Add candidate-canary labels/source/digest/failure to public JSON and Mermaid tests and assert none are exported.
- [ ] Carry accepted facts timestamps independently of latest receipt. Do not use receipt maximum as accepted facts or actual attempt time. Actual accepted attempt stays null until a real ingress exists; receipt time is last_receipt_observed_at_ms. Preserve existing anonymous kind/label/edge allowlist.
- [ ] Run targeted store/export tests and JS view tests.

## 4. Product connection and final verification

- [ ] Gate2 owner connects issued admission to candidate store call with exact payload and cleanup evidence. Confirm source SHA and tests; no product acceptance before this connection exists.
- [x] Run `CARGO_BUILD_JOBS=2 npm run check`, real Docker CLI regression, actionlint, OSS current and history, and diff check. Run Electron tests when shared SQLite compatibility code changes.
- [x] Freeze files and hashes, obtain independent exact-diff review, fix findings and rerun affected checks.
- [ ] Commit/push only the owned work and open the separate Draft PR, preserving PR64 dependency. Windows native/browser acceptance, merge and formal installation remain separate gates owned by the coordinating session.

## Evidence and rollback

PR64 baseline was checked before this WT was created: JS407 pass/16 existing skip, Rust418 pass/2 existing ignored, fmt/clippy successful. C changes are not validated by that baseline. Tests and source receipt must name the actual candidate head. Rollback disables only candidate writes/display and leaves accepted graph/data intact. Admission integration is an external code-ownership dependency, not a successful implementation.

## 5. Bounded UI read and explicit integrity scope

The coordinating operator adopted the spec's independent projection contract. The worker retains sole source ownership; parent edits these design documents. Product admission constructor and actual save wiring stay absent.

- [x] Add accepted-only Store read API and a real SQL failure probe proving candidate schema/payload reads are absent; store15/export3/candidate19 and clippy/fmt pass for this stage.
- [x] Build versioned envelope and normalized display rows from fully validated canonical payload in the same write transaction. Bind search/display/endpoints and envelope identity in each row digest. Bound cumulative projection bytes/rows separately from canonical64MiB; remove rows transactionally on eviction/expiry/ordered Complete.
- [x] Read selected rows only in one snapshot, retaining Complete/TTL/ordinal/generation/source/settings binding. Report stored and filtered stored totals, selected validation scope, archive/unselected unchecked, omitted reasons. Unknown/legacy projection gives candidate0 without raw fallback; detected corruption hides the whole scope.
- [x] Apply backend filters and stable order, overall200entities/200closed-endpoint edges/64scopes and1MiB response including metadata. Bound display strings/ports. Connect read-only Engine/CLI export/Tauri/renderer parameters within the newly agreed ownership ranges.
- [x] Add RED/GREEN corruption, legacy, unselected/archive-unchecked, same-snapshot, count/filter/byte cap/closure and public-export candidate-read0 tests. Keep existing fullAPI full validation as an explicit API. Review v2 closed count-mismatch, optional UTF8 poisoning, delayed Complete deletion and query-error recovery findings; Rust50/UI25 focused tests pass. This is source approval only.
- [x] Freeze source; independently review exact diff; run fresh fullcheck/Electron/privacy checks. Measure a representative valid fixture after actual storage pruning in debug/release: bounded view time, serialization time, actual returned bytes, synthetic uncontended shared DB mutex scope and whole-command CPU/RSS. Compare a partial display API with the fullAPI honestly; do not call it output-equivalent speedup. Disk free floor10GiB and buildjobs2; no full Tauri packaging locally.

## Source-stage progress and remaining gates

Tasks1-3 source and focused regressions passed independent review. Task2 review defects (optional schema/type corruption affecting accepted view and old-writer reservation ledger updates) were fixed and rechecked. Complete writes bind adopted facts metadata atomically; a read snapshot binds rows/metadata/receipts consistently. These statements describe synthetic/current-writer source tests only.

Final v3 source validation ran against the normal merge of reviewed main. JS418 passed/16 existing skips, Rust474 passed/2 existing ignored; workspace fmt/clippy and current/history privacy checks passed. Electron417 passed/17 skips (one additional existing crypto-dependent skip), real Docker CLI9 passed/0 skips, actionlint and diff checks passed. New focused Rust56/UI25 tests have no skips or ignored tests. Independent exact-source review closed count drift, optional-value corruption, ordered Complete cleanup, query-error recovery, unknown-schema startup and unknown-projection Complete blockers.

Product constructor/engine save wiring remain absent. Real source/settings+all-child ownership evidence, accepted actual-attempt ingress, accepted-revision/mixed-writer policy, engine ownership sequencing and actual Windows/app acceptance are separate uncompleted gates.

## Representative performance evidence

The original valid synthetic input seeded11scopes/67,104,508 canonical bytes. Enforcing the new projection budget evicted2 old scopes; this is expected storage policy, not full retention of the old fixture. The measured in-memory DB held9scopes,33,019entities/33,010edges,53,764,750canonical bytes and64,364,514projection logical bytes. All storage pruning remained enabled.

On that same retained state, the v2 explicit full API returned53,762,683bytes. The final v3 UI returned200entities/199closed-endpoint edges and193,499bytes. Its debug/release median read time (including synthetic uncontended mutex holding) was81.42/34.98ms; serialization median was4.35/0.16ms. The v3 recheck measured only UI after adding the schema type guard; the full API was not repeatedly benchmarked. Outputs differ by design; no output-equivalent speedup is claimed. Setup is excluded from these timings. CPU/RSS logs cover compile/setup/both APIs or UI recheck as a whole, not UI-only resource use. This proves neither real admission/store performance nor actual IPC, native app behavior or Windows performance. Temporary benchmark source was restored to the reviewed hashes before formal checks.
