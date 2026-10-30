# Signed RC1 distribution and Windows updater — 2026-10-25

**Status: published prerelease; stable/fleet acceptance is on hold.** The signed updater path succeeded on one Windows canary, but a separate canary quarantined the published CLI after a behavioral detection. A clean installer scan and valid update signature do not clear that detection.

## Published artifact identity

PR [#83](https://github.com/katalalab/katala-tune/pull/83) was merged with user authorization at source `9461397778b2a2db861f693f0ceac0d97ea502c2`. The tag `v0.7.0-rc.1` resolves to this commit. The [main CI](https://github.com/katalalab/katala-tune/actions/runs/40170612690) passed all six jobs; the [release workflow](https://github.com/katalalab/katala-tune/actions/runs/40170623477) successfully built Windows x64 and universal macOS and published [RC1](https://github.com/katalalab/katala-tune/releases/tag/v0.7.0-rc.1).

All seven published files were downloaded. Their sizes and SHA-256 digests match the GitHub release API. Both updater signatures verify cryptographically against the production public key, including their signed `0.7.0-rc.1` version. Feed platform keys, asset URLs and signatures match; the published `SHA256SUMS` matches independently computed hashes. The scanned Windows CI artifact and the public installer/signature are byte-identical.

- Windows installer SHA-256: `2f5010bb0699c1b51b2cf311be0add429e3ed30848656663d0e0dc6285d0c251`
- Embedded CLI SHA-256: `55448e8d24efab221aeee4eafc325202cf4d19b815f9c1390740bc7cf1806a9b`
- Stable feed remains version `0.6.4`, byte-identical to the saved pre-release feed, SHA-256 `48bb0267e45d057d86389cf90f5dc3a592f0b5c0eb13740dea5fe3188f35ceea`.

The update signature is separate from Windows Authenticode; the Windows payload is not Authenticode-signed. No private signing key was retrieved from CI.

## Actual Windows updater canary

A private debug baseline used the merged source with version `0.7.0-rc.0`, the public RC HTTPS feed and a private installer destination. It retained the production identifier, public key and signed-version enforcement. The debug-only trigger invoked the normal update function; the confirmation-answer override was absent. This is a controlled updater test, not a claim that an unchanged public v0.6.4 app discovered the prerelease through the stable feed.

The actual native update dialog displayed `0.7.0-rc.0 → 0.7.0-rc.1`. After selecting its install control, the updater downloaded, verified and installed the public RC, exited the baseline and restarted the production RC. The new process's version and all three installed executable hashes match extraction from the signed installer. The GUI has its expected x64 Windows subsystem and Common Controls v6 manifest; CLI and agent have the console subsystem.

With private configuration/database and automatic update/monitoring disabled:

| Check | Observed result |
| --- | --- |
| State retention | A pre-update database marker survives; automatic update stays off |
| Helper approval | Existing approved CI helper bytes and ledger record remain paired; the different bundled CLI reports `approval_required`, without overwriting the helper |
| Analysis | The real analyze button persists a new successful snapshot after the test start |
| Specifications/logs | 118 specification rows render; log collection returns |
| Live data | A new session and new sample arrive; explicit stop leaves no owned sampler |
| Rendered application | Native WebView2 at 1280×900 and 820×900, expected page identity/content and no page/console errors |
| Defender scope | All five GUI/CLI/agent/helper file paths checked individually as not excluded; final installer-payload scan clean with realtime, behavior and script protection enabled |
| Cleanup | Canary GUI, sampler and temporary tasks stopped; original app registration/shortcut restored; ordinary app files and all four saved ordinary DB/config files retain their original hashes |

Browser plugin not available; existing bundled Playwright connected to the native WebView2 through loopback CDP. Screenshots, inventories, raw events and transcripts remain private.

Failed private-harness attempts were retained: the node-local marker was initially placed at the wrong JSON level, and the native TaskDialog exposes its install control as a pane rather than a UI Automation button. Correcting these harness assumptions allowed the same published binaries to complete the checks. One initial private-baseline launch exited without a captured cause; the instrumented repeat remained running and completed the updater test. It is not recorded as an explained product fix.

## New Defender hold

The exact published CLI returned network-enabled snapshots on three non-excluded Windows machines. Two reported no new detections. The third reported `Behavior:Win32/DefenseEvasion.A!ml` (threat ID `2147738096`) at `2026-10-25T07:16:18.427Z`; event 1117 confirms successful quarantine at `07:16:41.939Z`, and that test copy is absent. Runtime acceptance therefore **failed** on that machine, despite its preceding clean static scan and returned snapshot.

### Local CLI / Process Lasso investigation

Read-only inspection of the affected machine's existing Defender support log establishes a narrower relationship than the event's `Process Name` alone. The mini-filter recorded Process Lasso opening the exact CLI file with `Reason: OnOpen`, and Defender blocked that file access. It is evidence of a file-open request, not evidence of process-memory access, injection, or Process Lasso launching the CLI.

| Time (UTC, 2026-10-25) | Existing local log evidence |
| --- | --- |
| 07:16:00.196 | CLI path receives an engine `lofi/sample/expensive` result; cloud requests follow |
| 07:16:15 | Behavior-monitor telemetry for a PowerShell child lists the CLI and SSH/bash ancestry; Process Lasso is absent from that recorded ancestry |
| 07:16:17.648–17.657 | Dynamic signature receipt follows a cloud request |
| 07:16:18.292–18.425 | An SDN query explicitly names the published CLI's full SHA-256 above and completes |
| 07:16:18.426 | Engine blocks the CLI, threat ID `0x8003e1f0`, signature sequence `0x26673684da86` |
| 07:16:18.430 | Mini-filter names `ProcessLasso.exe` as the process for the blocked CLI file-open, `Reason: OnOpen` |
| 07:16:18.433–18.437 | Realtime detection and `Behavior:Win32/DefenseEvasion.A!ml` detection-add are recorded |
| 07:16:41.939 | Operational event 1117 confirms successful quarantine |

Process Lasso/ProcessGovernor had identical binaries on all three machines, version `18.4.0.48`, with valid Bitsum signatures. Both processes were present on the two machines without a new CLI detection as well as on the affected machine. Thus their presence alone does not explain the difference. The affected machine used cloud block level High and controlled folder access; one unaffected machine had the same Defender engine and intelligence version but different policy settings. Readback of both unaffected machines found no new threat detections and no matching CLI-path/hash entries in the inspected support logs. Absence of those entries does not prove that no file-open occurred.

The established relationship is that Process Lasso was the requester of a file-open blocked by Defender. The logs do not expose which behavior or classifier feature caused the verdict, nor whether removing Process Lasso would change it. Cloud/signature timing and policy differences remain correlations. No controlled stop/start comparison was performed: ordinary applications and agents were active on the affected machine, and the quarantined copy was not restored or retried. Raw support logs, process inventories and paths remain private.

No Defender policy, exclusion, Process Lasso rule or quarantine state was changed. Investigation used existing local records; no sample or analysis report was manually submitted to Microsoft or another analysis service. Existing Defender cloud protection remained enabled and its automatic requests are visible in the log. Microsoft submission is on hold at the user's explicit instruction. This new behavioral detection is distinct from the historical Bearfoos detection of an older agent; neither has a Microsoft false-positive verdict.

### Protected follow-up — 2026-10-26 JST

The affected machine's intelligence had advanced to `1.461.194.0`. Realtime, behavior and script protection remained enabled, cloud block level High and controlled folder access remained enabled, and Process Lasso/ProcessGovernor remained running. The original quarantined copy remained absent and was not restored. A fresh copy of the exact public CLI, with the full SHA-256 recorded above, was evaluated in a separate private canary directory. Its actual file path was checked as not excluded.

| Phase | Observed result |
| --- | --- |
| Normal-remediation custom scan | No threats found, exit 0, using the canonical Windows path |
| `--version`, then `--help` | Both exit 0; no new detections after each observation interval |
| Fixed read-only probe, network off | Exit 0 and parsed snapshot; no new detections |
| Fixed read-only probe, network on | Exit 0 and parsed snapshot including `netsec`; no new detections |
| Application state | Private config and data destinations remained absent throughout |

The first scan invocation used forward slashes and returned `0x80508023`; it was not accepted as a clean scan. Resolving the same file to its canonical Windows path allowed the normal-remediation scan to complete. Both observations are retained privately. No scan-remediation override, exclusion, definition rollback, Process Lasso stop/start or policy change was used.

This follow-up did not reproduce the earlier classification under the current conditions. It does not identify the classifier's trigger, prove a false positive, or attribute the change to the updated intelligence: the earlier execution environment and dynamic verdict are not reproduced. The historical detection and stable/fleet hold remain recorded; Microsoft submission remains on hold.

## Remaining release gates

1. Resolve the new Defender classification and repeat acceptance under the affected machine's unchanged protection policy. Do not hide the failure by adding an exclusion or weakening policy.
2. Complete the coordinated helper/ledger rollout using [verified provenance and paired rollback](../windows-helper-approval.md); current canary success explicitly retains an already approved helper.
3. Obtain stable-release authorization after those gates pass. RC publication is not stable/fleet acceptance.
