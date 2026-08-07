#!/usr/bin/env python3
"""Focused tests for the fleet load gate."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("load.py")
SPEC = importlib.util.spec_from_file_location("fleet_load", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
load = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(load)


class PercentileTests(unittest.TestCase):
    def test_uses_nearest_rank(self) -> None:
        samples = [1.0, 2.0, 3.0, 4.0, 100.0]

        self.assertEqual(load.percentile(samples, 50), 3.0)
        self.assertEqual(load.percentile(samples, 95), 100.0)
        self.assertEqual(load.percentile(samples, 99), 100.0)

    def test_rejects_empty_samples(self) -> None:
        with self.assertRaisesRegex(ValueError, "no latency samples"):
            load.percentile([], 50)


class ThresholdTests(unittest.TestCase):
    def test_passes_at_threshold_boundaries(self) -> None:
        breaches = load.threshold_breaches(
            p95_ms=250.0,
            p99_ms=1000.0,
            error_rate_pct=1.0,
            max_p95_ms=250.0,
            max_p99_ms=1000.0,
            max_error_rate_pct=1.0,
        )

        self.assertEqual(breaches, [])

    def test_reports_each_breached_threshold(self) -> None:
        breaches = load.threshold_breaches(
            p95_ms=251.0,
            p99_ms=1001.0,
            error_rate_pct=1.1,
            max_p95_ms=250.0,
            max_p99_ms=1000.0,
            max_error_rate_pct=1.0,
        )

        self.assertEqual(
            breaches,
            [
                "p95 251.00ms > 250.00ms",
                "p99 1001.00ms > 1000.00ms",
                "error rate 1.100% > 1.000%",
            ],
        )


if __name__ == "__main__":
    unittest.main()
