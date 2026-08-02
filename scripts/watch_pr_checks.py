#!/usr/bin/env python3
"""Watch GitHub PR checks while emitting only state transitions."""

from __future__ import annotations

import argparse
import json
import subprocess
import time
from dataclasses import dataclass
from typing import Iterable


PENDING_STATES = {"PENDING", "QUEUED", "IN_PROGRESS", "WAITING", "REQUESTED"}
FAILURE_STATES = {
    "ACTION_REQUIRED",
    "CANCELLED",
    "ERROR",
    "FAILURE",
    "STARTUP_FAILURE",
    "STALE",
    "TIMED_OUT",
}


@dataclass(frozen=True)
class CheckState:
    name: str
    workflow: str
    state: str
    link: str

    @property
    def key(self) -> tuple[str, str]:
        return self.workflow, self.name


def parse_checks(payload: str) -> list[CheckState]:
    if not payload.strip():
        return []
    rows = json.loads(payload)
    return [
        CheckState(
            name=str(row["name"]),
            workflow=str(row.get("workflow") or ""),
            state=str(row["state"]).upper(),
            link=str(row.get("link") or ""),
        )
        for row in rows
    ]


def transitions(
    previous: dict[tuple[str, str], str],
    checks: Iterable[CheckState],
) -> list[CheckState]:
    return [check for check in checks if previous.get(check.key) != check.state]


def fetch(pr: str, repo: str | None) -> list[CheckState]:
    command = [
        "gh",
        "pr",
        "checks",
        pr,
        "--json",
        "name,state,workflow,link",
    ]
    if repo:
        command.extend(("--repo", repo))
    result = subprocess.run(
        command,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.stdout.strip():
        return parse_checks(result.stdout)
    if result.returncode in (0, 1, 8) and (
        result.returncode != 1
        or "no checks reported" in result.stderr.lower()
    ):
        return []
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "gh pr checks failed")
    return []


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("pr")
    parser.add_argument("--repo")
    parser.add_argument("--interval", type=float, default=10.0)
    args = parser.parse_args()

    previous: dict[tuple[str, str], str] = {}
    while True:
        checks = fetch(args.pr, args.repo)
        for check in transitions(previous, checks):
            label = f"{check.workflow} / {check.name}".strip(" /")
            suffix = f" {check.link}" if check.link else ""
            print(f"{check.state.lower():12} {label}{suffix}", flush=True)
        previous = {check.key: check.state for check in checks}

        if checks and not any(check.state in PENDING_STATES for check in checks):
            return int(any(check.state in FAILURE_STATES for check in checks))
        time.sleep(args.interval)


if __name__ == "__main__":
    raise SystemExit(main())
