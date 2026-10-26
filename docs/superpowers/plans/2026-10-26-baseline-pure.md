# Pure hourly baseline implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans inline for the pure component. Parent controls integration/heavy slots.

**Goal:** Deterministically distinguish hourly pressure deviation/trend from normal weekly patterns, and explicitly withhold conclusions when evidence is immature.

**Architecture:** Pure JS receives already-aggregated hourly numeric points. Disjoint historical/recent windows, fixed UTC profiles and coverage gates precede robust summary/slope calculation. No runtime caller is added in this slice.

**Tech Stack:** Existing Node24, plain JS/node:test; no dependencies, DB or host access.

**Spec:** ../specs/2026-10-26-baseline-pure-design.md

## Global constraints

- Independent branch on main17474e1 after parent merged PR #98; leave the retention review checkout/head37e0674 fixed.
- No renderer/IPC/shared schema/collector/notifications/actions/startup/release/deployment before parent coordination.
- Pure Node fixtures only; no heavy Rust/full/native tests until assigned.
- Hourly means only: do not infer raw quantiles, daily intra-day shape, leaks, speed, causes or #35 effects.
- Thresholds/windows/maturity exactly as the spec; no sparse-profile fallback.

## Review focus

- Recent training contamination: Task1 disjoint-window step fixture.
- Unknown readings masquerading as zero: Task1 malformed/missing/sparse coverage fixtures.
- Periodic workload false positives: Task1 weekly profile normal fixture and timezone invariance.
- Conflicting duplicates/order/gaps changing conclusions: Task1 normalization and time-gap slope fixtures.
- Excessive or coarse input producing confident answers: Task1 input cap and daily-only fixture.

### Task1: Pure classifier and literal fixtures

Files:create lib/hourly-baseline.js,test/hourly-baseline.test.js.
Interface:analyzeHours(points,{metric,now})->structured numerical report.

- [x] Write fixed-clock weekly/step/trend and immature fixtures; literal expected state/coverage/reason, plus null/zero/order/conflict/limits cases.
- [x] Run focused Node file and observe intended failing assertions against stubs.
- [x] Implement normalization and maturity gates, then matched-profile quantiles and deviation/trend; verify RED/GREEN for each behavior.
- [x] Verify timezone invariance/actual-gap slopes and no lookahead; run focused suite.
- [x] Independent read-only review; no correctness blockers. Both P3 test gaps fixed with literal slot/quantile/p90 assertions; candidate privacy/gitleaks required before local commit.
- [ ] Report source/evidence/remaining adapter+Rust+UI/causality limits to parent. Do not create a PR or broaden scope before coordination.

## Ledger

- Parent authorized independent acceptance/purefixture preparation while98CI/merge is pending; existing stability has priority.
- Branch uses a separate clone to keep98checkout frozen for its review.

- Parent merged PR #98 as17474e1; this independent branch fast-forwarded to that main commit. Pure component remains unconnected.
- Initial RED: 11 intended failures and one passing CPU guard against a stub; implementation GREEN: 12 tests. Boundary, minute-quality and timezone cases bring the focused suite to16 passing tests.
- Final focused suite:17/17 pass. Independent reviewer rechecked both strengthened fixtures and found no remaining blockers for the pure component; runtime adapter, Rust and UI are outside this review.
