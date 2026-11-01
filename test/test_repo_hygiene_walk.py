"""Synthetic directory traversal tests; no filesystem scan or subprocess is used."""
import importlib.util
import json
import pathlib
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("repo_hygiene", pathlib.Path(__file__).resolve().parents[1] / "probes" / "repo_hygiene.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class InlinePool:
    def __init__(self, **_kwargs):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def map(self, function, values):
        return map(function, values)


class WalkTests(unittest.TestCase):
    def run_probe(self, entries, repositories=(), deadline=None, stop_after_failure=False):
        def listdir(path):
            value = entries[probe.slash(path)]
            if isinstance(value, OSError):
                if stop_after_failure:
                    probe.DEADLINE = 0
                raise value
            return value

        def scan(path, _root):
            return {"path": probe.slash(path), "state": "ok", "tracked": 1, "untracked": 0}

        with mock.patch.object(probe.shutil, "which", return_value="synthetic-git"), \
             mock.patch.object(probe, "ghq_root", return_value=("/fixture", "opt")), \
             mock.patch.object(probe, "EXTRA_ROOTS", []), \
             mock.patch.object(probe, "HOME", "/synthetic-home"), \
             mock.patch.object(probe.os, "listdir", side_effect=listdir), \
             mock.patch.object(probe.os.path, "isdir", return_value=True), \
             mock.patch.object(probe.os.path, "islink", return_value=False), \
             mock.patch.object(probe.os.path, "isfile", return_value=False), \
             mock.patch.object(probe, "is_repo", side_effect=lambda path: probe.slash(path) in repositories), \
             mock.patch.object(probe, "scan", side_effect=scan), \
             mock.patch.object(probe, "ThreadPoolExecutor", InlinePool), \
             mock.patch.object(probe, "DEADLINE", deadline if deadline is not None else float("inf")), \
             mock.patch("builtins.print") as printed:
            probe.main()
        raw = printed.call_args.args[0]
        return json.loads(raw), raw

    def test_permission_error_and_disappearance_keep_successful_results(self):
        entries = {
            "/fixture": ["denied-private", "vanished-private", "repo", "group"],
            "/fixture/denied-private": PermissionError("private-error-text"),
            "/fixture/vanished-private": FileNotFoundError("private-error-text"),
            "/fixture/group": ["nested"],
        }
        repositories = {"/fixture/repo", "/fixture/group/nested"}
        data, raw = self.run_probe(entries, repositories)
        self.assertEqual(data.get("error"), "walk_incomplete")
        self.assertFalse(data["truncated"])
        self.assertEqual(data["enumerated"], 2)
        self.assertEqual({repo["path"] for repo in data["repos"]}, repositories)
        for secret in ("denied-private", "vanished-private", "private-error-text"):
            self.assertNotIn(secret, raw)

    def test_root_access_denied_or_disappeared_is_not_empty_success(self):
        for error in (PermissionError("private-error"), FileNotFoundError("private-error")):
            with self.subTest(error=type(error).__name__):
                data, raw = self.run_probe({"/fixture": error})
                self.assertEqual(data.get("error"), "walk_incomplete")
                self.assertFalse(data["truncated"])
                self.assertEqual(data["repos"], [])
                self.assertNotIn("private-error", raw)

    def test_success_and_time_cutoff_are_distinct_from_enumeration_error(self):
        data, _ = self.run_probe({"/fixture": ["repo"]}, {"/fixture/repo"})
        self.assertNotIn("error", data)
        self.assertFalse(data["truncated"])
        self.assertEqual(data["enumerated"], 1)
        stopped, _ = self.run_probe({}, deadline=0)
        self.assertNotIn("error", stopped)
        self.assertTrue(stopped["truncated"])
        self.assertEqual(stopped["enumerated"], 0)

    def test_failure_before_time_cutoff_reports_both(self):
        entries = {"/fixture": ["later", "private-denied"], "/fixture/private-denied": PermissionError("private-error")}
        data, raw = self.run_probe(entries, stop_after_failure=True)
        self.assertEqual(data.get("error"), "walk_incomplete")
        self.assertTrue(data["truncated"])
        self.assertNotIn("private-denied", raw)
        self.assertNotIn("private-error", raw)


if __name__ == "__main__":
    unittest.main()
