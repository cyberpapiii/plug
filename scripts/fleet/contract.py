#!/usr/bin/env python3
"""Check or regenerate stable MCP list-contract snapshots from the mock server."""

from __future__ import annotations

import argparse
import difflib
import json
import pathlib
import selectors
import subprocess
import sys
from typing import Any


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_SNAPSHOT = (
    REPO_ROOT / "docs" / "testing" / "fleet-contract" / "mock-lists.json"
)
SERVER_ARGS = [
    "--tools",
    "echo,greet",
    "--resources",
    "--resource-templates",
    "--prompts",
]
LIST_METHODS = (
    "tools/list",
    "resources/list",
    "resources/templates/list",
    "prompts/list",
)


def normalize_response(value: Any) -> Any:
    """Sort JSON objects and mask only the top-level JSON-RPC request ID."""
    if not isinstance(value, dict):
        return value
    return {
        key: "<masked:id>" if key == "id" else normalize_value(item)
        for key, item in sorted(value.items())
    }


def normalize_value(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: normalize_value(item) for key, item in sorted(value.items())}
    if isinstance(value, list):
        return [normalize_value(item) for item in value]
    return value


def normalized_text(value: Any) -> str:
    return json.dumps(normalize_response(value), indent=2, sort_keys=True) + "\n"


def contract_diff(
    expected: Any, actual: Any, expected_name: str, actual_name: str
) -> str:
    return "".join(
        difflib.unified_diff(
            normalized_text(expected).splitlines(keepends=True),
            normalized_text(actual).splitlines(keepends=True),
            fromfile=expected_name,
            tofile=actual_name,
        )
    )


def server_command() -> list[str]:
    return [
        "cargo",
        "run",
        "--quiet",
        "-p",
        "plug-test-harness",
        "--bin",
        "mock-mcp-server",
        "--",
        *SERVER_ARGS,
    ]


def read_response(process: subprocess.Popen[str], method: str, timeout: int) -> Any:
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    if not selector.select(timeout=timeout):
        raise RuntimeError(f"{method}: timed out waiting for mock response")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(
            f"{method}: mock server exited before responding "
            f"(status={process.poll()})"
        )
    return json.loads(line)


def send(process: subprocess.Popen[str], request: dict[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
    process.stdin.flush()


def capture_contract() -> dict[str, Any]:
    process = subprocess.Popen(
        server_command(),
        cwd=REPO_ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    try:
        send(
            process,
            {
                "jsonrpc": "2.0",
                "id": "contract-initialize",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "fleet-contract", "version": "1.0"},
                },
            },
        )
        read_response(process, "initialize", 120)
        send(
            process,
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
        )

        responses = []
        for index, method in enumerate(LIST_METHODS, start=1):
            send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": f"contract-list-{index}",
                    "method": method,
                    "params": {},
                },
            )
            responses.append(
                {
                    "method": method,
                    "response": normalize_response(
                        read_response(process, method, timeout=10)
                    ),
                }
            )
    finally:
        if process.stdin is not None:
            process.stdin.close()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=2)

    if process.returncode != 0:
        stderr = process.stderr.read() if process.stderr is not None else ""
        raise RuntimeError(f"mock server failed ({process.returncode}): {stderr}")
    return {
        "name": "mock MCP list contracts",
        "serverArgs": SERVER_ARGS,
        "responses": responses,
    }


def check(snapshot: pathlib.Path) -> int:
    expected = json.loads(snapshot.read_text())
    actual = capture_contract()
    diff = contract_diff(expected, actual, str(snapshot), f"{snapshot} (observed)")
    if diff:
        print(diff, end="", file=sys.stderr)
        print("CONTRACT mock-lists FAIL", file=sys.stderr)
        return 1
    print("CONTRACT mock-lists PASS")
    return 0


def regenerate(snapshot: pathlib.Path) -> int:
    observed = capture_contract()
    snapshot.parent.mkdir(parents=True, exist_ok=True)
    snapshot.write_text(json.dumps(observed, indent=2, sort_keys=True) + "\n")
    print(f"RECORDED {snapshot}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "regen"))
    parser.add_argument(
        "--snapshot",
        type=pathlib.Path,
        default=DEFAULT_SNAPSHOT,
        help=f"snapshot path (default: {DEFAULT_SNAPSHOT})",
    )
    args = parser.parse_args()
    try:
        if args.command == "regen":
            return regenerate(args.snapshot)
        return check(args.snapshot)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"contract: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
