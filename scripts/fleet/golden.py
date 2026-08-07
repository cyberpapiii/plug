#!/usr/bin/env python3
"""Record and replay small golden MCP JSON-RPC sessions."""

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
DEFAULT_FIXTURE_DIR = REPO_ROOT / "docs" / "testing" / "fleet-golden"
VOLATILE_FIELDS = frozenset(
    {
        "id",
        "requestId",
        "taskId",
        "sessionId",
        "progressToken",
        "createdAt",
        "lastUpdatedAt",
        "timestamp",
    }
)


def normalize(value: Any) -> Any:
    """Recursively replace protocol values that legitimately change per run."""
    if isinstance(value, dict):
        return {
            key: f"<masked:{key}>" if key in VOLATILE_FIELDS else normalize(item)
            for key, item in sorted(value.items())
        }
    if isinstance(value, list):
        return [normalize(item) for item in value]
    return value


def normalized_text(value: Any) -> str:
    return json.dumps(normalize(value), indent=2, sort_keys=True) + "\n"


def normalized_diff(
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


def server_command(server_args: list[str]) -> list[str]:
    return [
        "cargo",
        "run",
        "--quiet",
        "-p",
        "plug-test-harness",
        "--bin",
        "mock-mcp-server",
        "--",
        *server_args,
    ]


def read_response(
    process: subprocess.Popen[str], fixture_name: str, timeout_seconds: int
) -> Any:
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    if not selector.select(timeout=timeout_seconds):
        raise RuntimeError(f"{fixture_name}: timed out waiting for mock response")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(
            f"{fixture_name}: mock server exited before responding "
            f"(status={process.poll()})"
        )
    return json.loads(line)


def run_session(fixture: dict[str, Any], fixture_name: str) -> dict[str, Any]:
    server_args = fixture.get("serverArgs")
    exchanges = fixture.get("exchanges")
    if not isinstance(server_args, list) or not all(
        isinstance(item, str) for item in server_args
    ):
        raise ValueError(f"{fixture_name}: serverArgs must be a string array")
    if not isinstance(exchanges, list) or not exchanges:
        raise ValueError(f"{fixture_name}: exchanges must be a non-empty array")

    process = subprocess.Popen(
        server_command(server_args),
        cwd=REPO_ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    observed: list[dict[str, Any]] = []
    try:
        assert process.stdin is not None
        for index, exchange in enumerate(exchanges):
            request = exchange.get("request")
            if not isinstance(request, dict):
                raise ValueError(
                    f"{fixture_name}: exchange {index} request must be an object"
                )
            process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
            process.stdin.flush()
            observed.append(
                {
                    "request": request,
                    "response": read_response(
                        process, fixture_name, 120 if index == 0 else 10
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
        raise RuntimeError(
            f"{fixture_name}: mock server failed ({process.returncode}): {stderr}"
        )
    return {
        "name": fixture.get("name", fixture_name),
        "serverArgs": server_args,
        "exchanges": observed,
    }


def fixture_paths(directory: pathlib.Path) -> list[pathlib.Path]:
    paths = sorted(directory.glob("*.json"))
    if not paths:
        raise ValueError(f"no golden fixtures found in {directory}")
    return paths


def replay(directory: pathlib.Path) -> int:
    failed = 0
    for path in fixture_paths(directory):
        expected = json.loads(path.read_text())
        actual = run_session(expected, path.name)
        diff = normalized_diff(expected, actual, str(path), f"{path} (actual)")
        if diff:
            failed += 1
            print(diff, end="", file=sys.stderr)
            print(f"GOLDEN {path.name} FAIL", file=sys.stderr)
        else:
            print(f"GOLDEN {path.name} PASS")
    return 1 if failed else 0


def record(directory: pathlib.Path) -> int:
    for path in fixture_paths(directory):
        fixture = json.loads(path.read_text())
        observed = run_session(fixture, path.name)
        path.write_text(json.dumps(observed, indent=2, sort_keys=True) + "\n")
        print(f"RECORDED {path}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("record", "replay"))
    parser.add_argument(
        "--fixtures",
        type=pathlib.Path,
        default=DEFAULT_FIXTURE_DIR,
        help=f"fixture directory (default: {DEFAULT_FIXTURE_DIR})",
    )
    args = parser.parse_args()
    try:
        if args.command == "record":
            return record(args.fixtures)
        return replay(args.fixtures)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"golden: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
