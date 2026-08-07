#!/usr/bin/env python3
"""Focused tests for the fleet list-contract oracle."""

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("contract.py")
SPEC = importlib.util.spec_from_file_location("fleet_contract", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)


class NormalizeTests(unittest.TestCase):
    def test_masks_only_json_rpc_request_ids(self) -> None:
        value = {
            "id": "contract-tools-41",
            "jsonrpc": "2.0",
            "result": {
                "tools": [
                    {
                        "name": "echo",
                        "inputSchema": {
                            "$id": "https://example.test/echo.schema.json",
                            "type": "object",
                        },
                    }
                ]
            },
        }

        normalized = contract.normalize_response(value)

        self.assertEqual(normalized["id"], "<masked:id>")
        self.assertEqual(normalized["result"]["tools"][0]["name"], "echo")
        self.assertEqual(
            normalized["result"]["tools"][0]["inputSchema"]["$id"],
            "https://example.test/echo.schema.json",
        )

    def test_tool_name_and_schema_mutations_remain_visible(self) -> None:
        expected = {
            "id": 1,
            "result": {
                "tools": [{"name": "echo", "inputSchema": {"type": "object"}}]
            },
        }
        actual = {
            "id": 99,
            "result": {
                "tools": [{"name": "renamed", "inputSchema": {"type": "string"}}]
            },
        }

        diff = contract.contract_diff(expected, actual, "snapshot", "observed")

        self.assertIn('-        "name": "echo"', diff)
        self.assertIn('+        "name": "renamed"', diff)
        self.assertIn('-          "type": "object"', diff)
        self.assertIn('+          "type": "string"', diff)
        self.assertNotIn('-  "id": 1', diff)
        self.assertNotIn('+  "id": 99', diff)


if __name__ == "__main__":
    unittest.main()
