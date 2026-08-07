#!/usr/bin/env python3
"""Focused tests for the fleet golden transcript oracle."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("golden.py")
SPEC = importlib.util.spec_from_file_location("fleet_golden", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
golden = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(golden)


class NormalizeTests(unittest.TestCase):
    def test_masks_ids_timestamps_and_known_volatile_fields(self) -> None:
        value = {
            "jsonrpc": "2.0",
            "id": "request-37",
            "result": {
                "taskId": "task-f86c",
                "createdAt": "2026-08-07T21:00:00Z",
                "lastUpdatedAt": 1786136400,
                "requestId": "reverse-request-9",
                "progressToken": "progress-2",
                "sessionId": "session-abc",
                "stable": "keep-me",
            },
        }

        normalized = golden.normalize(value)

        self.assertEqual(normalized["id"], "<masked:id>")
        self.assertEqual(normalized["result"]["taskId"], "<masked:taskId>")
        self.assertEqual(normalized["result"]["createdAt"], "<masked:createdAt>")
        self.assertEqual(
            normalized["result"]["lastUpdatedAt"], "<masked:lastUpdatedAt>"
        )
        self.assertEqual(normalized["result"]["requestId"], "<masked:requestId>")
        self.assertEqual(
            normalized["result"]["progressToken"], "<masked:progressToken>"
        )
        self.assertEqual(normalized["result"]["sessionId"], "<masked:sessionId>")
        self.assertEqual(normalized["result"]["stable"], "keep-me")

    def test_semantic_mutation_survives_normalization(self) -> None:
        expected = {"id": 1, "result": {"content": [{"text": "original"}]}}
        actual = {"id": 99, "result": {"content": [{"text": "mutated"}]}}

        diff = golden.normalized_diff(expected, actual, "expected", "actual")

        self.assertIn('-        "text": "original"', diff)
        self.assertIn('+        "text": "mutated"', diff)
        self.assertNotIn("-  \"id\": 1", diff)
        self.assertNotIn("+  \"id\": 99", diff)


if __name__ == "__main__":
    unittest.main()
