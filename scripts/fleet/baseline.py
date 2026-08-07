#!/usr/bin/env python3
"""Run the required fleet gates and publish their baseline summary."""

from __future__ import annotations

import datetime
import pathlib
import re
import subprocess
import sys
import time
from collections.abc import Callable
from typing import NamedTuple


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_ARTIFACT = REPO_ROOT / "docs" / "testing" / "fleet-baseline" / "BASELINE.md"
FAST_STAGES = ("conformance", "golden", "contract")


class StageRow(NamedTuple):
    stage: str
    policy: str
    status: str
    metric: str
    elapsed: str


def stage_metric(stage: str, output: str) -> str:
    if stage == "conformance":
        match = re.search(r"passed=(\d+) failed=(\d+)", output)
        if match:
            passed, failed = (int(value) for value in match.groups())
            return f"{passed}/{passed + failed} checks"
        return "0/0 checks"

    marker = "GOLDEN " if stage == "golden" else "CONTRACT "
    passed = sum(
        1 for line in output.splitlines() if line.startswith(marker) and line.endswith(" PASS")
    )
    failed = sum(
        1 for line in output.splitlines() if line.startswith(marker) and line.endswith(" FAIL")
    )
    noun = "fixture" if stage == "golden" else "snapshot"
    total = passed + failed
    if total == 0:
        return f"0 {noun}s"
    if total == 1 and passed == 1:
        return f"1 {noun}"
    suffix = noun if total == 1 else f"{noun}s"
    return f"{passed}/{total} {suffix}"


def markdown_report(
    rows: list[StageRow], commit: str, recorded_at: str, overall: str
) -> str:
    table = [
        "| Stage | Policy | Status | Metric | Elapsed |",
        "| --- | --- | --- | --- | --- |",
    ]
    table.extend(
        f"| {row.stage} | {row.policy} | {row.status} | {row.metric} | {row.elapsed} |"
        for row in rows
    )
    return "\n".join(
        [
            "# Fleet truth baseline",
            "",
            f"Recorded at `{recorded_at}` from commit `{commit}`.",
            "",
            "Run the Phase 0 fast predicate from the repository root:",
            "",
            "```bash",
            "scripts/fleet-truth.sh all",
            "```",
            "",
            f"Overall result: **{overall}**.",
            "",
            *table,
            "",
            "The required predicate is conformance + golden + contract. The load, fault,",
            "and observability stages remain explicit opt-ins because they are slower or",
            "disruptive; their durable stage-specific baselines remain under `docs/testing/`.",
            "",
        ]
    )


def run_baseline(
    run_stage: Callable[[str], tuple[int, str]],
    artifact: pathlib.Path,
    *,
    commit: str,
    recorded_at: str,
) -> tuple[int, list[StageRow]]:
    rows: list[StageRow] = []
    failed = False
    for stage in FAST_STAGES:
        started = time.monotonic()
        returncode, output = run_stage(stage)
        elapsed = time.monotonic() - started
        status = "PASS" if returncode == 0 else "FAIL"
        failed = failed or returncode != 0
        rows.append(
            StageRow(
                stage,
                "required",
                status,
                stage_metric(stage, output),
                f"{elapsed:.2f}s",
            )
        )

    rows.extend(
        [
            StageRow("load", "opt-in", "SKIP", "default 2 × 5m", "—"),
            StageRow("fault", "opt-in", "SKIP", "deterministic faults", "—"),
            StageRow("obs", "opt-in", "SKIP", "default 2 × 5s", "—"),
        ]
    )
    result = 1 if failed else 0
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text(
        markdown_report(rows, commit, recorded_at, "FAIL" if failed else "PASS")
    )
    return result, rows


def print_table(rows: list[StageRow]) -> None:
    print("\nFLEET TRUTH BASELINE")
    print(f"{'stage':<14} {'policy':<10} {'status':<7} {'metric':<24} {'elapsed':>8}")
    print(f"{'-' * 14} {'-' * 10} {'-' * 7} {'-' * 24} {'-' * 8}")
    for row in rows:
        print(
            f"{row.stage:<14} {row.policy:<10} {row.status:<7} "
            f"{row.metric:<24} {row.elapsed:>8}"
        )


def main() -> int:
    fleet_truth = REPO_ROOT / "scripts" / "fleet-truth.sh"

    def run_stage(stage: str) -> tuple[int, str]:
        completed = subprocess.run(
            [str(fleet_truth), stage],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
        print(completed.stdout, end="")
        return completed.returncode, completed.stdout

    commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    recorded_at = (
        datetime.datetime.now(datetime.UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    )
    result, rows = run_baseline(
        run_stage,
        DEFAULT_ARTIFACT,
        commit=commit,
        recorded_at=recorded_at,
    )
    print_table(rows)
    print(f"\nartifact: {DEFAULT_ARTIFACT.relative_to(REPO_ROOT)}")
    print(f"FLEET TRUTH {'PASS' if result == 0 else 'FAIL'}")
    return result


if __name__ == "__main__":
    raise SystemExit(main())
