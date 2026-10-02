import copy
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


def load_script(name):
    path = Path(__file__).resolve().parents[1] / name
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


retention = load_script("test-artifact-retention.py")
guard = load_script("run-workspace-tests.py")


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.rows = [
            {"path": f"/target/debug/build/bootty-ui/{index:016x}", "package": "bootty-ui",
             "fingerprint": "test-integration-test-settings", "allocated_bytes": 100 + index,
             "newest_mtime": index,
             "identity": {"target": 1, "profile": 2, "features": "[]", "rustflags": [],
                          "rustc": 3, "compile_kind": 0, "config": 4, "path": 5}}
            for index in range(1, 5)
        ]

    def plan(self, **kwargs):
        return retention.retention_plan(self.rows, now=10000, **kwargs)

    def test_retains_two_newest_and_reports_allocated_before_after(self):
        result = self.plan()
        self.assertEqual([row["newest_mtime"] for row in result["candidates"]], [2, 1])
        self.assertEqual(result["known_bytes_before"], 410)
        self.assertEqual(result["candidate_bytes"], 203)
        self.assertEqual(result["known_bytes_after_proposed_retention"], 207)

    def test_every_identity_dimension_keeps_variants_separate(self):
        for field in retention.IDENTITY_FIELDS:
            with self.subTest(field=field):
                rows = copy.deepcopy(self.rows)
                for index, row in enumerate(rows):
                    row["identity"][field] = [f"flag-{index}"] if field == "rustflags" else str(index)
                result = retention.retention_plan(rows, now=10000)
                self.assertEqual(len(result["candidates"]), 0)
                self.assertEqual(result["identity_groups"], 4)

    def test_other_target_and_package_are_never_collapsed(self):
        self.rows[0]["fingerprint"] = "test-integration-test-other"
        self.rows[1]["package"] = "other"
        self.assertEqual(self.plan()["candidates"], [])

    def test_live_older_reader_is_protected(self):
        result = self.plan(open_paths=[self.rows[0]["path"] + "/out/settings"])
        self.assertEqual([row["newest_mtime"] for row in result["candidates"]], [2])

    def test_unverified_readers_protect_every_generation(self):
        self.assertEqual(self.plan(readers_verified=False)["candidates"], [])

    def test_recent_and_future_outputs_are_protected(self):
        self.rows[0]["newest_mtime"] = 9900
        self.rows[1]["newest_mtime"] = 11000
        result = self.plan()
        self.assertTrue(all("recent or future-dated output" in row["protection_reasons"]
                            for row in result["retained"]))

    def test_keep_less_than_two_is_rejected(self):
        with self.assertRaises(ValueError):
            self.plan(keep=1)

    @unittest.skipUnless(os.name == "posix", "POSIX per-artifact layout")
    def test_inventory_protects_symlinks_unknown_and_incomplete_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / "bootty-ui"
            package.mkdir()
            generation = package / ("a" * 16)
            (generation / "fingerprint").mkdir(parents=True)
            (generation / "out").mkdir()
            fingerprint = generation / "fingerprint/test-integration-test-settings.json"
            fingerprint.write_text(json.dumps(self.rows[0]["identity"]))
            artifact = generation / ("out/settings-" + "a" * 16)
            artifact.write_bytes(b"compiled fixture")
            rows, unknown = retention.inventory(root)
            self.assertEqual(len(rows), 1)
            before = rows[0]["snapshot_sha256"]
            artifact.write_bytes(b"changed compiled fixture")
            self.assertNotEqual(retention.inventory(root)[0][0]["snapshot_sha256"], before)
            foreign = generation / "out/user-note.txt"
            foreign.write_text("unrecognized content")
            self.assertEqual(retention.inventory(root)[0], [])
            foreign.unlink()
            (generation / "out/foreign").symlink_to(root)
            rows, unknown = retention.inventory(root)
            self.assertEqual(rows, [])
            self.assertEqual(len(unknown), 1)
            self.assertTrue(artifact.exists())
            (generation / "out/foreign").unlink()
            fingerprint.unlink()
            self.assertEqual(retention.inventory(root)[0], [])

    @unittest.skipUnless(retention.fcntl is not None, "POSIX build locks")
    def test_busy_build_lock_stops_retention(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "build.lock"
            with retention.idle_locks([path]):
                with self.assertRaises(BlockingIOError):
                    with retention.idle_locks([path]):
                        self.fail("busy lock was accepted")

    def test_manifest_cannot_overwrite_build_artifacts(self):
        script = Path(__file__).resolve().parents[1] / "test-artifact-retention.py"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            artifact = target / "keep-me"
            artifact.write_text("compiled output")
            result = subprocess.run([sys.executable, str(script), "--target-dir", str(target),
                                     "--manifest", str(artifact)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(artifact.read_text(), "compiled output")


class DiskGuardTests(unittest.TestCase):
    def test_budget_and_reserve_have_independent_boundaries(self):
        self.assertIsNone(guard.disk_stop_reason(100, 80, reserve=50, budget=20))
        self.assertEqual(guard.disk_stop_reason(100, 79, reserve=50, budget=20),
                         "disk growth exceeded the build budget")
        self.assertEqual(guard.disk_stop_reason(100, 49, reserve=50, budget=60),
                         "free space fell below the reserve")

    def test_nonfinite_and_nonpositive_budgets_are_rejected(self):
        import argparse
        for value in ("nan", "inf", "-1", "0"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                guard.positive_gib(value)

    def test_insufficient_space_stops_before_cargo(self):
        script = Path(__file__).resolve().parents[1] / "run-workspace-tests.py"
        result = subprocess.run([sys.executable, str(script), "--growth-budget-gib", "1000000", "--check-only"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('"status": "blocked"', result.stderr)

    @unittest.skipUnless(os.name == "posix", "POSIX process-group isolation")
    def test_stopping_owned_group_leaves_other_process_alive(self):
        command = [sys.executable, "-c", "import time; time.sleep(60)"]
        owned = subprocess.Popen(command, start_new_session=True)
        other = subprocess.Popen(command, start_new_session=True)
        try:
            guard.stop_child(owned)
            self.assertIsNotNone(owned.poll())
            self.assertIsNone(other.poll())
        finally:
            guard.stop_child(owned)
            guard.stop_child(other)


if __name__ == "__main__":
    unittest.main()
