#!/usr/bin/env python3
"""Focused tests for the fleet baseline summary."""

import importlib.util
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("baseline.py")
SPEC = importlib.util.spec_from_file_location("fleet_baseline", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
baseline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(baseline)


class BaselineTests(unittest.TestCase):
    def test_writes_full_fleet_table_and_passes_when_fast_stages_pass(self) -> None:
        outputs = {
            "conformance": (0, "checks           2 (passed=2 failed=0)\n"),
            "golden": (0, "GOLDEN lifecycle.json PASS\n"),
            "contract": (0, "CONTRACT mock-lists PASS\n"),
        }

        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "BASELINE.md"
            result, rows = baseline.run_baseline(
                lambda stage: outputs[stage],
                artifact,
                commit="abc123",
                recorded_at="2026-08-07T22:00:00Z",
            )

            self.assertEqual(result, 0)
            self.assertEqual([row.status for row in rows[:3]], ["PASS"] * 3)
            report = artifact.read_text()
            self.assertIn("| conformance | required | PASS | 2/2 checks |", report)
            self.assertIn("| golden | required | PASS | 1 fixture |", report)
            self.assertIn("| contract | required | PASS | 1 snapshot |", report)
            self.assertIn("| load | opt-in | SKIP | default 2 × 5m |", report)
            self.assertIn("| fault | opt-in | SKIP | deterministic faults |", report)
            self.assertIn("| obs | opt-in | SKIP | default 2 × 5s |", report)

    def test_runs_every_fast_stage_and_fails_if_any_stage_fails(self) -> None:
        called = []

        def run_stage(stage: str) -> tuple[int, str]:
            called.append(stage)
            return (1 if stage == "golden" else 0, f"STAGE {stage}\n")

        with tempfile.TemporaryDirectory() as directory:
            artifact = pathlib.Path(directory) / "BASELINE.md"
            result, rows = baseline.run_baseline(
                run_stage,
                artifact,
                commit="def456",
                recorded_at="2026-08-07T22:01:00Z",
            )

            self.assertEqual(called, ["conformance", "golden", "contract"])
            self.assertEqual(result, 1)
            self.assertEqual([row.status for row in rows[:3]], ["PASS", "FAIL", "PASS"])
            self.assertIn("| golden | required | FAIL | 0 fixtures |", artifact.read_text())


if __name__ == "__main__":
    unittest.main()
