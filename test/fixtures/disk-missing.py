"""Exercise the real Disk code with only its OS/CF boundary replaced; no OS probes."""
import ast
import ctypes as C
import json
import math
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
tree = ast.parse((ROOT / "probes/live_mac.py").read_text(encoding="utf-8"))
nodes = [n for n in tree.body if isinstance(n, (ast.ClassDef, ast.FunctionDef)) and n.name in ("Disk", "disk_rate", "main", "init")]
ns = {"C": C, "math": math}
exec(compile(ast.Module(body=nodes, type_ignores=[]), "live_mac_disk", "exec"), ns)
Disk = ns["Disk"]


class IO:
    def __init__(self, rows, failed=False, valid=True):
        self.rows = rows
        self.failed = failed
        self.valid = valid
        self.iterator = iter(())

    def IOServiceMatching(self, name):
        assert name == b"IOBlockStorageDriver"
        return 1

    def IOServiceGetMatchingServices(self, port, matching, out):
        self.iterator = iter(range(1, len(self.rows) + 1))
        out._obj.value = 1
        return 1 if self.failed else 0

    def IOIteratorNext(self, _):
        return next(self.iterator, 0)

    def IOIteratorIsValid(self, _):
        return self.valid

    def IOObjectRelease(self, _):
        pass

    def IORegistryEntryGetRegistryEntryID(self, service, out):
        identity = self.rows[service - 1][0]
        if identity is None:
            return 1
        out._obj.value = identity
        return 0

    def IORegistryEntryCreateCFProperty(self, service, key, allocator, options):
        assert key == "Statistics"
        return self.rows[service - 1][1]


class CF:
    def CFGetTypeID(self, obj):
        return "dict" if isinstance(obj, dict) else "number" if type(obj) is int else "other"

    def CFDictionaryGetTypeID(self):
        return "dict"

    def CFNumberGetTypeID(self):
        return "number"

    def CFDictionaryGetValue(self, obj, key):
        return obj.get(key)

    def CFNumberGetValue(self, n, kind, out):
        if type(n) is not int or not -(2**63) <= n < 2**63:
            return False
        out._obj.value = n
        return True

    def CFRelease(self, _):
        pass


def disk(rows, failed=False):
    d = Disk.__new__(Disk)
    d.io, d.cf = IO(rows, failed), CF()
    d.k_stats, d.k_read, d.k_write = "Statistics", "read", "write"
    d.services, d.found_at = [], None
    return d


class DiskMissing(unittest.TestCase):
    def test_empty_and_failed_enumeration_are_not_idle(self):
        for d in (disk([]), disk([(1, {"read": 10, "write": 20})], failed=True)):
            self.assertEqual(d.bytes(0), {})

    def test_provider_keeps_partial_missing_and_real_zero(self):
        for stats, want in [(None, (None, None)), ({"read": 0, "write": 0}, (0, 0)),
                            ({"read": 10}, (10, None)), ({"write": 20}, (None, 20)),
                            ({"read": -1, "write": "bad"}, (None, None)),
                            ("wrong type", (None, None))]:
            self.assertEqual(disk([(1, stats)]).bytes(0), {1: want})

    def test_missing_identity_invalidates_the_whole_sample(self):
        self.assertEqual(disk([(None, {"read": 10, "write": 20})]).bytes(0), {})

    def test_reenumeration_and_first_failure_can_recover(self):
        d = disk([], failed=True)
        self.assertEqual(d.bytes(0), {})
        d.io = IO([(2, {"read": 100, "write": 200})])
        self.assertEqual(d.bytes(31), {2: (100, 200)})

    def test_invalidated_enumeration_does_not_cache_partial_coverage(self):
        d = disk([(1, {"read": 100, "write": 200})])
        d.io.valid = False
        self.assertEqual(d.bytes(0), {})
        self.assertEqual(d.bytes(1), {})
        d.io.valid = True
        self.assertEqual(d.bytes(31), {1: (100, 200)})

    def test_duplicate_identity_and_invalid_numbers_are_not_totals(self):
        self.assertEqual(disk([(1, {"read": 10}), (1, {"read": 20})]).bytes(0), {})
        self.assertEqual(disk([(1, {"read": 2**64, "write": 20})]).bytes(0), {1: (None, 20)})
        for dt in [math.nan, math.inf]:
            self.assertEqual(ns["disk_rate"]({1: (0, 0)}, {1: (1, 1)}, dt), {"read_bps": None, "write_bps": None})

    def test_golden_rates(self):
        for case in json.loads((ROOT / "test/fixtures/disk-rates.json").read_text(encoding="utf-8")):
            prev = {}
            for frame in case["frames"]:
                cur = {int(k): v for k, v in frame["c"].items()}
                dt = frame.get("dt", 1)
                self.assertEqual(ns["disk_rate"](prev, cur, dt),
                                 dict(zip(("read_bps", "write_bps"), frame["want"])), case["name"])
                prev = cur if math.isfinite(dt) and dt > 0 else {}

    def test_main_retries_initial_failure_and_resets_after_exception(self):
        class Clock:
            now = 0
            def monotonic(self): return self.now
            def sleep(self, seconds): self.now += seconds
            def time(self): return self.now
            def time_ns(self): return self.now * 10**9

        for observations, wants in [
            ([None, (100, 200), (120, 240)], [[None, None], [20, 40]]),
            ([(100, 200), (120, 240), None, (9000, 10000), (9020, 10040)],
             [[20, 40], [None, None], [None, None], [20, 40]]),
        ]:
            frames, emitted = iter(observations), []
            class Provider:
                def bytes(self, now):
                    value = next(frames)
                    if value is None:
                        raise OSError("fixture unavailable")
                    return {1: value}
            def finish(): raise SystemExit(0)
            env = dict(ns, time=Clock(), Disk=Provider, Procs=lambda: None, Macmon=lambda: None,
                       sysctl=lambda *args: None, cpu_ticks=lambda: None, net_bytes=lambda: None,
                       cpu_pct=lambda *args: (None, []), memory=lambda: {}, emit=lambda v: emitted.append(v) or True,
                       finish=finish, ERRORS=[], V=1, INTERVAL=1, PROCS_EVERY=1,
                       WATCH=False, MAX_AGE=100, COUNT=len(wants))
            exec(compile(ast.Module(body=nodes, type_ignores=[]), "live_mac_disk", "exec"), env)
            env["Disk"] = Provider  # retain the real main/rate with only OS acquisition replaced
            with self.assertRaises(SystemExit):
                env["main"]()
            self.assertTrue(emitted[0]["has"]["disk"])
            self.assertEqual([[s["disk"]["read_bps"], s["disk"]["write_bps"]]
                              for s in emitted if s["type"] == "s"], wants)


if __name__ == "__main__":
    unittest.main()
