"""Porcelain v2 parsing tests. Git, subprocesses and repository I/O are mocked."""
import importlib.util
import pathlib
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("repo_hygiene", pathlib.Path(__file__).resolve().parents[1] / "probes" / "repo_hygiene.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)

OID = "a" * 40
HEADER = f"# branch.oid {OID}\0# branch.head topic\0"
FIELDS = f".M N... 100644 100644 100644 {OID} {OID}"
MERGE_FIELDS = f"UU N... 100644 100644 100644 100644 {OID} {OID} {OID}"


class StatusTests(unittest.TestCase):
    def scan(self, status):
        calls = []

        def git(_path, *args):
            calls.append(args[0])
            if args[0] == "status":
                return 0, status
            if args[0] == "stash":
                return 0, ""
            if args[0] == "rev-list":
                return 0, "0\n"
            if args[0] == "log":
                return 0, "0\n"
            if args[0] == "config":
                return 1, ""
            self.fail("unexpected synthetic command")

        with mock.patch.object(probe, "git", side_effect=git), \
             mock.patch.object(probe.subprocess, "Popen", side_effect=AssertionError("unexpected subprocess")), \
             mock.patch.object(probe.os.path, "isfile", return_value=False), \
             mock.patch.object(probe.os, "lstat", return_value=mock.Mock(st_mtime=probe.T0)), \
             mock.patch.object(probe, "DEADLINE", float("inf")):
            result = probe.scan("/fixture/project", "/fixture")
        return result, calls

    def assert_unobserved(self, status):
        result, calls = self.scan(status)
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "git_failed")
        self.assertNotIn("tracked", result)
        self.assertNotIn("untracked", result)
        self.assertEqual(calls, ["status"])

    def test_empty_output_is_not_clean(self):
        self.assert_unobserved("")

    def test_missing_or_unterminated_headers_are_not_clean(self):
        for status in ["\0", HEADER[:-1], f"# branch.oid {OID}\0", "# branch.head topic\0", "# branch.oid \0# branch.head topic\0", f"# branch.oid {OID}\0# branch.head \0"]:
            with self.subTest(case=repr(status)):
                self.assert_unobserved(status)

    def test_short_or_invalid_full_object_ids_are_not_clean(self):
        for oid in ["a", "a" * 39, "a" * 41, "a" * 63, "a" * 65, "g" * 40]:
            with self.subTest(length=len(oid)):
                self.assert_unobserved(f"# branch.oid {oid}\0# branch.head topic\0")

    def test_unknown_records_and_malformed_records_are_not_clean(self):
        bad = ["Z fixture\0", "?\0", "? \0", "?fixture\0", "1 fixture\0", "2 fixture\0", "u fixture\0", "\0", f"2 {FIELDS} R100 destination\0"]
        for suffix in bad:
            with self.subTest(case=repr(suffix)):
                self.assert_unobserved(HEADER + suffix)

    def test_initial_detached_and_unknown_headers_are_accepted(self):
        fixtures = [
            ("# branch.oid (initial)\0# branch.head topic\0# future.header ignored\0", False, "topic"),
            (f"# branch.oid {OID}\0# branch.head (detached)\0", True, None),
            (HEADER + "# branch.upstream origin/topic\0", False, "topic"),
        ]
        for status, detached, branch in fixtures:
            with self.subTest(detached=detached, upstream="branch.upstream" in status):
                result, _calls = self.scan(status)
                self.assertEqual(result["state"], "ok")
                self.assertEqual((result["tracked"], result["untracked"], result["detached"], result["branch"]), (0, 0, detached, branch))
                self.assertIsNone(result["ahead"])

    def test_quick_comparison_preserves_unknown_ahead_and_behind(self):
        status = HEADER + "# branch.upstream origin/topic\0# branch.ab +? -?\0"
        result, _calls = self.scan(status)
        self.assertEqual(result["state"], "ok")
        self.assertTrue(result["upstream"])
        self.assertIsNone(result["ahead"])
        self.assertIsNone(result["behind"])

    def test_unknown_branch_is_not_reported_as_a_named_branch(self):
        self.assert_unobserved(f"# branch.oid {OID}\0# branch.head (unknown)\0")

    def test_valid_tracked_untracked_and_rename_source_are_counted(self):
        status = (HEADER + "# branch.upstream origin/topic\0# branch.ab +3 -2\0# stash 2\0"
                  + f"1 {FIELDS} ordinary name\0"
                  + f"2 {FIELDS} R100 renamed name\0? source that looks like a record\0"
                  + f"u {MERGE_FIELDS} merge\nname\0? untracked name\0! ignored name\0")
        result, _calls = self.scan(status)
        self.assertEqual(result["state"], "ok")
        self.assertEqual((result["tracked"], result["untracked"], result["conflicted"]), (3, 1, 1))
        self.assertEqual((result["upstream"], result["ahead"], result["behind"], result["stash"]), (True, 3, 2, 2))

    def test_sha256_object_id_and_record_like_rename_source_are_accepted(self):
        oid = "b" * 64
        header = f"# branch.oid {oid}\0# branch.head topic\0"
        status = header + f"2 {FIELDS} R100 renamed name\0# branch.head source name\0?  leading space\0"
        result, _calls = self.scan(status)
        self.assertEqual(result["state"], "ok")
        self.assertEqual((result["tracked"], result["untracked"], result["branch"]), (1, 1, "topic"))

    def test_rename_source_cannot_impersonate_unborn_branch_header(self):
        status = HEADER + f"2 {FIELDS} R100 renamed name\0# branch.oid (initial)\0"
        result, calls = self.scan(status)
        self.assertEqual(result["state"], "ok")
        self.assertIn("rev-list", calls)
        self.assertIn("log", calls)
        self.assertEqual(result["local_only"], 0)
        self.assertIsNotNone(result["head_age_s"])

    def test_invalid_output_is_not_returned(self):
        marker = "synthetic-content-marker"
        result, _calls = self.scan(HEADER + "Z " + marker + "\0")
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "git_failed")
        self.assertNotIn(marker, repr(result))

    def test_duplicate_or_malformed_known_headers_are_not_clean(self):
        for suffix in [f"# branch.oid {OID}\0", "# branch.head another\0", "# branch.ab +1 -2extra\0", "# branch.ab +1 -2\0", "# stash not-a-count\0", "# stash " + "9" * 5000 + "\0"]:
            with self.subTest(case=repr(suffix)):
                self.assert_unobserved(HEADER + suffix)


if __name__ == "__main__":
    unittest.main()
