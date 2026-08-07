#!/usr/bin/env python3
"""Focused tests for the fleet fault gate."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("fault.py")
SPEC = importlib.util.spec_from_file_location("fleet_fault", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
fault = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fault)


class ScenarioDefinitionTests(unittest.TestCase):
    def test_covers_requested_fault_classes(self) -> None:
        self.assertEqual(
            [scenario.name for scenario in fault.SCENARIOS],
            ["malformed-frame", "reset", "slow-delay", "sigterm", "auth-expiry"],
        )

    def test_each_scenario_declares_failure_and_recovery_expectations(self) -> None:
        for scenario in fault.SCENARIOS:
            with self.subTest(scenario=scenario.name):
                self.assertTrue(scenario.expected_failure)
                self.assertTrue(scenario.expected_recovery)


class OutcomeTests(unittest.TestCase):
    def test_expected_failure_and_recovery_passes(self) -> None:
        outcome = fault.evaluate_outcome(
            fault.SCENARIOS[0], failure_observed=True, recovery_observed=True
        )

        self.assertEqual(outcome, "PASS")

    def test_missing_expected_failure_fails(self) -> None:
        outcome = fault.evaluate_outcome(
            fault.SCENARIOS[0], failure_observed=False, recovery_observed=True
        )

        self.assertEqual(outcome, "FAIL")


if __name__ == "__main__":
    unittest.main()
