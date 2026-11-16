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
nonexempt path is checked under both policies (at most two scans per blob).
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
The independent Linux GTK/glib advisory is outside this backport.
