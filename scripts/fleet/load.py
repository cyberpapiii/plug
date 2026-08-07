#!/usr/bin/env python3
"""Run concurrent sustained MCP tool calls against local mock servers."""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import selectors
import subprocess
import sys
import threading
import time
from typing import Any


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_DURATION_SECONDS = 5 * 60.0
DEFAULT_SESSIONS = 2
DEFAULT_MAX_P99_MS = 250.0
DEFAULT_MAX_ERROR_RATE = 0.01
RESPONSE_TIMEOUT_SECONDS = 10.0
STARTUP_TIMEOUT_SECONDS = 120.0


def parse_duration(value: str) -> float:
    """Parse a positive duration with an s, m, or h suffix."""
    if len(value) < 2:
        raise ValueError("duration must end in s, m, or h")
    factors = {"s": 1.0, "m": 60.0, "h": 3600.0}
    suffix = value[-1].lower()
    if suffix not in factors:
        raise ValueError("duration must end in s, m, or h")
    try:
        duration = float(value[:-1]) * factors[suffix]
    except ValueError as error:
        raise ValueError("duration must be a number followed by s, m, or h") from error
    if not math.isfinite(duration) or duration <= 0:
        raise ValueError("duration must be positive")
    return duration


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def non_negative_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed < 0:
        raise argparse.ArgumentTypeError("must be a non-negative finite number")
    return parsed


def error_rate_limit(value: str) -> float:
    parsed = non_negative_float(value)
    if parsed > 1:
        raise argparse.ArgumentTypeError("must be between 0 and 1")
    return parsed


def percentile(samples: list[float], percentage: int) -> float:
    """Return a nearest-rank percentile from one or more samples."""
    return percentiles(samples, (percentage,))[0]


def percentiles(
    samples: list[float], requested: tuple[int, ...]
) -> tuple[float, ...]:
    """Return nearest-rank percentiles after sorting the samples once."""
    if not samples:
        raise ValueError("cannot calculate a percentile without samples")
    ordered = sorted(samples)
    values = []
    for percentage in requested:
        rank = math.ceil((percentage / 100) * len(ordered))
        values.append(ordered[max(rank - 1, 0)])
    return tuple(values)


def threshold_breaches(
    *,
    p99_ms: float,
    error_rate: float,
    max_p99_ms: float,
    max_error_rate: float,
) -> list[str]:
    breaches = []
    if p99_ms > max_p99_ms:
        breaches.append(f"p99 {p99_ms:.3f}ms > {max_p99_ms:.3f}ms")
    if error_rate > max_error_rate:
        breaches.append(
            f"error rate {error_rate:.3%} > {max_error_rate:.3%}"
        )
    return breaches


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
        "--lifecycle",
        "modern-only",
    ]


class MockSession:
    def __init__(self, session_id: int) -> None:
        self.session_id = session_id
        self.next_request_id = 1
        self.process = subprocess.Popen(
            server_command(),
            cwd=REPO_ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        try:
            self.request(
                "server/discover",
                {},
                timeout_seconds=STARTUP_TIMEOUT_SECONDS,
            )
        except Exception:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
            raise

    def request(
        self,
        method: str,
        params: dict[str, Any],
        timeout_seconds: float = RESPONSE_TIMEOUT_SECONDS,
    ) -> dict[str, Any]:
        request_id = f"load-{self.session_id}-{self.next_request_id}"
        self.next_request_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }
        if self.process.stdin is None:
            raise RuntimeError("mock stdin is unavailable")
        self.process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        self.process.stdin.flush()
        response = self.read_response(timeout_seconds)
        if response.get("id") != request_id:
            raise RuntimeError(
                f"response id mismatch: expected {request_id}, got {response.get('id')}"
            )
        return response

    def call(self) -> bool:
        response = self.request(
            "tools/call",
            {
                "name": "echo",
                "arguments": {"session": self.session_id},
            },
        )
        result = response.get("result")
        return (
            "error" not in response
            and isinstance(result, dict)
            and result.get("isError") is False
        )

    def read_response(self, timeout_seconds: float) -> dict[str, Any]:
        if self.process.stdout is None:
            raise RuntimeError("mock stdout is unavailable")
        selector = selectors.DefaultSelector()
        try:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            if not selector.select(timeout=timeout_seconds):
                raise RuntimeError("timed out waiting for mock response")
        finally:
            selector.close()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(
                f"mock server exited before responding (status={self.process.poll()})"
            )
        response = json.loads(line)
        if not isinstance(response, dict):
            raise RuntimeError("mock response is not a JSON object")
        return response

    def close(self) -> None:
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()

        if self.process.returncode != 0:
            stderr = self.process.stderr.read() if self.process.stderr is not None else ""
            raise RuntimeError(
                f"mock server failed ({self.process.returncode}): {stderr.strip()}"
            )


def drive_session(
    session: MockSession,
    duration_seconds: float,
    start_barrier: threading.Barrier,
    latencies_ms: list[float],
    errors: list[str],
) -> None:
    start_barrier.wait()
    deadline = time.monotonic() + duration_seconds
    while time.monotonic() < deadline:
        started = time.perf_counter()
        try:
            succeeded = session.call()
        except (BrokenPipeError, json.JSONDecodeError, OSError, RuntimeError) as error:
            latencies_ms.append((time.perf_counter() - started) * 1000)
            errors.append(f"session {session.session_id}: {error}")
            return
        latencies_ms.append((time.perf_counter() - started) * 1000)
        if not succeeded:
            errors.append(f"session {session.session_id}: tool call returned an error")


def run_load(sessions_count: int, duration_seconds: float) -> tuple[list[float], list[str]]:
    sessions = []
    try:
        for session_id in range(1, sessions_count + 1):
            sessions.append(MockSession(session_id))

        barrier = threading.Barrier(sessions_count + 1)
        session_latencies = [[] for _ in sessions]
        session_errors = [[] for _ in sessions]
        threads = [
            threading.Thread(
                target=drive_session,
                args=(
                    session,
                    duration_seconds,
                    barrier,
                    session_latencies[index],
                    session_errors[index],
                ),
                name=f"load-session-{session.session_id}",
            )
            for index, session in enumerate(sessions)
        ]
        for thread in threads:
            thread.start()
        barrier.wait()
        for thread in threads:
            thread.join()

        return (
            [latency for group in session_latencies for latency in group],
            [error for group in session_errors for error in group],
        )
    finally:
        close_errors = []
        for session in sessions:
            try:
                session.close()
            except RuntimeError as error:
                close_errors.append(str(error))
        if close_errors and sys.exc_info()[0] is None:
            raise RuntimeError("; ".join(close_errors))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--duration",
        type=parse_duration,
        default=DEFAULT_DURATION_SECONDS,
        metavar="DURATION",
        help="run duration with s/m/h suffix (default: 5m)",
    )
    parser.add_argument(
        "--sessions",
        type=positive_int,
        default=DEFAULT_SESSIONS,
        help="number of concurrent sessions (default: 2)",
    )
    parser.add_argument(
        "--max-p99-ms",
        type=non_negative_float,
        default=DEFAULT_MAX_P99_MS,
        help=f"maximum p99 latency in ms (default: {DEFAULT_MAX_P99_MS:g})",
    )
    parser.add_argument(
        "--max-error-rate",
        type=error_rate_limit,
        default=DEFAULT_MAX_ERROR_RATE,
        help=f"maximum error fraction, 0..1 (default: {DEFAULT_MAX_ERROR_RATE:g})",
    )
    return parser


def main() -> int:
    parser = build_parser()
    try:
        args = parser.parse_args()
    except ValueError as error:
        parser.error(str(error))

    print(
        f"LOAD start sessions={args.sessions} duration={args.duration:g}s "
        f"max_p99={args.max_p99_ms:g}ms "
        f"max_error_rate={args.max_error_rate:.3%}",
        flush=True,
    )
    try:
        latencies_ms, errors = run_load(args.sessions, args.duration)
    except (OSError, RuntimeError, json.JSONDecodeError) as error:
        print(f"LOAD FAIL: {error}", file=sys.stderr)
        return 1

    total_calls = len(latencies_ms)
    if total_calls == 0:
        print("LOAD FAIL: no tool calls completed", file=sys.stderr)
        return 1

    p50_ms, p95_ms, p99_ms = percentiles(latencies_ms, (50, 95, 99))
    error_rate = len(errors) / total_calls
    print(
        f"LOAD metrics total_calls={total_calls} errors={len(errors)} "
        f"error_rate={error_rate:.3%} p50_ms={p50_ms:.3f} "
        f"p95_ms={p95_ms:.3f} p99_ms={p99_ms:.3f}"
    )

    breaches = threshold_breaches(
        p99_ms=p99_ms,
        error_rate=error_rate,
        max_p99_ms=args.max_p99_ms,
        max_error_rate=args.max_error_rate,
    )
    if breaches:
        for breach in breaches:
            print(f"LOAD threshold breach: {breach}", file=sys.stderr)
        print("LOAD FAIL", file=sys.stderr)
        return 1
    print("LOAD PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
