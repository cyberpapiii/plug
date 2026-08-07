#!/usr/bin/env python3
"""Exercise reproducible upstream faults through Plug's daemon path."""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
LOAD_MODULE_PATH = pathlib.Path(__file__).with_name("load.py")
LOAD_SPEC = importlib.util.spec_from_file_location("fleet_load", LOAD_MODULE_PATH)
assert LOAD_SPEC is not None and LOAD_SPEC.loader is not None
load = importlib.util.module_from_spec(LOAD_SPEC)
LOAD_SPEC.loader.exec_module(load)


class Scenario(NamedTuple):
    name: str
    fail_mode: str
    expected_failure: str
    expected_recovery: str
    restart_after_fault: bool = False
    delay_ms: int = 0


SCENARIOS = [
    Scenario(
        "malformed-frame",
        "malformed-frame",
        "invalid JSON-RPC response rejected",
        "upstream process restarts and next call succeeds",
        restart_after_fault=True,
    ),
    Scenario(
        "reset",
        "reset",
        "upstream closes stdio mid-call",
        "fresh Plug runtime connects to restarted upstream",
        restart_after_fault=True,
    ),
    Scenario(
        "slow-delay",
        "slow-delay",
        "call exceeds the configured one-second timeout",
        "upstream process restarts and next call succeeds",
        restart_after_fault=True,
        delay_ms=1500,
    ),
    Scenario(
        "sigterm",
        "sigterm",
        "upstream receives SIGTERM mid-call",
        "fresh Plug runtime connects to restarted upstream",
        restart_after_fault=True,
    ),
    Scenario(
        "auth-expiry",
        "auth-expiry",
        "simulated authentication-expired error returned",
        "next call succeeds without a live OAuth provider",
    ),
]


def evaluate_outcome(
    scenario: Scenario, *, failure_observed: bool, recovery_observed: bool
) -> str:
    return (
        "PASS"
        if failure_observed
        and recovery_observed
        and scenario.expected_failure
        and scenario.expected_recovery
        else "FAIL"
    )


def write_wrapper(
    path: pathlib.Path, mock_server: pathlib.Path, scenario: Scenario
) -> pathlib.Path:
    marker = path.with_suffix(".marker")
    path.write_text(
        "\n".join(
            [
                "#!/bin/sh",
                'marker="$1"',
                'if [ ! -e "$marker" ]; then',
                '  : > "$marker"',
                f"  exec {json.dumps(str(mock_server))} --tools echo "
                f"--fail-mode {scenario.fail_mode} --delay-ms {scenario.delay_ms}",
                "fi",
                f"exec {json.dumps(str(mock_server))} --tools echo",
                "",
            ]
        )
    )
    path.chmod(0o755)
    return marker


def write_config(
    path: pathlib.Path,
    mock_server: pathlib.Path,
    scenario: Scenario,
) -> None:
    call_timeout_secs = (
        1 if scenario.fail_mode in {"malformed-frame", "slow-delay"} else 5
    )
    command = mock_server
    args = [
        "--tools",
        "echo",
        "--fail-mode",
        scenario.fail_mode,
        "--delay-ms",
        str(scenario.delay_ms),
    ]
    if scenario.restart_after_fault:
        command = path.with_name(f"{scenario.name}-upstream.sh")
        marker = write_wrapper(command, mock_server, scenario)
        args = [str(marker)]
    path.write_text(
        "\n".join(
            [
                'log_level = "warn"',
                "daemon_grace_period_secs = 1",
                "",
                "[servers.mock]",
                f"command = {json.dumps(str(command))}",
                f"args = {json.dumps(args)}",
                'transport = "stdio"',
                "enabled = true",
                "timeout_secs = 10",
                f"call_timeout_secs = {call_timeout_secs}",
                "max_concurrent = 1",
                "",
            ]
        )
    )


def response_failed(response: dict[str, object]) -> bool:
    result = response.get("result")
    return "error" in response or (
        isinstance(result, dict) and bool(result.get("isError", False))
    )


def call_echo(session: object, timeout: float = 4.0) -> dict[str, object]:
    return session.request(
        "tools/call",
        {"name": "Mock__echo", "arguments": {"input": "fleet-fault"}},
        timeout=timeout,
    )


def wait_for_recovery(session: object, deadline_secs: float = 20.0) -> bool:
    deadline = time.monotonic() + deadline_secs
    while time.monotonic() < deadline:
        try:
            if not response_failed(call_echo(session)):
                return True
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError):
            pass
        time.sleep(0.2)
    return False


def start_daemon(
    plug: pathlib.Path,
    config: pathlib.Path,
    env: dict[str, str],
    socket_path: pathlib.Path,
) -> subprocess.Popen[bytes]:
    daemon = subprocess.Popen(
        [str(plug), "--config", str(config), "serve", "--daemon"],
        cwd=REPO_ROOT,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    load.wait_for_daemon(daemon, socket_path)
    return daemon


def stop_daemon(daemon: subprocess.Popen[bytes] | None) -> None:
    if daemon is None or daemon.poll() is not None:
        return
    daemon.terminate()
    try:
        daemon.wait(timeout=5)
    except subprocess.TimeoutExpired:
        daemon.kill()
        daemon.wait(timeout=2)


def run_scenario(
    plug: pathlib.Path, mock_server: pathlib.Path, scenario: Scenario
) -> tuple[bool, bool, str]:
    temp_dir = pathlib.Path(tempfile.mkdtemp(prefix=f"plug-fleet-{scenario.name}-"))
    daemon: subprocess.Popen[bytes] | None = None
    session = None
    detail = ""
    try:
        runtime_root = temp_dir / "runtime"
        state_root = temp_dir / "state"
        runtime_root.mkdir()
        state_root.mkdir()
        config = temp_dir / "plug.toml"
        write_config(config, mock_server, scenario)
        env = os.environ.copy()
        env["XDG_RUNTIME_DIR"] = str(runtime_root)
        env["XDG_STATE_HOME"] = str(state_root)
        socket_path = runtime_root / "plug" / "plug.sock"
        daemon = start_daemon(plug, config, env, socket_path)
        session = load.McpSession(plug, config, env, 1)
        session.initialize()

        try:
            first_response = call_echo(session)
            failure_observed = response_failed(first_response)
            detail = (
                str(first_response.get("error", "tool error"))
                if failure_observed
                else "first call unexpectedly succeeded"
            )
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            failure_observed = True
            detail = str(error)

        if scenario.fail_mode in {"reset", "sigterm"}:
            session.close()
            session = None
            stop_daemon(daemon)
            daemon = None
            socket_path.unlink(missing_ok=True)
            daemon = start_daemon(plug, config, env, socket_path)
            session = load.McpSession(plug, config, env, 2)
            session.initialize()

        recovery_observed = wait_for_recovery(session)
        return failure_observed, recovery_observed, detail
    finally:
        if session is not None:
            session.close()
        stop_daemon(daemon)
        shutil.rmtree(temp_dir, ignore_errors=True)


def main() -> int:
    try:
        plug, mock_server = load.build_binaries()
        failed = 0
        for scenario in SCENARIOS:
            print(f"FAULT {scenario.name}")
            print(f"  expected-fail    {scenario.expected_failure}")
            print(f"  expected-recover {scenario.expected_recovery}")
            failure_observed, recovery_observed, detail = run_scenario(
                plug, mock_server, scenario
            )
            outcome = evaluate_outcome(
                scenario,
                failure_observed=failure_observed,
                recovery_observed=recovery_observed,
            )
            print(
                "  observed         "
                f"fail={'yes' if failure_observed else 'no'} "
                f"recover={'yes' if recovery_observed else 'no'}"
            )
            print(f"  detail           {detail}")
            print(f"  RESULT           {outcome}")
            if outcome != "PASS":
                failed += 1
        print(f"fault summary    {len(SCENARIOS) - failed} passed; {failed} failed")
        return 1 if failed else 0
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"fault: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
