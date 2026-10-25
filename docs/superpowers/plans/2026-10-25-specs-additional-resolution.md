# Additional Specifications Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Expand acquisition and official verification beyond the six-machine specs.7 baseline of 400 observations, 192 unverified identities and 13 missing fields, then update six CLIs and three existing console applications.

**Architecture:** Continue the user-approved Rust/native-provider/SSH collector and exact model or section-scoped public-ID catalog. Keep OS observations, official family/function evidence and current configuration distinct. Do not remove observations or infer a retail SKU from shared IDs.

**Tech Stack:** Rust, existing OS tools/APIs, Tauri, vanilla JavaScript, existing SQLite storage.

**Spec:** `docs/machine-specs.md`, continued by the user's explicit request to research, implement and deploy additional resolution.

## Global Constraints

- Read-only acquisition; no driver installation, firmware update, optimization or host setting change.
- No new dependencies without a demonstrated need. Never commit raw machine reports, instance IDs, serials, addresses, private paths or node assignments.
- Use primary manufacturer/OEM/OS documentation and official distribution packages. Retain complete-source hashes, exact claims, identity scope and unsuccessful acquisition evidence privately.
- Preserve unresolved records and provider failures. Derived causes cannot manufacture verification or acquisition success.
- Keep the existing shared-machine specs opt-in and all other policies intact.
- Run Cargo tests and application builds sequentially on Windows; test fixtures retain approved private helper artifacts.

## Review Focus

- Shared receiver/hub IDs must not become exact retail peripheral models.
- Parent/composite IDs must not create endpoint-specific performance or current-setting claims.
- A framework, OS or physical-interface version must not substitute for an unassociated logical-interface driver.
- A source that lists capabilities without observed-ID correspondence cannot establish installed identity.
- Remote reports rebuild evidence and causes locally; all newly collected fields require public-field validation.

## Task 1: Freeze and investigate the remaining baseline

- [x] Preserve the specs.7 baseline outside Git and reconcile all 192 identities and 13 fields by cause, public identifier and section.
- [x] Investigate USB/receiver/hub/audio mappings through additional official technical specifications, support documents and archive-only driver sources.
- [x] Investigate remaining PCI/NVMe/network mappings and Windows baseboard/logical-NIC acquisition through independent official records and bounded read-only native APIs.
- [x] Investigate Mac panel and awdl fields through narrowly selected native associations; do not copy brand or physical-driver values.

## Task 2: Implement only evidenced additions

Root acquisition addition: Windows PnP `DEVPKEY_Device_BusReportedDeviceDesc` returned public product descriptions for 67 of 68 local USB records. Add a finite eight-second/128-record provider using exact private instance associations. Normalize `reported_model` only when the description exactly matches a trusted catalog model alias, never retaining unknown strings that could contain private serials. Preserve the original OS model. Only USB may use this alias as a fallback after existing public-ID matching; storage child names cannot inherit an enclosure's model. Required regressions cover unique/mismatched/duplicate IDs, unknown private-looking strings, driver-provider independence, provider failures, remote revalidation and unchanged internal-disk identity.

- [x] Write meaningful synthetic positive, wrong-vendor/section and ambiguity regressions; demonstrate failure before catalog/provider changes.
- [x] Add supported identity records or native acquisition fields with explicit scope and evidence. Keep main catalog integration under one writer.
- [x] Update cause classifications and renderer labels/material display only where actual behavior or newly available evidence requires it.
- [x] Document remaining causes and exact alternative routes tried.

## Task 3: Verify and deliver

- [x] Obtain independent source/contract review and resolve important findings.
- [x] Synchronize version metadata, run focused and full checks, commit immutable runtime source and scan HEAD/history.
- [x] Build Windows after checks and Mac from the immutable archive. Validate artifact/source hashes and signatures.
- [x] Back up and update six CLIs, the existing Windows application and both existing Mac applications without changing ledgers/login items.
- [x] Collect sequentially from all three consoles, reconcile baseline/current results, verify actual UI and remove temporary Windows debugging.
- [x] Record acceptance evidence, final history scan and clean Git state.
