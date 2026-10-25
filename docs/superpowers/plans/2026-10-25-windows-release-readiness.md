# Windows release readiness implementation plan

> Execute inline with superpowers:executing-plans. The user approved investigation, fixes, integration and real-machine acceptance on 2026-10-25.

**Goal:** Integrate machine comparison and CLI discovery, resolve reproducible Windows regressions, and produce a release candidate with explicit Defender and native acceptance evidence.

**Architecture:** Keep the Rust/Tauri desktop and existing private ledger. Diagnose historical binary detection separately from script behavior. Preserve confirmation and executable identity contracts, use isolated data for acceptance, and publish through a reviewed PR only.

**Tech Stack:** Rust workspace, Tauri 2, vanilla JS, existing Node tests, Windows PowerShell 5.1, Microsoft Defender.

**Spec:** User request in this chat; existing docs/safety.md and docs/release.md remain binding.

## Global constraints
- Do not disable Defender, add exclusions, restore quarantined artifacts, upload private diagnostics, or claim an alert is a false positive without evidence.
- Do not change real host configuration through a collector, bypass action confirmation, or use running GPU jobs for tests.
- Keep machine identifiers, private reports, native hashes and raw security history outside Git.
- Main merge that triggers public distribution and production release require the user's confirmation after concrete artifacts and validation are ready.

## Review focus
- A successful static scan is not behavioral clearance or a Microsoft verdict.
- A local SSH timeout is not remote child termination evidence.
- Windows TEMP and executable volumes may differ; retain artifact identity checks.
- CLI discovery must preserve all PR #82 commands, effects, and no-initialization guarantees.
- Integration must preserve last successful specification data after failure and require explicit shared-machine opt-in.

### Task 1: Establish incident and integration baseline
- [ ] Capture relevant Defender threat IDs, event timestamps and signature versions without command payloads.
- [ ] Verify source and asset hashes against published release, separate binary/script observations.
- [ ] Import latest machine-comparison branch; compare PR #82 discovery contract and restore any missing regressions.
- [ ] Run existing JS/Rust baseline and native Windows fixtures; save logs privately.

### Task 2: Resolve Windows transport regressions
**Files:** crates/tune-cli/src/windows_probe.rs; crates/tune-core/src/collect.rs; crates/tune-core/src/collect/windows.rs; related existing tests.
- [ ] Integrate PR #81's dependency chain in the isolated branch; preserve its activation holds until verified.
- [ ] Reproduce the nonzero native observation/benchmark exit-code failures and timed-out READY fixture.
- [ ] Add focused regression assertions first; repair the identified cause without retries, skipped assertions, or unbounded waits.
- [ ] Verify identity, timeout, stdout/stderr limits and descendant cleanup on native Windows.

### Task 3: Validate release candidate
**Files:** CHANGELOG.md; docs/acceptance/windows-release-readiness.md; release checks where evidence identifies a missing gate.
- [ ] Run full JS/Rust checks, Electron compatibility where SQLite changed, version consistency, gitleaks and OSS history checks.
- [ ] Build candidate from exact committed source; scan exact Windows binaries and fixed scripts with current Defender signatures before execution.
- [ ] Exercise isolated Windows GUI, local/SSH collection, monitor shutdown, specs comparison, help and failed/cancelled actions; preserve existing apps/data.
- [ ] Verify candidate packages and updater test contracts; record any remaining Microsoft review, signing or distribution gate explicitly.
- [ ] Request independent whole-branch review, fix blocking findings with reproductions, create and attach the integration PR, and check its CI.
