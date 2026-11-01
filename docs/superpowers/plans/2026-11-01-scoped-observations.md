# Fleet Studio Scoped Observations Implementation Plan

> Implement in the assigned worktree only. Use synthetic data and exact, individually reviewed tests. Do not run tests that delete files, start child processes or use Job Objects, SSH, real ledgers, or real databases.

**Goal:** Show all ledger entries without reading their saved observations until an operator explicitly authorizes each entry for this application session.

**Design:** Keep ledger projection separate from saved observations. Bind each session grant to a unique, valid ledger ID and fingerprint; check it before each bounded read. Clear values immediately when revoked, discard late results, and visibly distinguish a cancellation that still has an in-flight read. The grant covers saved machine observations only, not recent work, LLM access, SSH, or ownership.

**Files:** Update Fleet Studio's source model, view and README; add a small scope state module and synthetic tests. Add a minimal read-only snapshot summary API in tune-core without reading snapshot JSON bodies or GPU/process history.

## Task 1: Fail closed on lost access

- [x] Add a pure test showing that a ledger read failure must not restore saved values. Verify that it fails against the starting code with its exact test name.
- [x] Add session grant state with validated unique IDs, ledger fingerprints, revocation tokens, and in-flight generations. Tests cover denied, duplicate, changed, revoked and late results using synthetic values only.
- [x] Change previous-value preservation so only an unchanged valid grant may retain a previous value; authorization and ledger failures clear it.

## Task 2: Limit saved observations

- [x] Add in-memory SQLite tests for scoped, minimal projections and malformed, missing, future and stale observations; prove no raw snapshot data is needed.
- [x] Query only authorized IDs, with bind parameters and bounded projections. No grant means the dashboard path never opens the DB.
- [x] Treat missing data, invalid metadata, failure, stale data and intentionally stopped machines distinctly. No machine is contacted or started.

## Task 3: Connect the UI

- [x] Show ledger entries and an unapproved state by default. Require an explicit per-machine confirmation; do not infer authorization from role, shared, hostname, or recent-work settings.
- [x] On revoke, immediately remove old values and prevent late results from being adopted. Indicate that an already-started read may still finish; do not start another read for a revoked target after cancellation completes.
- [x] Keep the UI responsive, preserve sample isolation and existing independent recent-work and LLM settings, and explain the scope and limitations in README.

## Verification

- [x] Inspect each test body and its helpers for deletion, child processes, Job Objects, SSH, or real data before executing it; use individually confirmed fully qualified test names and --exact only.
- [x] Run the permitted formatting, exact synthetic tests, GUI build and publication checks as safe and available. Capture sensitive scanner output and report counts only. Never run the repository-wide test command here.
- [x] Ask for an independent code review before local commit; do not fetch, merge, push, publish, change other worktrees or invoke the application on real data.

## Review focus

1. Zero grants must not open the dashboard DB or fetch saved timestamps for retired entries.
2. Duplicate IDs and changed or unreadable ledger state must not permit or restore data.
3. Revocation must clear values immediately and discard old in-flight results.
4. Corrupt/missing/future data must not look current or healthy.
5. Separate saved-observation authorization from recent work and network access.

## Verification record

- On Windows, 28 synthetic Fleet Studio cases and five in-memory SQLite cases passed individually with exact names. The existing source and audit test targets were compiled with no execution.
- Formatting, the GUI-scoped Clippy check and the Windows GUI build succeeded. A dependency emitted a future-compatibility warning.
- No real ledger, database, audit, remote connection or deletion test was used. The repository-wide test command was not run.
- The publication scanner was run without real ledger terms; its coverage of fleet-specific names remains unverified. The final GUI sample interactions and macOS build remain to be confirmed.
- A refresh invalidates audit results and old open callbacks; an audit target is rechecked against its ledger identity after collection and immediately before OS handoff. External ledger edits are not auto-detected, and the final check and OS handoff are not atomic.
