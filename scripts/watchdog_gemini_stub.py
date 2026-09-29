#!/usr/bin/env python3
"""Deterministic watchdog oracle. It never invokes tools or accesses Gemini."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "tests/fixtures/watchdog/evidence-escalation.json"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--prompt", required=True)
    parser.add_argument("--output-format", required=True)
    parser.add_argument("--approval-mode", required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--skip-trust", action="store_true")
    args = parser.parse_args()

    policy = args.policy.read_text()
    if 'toolName = "*"' not in policy or 'decision = "deny"' not in policy:
        print("watchdog Gemini policy did not deny every tool", file=sys.stderr)
        return 3
    expected = {
        case["id"]: case["expected_state"]
        for case in json.loads(FIXTURE.read_text())["cases"]
    }
    expected.update({"human-wait": "blocked", "quiet-stall": "working"})
    for pane_id in re.findall(r'<pane id="([^"]+)"', args.prompt):
        print(f"{pane_id}\t{expected.get(pane_id, 'unknown')}\tdeterministic watchdog fixture oracle")
    return 0


if __name__ == "__main__":
    sys.exit(main())
