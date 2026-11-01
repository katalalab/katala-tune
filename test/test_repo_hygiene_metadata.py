"""Exercise the probe against an in-memory filesystem and synthetic Git output."""

import ast
import json
import pathlib
import posixpath
import re
import stat
import types
import unittest


class MemoryFilesystem:
    """The probe can only inspect entries explicitly defined in this fake."""

    def __init__(self, modes, directories, links=(), lstat_errors=None):
        self.modes = modes
        self.directories = directories
        self.links = set(links)
        self.lstat_errors = lstat_errors or {}
        self.denials = 0
        self.name = "posix"
        self.environ = {}
        self.pathsep = ":"
        self.path = types.SimpleNamespace(
            join=posixpath.join,
            splitdrive=posixpath.splitdrive,
            normcase=posixpath.normcase,
            sep="/",
            expanduser=lambda value: value,
            isdir=self.isdir,
            isfile=self.isfile,
            islink=self.islink,
            exists=self.exists,
        )

    def stat(self, path, *args, **kwargs):
        value = self.modes.get(path, FileNotFoundError())
        if isinstance(value, list):
            value = value.pop(0) if len(value) > 1 else value[0]
        if isinstance(value, OSError):
            if isinstance(value, PermissionError):
                self.denials += 1
            raise value
        return types.SimpleNamespace(st_mode=value)

    def lstat(self, path, *args, **kwargs):
        if path in self.lstat_errors:
            raise self.lstat_errors[path]
        if path in self.links:
            return types.SimpleNamespace(st_mode=stat.S_IFLNK)
        return self.stat(path, *args, **kwargs)

    def listdir(self, path):
        if path not in self.directories:
            raise AssertionError("unexpected synthetic listdir")
        value = self.directories[path]
        if isinstance(value, OSError):
            raise value
        return value

    def isdir(self, path):
        try:
            return stat.S_ISDIR(self.stat(path).st_mode)
        except (OSError, ValueError):
            return False

    def isfile(self, path):
        try:
            return stat.S_ISREG(self.stat(path).st_mode)
        except (OSError, ValueError):
            return False

    def islink(self, path):
        try:
            return stat.S_ISLNK(self.lstat(path).st_mode)
        except (OSError, ValueError):
            return False

    def exists(self, path):
        try:
            self.stat(path)
            return True
        except (OSError, ValueError):
            return False


class InlinePool:
    def __init__(self, **_kwargs):
        pass

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def map(self, function, values):
        return map(function, values)


def isolated_probe(fs, root="/virtual/root", extra_roots=(), max_repos=10, fake_scan=True):
    # Execute the real probe functions with fake globals; top-level environment
    # reads and process setup are intentionally not executed.
    source = pathlib.Path(__file__).parents[1] / "probes" / "repo_hygiene.py"
    tree = ast.parse(source.read_text(encoding="utf-8"))
    functions = [node for node in tree.body if isinstance(node, ast.FunctionDef)]
    module = ast.fix_missing_locations(ast.Module(body=functions, type_ignores=[]))
    output = []
    context = {
        "os": fs,
        "stat": stat,
        "json": json,
        "re": re,
        "sys": types.SimpleNamespace(platform="synthetic"),
        "time": types.SimpleNamespace(time=lambda: 0),
        "shutil": types.SimpleNamespace(which=lambda name: "synthetic" if name == "git" else None),
        "ThreadPoolExecutor": InlinePool,
        "print": output.append,
        "HOME": "/virtual/residence",
        "GHQ_ROOT_OPT": root,
        "EXTRA_ROOTS": extra_roots,
        "T0": 0,
        "DEADLINE": 1,
        "BUDGET_S": 1,
        "CMD_S": 1,
        "MAX_REPOS": max_repos,
        "MAX_WALK_DIRS": 100,
        "MAX_DIRTY_STAT": 10,
        "GHQ_DEPTH": 5,
        "EXTRA_DEPTH": 2,
        "WORKERS": 1,
        "SKIP_DIRS": set(),
        "KEEP_DOT": set(),
        "KT_VERSION": "synthetic",
    }
    exec(compile(module, "<synthetic-probe>", "exec"), context)
    context["run"] = lambda *_args: (1, "")
    if fake_scan:
        context["scan"] = lambda path, _root: {"path": path, "state": "ok", "linked": False}
    return context, output


def run_probe(fs, **options):
    context, output = isolated_probe(fs, **options)
    context["main"]()
    return json.loads(output[0])


class MetadataTests(unittest.TestCase):
    def test_child_stat_errors_preserve_known_results_and_mark_incomplete(self):
        for error in (PermissionError(), FileNotFoundError()):
            with self.subTest(error=type(error).__name__):
                fs = MemoryFilesystem(
                    {"/virtual/root": stat.S_IFDIR, "/virtual/root/blocked": error,
                     "/virtual/root/good": stat.S_IFDIR, "/virtual/root/good/.git": stat.S_IFDIR},
                    {"/virtual/root": ["blocked", "good"]},
                )
                result = run_probe(fs)
                self.assertEqual(result.get("error"), "walk_incomplete")
                self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/good"])
                self.assertEqual(result["enumerated"], 1)
                self.assertFalse(result["truncated"])

    def test_marker_access_denial_preserves_known_results_and_marks_incomplete(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/blocked": stat.S_IFDIR,
             "/virtual/root/blocked/.git": PermissionError("synthetic-private-error"), "/virtual/root/good": stat.S_IFDIR,
             "/virtual/root/good/.git": stat.S_IFDIR},
            {"/virtual/root": ["blocked", "good"], "/virtual/root/blocked": []},
        )
        result = run_probe(fs)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/good"])
        self.assertEqual(result["enumerated"], 1)
        self.assertFalse(result["truncated"])
        self.assertNotIn("synthetic-private-error", json.dumps(result))
        self.assertNotIn("/virtual/root/blocked", json.dumps(result))

    def test_parent_disappearing_after_metadata_check_keeps_known_results(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR,
             "/virtual/root/gone": [stat.S_IFDIR, stat.S_IFDIR, stat.S_IFDIR, FileNotFoundError("synthetic-private-error")],
             "/virtual/root/good": stat.S_IFDIR, "/virtual/root/good/.git": stat.S_IFDIR},
            {"/virtual/root": ["gone", "good"], "/virtual/root/gone": []},
        )
        result = run_probe(fs)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/good"])
        self.assertEqual(result["enumerated"], 1)
        self.assertNotIn("synthetic-private-error", json.dumps(result))
        self.assertNotIn("/virtual/root/gone", json.dumps(result))

    def test_default_root_denial_is_not_masked_by_successful_extra_root(self):
        fs = MemoryFilesystem(
            {"/virtual/residence/ghq": PermissionError(), "/virtual/extra": stat.S_IFDIR,
             "/virtual/extra/good": stat.S_IFDIR, "/virtual/extra/good/.git": stat.S_IFDIR},
            {"/virtual/extra": ["good"]},
        )
        result = run_probe(fs, root=None, extra_roots=["/virtual/extra"])
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/extra/good"])
        self.assertEqual(result["enumerated"], 1)

    def test_linked_metadata_denial_does_not_report_an_observed_clone(self):
        fs = MemoryFilesystem({"/virtual/root/item/.git": PermissionError()}, {})
        context, _ = isolated_probe(fs, fake_scan=False)
        status = "# branch.oid (initial)\0# branch.head main\0# stash 0\0"
        context["git"] = lambda _path, command, *_args: (0, status) if command == "status" else (1, "")
        result = context["scan"]("/virtual/root/item", "/virtual/root")
        self.assertEqual(result["state"], "unobserved")
        self.assertIsNone(result["linked"])
        self.assertEqual(result["reason"], "probe_failed")

    def test_linked_marker_types_preserve_known_boolean(self):
        status = "# branch.oid (initial)\0# branch.head main\0# stash 0\0"
        for marker, linked in ((stat.S_IFREG, True), (stat.S_IFDIR, False)):
            with self.subTest(linked=linked):
                fs = MemoryFilesystem({"/virtual/root/item/.git": marker}, {})
                context, _ = isolated_probe(fs, fake_scan=False)
                context["git"] = lambda _path, command, *_args: (0, status) if command == "status" else (1, "")
                result = context["scan"]("/virtual/root/item", "/virtual/root")
                self.assertEqual(result["state"], "ok")
                self.assertIs(result["linked"], linked)

    def test_priority_failure_does_not_displace_known_primary(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/a": stat.S_IFDIR,
             "/virtual/root/a/.git": [stat.S_IFREG, PermissionError()],
             "/virtual/root/z": stat.S_IFDIR, "/virtual/root/z/.git": stat.S_IFDIR},
            {"/virtual/root": ["a", "z"]},
        )
        result = run_probe(fs, max_repos=1)
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/z"])
        self.assertGreater(fs.denials, 0)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual(result["skipped"], 1)

    def test_missing_optional_default_root_keeps_successful_extra_root(self):
        fs = MemoryFilesystem(
            {"/virtual/extra": stat.S_IFDIR, "/virtual/extra/good": stat.S_IFDIR,
             "/virtual/extra/good/.git": stat.S_IFDIR},
            {"/virtual/extra": ["good"]},
        )
        result = run_probe(fs, root=None, extra_roots=["/virtual/extra"])
        self.assertNotIn("error", result)
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/extra/good"])

    def test_dangling_default_root_is_not_treated_as_absent(self):
        fs = MemoryFilesystem(
            {"/virtual/residence/ghq": FileNotFoundError(), "/virtual/extra": stat.S_IFDIR,
             "/virtual/extra/good": stat.S_IFDIR, "/virtual/extra/good/.git": stat.S_IFDIR},
            {"/virtual/extra": ["good"]}, links=["/virtual/residence/ghq"],
        )
        result = run_probe(fs, root=None, extra_roots=["/virtual/extra"])
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/extra/good"])

    def test_non_directory_default_root_is_optional(self):
        fs = MemoryFilesystem(
            {"/virtual/residence/ghq": stat.S_IFREG, "/virtual/extra": stat.S_IFDIR,
             "/virtual/extra/good": stat.S_IFDIR, "/virtual/extra/good/.git": stat.S_IFDIR},
            {"/virtual/extra": ["good"]},
        )
        result = run_probe(fs, root=None, extra_roots=["/virtual/extra"])
        self.assertNotIn("error", result)
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/extra/good"])

    def test_missing_marker_is_not_a_walk_error(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/group": stat.S_IFDIR},
            {"/virtual/root": ["group"], "/virtual/root/group": []},
        )
        result = run_probe(fs)
        self.assertNotIn("error", result)
        self.assertEqual(result["repos"], [])

    def test_valid_marker_symbolic_link_is_discovered(self):
        for target in (stat.S_IFDIR, stat.S_IFREG):
            with self.subTest(target=target):
                fs = MemoryFilesystem(
                    {"/virtual/root": stat.S_IFDIR, "/virtual/root/good": stat.S_IFDIR,
                     "/virtual/root/good/.git": target},
                    {"/virtual/root": ["good"]}, links=["/virtual/root/good/.git"],
                )
                result = run_probe(fs)
                self.assertNotIn("error", result)
                self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/good"])

    def test_dangling_marker_symbolic_link_is_incomplete(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/broken": stat.S_IFDIR,
             "/virtual/root/broken/.git": FileNotFoundError(), "/virtual/root/good": stat.S_IFDIR,
             "/virtual/root/good/.git": stat.S_IFDIR},
            {"/virtual/root": ["broken", "good"]}, links=["/virtual/root/broken/.git"],
        )
        result = run_probe(fs)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/good"])
        self.assertFalse(result["truncated"])

    def test_child_symbolic_link_is_not_followed(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/link": stat.S_IFDIR},
            {"/virtual/root": ["link"]},
            links=["/virtual/root/link"],
        )
        result = run_probe(fs)
        self.assertNotIn("error", result)
        self.assertEqual(result["repos"], [])

    def test_broken_child_symbolic_link_is_not_followed(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/link": FileNotFoundError()},
            {"/virtual/root": ["link"]}, links=["/virtual/root/link"],
        )
        result = run_probe(fs)
        self.assertNotIn("error", result)
        self.assertEqual(result["repos"], [])

    def test_child_link_metadata_denial_marks_incomplete(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/group": stat.S_IFDIR},
            {"/virtual/root": ["group"]}, lstat_errors={"/virtual/root/group": PermissionError()},
        )
        result = run_probe(fs)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual(result["repos"], [])

    def test_inconsistent_child_link_metadata_is_incomplete(self):
        # Inconsistent synthetic metadata does not prove a Windows junction.
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/group": stat.S_IFDIR,
             "/virtual/root/group/.git": stat.S_IFDIR},
            {"/virtual/root": ["group"]},
        )
        original_lstat = fs.lstat
        fs.lstat = lambda path, *args, **kwargs: (
            types.SimpleNamespace(st_mode=stat.S_IFLNK) if path == "/virtual/root/group" else original_lstat(path, *args, **kwargs)
        )
        fs.path.islink = lambda _path: False
        result = run_probe(fs)
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertEqual(result["repos"], [])

    def test_known_synthetic_junction_keeps_existing_nonlink_behavior(self):
        for mode in (stat.S_IFDIR, stat.S_IFLNK):
            with self.subTest(mode=mode):
                fs = MemoryFilesystem(
                    {"/virtual/root": stat.S_IFDIR, "/virtual/root/group": stat.S_IFDIR,
                     "/virtual/root/group/.git": stat.S_IFDIR},
                    {"/virtual/root": ["group"]},
                )
                fs.name = "nt"
                original_lstat = fs.lstat
                fs.lstat = lambda path, *args, **kwargs: (
                    types.SimpleNamespace(st_mode=mode, st_reparse_tag=0xA0000003)
                    if path == "/virtual/root/group" else original_lstat(path, *args, **kwargs)
                )
                fs.path.islink = lambda _path: False
                result = run_probe(fs)
                self.assertNotIn("error", result)
                self.assertEqual([row["path"] for row in result["repos"]], ["/virtual/root/group"])

    def test_walk_failure_and_truncation_are_independent(self):
        fs = MemoryFilesystem(
            {"/virtual/root": stat.S_IFDIR, "/virtual/root/blocked": PermissionError(),
             "/virtual/root/group": stat.S_IFDIR},
            {"/virtual/root": ["blocked", "group"]},
        )
        context, output = isolated_probe(fs)
        context["MAX_WALK_DIRS"] = 1
        context["main"]()
        result = json.loads(output[0])
        self.assertEqual(result.get("error"), "walk_incomplete")
        self.assertTrue(result["truncated"])


if __name__ == "__main__":
    unittest.main()
