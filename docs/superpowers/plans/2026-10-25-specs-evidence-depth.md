# Deeper specifications evidence implementation plan

> **For agentic workers:** Use superpowers:executing-plans or independent delegated tasks. Keep each native collector and the catalog under separate ownership.

**Goal:** Expand the remaining observable inventory and replace excerpt-only manufacturer evidence with verified full documents wherever accessible.

**Architecture:** Continue the approved Rust collectors, finite trusted catalog, private SSH ledger and existing comparison view. Native device identifiers may be used transiently to associate records; only public product IDs and explicitly allowed metadata cross the normalized report boundary. Official capabilities, hardware identity, observed settings and software/aggregate records remain distinct.

**Tech stack:** Existing Rust/Tauri, built-in OS providers, vanilla JavaScript and existing PDF tooling. No new runtime dependency.

**Design basis:** Existing machine-specifications design and the accepted coverage-expansion plan; this is an extension of that authorized design.

## Constraints and review focus

- All collection remains read-only; shared-machine authorization covers specifications only.
- Never publish machine/user names, serials, addresses, SSIDs, instance IDs or private paths in source or fixtures.
- No fuzzy model/SKU matching, inferred memory kits, or current-connection claims based on cached EDID.
- Never upgrade source access from indexed excerpt to full document without obtaining and checking the body. Record source URL, scope and verification evidence; inaccessible sources retain their limitations.
- Empty grouping nodes and software endpoints must not be counted as missing physical product identities. This does not remove actual observations or hide their source.

## Tasks

- [x] Refresh all six reports and group remaining gaps by native source and identity specificity.
- [x] Obtain accessible manufacturer bodies, verify claims and retain hashes/access evidence privately. EIZO, RME and Intel bodies were checked; Crucial's original/regional/current routes remained blocked and their limits were preserved. Research additional exact identities using manufacturer public-ID tables.
- [x] Add failing native Windows regressions, then implement private device-instance association for drivers, public PCI/USB identity details and additional supported metadata. Verify exact native property names against actual providers.
- [x] Add failing macOS regressions, then improve grouping/aggregate handling and exposed identity/configuration properties. Retain non-public fields as unavailable.
- [x] Add trusted source-evidence and applicability handling at normalization/remote validation boundaries, with escaped and source-aware comparison details.
- [x] Run focused regressions and independent review, then the complete repository checks. Synchronize versions and build immutable platform artifacts.
- [x] Back up and update six CLIs plus the two Mac applications and Windows console app, collect fresh reports from all three consoles, inspect actual UI and record remaining limitations.
