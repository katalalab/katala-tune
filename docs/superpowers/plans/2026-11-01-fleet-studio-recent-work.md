# Fleet Studio Recent Work Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans and test-driven development. The live task request supplies the specification and authorizes independent design decisions.

**Goal:** Return to recent work using existing Katala Tune metadata without opening conversation files or connecting to another machine.

**Architecture:** Project a bounded, permission-filtered subset of the existing read-only session API and a narrowly filtered cursor-summary API into dedicated presentation types. Load it with the existing background dashboard refresh, clear sensitive previous rows whenever authorization cannot be established, and offer only verified local-directory and GitHub PR links. The cursor SQL selects only requested IDs and exposes counts and flags, never error text or file names.

**Tech Stack:** Rust, existing `tune-core` SQLite APIs, GPUI 0.2.2, existing `url` crate.

**Spec:** The Fleet Studio recent-work task in the active conversation, based on commit `023053c`.

## Global Constraints

- Active, non-shared machines only; remote machines require `ai_sessions: true`; respect explicit local opt-out.
- No conversation contents, session identifiers, real paths, real PR links, credentials or unfiltered errors in logs, reports or test fixtures.
- No remote collection, mutation, arbitrary commands, UI-triggered resume commands, or changes to the existing worktree or running app.
- Limit machines to 32, sessions to 20 per machine and displayed sessions to 50; make search scope and incomplete coverage visible.
- Treat collection status, last successful collection, last attempt, activity time, and staleness as distinct facts.
- Preserve the existing pinned dependencies and use a separate target directory in this worktree.

## Review Focus

- Missing, invalid, duplicate or newly revoked machine IDs must not reveal saved sessions.
- Failed, missing, partial and pending collection status must never become a successful observation.
- Future or missing activity and collection timestamps are unknown, never current.
- Credentials, query strings, fragments, deceptive hosts and malformed PR paths must not open.
- Remote working directories and UNC paths must never be opened locally.
- Refresh failure and permission changes must discard stale private rows until authorization is established.

### Task 1: Safe projection and collection

**Files:** Create `crates/fleet-studio/src/recent.rs` and `crates/fleet-studio/tests/recent.rs`; modify `src/lib.rs` and add a bounded summary alongside the existing `tune-core` cursor API without changing its current callers.

**Interfaces:** `recent::load(config: &Path, data_dir: &Path, now: i64) -> RecentWork`; pure helpers validate eligibility, cursor state, PR URL and local navigation. Result contains only machine name, permitted tool/work path/activity, verified links and safe collection state.

- [ ] Add fixture tests for permissions, missing and duplicate IDs, safe/unsafe links, remote paths, cursor failure/partial/pending/unknown and freshness. Confirm RED.
- [ ] Implement the smallest bounded read-only projection and ensure unselected data is neither displayed nor retained. Confirm GREEN.
- [ ] Test a real temporary Katala Tune database, missing DB and changed eligibility without exposing real data.

### Task 2: GPUI integration

**Files:** Modify `crates/fleet-studio/src/view.rs` and `crates/fleet-studio/README.md`; extend focused tests where applicable.

**Interfaces:** Add recent-work state to the background dashboard loading path and use one shared search filter; include visible scope/limits and independently labeled collection times.

- [ ] Add tests for safe refresh replacement and local-vs-remote navigation; confirm RED before implementation.
- [ ] Clear old private rows when refresh begins, distinguish sample data, allow keyboard navigation without executing resume commands, and show failed/unknown/partial/stale states.
- [ ] Document the data source, status, scope, limits, search and update behavior.

### Task 3: Verify

- [ ] Run `cargo fmt --check`, the feature tests and the existing Fleet Studio suite using a dedicated target directory.
- [ ] Build the Windows GPUI binary with locked dependencies in the dedicated target directory; do not stop or replace the running app.
- [ ] Review the diff for disclosure, stale authorization, unintended I/O and clean worktree boundaries; report verification and remaining limits.
