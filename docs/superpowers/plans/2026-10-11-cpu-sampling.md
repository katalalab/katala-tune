# Windows CPU Sampling Implementation Plan

> **For agentic workers:** Use executing-plans inline; no further agents are authorized.

**Goal:** Correct Windows CPU sampling intervals and preserve unknown readings.

**Architecture:** Per-process immutable snapshots pair CPU seconds with monotonic ticks and PID/start identity. Pure helpers calculate process percentage and host average independently of CIM collection.

**Tech Stack:** PowerShell 5.1, Node built-in tests, existing Rust check suite.

---

- [x] Baseline: `npm test`; investigate any pre-existing failure without changing unrelated code.
- [x] Add [CPU contracts](../../../test/cpu-sampling.test.js) and [PS5.1 payload](../../../test/win-cpu-sampling-regression.ps1). Test `(13.5-10)/(4.5-1)*100/8 = 12.5` despite a nominal 1.5s sleep; changed PID/start, null values, negative delta and nonpositive elapsed return null. Host `(null,40)` returns 40/partial/1; `(null,null)` null/unavailable/0; `(0,0)` 0/available/2.
- [x] Run `node --test test/cpu-sampling.test.js` and record the missing snapshot/helper contracts as RED. PowerShell execution is deferred to Windows, not counted as local GREEN.
- [x] Modify only [probe CPU collection](../../../probes/win_probe.ps1): add snapshot/process/host helpers, capture both samples before unrelated provider work, calculate `delta / elapsed / cores * 100`, preserve null groups, and add host sample metadata. Keep ASCII/BOM.
- [x] Run targeted Node tests, then `CARGO_TARGET_DIR="$PWD/target/cpu-sampling" npm run check` and `npm run oss-check -- --history`. The target directory is dedicated to this worktree.
- [x] Self-review scope/diff and commit only these owned files. Parent executes `powershell.exe -NoProfile -NonInteractive -File test/win-cpu-sampling-regression.ps1` and read-only live checks before publication.

## Evidence and handoff

Facts: original stopwatch starts after first provider wait and stops before second CPU read; null casts dilute host CPU. Assumption: adjacent monotonic timestamps adequately bound ordinary property-read overhead (not atomic kernel snapshots). Rollback: scoped commit revert; no production settings change.

### Verification record

- Baseline `npm test`: exit 1, 212 passed / 12 skipped / 1 pre-existing close-grace timing failure. Isolated `node --test test/collect.test.js`: exit 0, all 6 passed, no unrelated change.
- CPU source contracts: RED exit 1 (3 missing helper contracts), then GREEN. Non-finite/overflow guard contracts also RED exit 1 before implementation.
- An intermediate targeted/full check detected loss of the UTF-8 BOM by the text editor (exit 1); restored BOM bytes, then repeated checks without weakening assertions.
- Final `node --test test/cpu-sampling.test.js`: exit 0, 5 passed / 1 Windows-only skipped locally.
- Final `CARGO_TARGET_DIR="$PWD/target/cpu-sampling" npm run check`: exit 0. Node 218 passed / 13 Windows-only skipped; Rust fmt, clippy and workspace tests passed.
- `npm run oss-check -- --history`: exit 0 before commit; rerun against the new commit at handoff.
- Parent reports `powershell.exe -NoProfile -NonInteractive -File test/win-cpu-sampling-regression.ps1`: exit 0, PS5.1, `CPU sampling regression: PASS`. Locally confirmed SHA-256 matches the files tested by parent: probe `590e171d37e80d07cf177769dba2f3d7877fc7e531f0975c6c918e5120dab9e9`; payload `d56810ae293f9f1ab2080ee97223b64b78b4c843825ccb05092b6f58b692fb06`.

### Remaining boundary

After write ownership handoff, the parent reproduced the [process renderer](../../../renderer/app.js#L338-L359) converting Windows null CPU to zero and throwing on macOS null CPU. [Renderer regressions](../../../test/cpu-renderer.test.js) initially failed 4 of 8 tests (exit 1). Both machine and fleet process views now show unknown readings as an em dash without a numeric bar, preserve actual zero and rank unknown CPU after measured CPU. CPU-specific renderer, sampling and existing renderer review tests passed (18 passed, 1 Windows-only skip, exit 0). No rule, action or permission changes were made.

The native PS5.1 regression passed, and the updated read-only probe completed on four Windows machines. Three unchanged process identities previously reported 234–238% of one core now report about 98%; this corrects measurement and does not claim a reduction in workload CPU use.

The mandatory full check then exposed the pre-existing 100ms shell-start timing flake again. The close-grace regression now uses the actual collector with controlled spawn/timers; a mutant with the timer cancellation removed is explicitly detected. Real-process pipe/grandchild integration tests remain unchanged. Final `CARGO_TARGET_DIR="$PWD/target/cpu-sampling" npm run check`: exit 0, 227 Node tests passed / 13 platform skips, all Rust checks passed. No verification threshold was weakened.

GUI publication and installed-application verification remain separate acceptance steps. No services, installed applications or host settings were changed by this implementation.
