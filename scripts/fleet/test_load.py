#!/usr/bin/env python3
"""Focused tests for the fleet concurrent load rig."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("load.py")
SPEC = importlib.util.spec_from_file_location("fleet_load", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
load = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(load)


class DurationTests(unittest.TestCase):
    def test_accepts_seconds_minutes_and_hours(self) -> None:
        self.assertEqual(load.parse_duration("10s"), 10.0)
        self.assertEqual(load.parse_duration("5m"), 300.0)
        self.assertEqual(load.parse_duration("1.5h"), 5400.0)

    def test_rejects_non_positive_duration(self) -> None:
        with self.assertRaises(ValueError):
            load.parse_duration("0s")


class TimeoutTests(unittest.TestCase):
    def test_startup_timeout_allows_initial_cargo_build(self) -> None:
        self.assertGreater(
            load.STARTUP_TIMEOUT_SECONDS,
            load.RESPONSE_TIMEOUT_SECONDS,
        )


class PercentileTests(unittest.TestCase):
    def test_uses_nearest_rank(self) -> None:
        samples = list(range(1, 101))

        self.assertEqual(load.percentile(samples, 50), 50)
        self.assertEqual(load.percentile(samples, 95), 95)
        self.assertEqual(load.percentile(samples, 99), 99)

    def test_single_sample_is_every_percentile(self) -> None:
        self.assertEqual(load.percentile([7.5], 99), 7.5)

    def test_calculates_requested_percentiles_together(self) -> None:
        samples = list(range(100, 0, -1))

        self.assertEqual(
            load.percentiles(samples, (50, 95, 99)),
            (50, 95, 99),
        )


class ThresholdTests(unittest.TestCase):
    def test_reports_p99_breach(self) -> None:
        breaches = load.threshold_breaches(
            p99_ms=12.0,
            error_rate=0.0,
            max_p99_ms=10.0,
            max_error_rate=0.01,
        )

        self.assertEqual(breaches, ["p99 12.000ms > 10.000ms"])

    def test_threshold_is_inclusive(self) -> None:
        breaches = load.threshold_breaches(
            p99_ms=10.0,
            error_rate=0.01,
            max_p99_ms=10.0,
            max_error_rate=0.01,
        )

        self.assertEqual(breaches, [])

    def test_reports_error_rate_breach(self) -> None:
        breaches = load.threshold_breaches(
            p99_ms=1.0,
            error_rate=0.25,
            max_p99_ms=10.0,
            max_error_rate=0.0,
        )

        self.assertEqual(breaches, ["error rate 25.000% > 0.000%"])


if __name__ == "__main__":
    unittest.main()
