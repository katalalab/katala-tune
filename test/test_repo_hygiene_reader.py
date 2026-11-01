"""Run directly with Python; all Git processes and repository I/O are mocked."""
import importlib.util
import pathlib
import threading
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("repo_hygiene", pathlib.Path(__file__).resolve().parents[1] / "probes" / "repo_hygiene.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
REAL_THREAD = threading.Thread

HEADER = b"# branch.oid (initial)\0# branch.head sample\0# stash 0\0"


class FakeStream:
    def __init__(self, chunks, pending=False, error=False):
        self.chunks = iter(chunks)
        self.pending = pending
        self.error = error
        self.blocked = threading.Event()
        self.release = threading.Event()

    def read(self, _size):
        chunk = next(self.chunks, None)
        if chunk is not None:
            return chunk
        if self.pending:
            self.blocked.set()
            self.release.wait(2)
            self.pending = False
            return b"? sample-later\0"
        if self.error:
            self.error = False
            raise OSError("synthetic read failure")
        return b""


class FakeProcess:
    def __init__(self, stream, code=0, timeout=False):
        self.stdout = stream
        self.returncode = code
        self.killed = False
        self.timeout = timeout
        self.wait_calls = 0

    def wait(self, timeout=None):
        self.wait_calls += 1
        if self.timeout and self.wait_calls == 1:
            raise probe.subprocess.TimeoutExpired("synthetic", timeout)
        return self.returncode

    def kill(self):
        self.killed = True


class ReaderTests(unittest.TestCase):
    def setUp(self):
        self.streams = []
        self.threads = []
        self.uncaught = []
        self.addCleanup(self.release_readers)

    def release_readers(self):
        for stream in self.streams:
            stream.release.set()
        for thread in self.threads:
            thread.join(2)

    def scan(self, status_stream, origin_stream=None, timeout=False):
        self.streams.append(status_stream)
        status = FakeProcess(status_stream, timeout=timeout)
        origin = FakeProcess(origin_stream or FakeStream([]), code=1)
        calls = []
        last = [None]

        def spawn(cmd, **_kwargs):
            self.assertEqual(cmd[0], "git")
            calls.append(cmd)
            if "status" in cmd:
                last[0] = status
            elif "config" in cmd:
                last[0] = origin
            else:
                self.fail("unexpected Git command in synthetic fixture")
            return last[0]

        test = self

        class FakeReaderThread:
            def __init__(self, target, daemon):
                self.stream = last[0].stdout

                def run_target():
                    try:
                        target()
                    except Exception:
                        # A real reader thread also stops without notifying run().
                        # Capture only the fact of an uncaught exception, never its text.
                        test.uncaught.append(True)

                self.thread = REAL_THREAD(target=run_target, daemon=daemon)
                test.threads.append(self.thread)

            def start(self):
                self.thread.start()
                if self.stream.pending:
                    if not self.stream.blocked.wait(1):
                        raise AssertionError("synthetic reader did not reach the held read")

            def join(self, _timeout):
                # Simulate the existing bounded join expiring without a five-second delay.
                self.thread.join(0 if self.stream.pending else 1)

            def is_alive(self):
                return self.thread.is_alive()

        with mock.patch.object(probe.subprocess, "Popen", side_effect=spawn), \
             mock.patch.object(probe.threading, "Thread", FakeReaderThread), \
             mock.patch.object(probe.os.path, "isfile", return_value=False), \
             mock.patch.object(probe.os, "lstat", return_value=mock.Mock(st_mtime=probe.T0)):
            result = probe.scan("/fixture/project", "/fixture")
        return result, status, calls

    def test_reader_still_waiting_is_not_a_clean_repository(self):
        result, status, calls = self.scan(FakeStream([HEADER], pending=True))
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "timeout")
        self.assertNotIn("tracked", result)
        self.assertNotIn("untracked", result)
        self.assertTrue(status.stdout.blocked.is_set())
        self.assertFalse(status.killed, "an unfinished reader must not cause process termination")
        self.assertEqual(len(calls), 1)

    def test_reader_exception_is_not_a_clean_repository_or_raw_error(self):
        result, _status, calls = self.scan(FakeStream([HEADER], error=True))
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "git_failed")
        self.assertNotIn("tracked", result)
        self.assertNotIn("synthetic read failure", repr(result))
        self.assertEqual(len(self.uncaught), 0, "reader must handle its own exception")
        self.assertEqual(len(calls), 1)

    def test_complete_status_counts_tracked_and_untracked(self):
        result, _status, calls = self.scan(FakeStream([HEADER, b"1 sample\0? sample-new\0"]))
        self.assertEqual(result["state"], "ok")
        self.assertEqual((result["tracked"], result["untracked"]), (1, 1))
        self.assertEqual(len(calls), 2)

    def test_exact_output_limit_with_eof_is_accepted(self):
        with mock.patch.object(probe, "MAX_OUT", len(HEADER)):
            result, status, calls = self.scan(FakeStream([HEADER]))
        self.assertEqual(result["state"], "ok")
        self.assertEqual((result["tracked"], result["untracked"]), (0, 0))
        self.assertFalse(status.killed)
        self.assertEqual(len(calls), 2)

    def test_later_reader_exception_is_not_treated_as_missing_origin(self):
        result, _status, calls = self.scan(FakeStream([HEADER]), FakeStream([], error=True))
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "git_failed")
        self.assertNotIn("has_origin", result)
        self.assertEqual(len(self.uncaught), 0, "reader must handle its own exception")
        self.assertEqual(len(calls), 2)

    def test_output_limit_is_not_reported_as_a_clean_repository(self):
        with mock.patch.object(probe, "MAX_OUT", 8):
            result, status, calls = self.scan(FakeStream([b"12345", b"6789"]))
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "over_limit")
        self.assertNotIn("tracked", result)
        self.assertTrue(status.killed, "only the fake process receives kill")
        self.assertEqual(len(calls), 1)

    def test_process_timeout_discards_partial_output(self):
        result, status, calls = self.scan(FakeStream([HEADER], pending=True), timeout=True)
        self.assertEqual(result["state"], "unobserved")
        self.assertEqual(result["reason"], "timeout")
        self.assertNotIn("tracked", result)
        self.assertTrue(status.killed, "only the fake process receives kill")
        self.assertEqual(status.wait_calls, 2)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
