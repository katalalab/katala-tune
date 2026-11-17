# Optional hybrid collector canary

This experiment is **not** a new resident agent and is **not** the Fleet/Sites
wire schema. The existing `tune-agent sample` uses native macOS/Windows counters
and a bounded `nvidia-smi` fallback. Keep that path unchanged until comparative
measurements show a benefit. Fleet remains responsible for continuous monitoring.

## Scope and dependencies

`hybrid-canary` is an opt-in feature and one-shot example. It prints one JSON
object to stdout, accepts no arguments, writes no files, performs no networking,
does not enumerate processes, and never opens listeners, registers a service,
reads credentials or invokes GPU setters. No hostnames, usernames, addresses,
GPU serials, disk paths, or command lines are included.

- `sysinfo = 0.37.2`, MIT, MSRV 1.88, `system` feature only; default features and
  process refresh are disabled. The workspace keeps its Rust 1.90 declaration.
  Do not track current sysinfo main (which requires Rust 1.95).
- `nvml-wrapper = 0.11.0`, MIT/Apache-2.0, MSRV 1.60. Linux/Windows only. Dynamically
  loads the installed NVML driver library; requires no CUDA toolkit download.
  macOS reports `unsupported_platform`; this does not imply the Mac is offline.
- Versions and transitive dependencies are recorded in Cargo.lock. This feature
  is absent from default builds and does not replace the native sampler.

## Run locally, never as a service

```sh
cargo test -p tune-agent --example hybrid-canary --features hybrid-canary --locked
cargo build -p tune-agent --example hybrid-canary --features hybrid-canary --locked -j 2
node scripts/check-hybrid-canary.js target/debug/examples/hybrid-canary
```

On Windows, pass `target/debug/examples/hybrid-canary.exe`. The Node harness
bounds runtime to 10 seconds and output to 24 KiB; use it rather than an unbounded
scheduler. A driver call has no library-level hard deadline. A timeout is a
failed diagnostic, not evidence that the machine is offline. No background
process is intentionally retained. Build artifacts remain in this checkout.

CPU needs two observations, with at least max(250 ms, sysinfo minimum) between
them. Only CPU and RAM are refreshed. Host temperature is deliberately unknown:
WSL/container sensor absence must never be represented as zero degrees. A valid
CPU value of zero is distinct from missing CPU data. sysinfo does not expose
all underlying permission errors: `observed` means a structurally valid value,
not proof that every OS counter was accessible. Compare with the native probe.

All readings have `{value, status, reason}`. Unknown values are null with a
sanitized reason. RAM must have nonzero total and used <= total. NVIDIA device
queries preserve per-field failures, and lack of NVML does not drop valid host
measurements. At most 16 GPU devices are read. No hardware model is inferred.
Raw driver errors are not emitted because they may contain local paths.

## Canary and rollout gates

1. Run on one **personal** machine with existing workloads left running. Record
   version, elapsed time, sample validity, RSS/CPU separately from performance
   under load. No stress test or cache flush. Stop on errors or interference.
2. Compare existing native sample with this opt-in sample using matching time
   windows. Mismatched percentages from non-overlapping intervals are not bugs.
   Apple GPU remains unsupported; Windows/NVIDIA needs a separate real-machine
   check, including unsupported NVML fields, before GPU compatibility is claimed.
3. Independently review code, run the existing checks plus the optional feature
   tests on macOS/Linux/Windows and Rust 1.90. Do not weaken mandatory checks if
   CI is unavailable. No automatic deployment, merge, or release is added here.
4. Produce an explicit adapter for the separately versioned Fleet contract.
   Byte units must be converted to MiB, freshness validated, CPU missing kept
   null, and multi-GPU handling decided instead of silently choosing a device.
   An operator-provided personal machine ID must be validated by that adapter.
5. Stage one personal NVIDIA machine, then a second OS, then the remaining
   authorized personal machines only after passing the same gates. Do not touch
   company-managed machines, quarantine flags, network ACLs, drivers, power
   limits, or production keys. Keep existing executable as rollback.

Sites sending credentials, persistent telemetry, and authentication/network
changes are outside this canary. No synthetic fixture or local diagnostic is
sent to Sites. This change alone is not a fleet rollout.

## Sources

- https://docs.rs/crate/sysinfo/0.37.2
- https://docs.rs/crate/nvml-wrapper/0.11.0
- https://developer.nvidia.com/management-library-nvml
