"""Durable producer capture regressions; parent runs unittest discovery."""

import concurrent.futures
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
import threading
from pathlib import Path
from unittest.mock import patch

from replay_capture import ReplayCapture


# A real producer transforms input, emits diagnostics, and can return nonzero.
PRODUCER = """
import json
import sys
values = [int(value) for value in sys.stdin.read().split()]
sys.stdout.write(json.dumps({'sum': sum(values), 'count': len(values)}) + '\\n')
sys.stderr.write('diagnostic\\n')
sys.exit(int(sys.argv[1]))
"""


class ReplayCaptureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "capture"
        self.identity = {"producer_sha256": "sample-digest", "seed": 11,
                         "inputs": [{"sha256": "sample-input-digest", "size": 7}]}
        self.args = [sys.executable, "-c", PRODUCER, "0"]
        self.options = {"input": "2 3 5", "capture_output": True, "text": True,
                        "timeout": 10, "check": False}

    def capture(self, args=None, options=None):
        capture = ReplayCapture(self.directory, self.identity)
        result = capture.run(args or self.args, **(options or self.options))
        capture.finish()
        return result

    def no_producers(self):
        return patch("replay_capture.subprocess.run",
                     side_effect=AssertionError("replay launched a producer"))

    def manifest(self):
        return json.loads((self.directory / "manifest.json").read_text())

    def save_manifest(self, envelope):
        data = json.dumps(envelope["payload"], sort_keys=True, ensure_ascii=True,
                          separators=(",", ":"), allow_nan=False).encode()
        envelope["sha256"] = hashlib.sha256(data).hexdigest()
        (self.directory / "manifest.json").write_text(json.dumps(envelope))

    def test_real_response_replays_for_changed_oracle_without_execution(self):
        original = self.capture()
        self.assertEqual(json.loads(original.stdout), {"sum": 10, "count": 3})
        self.assertEqual(original.stderr, "diagnostic\n")
        # The classifier is intentionally outside producer identity.
        first_oracle = lambda result: json.loads(result.stdout)["sum"] >= 10
        changed_oracle = lambda result: json.loads(result.stdout)["sum"] > 10
        self.assertTrue(first_oracle(original))
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            result = replay.run(self.args, **self.options)
            replay.finish()
        self.assertFalse(changed_oracle(result))
        self.assertEqual((result.args, result.returncode, result.stdout, result.stderr),
                         (original.args, original.returncode, original.stdout, original.stderr))

    def test_explicit_nonzero_is_a_recorded_diagnostic_response(self):
        args = [*self.args[:-1], "9"]
        original = self.capture(args=args)
        self.assertEqual(original.returncode, 9)
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            result = replay.run(args, **self.options)
        self.assertEqual((result.stdout, result.stderr, result.returncode),
                         (original.stdout, original.stderr, 9))

    def test_stale_identity_is_rejected_without_execution(self):
        self.capture()
        changed = {**self.identity, "seed": 12}
        with self.no_producers(), self.assertRaisesRegex(RuntimeError, "identity mismatch"):
            ReplayCapture(self.directory, changed, replay=True)

    def test_unfinished_capture_has_a_durable_checkpoint_but_cannot_replay(self):
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(self.args, **self.options)
        manifest = self.manifest()["payload"]
        self.assertIs(manifest["complete"], False)
        responses = [path for path in self.directory.glob("*.json") if path.name != "manifest.json"]
        self.assertEqual(len(responses), 1)
        response = json.loads(responses[0].read_text())
        self.assertEqual(json.loads(response["stdout"])["sum"], 10)
        with self.no_producers(), self.assertRaisesRegex(RuntimeError, "incomplete"):
            ReplayCapture(self.directory, self.identity, replay=True)

    def test_changed_stdin_argv_cwd_timeout_or_decoding_is_not_reused(self):
        self.capture()
        self.root.joinpath("other").mkdir()
        variants = [
            (self.args, {**self.options, "input": "2 3 6"}),
            ([*self.args[:-1], "1"], self.options),
            (self.args, {**self.options, "cwd": self.root / "other"}),
            (self.args, {**self.options, "timeout": 11}),
            (self.args, {**self.options, "encoding": "latin-1"}),
            (self.args, {**self.options, "errors": "replace"}),
        ]
        with self.no_producers():
            for args, options in variants:
                with self.subTest(options=options, args=args):
                    replay = ReplayCapture(self.directory, self.identity, replay=True)
                    with self.assertRaisesRegex(RuntimeError, "absent"):
                        replay.run(args, **options)

    def test_requests_are_lookup_based_not_order_dependent(self):
        capture = ReplayCapture(self.directory, self.identity)
        first = capture.run(self.args, **self.options)
        other = {**self.options, "input": "8 13"}
        second = capture.run(self.args, **other)
        capture.finish()
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            self.assertEqual(replay.run(self.args, **other).stdout, second.stdout)
            self.assertEqual(replay.run(self.args, **self.options).stdout, first.stdout)
            replay.finish()

    def test_replay_finish_rejects_subset_without_modifying_capture(self):
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(self.args, **self.options)
        other = {**self.options, "input": "8 13"}
        capture.run(self.args, **other)
        capture.finish()
        before = {path.name: path.read_bytes() for path in self.directory.iterdir()}
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            replay.run(self.args, **self.options)
            with self.assertRaisesRegex(RuntimeError, "all captured requests.*1 unused"):
                replay.finish()
        self.assertEqual(before, {path.name: path.read_bytes() for path in self.directory.iterdir()})

    def test_swapped_response_files_are_rejected_even_with_updated_checksums(self):
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(self.args, **self.options)
        capture.run(self.args, **{**self.options, "input": "8 13"})
        capture.finish()
        envelope = self.manifest()
        responses = envelope["payload"]["responses"]
        first, second = responses
        first_path, second_path = (self.directory / f"{key}.json" for key in (first, second))
        first_data, second_data = first_path.read_bytes(), second_path.read_bytes()
        first_path.write_bytes(second_data)
        second_path.write_bytes(first_data)
        responses[first] = hashlib.sha256(second_data).hexdigest()
        responses[second] = hashlib.sha256(first_data).hexdigest()
        self.save_manifest(envelope)
        with self.no_producers(), self.assertRaisesRegex(RuntimeError, "corrupt capture response"):
            ReplayCapture(self.directory, self.identity, replay=True)

    def test_missing_truncated_and_modified_response_are_rejected(self):
        self.capture()
        key = next(iter(self.manifest()["payload"]["responses"]))
        path = self.directory / f"{key}.json"
        original = path.read_bytes()
        for corruption in (None, b'{"request":', original.replace(b"diagnostic", b"alteration")):
            with self.subTest(corruption=corruption):
                if corruption is None:
                    path.unlink()
                else:
                    path.write_bytes(corruption)
                with self.no_producers(), self.assertRaises(RuntimeError):
                    ReplayCapture(self.directory, self.identity, replay=True)
                path.write_bytes(original)

    def test_truncated_and_checksum_invalid_manifest_are_rejected(self):
        self.capture()
        path = self.directory / "manifest.json"
        original = path.read_bytes()
        for corruption in (b'{"payload":', original.replace(b'"seed":11', b'"seed":12')):
            with self.subTest(corruption=corruption):
                path.write_bytes(corruption)
                with self.no_producers(), self.assertRaises(RuntimeError):
                    ReplayCapture(self.directory, self.identity, replay=True)
        path.write_bytes(original)

    def test_replay_checks_response_again_if_changed_after_open(self):
        self.capture()
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            key = next(iter(self.manifest()["payload"]["responses"]))
            (self.directory / f"{key}.json").write_text("{}")
            with self.assertRaisesRegex(RuntimeError, "corrupt capture response"):
                replay.run(self.args, **self.options)

    def test_existing_directory_is_never_modified(self):
        self.capture()
        before = {path.name: path.read_bytes() for path in self.directory.iterdir()}
        with self.assertRaises(FileExistsError):
            ReplayCapture(self.directory, self.identity)
        self.assertEqual(before, {path.name: path.read_bytes() for path in self.directory.iterdir()})
        empty = self.root / "empty"
        empty.mkdir()
        with self.assertRaises(FileExistsError):
            ReplayCapture(empty, self.identity)
        self.assertEqual(list(empty.iterdir()), [])

    def test_duplicate_requests_execute_and_require_consistent_responses(self):
        counter = self.root / "executions"
        code = """
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
with path.open('a') as stream:
    stream.write('execution\\n')
sys.stdout.write('stable\\n')
"""
        args = [sys.executable, "-c", code, str(counter)]
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(args, **self.options)
        capture.run(args, **self.options)
        capture.finish()
        self.assertEqual(counter.read_text().splitlines(), ["execution", "execution"])
        self.assertEqual(len(self.manifest()["payload"]["responses"]), 1)

    def test_concurrent_duplicates_execute_in_parallel_and_reject_disagreement(self):
        capture = ReplayCapture(self.directory, self.identity)
        args = [sys.executable, "-c", "import os; print(os.getpid())"]
        started = threading.Barrier(3)
        release = threading.Event()
        actual_run = subprocess.run

        def concurrent_run(*args, **kwargs):
            started.wait(timeout=10)
            if not release.wait(timeout=10):
                raise RuntimeError("producer release timed out")
            return actual_run(*args, **kwargs)

        with patch("replay_capture.subprocess.run", side_effect=concurrent_run), \
                concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(capture.run, args, **self.options) for _ in range(2)]
            try:
                # Both executions must reach subprocess.run without taking the state lock.
                started.wait(timeout=10)
                with self.assertRaisesRegex(RuntimeError, "requests are running"):
                    capture.finish()
            finally:
                release.set()
            successes, errors = [], []
            for future in futures:
                try:
                    successes.append(future.result(timeout=15))
                except RuntimeError as error:
                    errors.append(str(error))
        self.assertEqual(len(successes), 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("inconsistent", errors[0])
        with self.assertRaisesRegex(RuntimeError, "failed"):
            capture.finish()
        self.assertIs(self.manifest()["payload"]["complete"], False)
        with self.no_producers(), self.assertRaisesRegex(RuntimeError, "incomplete"):
            ReplayCapture(self.directory, self.identity, replay=True)

    def test_inconsistent_duplicate_prevents_completion(self):
        counter = self.root / "counter"
        code = """
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
count = int(path.read_text()) + 1 if path.exists() else 1
path.write_text(str(count))
sys.stdout.write(str(count))
"""
        args = [sys.executable, "-c", code, str(counter)]
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(args, **self.options)
        with self.assertRaisesRegex(RuntimeError, "inconsistent"):
            capture.run(args, **self.options)
        with self.assertRaisesRegex(RuntimeError, "failed"):
            capture.finish()
        self.assertIs(self.manifest()["payload"]["complete"], False)

    def test_launch_failure_and_timeout_propagate_and_prevent_completion(self):
        cases = [([str(self.root / "absent-producer")], self.options, FileNotFoundError),
                 ([sys.executable, "-c", "import time; time.sleep(2)"],
                  {**self.options, "timeout": 0.01}, subprocess.TimeoutExpired)]
        for index, (args, options, exception) in enumerate(cases):
            with self.subTest(exception=exception):
                directory = self.root / f"failed-{index}"
                capture = ReplayCapture(directory, self.identity)
                with self.assertRaises(exception):
                    capture.run(args, **options)
                with self.assertRaisesRegex(RuntimeError, "failed"):
                    capture.finish()
                with self.no_producers(), self.assertRaisesRegex(RuntimeError, "incomplete"):
                    ReplayCapture(directory, self.identity, replay=True)

    def test_failed_atomic_completion_keeps_capture_incomplete(self):
        capture = ReplayCapture(self.directory, self.identity)
        capture.run(self.args, **self.options)
        with patch("replay_capture.os.replace", side_effect=OSError("write interrupted")):
            with self.assertRaisesRegex(OSError, "write interrupted"):
                capture.finish()
        self.assertIs(self.manifest()["payload"]["complete"], False)
        self.assertEqual(list(self.directory.glob(".pending-*")), [])
        with self.no_producers(), self.assertRaisesRegex(RuntimeError, "incomplete"):
            ReplayCapture(self.directory, self.identity, replay=True)

    def test_decoding_options_preserve_raw_text_and_diagnostics(self):
        args = [sys.executable, "-c",
                "import sys; sys.stdout.buffer.write(b'\\xff\\x00\\n'); sys.stderr.buffer.write(b'\\xfe\\n')"]
        options = {**self.options, "encoding": "utf-8", "errors": "replace"}
        original = self.capture(args, options)
        self.assertEqual((original.stdout, original.stderr), ("\ufffd\x00\n", "\ufffd\n"))
        with self.no_producers():
            replay = ReplayCapture(self.directory, self.identity, replay=True)
            result = replay.run(args, **options)
        self.assertEqual((result.stdout, result.stderr), (original.stdout, original.stderr))

    @unittest.skipUnless(os.name == "posix", "requires POSIX permission bits")
    def test_capture_files_and_directory_are_owner_only(self):
        self.capture()
        self.assertEqual(self.directory.stat().st_mode & 0o077, 0)
        for path in self.directory.iterdir():
            self.assertEqual(path.stat().st_mode & 0o077, 0)


if __name__ == "__main__":
    unittest.main()
