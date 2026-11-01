# Fleet Studio Resume Path Consistency Implementation Plan

**Goal:** Resolve recent-work candidates and open targets from one validated local path, preserving authorization and PR fallback.

**Architecture:** Keep the existing local file URL as the candidate representation, validate ambiguous home syntax and path traversal before encoding, and derive the displayed and dispatched OpenTarget from its safely round-tripped path. Do not change global home expansion, recent/audit tickets, sample handling, or the final OS guard.

**Tech Stack:** Rust, existing URL library and Fleet Studio tests.

**Specification:** This task's explicit acceptance criteria.

## Constraints

- Preserve existing worktrees and generated files. No real user paths, filesystem/DB access, network, OS launch or cleanup in tests.
- Dedicated target; offline, locked, single-job builds; only inspected tests with fully qualified exact names.
- Distinguish native Windows observations from virtual POSIX/Windows lexical tests and untested GUI/OS behavior.
- Review the finished diff before any local commit; no push or PR.

## Review Focus

- Home shorthand must not change root through doubled or unsupported separators; lexical tests cover Windows and virtual POSIX.
- Parent traversal, UNC, control characters and nonlocal file URLs must not become directory targets.
- The candidate, caption, keyboard and click must share the same target; a synthetic mismatch exposes accidental re-expansion.
- Invalid directories retain PR fallback; valid directories retain their original target.
- Sample and permission tickets continue to prevent external effects and stale selection.

## Steps

- [ ] Add pure tests using synthetic inputs to show the current candidate and target mismatch and invalid home shorthand.
- [ ] Inspect the exact tests and dependency paths; run only the named tests and record their expected failures.
- [ ] Apply the smallest candidate-validation and validated-URL-to-target change, retaining existing APIs and guards.
- [ ] Add/run pure virtual-platform edge cases, existing safe fallback/caption tests and relevant ticket tests by exact name.
- [ ] Review diffs, document practical limitations, and ask the independent reviewer before committing.