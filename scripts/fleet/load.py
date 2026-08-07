#!/usr/bin/env python3
"""Run concurrent MCP sessions through Plug against its mock upstream."""

from __future__ import annotations

import json
import math
import os
import pathlib
import selectors
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_DURATION_SECS = 300
DEFAULT_SESSIONS = 2
DEFAULT_P95_MS = 250.0
DEFAULT_P99_MS = 1000.0
DEFAULT_ERROR_RATE_PCT = 1.0
RESPONSE_TIMEOUT_SECS = 10.0


def percentile(samples: list[float], rank: int) -> float:
    """Return a nearest-rank percentile."""
    if not samples:
        raise ValueError("no latency samples")
    ordered = sorted(samples)
    index = max(0, math.ceil((rank / 100) * len(ordered)) - 1)
    return ordered[index]


def threshold_breaches(
    *,
    p95_ms: float,
    p99_ms: float,
    error_rate_pct: float,
    max_p95_ms: float,
    max_p99_ms: float,
    max_error_rate_pct: float,
) -> list[str]:
    breaches = []
    if p95_ms > max_p95_ms:
        breaches.append(f"p95 {p95_ms:.2f}ms > {max_p95_ms:.2f}ms")
    if p99_ms > max_p99_ms:
        breaches.append(f"p99 {p99_ms:.2f}ms > {max_p99_ms:.2f}ms")
    if error_rate_pct > max_error_rate_pct:
        breaches.append(
            f"error rate {error_rate_pct:.3f}% > {max_error_rate_pct:.3f}%"
        )
    return breaches


def positive_int_env(name: str, default: int) -> int:
    raw = os.environ.get(name, str(default))
    try:
        value = int(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be an integer") from error
    if value <= 0:
        raise ValueError(f"{name} must be greater than zero")
    return value


def nonnegative_float_env(name: str, default: float) -> float:
    raw = os.environ.get(name, str(default))
    try:
        value = float(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be a number") from error
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"{name} must be a finite non-negative number")
    return value


def build_binaries() -> tuple[pathlib.Path, pathlib.Path]:
    subprocess.run(
        [
            "cargo",
            "build",
            "--quiet",
            "-p",
            "plug-mcp",
            "--bin",
            "plug",
            "-p",
            "plug-test-harness",
            "--bin",
            "mock-mcp-server",
        ],
        cwd=REPO_ROOT,
        check=True,
    )
    target_dir = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    return target_dir / "debug" / "plug", target_dir / "debug" / "mock-mcp-server"


def write_config(path: pathlib.Path, mock_server: pathlib.Path, sessions: int) -> None:
    path.write_text(
        "\n".join(
            [
                'log_level = "warn"',
                "daemon_grace_period_secs = 1",
                "",
                "[servers.mock]",
                f"command = {json.dumps(str(mock_server))}",
                'args = ["--tools", "echo"]',
                'transport = "stdio"',
                "enabled = true",
                "timeout_secs = 30",
                "call_timeout_secs = 10",
                f"max_concurrent = {sessions}",
                "",
            ]
        )
    )


class McpSession:
    def __init__(
        self, plug: pathlib.Path, config: pathlib.Path, env: dict[str, str], index: int
    ) -> None:
        self.index = index
        self.next_id = 1
        self.process = subprocess.Popen(
            [str(plug), "--config", str(config), "connect"],
            cwd=REPO_ROOT,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )

    def close(self) -> None:
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=2)

    def request(
        self, method: str, params: dict[str, Any], timeout: float = RESPONSE_TIMEOUT_SECS
    ) -> dict[str, Any]:
        request_id = f"load-{self.index}-{self.next_id}"
        self.next_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }
        if self.process.stdin is None or self.process.stdout is None:
            raise RuntimeError("session pipes are unavailable")
        self.process.stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

        selector = selectors.DefaultSelector()
        selector.register(self.process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout
        try:
            while time.monotonic() < deadline:
                remaining = deadline - time.monotonic()
                if not selector.select(timeout=remaining):
                    break
                line = self.process.stdout.readline()
                if not line:
                    raise RuntimeError(
                        f"session exited while awaiting {method} "
                        f"(status={self.process.poll()})"
                    )
                response = json.loads(line)
                if response.get("id") == request_id:
                    return response
        finally:
            selector.close()
        raise RuntimeError(f"timed out awaiting {method}")

    def initialize(self) -> None:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "fleet-load", "version": "1.0"},
            },
            timeout=120,
        )
        if "error" in response:
            raise RuntimeError(f"initialize failed: {response['error']}")
        assert self.process.stdin is not None
        self.process.stdin.write(
            '{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
        )
        self.process.stdin.flush()

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            listed = self.request("tools/list", {})
            tools = listed.get("result", {}).get("tools", [])
            if any(tool.get("name") == "Mock__echo" for tool in tools):
                return
            time.sleep(0.05)
        raise RuntimeError("Mock__echo did not become ready")

    def call_echo(self) -> bool:
        response = self.request(
            "tools/call",
            {"name": "Mock__echo", "arguments": {"message": "fleet-load"}},
        )
        return "error" not in response and not response.get("result", {}).get(
            "isError", False
        )


class SessionResult:
    def __init__(self) -> None:
        self.latencies_ms: list[float] = []
        self.calls = 0
        self.errors = 0
        self.fatal_error: str | None = None


def run_calls(
    session: McpSession,
    start: threading.Barrier,
    duration_secs: int,
    result: SessionResult,
) -> None:
    try:
        start.wait()
        deadline = time.monotonic() + duration_secs
        while time.monotonic() < deadline:
            began = time.perf_counter()
            result.calls += 1
            try:
                succeeded = session.call_echo()
            except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
                result.errors += 1
                result.fatal_error = str(error)
                return
            result.latencies_ms.append((time.perf_counter() - began) * 1000)
            if not succeeded:
                result.errors += 1
    except threading.BrokenBarrierError:
        result.fatal_error = "load start barrier broke"


def aggregate_results(
    results: list[SessionResult],
) -> tuple[list[float], int, int, list[str]]:
    latencies = [latency for result in results for latency in result.latencies_ms]
    calls = sum(result.calls for result in results)
    errors = sum(result.errors for result in results)
    fatal_errors = [
        f"session {index}: {result.fatal_error}"
        for index, result in enumerate(results, start=1)
        if result.fatal_error is not None
    ]
    return latencies, calls, errors, fatal_errors


def wait_for_daemon(
    daemon: subprocess.Popen[bytes], socket_path: pathlib.Path
) -> None:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if socket_path.exists():
            return
        status = daemon.poll()
        if status is not None:
            raise RuntimeError(f"Plug daemon exited during startup (status={status})")
        time.sleep(0.05)
    raise RuntimeError("timed out waiting for Plug daemon")


def execute(
    duration_secs: int, session_count: int
) -> tuple[list[float], int, int, list[str]]:
    plug, mock_server = build_binaries()
    temp_dir = pathlib.Path(tempfile.mkdtemp(prefix="plug-fleet-load-"))
    daemon: subprocess.Popen[bytes] | None = None
    sessions: list[McpSession] = []
    try:
        runtime_root = temp_dir / "runtime"
        state_root = temp_dir / "state"
        runtime_root.mkdir()
        state_root.mkdir()
        config = temp_dir / "plug.toml"
        write_config(config, mock_server, session_count)
        env = os.environ.copy()
        env["XDG_RUNTIME_DIR"] = str(runtime_root)
        env["XDG_STATE_HOME"] = str(state_root)
        daemon = subprocess.Popen(
            [str(plug), "--config", str(config), "serve", "--daemon"],
            cwd=REPO_ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        wait_for_daemon(daemon, runtime_root / "plug" / "plug.sock")

        sessions = [
            McpSession(plug, config, env, index)
            for index in range(1, session_count + 1)
        ]
        for session in sessions:
            session.initialize()

        barrier = threading.Barrier(session_count)
        results = [SessionResult() for _ in sessions]
        threads = [
            threading.Thread(
                target=run_calls,
                args=(session, barrier, duration_secs, result),
                name=f"fleet-load-{session.index}",
            )
            for session, result in zip(sessions, results, strict=True)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        return aggregate_results(results)
    finally:
        for session in sessions:
            session.close()
        if daemon is not None and daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=2)
        shutil.rmtree(temp_dir, ignore_errors=True)


def main() -> int:
    try:
        duration_secs = positive_int_env(
            "FLEET_LOAD_DURATION_SECS", DEFAULT_DURATION_SECS
        )
        session_count = positive_int_env("FLEET_LOAD_SESSIONS", DEFAULT_SESSIONS)
        max_p95_ms = nonnegative_float_env("FLEET_LOAD_MAX_P95_MS", DEFAULT_P95_MS)
        max_p99_ms = nonnegative_float_env("FLEET_LOAD_MAX_P99_MS", DEFAULT_P99_MS)
        max_error_rate_pct = nonnegative_float_env(
            "FLEET_LOAD_MAX_ERROR_RATE_PCT", DEFAULT_ERROR_RATE_PCT
        )
        print(f"sessions         {session_count}")
        print(f"duration         {duration_secs}s")
        print(
            "thresholds       "
            f"p95<={max_p95_ms:.2f}ms p99<={max_p99_ms:.2f}ms "
            f"errors<={max_error_rate_pct:.3f}%"
        )
        latencies, total, errors, fatal_errors = execute(
            duration_secs, session_count
        )
        error_rate_pct = (errors / total * 100) if total else 100.0
        p50_ms = percentile(latencies, 50) if latencies else math.inf
        p95_ms = percentile(latencies, 95) if latencies else math.inf
        p99_ms = percentile(latencies, 99) if latencies else math.inf
        print(f"calls            {total} (success={total - errors} errors={errors})")
        print(f"latency          p50={p50_ms:.2f}ms p95={p95_ms:.2f}ms p99={p99_ms:.2f}ms")
        print(f"error rate       {error_rate_pct:.3f}%")
        for fatal_error in fatal_errors:
            print(f"load: {fatal_error}", file=sys.stderr)
        breaches = threshold_breaches(
            p95_ms=p95_ms,
            p99_ms=p99_ms,
            error_rate_pct=error_rate_pct,
            max_p95_ms=max_p95_ms,
            max_p99_ms=max_p99_ms,
            max_error_rate_pct=max_error_rate_pct,
        )
        if fatal_errors:
            breaches.append("one or more sessions stopped early")
        for breach in breaches:
            print(f"BREACH {breach}", file=sys.stderr)
        return 1 if breaches else 0
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"load: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
