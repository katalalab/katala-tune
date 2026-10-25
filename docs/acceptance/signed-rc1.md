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

Defender's `Process Name` field names Process Lasso. This does not establish it as the cause or as the detecting engine. Process Lasso/ProcessGovernor had identical binaries on all three machines, version `18.4.0.48`, with valid Bitsum signatures. The affected machine used cloud block level High and controlled folder access; the unaffected machine with the same Defender engine and intelligence version used different policy settings. These are observed correlations, not a causal diagnosis. No relevant process-access trace was available to establish the interaction.

No Defender policy, exclusion, Process Lasso rule or quarantine state was changed. The affected copy was not restored or retried. This new behavioral detection is distinct from the historical Bearfoos detection of an older agent. Neither has a Microsoft false-positive verdict. A sanitized analysis report and exact public artifacts are prepared privately; no sample has been submitted.

## Remaining release gates

1. Resolve the new Defender classification and repeat acceptance under the affected machine's unchanged protection policy. Do not hide the failure by adding an exclusion or weakening policy.
2. Complete the coordinated helper/ledger rollout using [verified provenance and paired rollback](../windows-helper-approval.md); current canary success explicitly retains an already approved helper.
3. Obtain stable-release authorization after those gates pass. RC publication is not stable/fleet acceptance.
