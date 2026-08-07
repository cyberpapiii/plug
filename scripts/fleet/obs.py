#!/usr/bin/env python3
"""Measure the minimum observability signals required for fleet truth."""

from __future__ import annotations

from collections import Counter
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import NamedTuple


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
LOAD_MODULE_PATH = pathlib.Path(__file__).with_name("load.py")
LOAD_SPEC = importlib.util.spec_from_file_location("fleet_load", LOAD_MODULE_PATH)
assert LOAD_SPEC is not None and LOAD_SPEC.loader is not None
load = importlib.util.module_from_spec(LOAD_SPEC)
LOAD_SPEC.loader.exec_module(load)

DEFAULT_DURATION_SECS = 5
DEFAULT_SESSIONS = 2
HISTOGRAM_BOUNDS_MS = (1, 5, 10, 25, 50, 100, 250, 500, 1000)
ERROR_CLASSES = (
    "success",
    "tool_error",
    "timeout",
    "protocol_error",
    "transport_error",
    "runtime_error",
)
STDERR_CRASH_SIGNATURES = (
    " panicked at ",
    "fatal error",
    "segmentation fault",
    "stack backtrace",
)
REQUIRED_SIGNALS = (
    "latency_histogram",
    "error_taxonomy",
    "in_flight",
    "rss_samples",
    "fd_samples",
    "stderr_assert",
)


class ObsResult(NamedTuple):
    latencies_ms: list[float]
    error_taxonomy: Counter[str]
    max_in_flight: int
    final_in_flight: int
    in_flight_samples: list[int]
    rss_samples_kib: list[int]
    fd_samples: list[int]
    stderr_bytes: int
    stderr_violations: list[str]


class InFlight:
    def __init__(self) -> None:
        self.current = 0
        self.maximum = 0
        self.samples = [0]
        self.lock = threading.Lock()

    def enter(self) -> None:
        with self.lock:
            self.current += 1
            self.maximum = max(self.maximum, self.current)
            self.samples.append(self.current)

    def leave(self) -> None:
        with self.lock:
            self.current -= 1
            self.samples.append(self.current)


def latency_histogram(samples: list[float]) -> list[tuple[str, int]]:
    histogram = [
        (f"<={bound}ms", sum(sample <= bound for sample in samples))
        for bound in HISTOGRAM_BOUNDS_MS
    ]
    histogram.append(("+Inf", len(samples)))
    return histogram


def classify_error(error: BaseException | None, succeeded: bool) -> str:
    if error is None:
        return "success" if succeeded else "tool_error"
    if isinstance(error, RuntimeError) and "timed out" in str(error):
        return "timeout"
    if isinstance(error, (ValueError, json.JSONDecodeError)):
        return "protocol_error"
    if isinstance(error, OSError):
        return "transport_error"
    return "runtime_error"


def missing_required_signals(signals: dict[str, bool]) -> list[str]:
    return [name for name in REQUIRED_SIGNALS if not signals.get(name, False)]


def stderr_violations(stderr: str) -> list[str]:
    return [
        line
        for line in stderr.splitlines()
        if any(signature in line.lower() for signature in STDERR_CRASH_SIGNATURES)
    ]


def process_resources(pids: list[int]) -> tuple[int, int] | None:
    rss_kib = 0
    fd_count = 0
    sampled = 0
    for pid in pids:
        process_root = pathlib.Path("/proc") / str(pid)
        try:
            status = (process_root / "status").read_text()
            rss_line = next(
                line for line in status.splitlines() if line.startswith("VmRSS:")
            )
            rss_kib += int(rss_line.split()[1])
            fd_count += len(list((process_root / "fd").iterdir()))
            sampled += 1
        except (FileNotFoundError, PermissionError, StopIteration, ValueError):
            continue
    return (rss_kib, fd_count) if sampled else None


def sample_resources(
    pids: list[int],
    stop: threading.Event,
    rss_samples: list[int],
    fd_samples: list[int],
) -> None:
    while not stop.is_set():
        sample = process_resources(pids)
        if sample is not None:
            rss_kib, fd_count = sample
            rss_samples.append(rss_kib)
            fd_samples.append(fd_count)
        stop.wait(0.1)


def run_observed_calls(
    session: object,
    start: threading.Barrier,
    duration_secs: int,
    in_flight: InFlight,
    latencies_ms: list[float],
    taxonomy: Counter[str],
    results_lock: threading.Lock,
) -> None:
    try:
        start.wait()
    except threading.BrokenBarrierError:
        with results_lock:
            taxonomy["runtime_error"] += 1
        return

    deadline = time.monotonic() + duration_secs
    while time.monotonic() < deadline:
        began = time.perf_counter()
        error: BaseException | None = None
        succeeded = False
        in_flight.enter()
        try:
            succeeded = session.call_echo()
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as caught:
            error = caught
        finally:
            elapsed_ms = (time.perf_counter() - began) * 1000
            in_flight.leave()
        with results_lock:
            latencies_ms.append(elapsed_ms)
            taxonomy[classify_error(error, succeeded)] += 1
        if error is not None:
            return


def stop_daemon(daemon: subprocess.Popen[bytes] | None) -> None:
    if daemon is None or daemon.poll() is not None:
        return
    daemon.terminate()
    try:
        daemon.wait(timeout=5)
    except subprocess.TimeoutExpired:
        daemon.kill()
        daemon.wait(timeout=2)


def execute(duration_secs: int, session_count: int) -> ObsResult:
    plug, mock_server = load.build_binaries()
    temp_dir = pathlib.Path(tempfile.mkdtemp(prefix="plug-fleet-obs-"))
    daemon: subprocess.Popen[bytes] | None = None
    sessions = []
    stderr_files = []
    stderr_paths: list[pathlib.Path] = []
    latencies_ms: list[float] = []
    taxonomy: Counter[str] = Counter()
    in_flight = InFlight()
    rss_samples: list[int] = []
    fd_samples: list[int] = []
    sampler_stop = threading.Event()
    sampler: threading.Thread | None = None
    try:
        runtime_root = temp_dir / "runtime"
        state_root = temp_dir / "state"
        runtime_root.mkdir()
        state_root.mkdir()
        config = temp_dir / "plug.toml"
        load.write_config(config, mock_server, session_count)
        env = os.environ.copy()
        env["XDG_RUNTIME_DIR"] = str(runtime_root)
        env["XDG_STATE_HOME"] = str(state_root)

        daemon_stderr_path = temp_dir / "daemon.stderr"
        daemon_stderr = daemon_stderr_path.open("wb")
        stderr_files.append(daemon_stderr)
        stderr_paths.append(daemon_stderr_path)
        daemon = subprocess.Popen(
            [str(plug), "--config", str(config), "serve", "--daemon"],
            cwd=REPO_ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=daemon_stderr,
        )
        load.wait_for_daemon(daemon, runtime_root / "plug" / "plug.sock")

        for index in range(1, session_count + 1):
            stderr_path = temp_dir / f"session-{index}.stderr"
            stderr_file = stderr_path.open("w")
            stderr_files.append(stderr_file)
            stderr_paths.append(stderr_path)
            session = load.McpSession(
                plug, config, env, index, stderr=stderr_file
            )
            session.initialize()
            sessions.append(session)

        pids = [daemon.pid, *(session.process.pid for session in sessions)]
        sampler = threading.Thread(
            target=sample_resources,
            args=(pids, sampler_stop, rss_samples, fd_samples),
            name="fleet-obs-resource-sampler",
        )
        sampler.start()

        barrier = threading.Barrier(session_count)
        results_lock = threading.Lock()
        threads = [
            threading.Thread(
                target=run_observed_calls,
                args=(
                    session,
                    barrier,
                    duration_secs,
                    in_flight,
                    latencies_ms,
                    taxonomy,
                    results_lock,
                ),
                name=f"fleet-obs-{session.index}",
            )
            for session in sessions
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    finally:
        sampler_stop.set()
        if sampler is not None:
            sampler.join()
        for session in sessions:
            session.close()
        stop_daemon(daemon)
        for stderr_file in stderr_files:
            stderr_file.close()

    try:
        stderr_bytes = sum(path.stat().st_size for path in stderr_paths)
        violations = stderr_violations(
            "\n".join(path.read_text(errors="replace") for path in stderr_paths)
        )
        return ObsResult(
            latencies_ms,
            taxonomy,
            in_flight.maximum,
            in_flight.current,
            in_flight.samples,
            rss_samples,
            fd_samples,
            stderr_bytes,
            violations,
        )
    finally:
        shutil.rmtree(temp_dir, ignore_errors=True)


def main() -> int:
    try:
        duration_secs = load.positive_int_env(
            "FLEET_OBS_DURATION_SECS", DEFAULT_DURATION_SECS
        )
        session_count = load.positive_int_env(
            "FLEET_OBS_SESSIONS", DEFAULT_SESSIONS
        )
        result = execute(duration_secs, session_count)
        histogram = latency_histogram(result.latencies_ms)
        print("latency histogram")
        for bucket, count in histogram:
            print(f"  {bucket:<9} {count}")
        print("error taxonomy")
        for error_class in ERROR_CLASSES:
            print(f"  {error_class:<16} {result.error_taxonomy[error_class]}")
        print(
            "in-flight count "
            f"max={result.max_in_flight} final={result.final_in_flight} "
            f"samples={len(result.in_flight_samples)}"
        )
        if result.rss_samples_kib:
            print(
                "RSS samples     "
                f"count={len(result.rss_samples_kib)} "
                f"min={min(result.rss_samples_kib)}KiB "
                f"max={max(result.rss_samples_kib)}KiB"
            )
        if result.fd_samples:
            print(
                "FD samples      "
                f"count={len(result.fd_samples)} "
                f"min={min(result.fd_samples)} max={max(result.fd_samples)}"
            )
        stderr_ok = not result.stderr_violations
        print(
            f"stderr assert   {'PASS' if stderr_ok else 'FAIL'} "
            f"bytes={result.stderr_bytes} crash-signatures={len(result.stderr_violations)}"
        )
        for violation in result.stderr_violations:
            print(f"STDERR violation: {violation}", file=sys.stderr)

        signals = {
            "latency_histogram": bool(result.latencies_ms),
            "error_taxonomy": sum(result.error_taxonomy.values()) > 0,
            "in_flight": result.max_in_flight > 0
            and result.final_in_flight == 0
            and bool(result.in_flight_samples),
            "rss_samples": bool(result.rss_samples_kib),
            "fd_samples": bool(result.fd_samples),
            "stderr_assert": stderr_ok,
        }
        omitted = os.environ.get("FLEET_OBS_OMIT_SIGNAL")
        if omitted:
            signals[omitted] = False
        missing = missing_required_signals(signals)
        for signal in missing:
            print(f"MISSING required signal: {signal}", file=sys.stderr)
        observed_errors = sum(
            count
            for error_class, count in result.error_taxonomy.items()
            if error_class != "success"
        )
        if observed_errors:
            print(f"obs: observed {observed_errors} workload errors", file=sys.stderr)
        return 1 if missing or observed_errors else 0
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"obs: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
