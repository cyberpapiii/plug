#!/usr/bin/env python3
"""Focused tests for the fleet observability gate."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("obs.py")
SPEC = importlib.util.spec_from_file_location("fleet_obs", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
obs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(obs)


class HistogramTests(unittest.TestCase):
    def test_assigns_each_latency_to_one_cumulative_bucket(self) -> None:
        histogram = obs.latency_histogram([0.5, 5.0, 6.0, 1001.0])

        self.assertEqual(
            histogram,
            [
                ("<=1ms", 1),
                ("<=5ms", 2),
                ("<=10ms", 3),
                ("<=25ms", 3),
                ("<=50ms", 3),
                ("<=100ms", 3),
                ("<=250ms", 3),
                ("<=500ms", 3),
                ("<=1000ms", 3),
                ("+Inf", 4),
            ],
        )


class ErrorTaxonomyTests(unittest.TestCase):
    def test_classifies_expected_outcomes(self) -> None:
        self.assertEqual(obs.classify_error(None, True), "success")
        self.assertEqual(obs.classify_error(None, False), "tool_error")
        self.assertEqual(
            obs.classify_error(RuntimeError("timed out awaiting tools/call"), False),
            "timeout",
        )
        self.assertEqual(
            obs.classify_error(ValueError("invalid response"), False),
            "protocol_error",
        )
        self.assertEqual(
            obs.classify_error(BrokenPipeError("closed"), False),
            "transport_error",
        )


class RequiredSignalTests(unittest.TestCase):
    def test_accepts_complete_signal_set(self) -> None:
        signals = {
            "latency_histogram": True,
            "error_taxonomy": True,
            "in_flight": True,
            "rss_samples": True,
            "fd_samples": True,
            "stderr_assert": True,
        }

        self.assertEqual(obs.missing_required_signals(signals), [])

    def test_fails_closed_when_any_required_signal_is_missing(self) -> None:
        signals = {
            "latency_histogram": True,
            "error_taxonomy": True,
            "in_flight": True,
            "rss_samples": True,
            "fd_samples": False,
            "stderr_assert": True,
        }

        self.assertEqual(obs.missing_required_signals(signals), ["fd_samples"])


class StderrAssertionTests(unittest.TestCase):
    def test_allows_expected_diagnostics(self) -> None:
        stderr = (
            "INFO mock_mcp_server: starting mock MCP server\n"
            "WARN rmcp::service: response error id=2\n"
        )

        self.assertEqual(obs.stderr_violations(stderr), [])

    def test_rejects_crash_signatures(self) -> None:
        stderr = "thread 'tokio-runtime-worker' panicked at src/main.rs:1\n"

        self.assertEqual(
            obs.stderr_violations(stderr),
            ["thread 'tokio-runtime-worker' panicked at src/main.rs:1"],
        )


if __name__ == "__main__":
    unittest.main()
