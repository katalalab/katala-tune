# Fixed read-only inventory. Never forward raw system_profiler output.
import json
import subprocess

report = {"schema": "katala_capabilities.v1", "platform": "darwin",
          "inventory_status": "error", "inventory_reason": "error", "secure_boot": {"state": "not_collected"}}
try:
    result = subprocess.run(["/usr/sbin/system_profiler", "SPHardwareDataType", "-json"],
                            capture_output=True, timeout=25, check=True)
    hardware = json.loads(result.stdout)["SPHardwareDataType"][0]
    report.update({"manufacturer": "Apple", "model": hardware.get("machine_model"),
                   "bios_version": hardware.get("boot_rom_version"), "inventory_status": "ok", "inventory_reason": None})
except (OSError, subprocess.SubprocessError, ValueError, KeyError, IndexError, TypeError):
    pass
print(json.dumps(report, ensure_ascii=True))
