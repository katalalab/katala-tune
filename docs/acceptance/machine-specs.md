# Machine specifications acceptance — 0.6.5-specs.2

## Scope and evidence

- Rust collectors built and executed on Windows x86_64 and macOS arm64.
- Existing private SSH ledger: five eligible machines (two Macs, three Windows PCs) collected successfully; one shared machine excluded. All returned `katala_specs.v1` and collector `0.6.5-specs.2`. Private ledger, reports, screenshots and deployment hashes are kept outside this repository.
- Windows: physical memory module model/slot/speed, motherboard, firmware, drivers, storage, peripheral hardware, display EDID model/manufacturer (without serial fields), and NVIDIA dedicated memory larger than 4GiB observed.
- macOS: M2 Pro and M5 Max, unified-memory bytes, firmware, display, Wi-Fi, Thunderbolt and power settings observed. M5 Super/Performance cluster names retained. Intel Mac numeric total memory covered by a fixture.
- Real installed Tauri WebView, using actual IPC and private DB: five successful reports, one excluded shared machine, 145 comparison rows, 131 difference rows in the sampled inventory; search and difference toggles exercised. A fresh Windows collection through `specs_refresh` succeeded. Official links present, no page errors. Desktop, dark and narrow layouts rendered and inspected.
- Local Windows build installed side by side, Start Menu shortcut updated with the prior shortcut backed up. Original application files retained. Updated CLI deployed to the four eligible remote machines with backups; local helpers synchronize from the new application. Collection itself does not install anything.

## Automated checks

- `cargo test -p tune-core --test specs --test version_inventory --locked`: 14 specs and 3 version-inventory tests passed, Windows and macOS.
- CLI discovery tests: 8 passed, Windows and macOS. Help/version require no config or DB; invalid requests do not initialize state; saved specs preserve the database bytes; version audits signal failure through their exit code while retaining JSON output.
- `node --test test/specs.test.js`: 7 passed.
- Full JavaScript suite: 450 tests, 437 passed, 13 platform skips, zero failures.
- Workspace Clippy (`--all-targets --locked -- -D warnings`) and Rust formatting checked.
- OSS working-tree/HEAD and reachable-history scans checked; personal reports and deployment material excluded from source control.
- Final `npm run check` passed: 437 JavaScript tests passed / 13 platform skips, 624 Rust tests passed / 4 ignored, including doc tests. Core unit tests: 291 passed / 2 ignored. Formatting, workspace Clippy and OSS scan passed within that command.
- The previous deterministic shared-metadata fixture failure was reproduced with TEMP/TMP redirected to another volume. Its same-bytes copied control now resides beside the actual test executable and checks the original artifact volume, instead of assuming the system drive. Process ownership, lease, exit-policy and executable-identity assertions remain intact. Focused and full checks passed.
- An initial follow-up full run also encountered an intermittent existing native-reader integration failure (early exit 2, no output). Its focused replay passed all three native roles; the next complete check passed all seven native-entry tests. Its cause is not established; no retry, skip or weakened assertion was added to the tests or product.

## CLI and version management

`tune --version`, `tune version --json`, `tune --help`, per-command help and `tune cli-info --json` expose compatible discovery information. `tune versions` audited all five eligible machines with zero mismatches or failures; SHA-256 values matched the deployed platform artifacts. `tune specs --saved` exported the actual stored reports without collecting. Versions are synchronized through `scripts/version.js` across Cargo, npm and Tauri metadata.

Both macOS arm64 application bundles were built from committed source revision `2821baf13b9c58734f44ad9462c138b3f7400657` and replaced at their existing application paths. App version, strict signature validation, running executable path, bundled/standalone CLI hashes, backup/live SQLite `quick_check`, and unchanged login items were verified. Application, configuration, database and standalone CLI backups remain private. One Mac's stale local-host and SSH-alias transport fields required a separately backed-up correction; shared-machine and scheduling settings were preserved.

## Review corrections

Independent review identified four issues, each reproduced before its fix: static Mac coverage notes promoting a wholly failed collection to success; failed constituent providers disappearing from aggregate Mac sections; device/value associations lost when comparing multiple GPUs; cached backend-running state disabling manual reload. Regression tests cover all four. The review's Intel memory normalization minor was also implemented and tested. No deferred review findings remain.

## Limits

The official catalog is finite, versioned and sourced. Registered models are matched exactly, including explicit family/variant notes. Unregistered parts stay unverified. Standard OS providers cannot expose every firmware setting, PSU/cooling/cable model or peripheral firmware version. XMP/EXPO state and macOS Recovery policy are explicitly unavailable, rather than inferred. NVIDIA boards with duplicate reported model names retain unknown dedicated memory when the CIM-to-driver association cannot be established uniquely. No host setting, driver or firmware is changed by collecting specifications.
