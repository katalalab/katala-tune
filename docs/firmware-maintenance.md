# Firmware inventory and maintenance boundaries

## Existing capability map

| Surface | Existing responsibility | Boundary |
| --- | --- | --- |
| `collect`, `rules`, `health` | OS probe, performance findings, score and health | firmware control is not part of the score |
| `logs`, `db` | finite log collection, sanitized retention | not a firmware recovery backup |
| `inventory` | installed tool inventory | not an OEM support matrix |
| `netsec`, `network` | read-only security and connection checks | no network/security changes |
| `power`, `power_observation` | power information and observations | not firmware flashing readiness |
| `actions`, `engine` | allowlisted changes, validation, confirmation | new firmware writes are not allowlisted |
| `monitor`, schedules, `tune-agent` | sampling and scheduling | no automatic firmware probes or updates |
| `tune-core`, `tune-cli`, Tauri | Rust engine, CLI, application shell | CLI addition does not imply deployed GUI support |

## Added contract

`capabilities [ids]` reads manufacturer/model, board manufacturer/product, BIOS version/date and Secure Boot state. Empty IDs select only local nodes. `capabilities:false`, non-online states, unknown IDs and unsupported OS are rejected before transport. Local nodes run first and selected nodes run sequentially. There is no DB/helper/config write. Python runs in isolated mode (`-I`) using the existing PATH interpreter with a remote OS fallback; cwd and PYTHONPATH modules cannot shadow standard imports. The fixed script travels through stdin with a **client transport** bound of 35 seconds, including SSH connection setup (up to 10 seconds), and stdout/stderr bounds of 64/8 KiB. Windows has a separate owned-worker deadline: 17 seconds from supervisor start, a finite wait for termination acknowledgement until the 19.5-second deadline, and pipe completion grace up to 0.5 seconds within an absolute 20-second budget. If termination acknowledgement is absent, the result is cleanup_unconfirmed. The stdin write and flush together have a 2-second bound. The client deadline alone does not prove remote termination. SSH fixes StrictHostKeyChecking=yes, UpdateHostKeys=no, ClearAllForwardings=yes and PermitLocalCommand=no for this feature. Existing alias, authentication, identity and proxy routes remain in use; implicit host-key updates, configured forwards and LocalCommand execution are disabled. Only whitelisted report fields are emitted; raw command errors are discarded.

Windows runs its fixed read-only collector in an owned native Process child of the same PowerShell executable, using stdin and no helper files or jobs. It retains that Process association/handle for cleanup and never looks up a PID to terminate an unrelated process. Terminating this collector is lifecycle cleanup only; existing jobs and maintenance actions are unchanged. A `katala_capabilities_envelope.v1` reply distinguishes completed, timeout_cleanup_confirmed, cleanup_unconfirmed and worker_failed. Only completed reports with worker-exit and output-pipe acknowledgement can become successful inventory. Missing cleanup/pipe acknowledgement discards the report. Local transport timeout, overflow or failure leaves remote_completion/worker_exit unconfirmed; a local killed SSH process is not remote termination evidence. macOS retains its previous report contract and client timeout without this Windows lifecycle acknowledgement.

Windows uses explicit CIM properties and distinguishes Secure Boot enabled, disabled, permission denied, unsupported, missing cmdlet and other errors. macOS selects fields from system_profiler in memory; its raw hardware JSON never leaves the script. Secure Boot is not collected; RecoveryOS policy inspection and privileges have not been evaluated. Serial/UUID and account or network identity are excluded from output. system_profiler itself can read additional hardware fields internally; its stdout is captured only inside Python and never retained or forwarded.

`maintenance-plan [ids]` refreshes read-only evidence and returns disabled plans for firmware update, BIOS settings, Secure Boot, TPM, BitLocker, boot order and reboot. Evidence does not prove write support. All write paths need an exact model/firmware support decision, fresh evidence, OEM recovery instructions, physical access, power, backup, recovery-key possession confirmation without retrieving keys, a maintenance window, a fresh ledger read and application confirmation. The matrix includes per-action access, privilege, reboot uncertainty, recovery conditions and references to node/time/source/model/BIOS evidence. PC BIOS settings, TPM reset and BitLocker paths are platform-not-applicable on macOS. They remain disabled in this implementation regardless of those conditions. WSL is not treated as an independent firmware endpoint.

## Primary-source comparison (reviewed 2026-10-20)

| Option | Fit / license / maintenance / load | Decision |
| --- | --- | --- |
| Windows CIM and Secure Boot cmdlet | OS-supported read API, no new daemon or redistributed OEM tool | reuse native read calls; [BIOS fields](https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-bios), [Secure Boot result and errors](https://learn.microsoft.com/en-us/powershell/module/secureboot/confirm-securebootuefi) |
| fwupd / LVFS | maintained Linux service/CLI; component-specific licensing including LGPL-2.1-or-later; requires supported device and firmware metadata | Linux is outside current collector targets; no refresh/update invocation; [commands](https://fwupd.github.io/libfwupdplugin/fwupdmgr.html), [licenses](https://github.com/fwupd/fwupd/blob/main/COPYING) |
| osquery | cross-platform secureboot table, additional executable/daemon and broad inventory surface; Apache-2.0 OR GPL-2.0 | avoid extra deployment for a finite small inventory; [table](https://github.com/osquery/osquery/blob/master/specs/secureboot.table), [license](https://raw.githubusercontent.com/osquery/osquery/master/LICENSE) |
| Redfish | standardized BIOS attributes and registry, needs BMC presence, credentials and exact registry | no HTTP discovery; BMC support unverified; [schema](https://redfish.dmtf.org/schemas/DSP2046_2024.1.html) |
| Lenovo / Dell WMI tooling | OEM/model-specific interfaces and distribution terms | no proprietary tooling bundled; [Lenovo guide](https://docs.lenovocdrt.com/ref/bios/wmi/wmi_guide/), [Dell supported platforms](https://www.dell.com/support/manuals/en-us/command-configure/dellcommandconfigure_ig_4.7/supported-platforms?guid=guid-77646286-727c-4330-b491-bc2f0c4bfaff&lang=en-us) |
| Rust `uefi`, `wmi`, `sysinfo` | uefi (MIT OR Apache-2.0) is a boot-target API; wmi (MIT OR Apache-2.0) adds Windows COM dependencies; sysinfo (MIT) focuses on process/CPU/disk | no new crate needed; [uefi](https://docs.rs/uefi/latest/uefi/), [wmi license](https://docs.rs/crate/wmi/latest#license), [sysinfo license](https://raw.githubusercontent.com/GuillaumeGomez/sysinfo/master/Cargo.toml) |
| Apple firmware | authenticated model/device-specific OS update path | no generic PC flash support claim; [Apple security](https://support.apple.com/en-lamr/guide/security/secf683e0b36/web) |

Board-vendor recovery and update instructions are model-specific. A manual describing flash utilities does not prove unattended remote writes or a safe rollback. OEM support and the per-machine evidence matrix belong in private operational records, not this repository.

## Verification and rollback

Synthetic tests verify parser provenance, sanitization, transport bounds/shape, rejection and disabled plans. Hardware collection, privilege results and live platform compatibility require separate finite real-machine evidence. GUI integration, Windows packaged application behavior, firmware write execution, reboot and recovery are not claimed. This read-only addition makes no state change requiring machine rollback; reverting its code removes the feature.

## Review-2 verification boundary

The initial implementation at `4fc8bb5` had local/full-check and private hardware receipts. Those receipts do not verify the later SSH-option and owned-worker changes. The new regression suite evaluates the actual transport options with `ssh -G` against hostile fixture defaults without connecting, validates the lifecycle envelope, and provides native Windows worker-marker mocks including a 45-second Secure Boot sleep beyond the production 17-second deadline. Synthetic bounded results validate classification only; they do not demonstrate actual remote exit. New-head native SSH PID/start/parent-process absence receipts, source-hash matching, independent review and the full check remain pending until separately recorded by the integrating session. No firmware write, reboot, GUI acceptance or broader process cleanup is claimed.
