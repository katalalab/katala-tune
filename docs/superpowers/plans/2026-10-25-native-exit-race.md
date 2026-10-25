# Native Windows exit-race implementation plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Eliminate the reproduced intermittent native-reader rejection without weakening OS completion evidence.

**Architecture:** Keep process-handle waits, Job emptiness, EOF, ownership and deadlines authoritative. Terminal status on a retained nonsignaled descendant only permits another observation within the existing cleanup window; it never proves completion. Running or uncertain descendants remain rejected.

**Tech Stack:** Rust, Win32 process/Job APIs, existing Tokio polling.

**Spec:** User request to identify and fix the intermittent failure; existing boundaries in `docs/safety.md` and `crates/tune-core/src/collect/bounded.rs`.

## Constraints and review focus

- No process reruns, extended deadlines, name-based exceptions or skipped assertions.
- Code 259 is ambiguous until the retained handle signals; signaled exit 259 remains valid.
- Query/wait failures and a mix of terminating/running children fail closed.
- Terminal status alone must never expose output or create an OS witness.
- Private diagnostic logs and deployment material stay outside Git.

## Tasks

- [x] Reproduce original integration failure and diagnostic rejection; capture retained member state and basename. Private logs show terminal exit 0 before a conhost wait signals, then immediate erroneous `LingeringDescendants` rejection.
- [x] Add a failing regression for pending terminal exit, sealed output and a cleanup deadline that does not restart. Keep existing classifier tests.
- [x] In `collect/windows.rs`, distinguish retained terminal-exit hints from running/unknown descendants, preserving all final completion checks. Add boundary tests including 259 and errors.
- [x] Run real Git diagnostics and native integration repetitions, Windows guard regressions, full `npm run check`, independent review and HEAD OSS scan. Document observed cause and results. Run the final reachable-history scan after the documentation commit.
- [x] Version and deliver the corrected runtime to the existing installations, retain backups, audit all authorized machines.
