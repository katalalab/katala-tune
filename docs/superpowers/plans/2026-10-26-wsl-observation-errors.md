# WSL observation errors implementation plan

**Goal:** Fix the two P2 findings from issue #29 without changing WSL execution admission or claiming real-machine acceptance.

**Architecture:** Preserve `wsl.list_error` in a dedicated JS/Rust finding. The fixed inside script emits command status markers; its PowerShell parser records section failures in `inside.errors` while retaining successful memory data. Render those errors and emit matching partial-observation findings. Existing snapshots without `errors` remain compatible.

**Scope approved:** The user requested a dedicated branch, regression tests and a PR. P1, Windows real-machine acceptance, Docker collection, ledger expansion and merge remain outside this patch.

**Tech stack:** Existing PowerShell 5.1, POSIX shell, JavaScript and Rust; no dependencies or schema migration.

## Tasks

- [x] Add failing JS findings and renderer tests for list failures and partial observations; keep normal empty/disabled/legacy data silent.
- [x] Add an executable shell fixture test that reproduces failed ps/df despite successful memory. Add PS5.1 regression cases for missing statuses, nonzero/partial output, malformed data, valid empty process results and JSON roundtrip.
- [x] Add fixed per-command status markers without temporary files or additional WSL invocations. Validate section results; store only fixed error enums, never raw stderr.
- [x] Implement matching JS/Rust findings and renderer detail. Add Rust tests and cross-language parity fixtures.
- [x] Run focused local tests, formatting and OSS/gitleaks checks. Leave full suites and native PowerShell validation to the existing PR CI while parent scheduling restrictions apply. Document every unexecuted acceptance gate.
- [x] Update the partial acceptance record, review the complete diff, commit/push the branch and create a PR referring to #29 without closing it. Attach the PR to this chat. Do not merge.

## Review focus

- A failed distro list must not become a normal empty-list finding result.
- A command returning partial output then nonzero must not publish that section as successful.
- Empty successful process results and failed/malformed process results must remain distinguishable.
- Missing status/data and unsupported PSI must remain visible without discarding successful memory.
- Old snapshots, stopped distros, opt-in defaults, ASCII/BOM and bounded nested capture must retain their existing behavior.

## Verification ledger

- Initial JS regressions: 5 failures before the fix, then 15/15 pass. Rust WSL regressions: 2 failures before the fix, then 4/4 pass; cross-language rules parity passed.
- Independent review found CRLF extraction in the shell-payload test could fail before its Windows skip. CRLF cases reproduced 2 failures, then LF/CRLF payload execution passed after normalization.
- P1 and real-machine acceptance remain outside this patch. Full local suites remain unscheduled; the existing PR CI supplies the cross-platform suite and PS5.1 fixture gate.
