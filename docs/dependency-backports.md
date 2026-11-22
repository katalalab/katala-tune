# Dependency backports

## Taffy 0.9.0 → registry grid 1.0.1

GPUI 0.2.2 requires Taffy `=0.9.0`. That release requires grid `^0.18.0`,
which cannot select the fix for [GHSA-38c5-483c-4qqp](https://github.com/advisories/GHSA-38c5-483c-4qqp).
The workspace patches Taffy with its genuine 0.9.0 source and changes only the
normalized manifest's grid requirement to `^1.0.1`. The package remains Taffy
0.9.0; grid remains the real crates.io package. No GPUI or Taffy Rust code is changed.

This follows [upstream PR #864](https://github.com/DioxusLabs/taffy/pull/864.diff),
which changed the grid dependency from 0.18.0 to 1.0.0 without Rust changes.
Our lower bound is 1.0.1 because 1.0.0 is also affected. This does not include
other Taffy 0.9.2 layout changes. Remove the patch when the selected upstream GPUI
permits a Taffy release that requires fixed grid, after native layout validation.

### Provenance

The source is the [official Taffy 0.9.0 archive](https://crates.io/api/v1/crates/taffy/0.9.0/download).
Its SHA-256, verified against the [official version metadata](https://crates.io/api/v1/crates/taffy/0.9.0), is
`a13e5d13f79d558b5d353a98072ca8ca0e99da429467804de959aa8c83c9a004`.
The archive records upstream commit `599b1b7d7410d97e13bafc4c01fbe6d7a9cf060a`
in `.cargo_vcs_info.json`. All 57 archived Rust files, examples, README,
`Cargo.toml.orig`, and VCS metadata are preserved byte for byte.
The archive's generated `Cargo.lock` is omitted: this workspace owns dependency
resolution, and the archive lock still describes the unpatched graph.
The only changed archive file is `Cargo.toml`, one grid version line.

The archive does not contain a license file. `vendor/taffy-0.9.0/LICENSE.md` is
[the unmodified MIT license from that exact upstream commit](https://raw.githubusercontent.com/DioxusLabs/taffy/599b1b7d7410d97e13bafc4c01fbe6d7a9cf060a/LICENSE.md),
SHA-256 `a7083ef3dd41a65eeb2162c2c091b0082e2724fb4c6ed029b363c7840d244ecd`.
It retains contributor ownership and the original Stretch/Visly notice.

The selected registry grid 1.0.1 archive checksum is
`b40ca9252762c466af32d0b1002e91e4e1bc5398f77455e55474deb466355ff5`,
verified against [official grid metadata](https://crates.io/api/v1/crates/grid/1.0.1)
and retained in the workspace lock. No other package version changes in this slice.

### Privacy scanning

The upstream authors' public email metadata is retained. The OSS scanner skips
only its email heuristic for the exact paths `vendor/taffy-0.9.0/Cargo.toml` and
`vendor/taffy-0.9.0/Cargo.toml.orig`, consistently for HEAD and history scans.
Other patterns and ledger terms remain active, as tested by the focused scanner
regressions. Other vendor files and application files retain email scanning. Blob deduplication
includes the effective scan policy, so identical content at an exempt and a
nonexempt path is checked under each policy (at most three scans per blob,
including the separate GLib comment policy below).
The cost is that future email metadata in these two manifests is not checked by
that heuristic; source provenance review and private-ledger scanning remain required.

### Validation and remaining gates

Use owned copies when executing upstream dependency tests; do not edit Cargo's
shared registry sources. Taffy's existing `compute::grid` unit tests include grid
placement and occupancy assertions. grid 1.0.1's existing `overflow` regressions
must pass with both `cargo test --lib` and `cargo test --lib --release` filters.
The equivalent old-grid growth tests demonstrate failure with optimization;
debug overflow checks alone do not reproduce it.

`cargo tree --locked -p tune-gpui --target <triple> -i grid` resolves registry
1.0.1 through the patched Taffy on `aarch64-apple-darwin` and
`x86_64-pc-windows-msvc`. The Linux GPUI package has no grid edge.
Native Windows CI/artifact verification and actual Mac/Windows layout, resize,
and scroll checks are release gates; a target dependency tree is not a native build.
The independent Linux GTK/GLib repair is described separately below.

## GLib 0.18.5 VariantStrIter out-pointer

The Linux Tauri GTK 0.18 graph still requires GLib 0.18. The workspace patches
the genuine GLib 0.18.5 binding for
[GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g),
without changing its version or GTK ABI. In `src/variant_iter.rs`, the only
changes are `let p` → `let mut p` and `&p` → `&mut p`, exactly as in
[upstream PR #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343.diff).
The C call writes through this output-pointer argument; the returned `&s`
string remains borrowed. See the official
[GLib pointer contract](https://docs.gtk.org/glib/gvariant-format-strings.html#pointers).

### Provenance and license

All 121 files from the
[official GLib 0.18.5 archive](https://crates.io/api/v1/crates/glib/0.18.5/download)
are retained, including both manifests, README, VCS metadata, LICENSE, and
COPYRIGHT. Its SHA-256, also recorded in the original registry lock, is
`233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.
The archive records upstream commit
`42b9caf98e03ded086362d9653ca58fe94dc8658`, path `glib`.
Only the two lines above differ from the archive; the other 120 files,
including the other 107 Rust files, are unchanged byte for byte.

The original `src/variant_iter.rs` SHA-256 is
`1fd02859333761c45321b32f28b24233446b97d0022a90d3a937ed162585b90e`.
The unchanged LICENSE SHA-256 is
`8cf56d10131ce201cf69ab74b111d3ebac1acca3833d7efb39ae357224b70edb`;
COPYRIGHT is
`dae402989de65164815b7e2b6bc2b9576285434c3785934c8b6ece0fa055960d`.
The Rust binding is MIT licensed. Native GNOME libraries retain their own
licenses; this backport does not relicense them.

The OSS scanner exempts only `home-path` findings on the two exact original
comment lines about public Windows CI path shortening in
`vendor/glib-0.18.5/src/translate.rs`. Leading indentation may vary; any changed
wording, suffix, or path remains checked. Other rules and all supplied ledger
terms stay active, including on those lines. This preserves the canonical
upstream comments at the cost of no longer reporting those two public path
examples under that one heuristic. The distinct policy participates in blob
deduplication, so identical content at another path remains checked in HEAD
and history scans. No GLib email exception is needed.

On GitHub Actions Windows only (`win32` and `GITHUB_ACTIONS` exactly `true`),
the known public hosted-runner account is classified as public runtime metadata
when adding the current OS username to scan terms. Matching is case-insensitive.
The cost is that this one automatically supplied CI username is no longer a
private term in that context. Explicit node/fleet ledger terms, even the same
name, and the hostname remain checked everywhere, including comments and
commit messages. Other usernames and contexts retain their existing behavior;
the exact-comment home-path policy above is unchanged.

### Reproducible regression boundary

The root workspace lock selects `vendor/glib-0.18.5` for the Linux Tauri graph.
`test/glib-variant-regression` is an excluded, test-only workspace with its own
committed lock and an exact 0.18.5 path dev-dependency to that same directory.
Its lock was seeded from the existing root lock and pruned offline; shared
registry packages retain the same versions, sources, and checksums. The vendor
archive contains no lock, and no vendor lock is added.

On native Linux, run:

```sh
cargo fmt --manifest-path test/glib-variant-regression/Cargo.toml --check
cargo test --manifest-path test/glib-variant-regression/Cargo.toml --locked --test variant_str_iter
cargo test --manifest-path test/glib-variant-regression/Cargo.toml --locked --release --verbose --test variant_str_iter
cargo check --locked -p katala-tune --all-targets
```

The three tests cover empty, ordinary (including an empty string), and Unicode
arrays through the actual `Variant::array_from_iter` / `array_iter_str` API.
Each checks all string values in forward, reverse, and mixed directions, plus
length, size hints, and repeated exhaustion. No GTK initialization or display
is needed. The harness rejects non-Linux compilation, rather than reporting an
empty test suite as native validation.

The harness debug profile is unoptimized with debug assertions. Its release
profile applies optimization level 3, LTO off, debug assertions off, and
overflow checks off to GLib as well as the tests. Linux CI records native
library versions and runs both profiles in a step-scoped harness target
directory, then separately compile-checks Tauri with the normal root target.
Existing workspace checks remain required. The Tauri check does not prove GUI
execution or release linking.

### Maintenance and remaining gates

The old immutable-out-pointer code is structural defect evidence against the
official C contract and upstream repair. Do not execute known undefined
behavior merely to force a failing test. Native Linux debug and optimized
results must establish the patched regression gate; Mac dependency resolution
or formatting checks cannot establish it.

Version-based advisories may still report genuine 0.18.5. No version spoofing,
advisory dismissal, or blanket vendor scanner exception is used. Remove this
patch when the compatible Tauri/GTK graph selects an official fixed GLib,
after rerunning these native regressions and the Tauri Linux compile gate.
