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

## 2026-10-26 review follow-up (focused Rust verified; native follow-up pending)

The native results above describe the original debug-build repair. Subsequent review changes use fresh, fallible configuration reads at both preparation and launch, and write a pending audit row before file creation or terminal launch. Completion updates that same row. Failed completion writes are reported as incomplete without retrying launch; cancellation audit errors are surfaced as well. Shared database signatures and schema are unchanged.

The prepared protection list is retained with the prompt; a changed protection list requires a new preview. The page generation is checked after receiving preview acceptance and on both sides of native confirmation, so page replacement invalidates an already queued acceptance or a native approval in progress.

Regression sources use synthetic configuration, a real temporary SQLite database and an injected terminal launcher. They cover malformed/missing/unreadable configuration, audit insert rejection, audit completion failure or zero updated rows, and interruption followed by database reopen. No real AI CLI is called by these regressions.

Focused Rust verification on macOS used `CARGO_BUILD_JOBS=2` and `RUST_TEST_THREADS=1`, with a dedicated target directory. The intentionally failing baselines compiled before their tests failed:

| Source revision | Focused command | Result |
|---|---|---|
| `59e5296` | `cargo test -p tune-core --lib handoff::regression --locked` | Expected RED: 7 failed, 2 passed; stale permissions and missing audit outcomes reproduced |
| `b71530e` | `cargo test -p tune-core --lib handoff::regression --locked` | Expected RED: protection change test failed, 9 passed |
| `b71530e` | `cargo test -p katala-tune --lib handoff_review::tests --locked` | Expected RED: queued acceptance/page replacement and page replacement during native confirmation failed, 3 passed |
| `d0699ea68ac5b0197ec552c5d1f7259ddf381b61` | `cargo test -p tune-core --lib handoff --locked` | GREEN: 17 passed |
| `d0699ea68ac5b0197ec552c5d1f7259ddf381b61` | `cargo test -p katala-tune --lib handoff_review::tests --locked` | GREEN: 5 passed |
| `d0699ea68ac5b0197ec552c5d1f7259ddf381b61` | `cargo test -p katala-tune --lib confirm:: --locked` | GREEN: 2 passed |

These focused runs required no compile fixes. They do not run the full workspace, the Windows-specific console tests, or real application windows. Earlier native acceptance does not certify the follow-up changes. Remaining native checks must use isolated application data and synthetic findings: cancel/reload after preview acceptance and during native confirmation, protection/configuration changes while confirmation is open, and audit failure outcomes without claiming that an uncertain launch never occurred. Recheck bounded text/visible buttons and default cancellation on both supported platforms, and use a fake CLI rather than submitting to an AI service.

## 2026-10-29 post-merge follow-up

On `db56810223b28d987eebd53a9361e844e656ed23`, isolated macOS and Windows debug builds used synthetic findings, separate application data and a recording-only fake CLI. Native default cancellation, protection changes during confirmation, audit insertion failure, audit finalization failure, prompt hash and fake CLI arguments were checked on both platforms. Closing and reopening the review also allowed a new review on both platforms. Windows additionally refused a changed connection and unreadable configuration during confirmation and surfaced cancellation audit failure. Update checks were disabled only in the isolated test builds; approvals were not automated. Reopening the dashboard briefly started its local live sampler, but the reviewed findings remained synthetic and no AI service was called.

A separate Windows test with the production launcher and no CLI on PATH reproduced a misleading success message: the terminal reported `codex command not found` while the app and audit said Codex had opened. The launcher can confirm only that the terminal open request succeeded. This follow-up changes the message and audit to record that request and say CLI startup is unverified. The corrected Windows debug build showed the missing-command message and recorded the new wording. Focused handoff tests passed on macOS (20) and Windows (21), and the outcome display tests passed on both (3 each). The macOS full `npm run check` passed using two Rust test threads and process-local Git signing disabled for temporary test repositories; no tests were filtered.

Real CLI installation, authentication and network behavior were not tested in this follow-up; this is not full release acceptance.


## 2026-10-29 native follow-up: reload and minimum-size review

The corrected build was tested on macOS and Windows with isolated data, a fake CLI and update checks disabled in the test builds. A debug-only injected script invoked `location.reload()` during the review and three seconds after proceeding to the native confirmation; this tests page replacement, not a normal user-facing reload control. On both platforms the review was cancelled on reload, and approving the already-open native confirmation after the replacement wrote a cancellation rather than launching. A new review could be opened and cancelled afterward. The reload produced expected Tauri warnings about callbacks belonging to the previous page.

At the minimum window size (1040×640 logical pixels), both platforms showed the review buttons and allowed scrolling to the end of a synthetic prompt containing 81 protected entries. On macOS, additional native tests confirmed that a changed alias and invalid configuration are refused after approval, and cancellation audit insertion failure is reported. The injected database trigger and temporary configuration were restored. On Windows, the dashboard title wrapped awkwardly at this size; this is outside the handoff dialog and was not changed. Reload briefly activated the dashboard's local live sampler on both machines; the reviewed findings were synthetic. No real AI service was called.

The page replacement tests cover the explicit test-triggered reload scenarios above, not every possible timing race. The native popup remains visible until dismissed, but its approval is invalid after replacement. Real CLI installation, authentication, network operation and release packaging remain untested.

## Audit-list review follow-up

Successful `ai-handoff` entries display “起動要求済み” rather than a generic success badge. The list renders the legacy Codex and Claude Code success wording as a terminal request with CLI startup unverified; stored rows and the `ok` contract remain unchanged. Focused renderer tests cover both legacy agents, the current wording, escaping, incomplete and failed records, and unrelated actions. Native macOS and Windows views were checked with isolated synthetic audit databases containing both old and new wording; the legacy stored value was verified unchanged.
