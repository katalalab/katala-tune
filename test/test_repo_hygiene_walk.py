"""Synthetic directory traversal tests; no filesystem scan or subprocess is used."""
import importlib.util
import json
import ntpath
import pathlib
import posixpath
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
    def test_root_normalization_preserves_posix_windows_and_unc_roots(self):
        cases = [
            ("/", posixpath, "/"),
            ("//", posixpath, "//"),
            ("///", posixpath, "/"),
            ("/fixture///", posixpath, "/fixture"),
            ("", posixpath, ""),
            ("C:\\", ntpath, "C:/"),
            ("C:", ntpath, "C:"),
            ("//", ntpath, "//"),
            ("///", ntpath, "///"),
            ("C:\\fixture\\", ntpath, "C:/fixture"),
            ("\\\\fixture-server\\share\\", ntpath, "//fixture-server/share/"),
            ("\\\\fixture-server\\share", ntpath, "//fixture-server/share"),
            ("", ntpath, ""),
        ]
        for value, pathmod, expected in cases:
            with self.subTest(value=value, pathmod=pathmod.__name__):
                self.assertEqual(probe.normalize_root(value, pathmod), expected)

    def test_relative_paths_preserve_absolute_and_drive_relative_roots(self):
        cases = [
            ("/fixture/project", "/", posixpath, "fixture/project"),
            ("//fixture/project", "//", posixpath, "fixture/project"),
            ("C:/fixture/project", "C:/", ntpath, "fixture/project"),
            ("C:project", "C:", ntpath, "project"),
            ("C:/other", "C:", ntpath, None),
            ("C:../other", "C:", ntpath, None),
            ("C:child/../other", "C:", ntpath, None),
            ("//fixture-server/share/project", "//fixture-server/share/", ntpath, "project"),
            ("//fixture-server/share-other/project", "//fixture-server/share", ntpath, None),
        ]
        for path, root, pathmod, expected in cases:
            with self.subTest(path=path, root=root):
                self.assertEqual(probe.relative_to_root(path, root, pathmod), expected)

    def test_unc_share_root_with_or_without_trailing_slash_is_same(self):
        self.assertTrue(probe.same_root("//fixture-server/share/", "//fixture-server/share", ntpath))
        self.assertTrue(probe.same_root("//FIXTURE-SERVER/share", "//fixture-server/share/", ntpath))
        self.assertFalse(probe.same_root("//fixture-server/share/", "//fixture-server/other", ntpath))
        self.assertFalse(probe.same_root("C:/", "C:", ntpath))

    def test_explicit_posix_root_survives_ghq_selection(self):
        with mock.patch.object(probe, "GHQ_ROOT_OPT", "/"), \
             mock.patch.object(probe.shutil, "which", side_effect=AssertionError("must not run discovery")):
            self.assertEqual(probe.ghq_root(), ("/", "opt"))

    def run_probe(self, entries, repositories=(), deadline=None, stop_after_failure=False, root="/fixture", extra_roots=(), unavailable=(), links=()):
        def listdir(path):
            value = entries[probe.slash(path)]
            if isinstance(value, OSError):
                if stop_after_failure:
                    probe.DEADLINE = 0
                raise value
            return value

        def scan(path, _root):
            return {"path": probe.slash(path), "state": "ok", "tracked": 1, "untracked": 0, "conflicted": 0, "upstream": False, "detached": False}

        with mock.patch.object(probe.shutil, "which", return_value="synthetic-git"), \
             mock.patch.object(probe, "ghq_root", return_value=(root, "opt" if root else None)), \
             mock.patch.object(probe, "EXTRA_ROOTS", extra_roots), \
             mock.patch.object(probe, "HOME", "/synthetic-home"), \
             mock.patch.object(probe.os, "listdir", side_effect=listdir), \
             mock.patch.object(probe.os.path, "isdir", side_effect=lambda path: probe.slash(path) not in unavailable), \
             mock.patch.object(probe.os.path, "islink", side_effect=lambda path: probe.slash(path) in links), \
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

    def test_selected_root_unavailable_is_unknown_but_missing_optional_root_is_not(self):
        data, raw = self.run_probe({}, unavailable={"/fixture"})
        self.assertEqual(data.get("error"), "walk_incomplete")
        self.assertFalse(data["truncated"])
        self.assertEqual(data["repos"], [])
        self.assertNotIn("/fixture", raw)
        optional, _ = self.run_probe({}, root=None)
        self.assertNotIn("error", optional)
        self.assertEqual(optional["roots"], [])
        blank, _ = self.run_probe({}, root=None, extra_roots=[""])
        self.assertNotIn("error", blank)

    def test_unavailable_extra_root_preserves_main_root_and_symlink_root_is_followed(self):
        data, raw = self.run_probe(
            {"/fixture": ["repo"]},
            {"/fixture/repo"},
            extra_roots=["/private-extra"],
            unavailable={"/private-extra"},
            links={"/fixture"},
        )
        self.assertEqual(data.get("error"), "walk_incomplete")
        self.assertFalse(data["truncated"])
        self.assertEqual(data["enumerated"], 1)
        self.assertEqual(data["repos"][0]["path"], "/fixture/repo")
        self.assertNotIn("private-extra", raw)

    def test_unc_share_root_is_not_walked_twice(self):
        data, _ = self.run_probe(
            {"//fixture-server/share/": ["repo"]},
            {"//fixture-server/share/repo"},
            root="//fixture-server/share/",
            extra_roots=["//fixture-server/share"],
        )
        self.assertNotIn("error", data)
        self.assertEqual(len(data["roots"]), 1)
        self.assertEqual(data["enumerated"], 1)

    def test_unc_share_extra_roots_are_not_walked_twice(self):
        data, _ = self.run_probe(
            {"//fixture-server/share": ["repo"]},
            {"//fixture-server/share/repo"},
            root=None,
            extra_roots=["//fixture-server/share", "//fixture-server/share/"],
        )
        self.assertNotIn("error", data)
        self.assertEqual(len(data["roots"]), 1)
        self.assertEqual(data["enumerated"], 1)


if __name__ == "__main__":
    unittest.main()
