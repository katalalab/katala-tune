# Machine specifications acceptance — 0.6.5-specs.1

## Scope and evidence

- Rust collectors built and executed on Windows x86_64 and macOS arm64.
- Existing private SSH ledger: five eligible machines (two Macs, three Windows PCs) collected successfully; one shared machine excluded. All returned `katala_specs.v1` and collector `0.6.5-specs.1`. Private ledger, reports, screenshots and deployment hashes are kept outside this repository.
- Windows: physical memory module model/slot/speed, motherboard, firmware, drivers, storage, peripheral hardware, display EDID model/manufacturer (without serial fields), and NVIDIA dedicated memory larger than 4GiB observed.
- macOS: M2 Pro and M5 Max, unified-memory bytes, firmware, display, Wi-Fi, Thunderbolt and power settings observed. M5 Super/Performance cluster names retained. Intel Mac numeric total memory covered by a fixture.
- Real installed Tauri WebView, using actual IPC and private DB: five successful reports, one excluded shared machine, 145 comparison rows, 131 difference rows in the sampled inventory; search and difference toggles exercised. A fresh Windows collection through `specs_refresh` succeeded. Official links present, no page errors. Desktop, dark and narrow layouts rendered and inspected.
- Local Windows build installed side by side, Start Menu shortcut updated with the prior shortcut backed up. Original application files retained. Updated CLI deployed to the four eligible remote machines with backups; local helpers synchronize from the new application. Collection itself does not install anything.

## Automated checks

- `cargo test -p tune-core --test specs --locked`: 13 passed, Windows and macOS.
- `node --test test/specs.test.js`: 7 passed.
- Full JavaScript suite: 450 tests, 437 passed, 13 platform skips, zero failures.
- Workspace Clippy (`--all-targets --locked -- -D warnings`) and Rust formatting checked.
- OSS working-tree/HEAD and reachable-history scans checked; personal reports and deployment material excluded from source control.
- `npm run check` does **not** fully pass on this Windows environment: existing `collect::windows::tests::shared_metadata_keeps_one_lease_and_rejects_another_same_bytes_program` fails its isolated subprocess fixture. The pre-change core baseline also failed this fixture (and initially one native-reader diagnostic). Later full runs have 290 passing core unit tests, this one failure and two ignored tests. This feature does not alter that fixture or its process implementation. Do not describe the complete repository suite as green.

## Review corrections

Independent review identified four issues, each reproduced before its fix: static Mac coverage notes promoting a wholly failed collection to success; failed constituent providers disappearing from aggregate Mac sections; device/value associations lost when comparing multiple GPUs; cached backend-running state disabling manual reload. Regression tests cover all four. The review's Intel memory normalization minor was also implemented and tested. No deferred review findings remain.

## Limits

The official catalog is finite, versioned and sourced. Registered models are matched exactly, including explicit family/variant notes. Unregistered parts stay unverified. Standard OS providers cannot expose every firmware setting, PSU/cooling/cable model or peripheral firmware version. XMP/EXPO state and macOS Recovery policy are explicitly unavailable, rather than inferred. NVIDIA boards with duplicate reported model names retain unknown dedicated memory when the CIM-to-driver association cannot be established uniquely. No host setting, driver or firmware is changed by collecting specifications.
