# Fleet Studio Search Input Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing read-only search usable with Japanese IME and Unicode selection without confusing search confirmation with link activation. Explicit paste is reported as unavailable until a bounded implementation can be demonstrated.

**Architecture:** Keep a bounded, testable Unicode editing state independent of GPUI and data sources. A small GPUI input adapter translates UTF-16 and layout coordinates, and the existing view remains responsible for filtering and navigation. No clipboard access is performed.

**Tech Stack:** Rust 1.99, pinned GPUI 0.2.2, Windows and macOS adapters, pinned unicode-segmentation 1.13.3 already present in the main workspace lockfile.

**Spec:** User's Fleet Studio daily-search request and the parent task instructions for this branch.

## Global Constraints

- Preserve sample/live separation, read-only data access, current navigation protections, and the 256-byte search limit.
- Do not read the operator's clipboard during development or change OS input settings.
- Do not emit search or clipboard contents, session data, or private paths in logs or fixtures.
- Bound per-operation input; distinguish composing, committed, cancelled, invalid and oversized text.
- Keep the UI responsive and report paste as unavailable; do not read the clipboard.
- Finish with tests, GUI build, public-data checks, independent review, and a local commit only.

## Review Focus

- UTF-16 surrogate interiors and out-of-range selections must never split UTF-8 or panic.
- IME updates after a nonempty prefix must interpret selection offsets relative to the inserted text.
- IME cancellation restores replaced selection and does not clear unrelated search text.
- Grapheme movement, deletion, selection, emoji, combining characters and shaped ligatures stay coherent.
- IME and keyboard events must not open a link or bypass the byte limit.

### Task 1: Test and implement the bounded input state

**Files:** `crates/fleet-studio/src/search_input.rs`, `crates/fleet-studio/src/lib.rs`, `crates/fleet-studio/tests/search_input.rs`, `crates/fleet-studio/Cargo.toml`, lockfile.

- [x] Write failing tests for UTF-16 ranges, surrogate interiors, emoji, combining graphemes, marked selection, cancellation, replacement and over-limit input.
- [x] Run the tests and verify failure before implementation.
- [x] Implement the smallest state API with stable byte-boundary and selection invariants, rejection on invalid/oversize input, and no I/O.
- [x] Run the revised tests and verify they pass.

### Task 2: Connect native text events and geometry

**Files:** `crates/fleet-studio/src/view.rs`, `crates/fleet-studio/src/search_view.rs`, `crates/fleet-studio/src/main.rs`.

- [x] Test input event routing and composition Enter/Escape before implementing; real OS input and focus ordering require separate verification.
- [x] Register GPUI's input handler only while editing; let the platform deliver text and IME events.
- [x] Render selection, composition underline, horizontally scrolled caret and hit testing from the same shaped line; handle invalid geometry.
- [x] Route Ctrl on Windows and Command on macOS; explicitly report paste unavailable without reading the clipboard.
- [x] Run the Fleet Studio regression tests, including sample/live boundaries; Windows IME basic conversion, confirmation and cancellation were checked on sample data. Focus loss, input limits, mouse down/up across redraw, and complete OS event ordering remain unverified.

### Task 3: Verify and review

**Files:** `crates/fleet-studio/README.md`, tests and this plan.

- [x] Document input, actual platform limitations, build and verification without private data.
- [x] Run formatting, Fleet Studio tests, GUI Clippy, Windows GUI build, and public-data checks on the change and history. Recheck after committing.
- [x] Request independent review; fix actionable findings and rerun affected checks.
- [x] Hand the built sample to the parent: ASCII search, selection, cursor edits, confirmation, sample links, and basic Japanese IME conversion, confirmation and cancellation were checked on Windows. macOS remains unverified.
- [x] Prepare the change for a local-only commit; do not push, create a PR, or merge.
