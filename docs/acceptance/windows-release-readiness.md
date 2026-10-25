# Windows release readiness — 2026-10-25

Runtime candidate: `ba01c81811471ba1198deef10d500bf0dca0d19e`, version `0.7.0-rc.1`, [integration PR #83](https://github.com/katalalab/katala-tune/pull/83). [CI run 40165177157](https://github.com/katalalab/katala-tune/actions/runs/40165177157) succeeded on all six Windows/macOS/Linux JS/Rust jobs, including the actual embedded Windows collector. Private machine names, inventories, raw security history and screenshots are kept outside the repository.

## Reproduced fixes

- Windows inherited environment contains legitimate `=C:` drive entries. Rejecting them prevented the native process owner and receiver from starting. Only the exact drive-key form is admitted; malformed keys, embedded NUL and duplicate names remain rejected.
- The UTF-8 BOM required for a PowerShell 5.1 file became an executable token when decoded into `ScriptBlock::Create`. Strip the file encoding marker at that boundary, preserving the stored file's BOM. A BOM/no-BOM counterfactual and an actual embedded snapshot test cover the failure that synthetic payloads missed.
- GPU relationship annotation used the reserved `$PID` parameter name. Native NVIDIA LF/CRLF output retained a terminal blank row and failed legitimate parsing. Native regressions exercise both fixes and retain rejection of nonblank malformed output.
- Nested network/WSL capture now concurrently caps stdout/stderr, bounds stdin/root exit/both EOFs with one monotonic deadline, and rejects incomplete output. Ten native cases include byte boundaries, overflow, UTF-16, exit failure, timeout and a descendant holding EOF.
- QA collection uses its isolated `data/bin`; local GPU actions use the approved finite KT2 receiver. Windows app sync preserves an existing helper rather than silently invalidating its approved hash.
- Test controls now reach the intended native exit, and classification/READY tests distinguish protocol behavior from interpreter startup scheduling. No production deadline was relaxed.

## Verification already observed

| Surface | Evidence |
| --- | --- |
| macOS | Full `npm run check`: JS 453 pass / 21 platform skips; Rust 608 pass / 5 ignored owned-worker or benchmark fixtures; fmt/clippy/version/OSS checks pass |
| Windows | JS 461 pass / 13 platform skips; final workspace clippy and Rust 663 pass / 4 ignored fixtures; actual embedded snapshot succeeds without creating application state |
| Electron compatibility | JS 452 pass / 22 platform skips under Electron's bundled Node; SQLite/specification contracts preserved |
| Discovery/specifications | All 5 original PR #82 discovery regressions retained, 13 discovery tests total; successful specifications survive failed attempts; shared-machine collection requires opt-in |
| Actual WebView2 | Two real machines, 119 comparison rows / 85 differences; VRAM search, source links, shared exclusion, desktop/narrow/dark views; no page errors during that interaction flow |
| Confirmation/actions | Actual native dialog close returns cancelled, leaves the owned fixture alive and audit unchanged. An approved dedicated fixture operation with unknown compute activity returns exit 9, leaves the fixture alive and records the refusal |
| Process lifetime | Fresh live session and new sample received; stop acknowledged; no owned agent remains. Real SSH fault injection returns controller code 7 after commit; root/descendant identities are gone and delayed writes absent after the receiver budget |
| Native release sidecar | Exact local release CLI from this runtime commit collected network-enabled snapshots on three Windows machines in about 17–30 seconds; no application state was initialized |
| NSIS QA package | Release-profile, separate QA product/identifier installed successfully; installed payload matches extraction; x64 GUI/console types and Common Controls v6 manifest verified; ordinary app processes unchanged |

The NSIS GUI's expected `__TAURI_BUNDLE_TYPE_VAR_UNK` → `..._NSS` patch is the only difference from the raw GUI build; sidecar bytes are identical. This is checked rather than treating raw and packaged GUI hashes as interchangeable. QA package verification is not production signing or updater acceptance.

The Windows CI helper used for controller acceptance is artifact `12587942295` from run `40165177157`, attempt 1. The downloaded ZIP digest was checked against the GitHub API before extraction. Manifest/binary hashes, successful run, PR head/base, merge tree and canonical Cargo.lock were independently compared. Its v2 record is only installed in private QA data/ledger paths; local release sidecar tests are explicitly recorded as local builds, not mislabelled as CI artifacts.

## Defender findings and limit

The historical incident includes separate binary ML, AMSI-script and PowerShell-behavior detections. The published v0.6.4 installer matches its published checksum; the extracted agent matches the historical detected bytes. On the investigation date those unchanged artifacts scan clean with protection enabled, including on the affected machine. This establishes the artifact link and the current scan result, not why Microsoft's earlier classifier fired.

The corrected candidate CLI, GUI, agent and QA NSIS package scan clean. Network-enabled collection on all three Windows canaries completes with realtime protection enabled and no new detections. Signatures observed during these checks include `1.461.188.0` and `1.461.186.0`. No exclusions, protection disablement, quarantine restoration, or sample upload was performed. A Microsoft verdict remains unknown; no false-positive claim is made.

## Distribution gate

This branch prepares a release candidate; it has not tagged, published, replaced the public update feed, or rolled out into ordinary app data. Before formal release:

1. Obtain authorization for the main integration that triggers public RC distribution. The current prerelease version does not become `latest`.
2. Verify the production-identity CI release assets, updater signatures/signed version/checksums, and actual RC installation/update path. Existing fixture tests reject tampering, wrong keys, wrong origins and version relabelling; they do not substitute for this exact signed RC.
3. Coordinate the Windows helper and ledger rollout while affected apps/monitoring are stopped, canary first, with the paired rollback in [Windows helper approval](../windows-helper-approval.md).
4. Promote to a stable version only after signed RC acceptance. Do not infer permission to publish stable from a successful unsigned QA package.
