# Machine Specification Comparison Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for this approved implementation in the current session.

**Goal:** Collect detailed Mac/Windows machine specifications in Rust and compare them in Katala Tune.

**Architecture:** `tune-core::specs` supplies finite local collection, normalization, catalog matching and SSH transport. Engine methods persist private reports in meta. Tauri exposes reports to a vanilla JS comparison view.

**Tech Stack:** Rust, tokio, serde_json, existing SQLite meta, Tauri, vanilla JavaScript.

**Spec:** `docs/superpowers/specs/2026-10-25-machine-specs-design.md`

## Global Constraints
- Read-only collection; no automatic remote installation or host-setting changes.
- No new dependencies or personal machine data in tracked files.
- Exact official model matches only; unknown fields remain unknown.
- Keep existing successful reports if refresh fails.

## Review Focus
- Partial provider errors and empty devices remain distinguishable.
- No secrets/identifiers survive provider normalization.
- Malicious remote output cannot become markup or executable commands.
- A navigation change or failed refresh cannot erase last successful data.
- Old remote CLI versions return an actionable error without a fallback to script collectors.

### Task 1: Rust report contract and collectors
**Files:** `crates/tune-core/src/specs/{mod.rs,windows.rs,macos.rs,catalog.rs}`, `crates/tune-core/tests/specs.rs`, `data/specs-catalog.json`, `crates/tune-cli/src/main.rs`.
**Interfaces:** `collect_local().await -> Value`, `normalize(platform, raw) -> Value`, `collect_node(&Node).await -> Value`, `select(&[Node], &[String]) -> Result<Vec<Node>,String>`; schema `katala_specs.v1`.
- [x] Write normalization tests for privacy, partial failures, units, exact catalog matching and invalid reports; run to observe missing module failure.
- [x] Implement bounded native-provider collection and fixed SSH command; add `tune specs-local` before CLI configuration initialization.
- [x] Run `cargo test -p tune-core --test specs --locked`, collect Windows/Mac reports and verify schema/privacy/source/missing fields.

### Task 2: Private aggregation and Tauri bridge
**Files:** `crates/tune-core/src/engine_specs.rs`, Engine state, Tauri commands/permissions/bridge, CLI.
**Interfaces:** `Engine::specs_view() -> Result<Value,String>`, `Engine::specs_refresh(ids).await -> Result<Value,String>`; latest report and last attempt in meta.
- [x] Add tests that an unsuccessful attempt retains a successful report and unknown/paused/shared nodes are refused before transport.
- [x] Implement persistence, single-refresh lock, validated results and Tauri `specs_view` / `specs_refresh`; add `tune specs [node...]` for aggregation.
- [x] Run focused Rust tests and Tauri permission contract checks. Expected: all pass.

### Task 3: Comparison UI and delivery
**Files:** `renderer/specs.js`, `renderer/specs.css`, `renderer/index.html`, `test/specs.test.js`, README and feature documentation.
**Interfaces:** `window.tune.specsView()`, `window.tune.specsRefresh(ids)`; `KT_VIEWS` registration.
- [x] Write renderer tests for escaping, differences, missing values, unified/dedicated memory, stale navigation and failure rendering; observe failure before implementation.
- [x] Implement selection, refresh, copy JSON, search and comparison/details using existing UI conventions.
- [x] Run `npm run check`, `npm run oss-check -- --history`, actual Mac/Windows collection and rendered UI checks. Expected: pass, or record environmental/baseline limitations precisely.
- [x] Review full diff, build requested update and provide reviewable artifacts with remaining limitations.

