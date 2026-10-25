# PR #84: AI handoff native acceptance

Date: 2026-10-25. Baseline: `ec695e05140e05a4984793c8935efea2abee98e1`.

## Method and scope

Used real Tauri debug builds on Apple Silicon macOS 26.6.2 and Windows 11 x64 (10.0.26200). Each application used a separate identifier, configuration and database, with synthetic snapshots from `test/fixtures/ui/results.json`. Normal application data and installed applications were not replaced. `KATALA_TUNE_DEV_CONFIRM` was unset: native confirmation buttons were exercised. Endpoint protection and CLI permissions were not relaxed.

Flow: select a finding → review the complete server-generated prompt → proceed to native confirmation → explicitly approve → open a new terminal with the installed CLI. The CLI's own additional trust or execution approval remains separate.

## Defects reproduced and repaired

1. **Windows terminal launch failed after approval.** With a slash-separated data directory without spaces, `start /D C:/...` returned exit 1 (`Invalid switch - "/Users"` or `"/temp"`). Reproduced through the application and an executable regression fixture. The working directory now uses `Command::current_dir`; terminal arguments contain fixed values. `start` still creates the interactive console. AutoRun and delayed expansion are disabled. The native test runs in both a simple slash-separated path and a path containing spaces, Japanese characters, `&`, `%` and `!`, and verifies actual console stdin, stdout and stderr.
2. **Long native alerts grew beyond the available screen.** A prompt with 80 synthetic protected-process names exposed the problem on macOS. Full text now appears in a bounded, scrollable dialog. A separate native confirmation still controls launch; proceeding from the text review alone cannot launch. The native dialog identifies the vendor, mode and SHA-256 of the same prompt retained in Rust. Renderer content uses `textContent`.
3. **Cancellation was absent from the audit log.** Both review cancellation and native cancellation now record `ai-handoff`, agent, finding ID and prompt hash, without creating a handoff directory. Launch failure and route refusal continue to be recorded.
4. **Closing/reloading the review window could leave handoff busy.** Window lifecycle cancellation releases the waiter. A generation/readiness check also rejects a preview prepared for a page that was replaced during configuration/DB reads. Replies are matched and consumed once; concurrent handoffs are refused.

## Native results

| Check | macOS | Windows |
|---|---|---|
| Finding buttons select the correct server-side finding | Pass | Pass |
| Full prompt, vendor and read-only mode visible before native approval | Pass; long text scrolled to its final marker | Pass |
| Review default/Enter cancels | Pass | Pass |
| Native default/Enter cancels | Pass | Pass |
| Explicit approval opens Codex in a new terminal | Pass; process argv and generated files verified | Pass; interactive CLI read `PROMPT.md` and stopped for an additional execution approval |
| Explicit approval opens Claude Code in a new terminal | Pass; `--permission-mode plan` process verified | Pass; `--permission-mode plan` process and folder-trust prompt verified |
| Prompt file SHA-256 matches audit row | Pass | Pass, for both agents |
| Shared and paused machines refuse launch and copy | Pass through application UI | Pass through application UI |
| Changing alias during native confirmation refuses launch | Pass through application UI; no new launcher | Covered by portable route-change tests |
| Long review at 1440×920 and minimum 1040×640 | Pass; buttons remain reachable | Native WebView2 dialog rendered correctly; minimum-size retest not performed |
| Close review window, reopen and immediately start a new review | Pass through application UI | Pass through application UI on the final build |

The macOS computer-use tool refused direct access to the Terminal app. macOS terminal verification therefore used actual CLI process arguments, generated launcher/prompt files, successful `open -a Terminal`, and audit rows, rather than interaction inside Terminal. On Windows, Codex's additional execution approval and Claude's folder-trust prompt were not approved. CLI startup is accepted; investigation completion or system optimization is not claimed.

## Automated verification

- macOS: final `npm run check` passed, including JS 430 passed / 16 skipped, Tauri 11 passed, formatting, clippy and the full workspace suite. The final run used `RUST_TEST_THREADS=2` and process-local `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false`: high parallelism had triggered three existing timer tests, and the topology fixture inherited interactive Git signing and timed out while committing its temporary repository. No tests were filtered out or weakened; global Git configuration was unchanged. The earlier unmodified-environment full check also passed before the lifecycle follow-up.
- Windows: JS tests 433 passed / 13 skipped; version, OSS check, formatting and clippy passed in `npm run check`. The unfiltered Rust suite failed `live::tests::windows_boot_preserves_script_scope_and_stdin_lifetime` with a 4120-byte diagnostic-tail assertion. The same failure was reproduced on pristine PR head `ec695e0`, without these fixes. The test and limits were not changed. Remaining workspace tests passed with only this exact baseline failure excluded (`cargo test --workspace --exclude katala-tune --locked -- --skip live::tests::windows_boot_preserves_script_scope_and_stdin_lifetime`). Final Tauri lifecycle tests and clippy also passed. The unfiltered Windows full check remains red for that baseline failure.
- Handoff tests: macOS 7 passed; Windows 9 passed, including real console I/O and literal working-directory checks.
- Review lifecycle: 3 unit tests passed on both macOS and Windows (one-use replies, cancellation/reopen, preparation interrupted by page replacement).
- `npm run oss-check -- --history`, staged gitleaks and `git diff --check` passed before committing.

## Remaining limits

This is source/debug-build acceptance, not an installer, signing, notarization, updater, or release certification. It does not accept every supported OS version or every third-party CLI installation layout. Actual system findings were not submitted as test prompts, and no extra CLI execution permissions were granted. Private screenshots and process/audit receipts are retained outside the public repository.

## 2026-10-26 review follow-up (validation pending)

The native results above describe the original debug-build repair. Subsequent review changes use fresh, fallible configuration reads at both preparation and launch, and write a pending audit row before file creation or terminal launch. Completion updates that same row. Failed completion writes are reported as incomplete without retrying launch; cancellation audit errors are surfaced as well. Shared database signatures and schema are unchanged.

The prepared protection list is retained with the prompt; a changed protection list requires a new preview. The page generation is checked after receiving preview acceptance and on both sides of native confirmation, so page replacement invalidates an already queued acceptance or a native approval in progress.

Regression sources use synthetic configuration, a real temporary SQLite database and an injected terminal launcher. They cover malformed/missing/unreadable configuration, audit insert rejection, audit completion failure or zero updated rows, and interruption followed by database reopen. No real AI CLI is called by these regressions. Rust RED/GREEN and OS acceptance for these follow-up changes await the assigned validation slot; earlier native acceptance does not certify these new changes.
