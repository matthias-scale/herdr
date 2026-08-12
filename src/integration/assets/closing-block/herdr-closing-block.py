#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=1
"""Claude Code `Stop` hook -> herdr turn-end status.

Installed *beside* herdr's managed `herdr-agent-state.sh`, which herdr overwrites
on integration update and which today reports session identity only.

    "Stop": [{"hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-closing-block.py"}]}]

The hook is the trigger -- it fires deterministically at turn end, unlike a tool
call the model has to remember. It extracts the closing block from the last
main-chain assistant message and hands the structured counts to herdr_status.

Fails silent and non-blocking in every path.
"""

from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse  # noqa: E402
from herdr_status import report  # noqa: E402


def last_assistant_text(transcript_path: str) -> str | None:
    rows = []
    try:
        with open(transcript_path, encoding="utf-8") as fh:
            for line in fh:
                if not line.strip():
                    continue
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    # Claude Code appends concurrently with the Stop hook; a
                    # torn trailing line must not discard the whole transcript.
                    continue
    except OSError:
        return None
    for row in reversed(rows):
        if row.get("type") != "assistant" or row.get("isSidechain"):
            continue
        text = "\n".join(
            c.get("text", "")
            for c in row.get("message", {}).get("content", [])
            if c.get("type") == "text"
        )
        if text.strip():
            return text
    return None


def main() -> int:
    if os.environ.get("HERDR_ENV") != "1" or not os.environ.get("HERDR_PANE_ID"):
        return 0
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        return 0
    if payload.get("agent_id"):  # subagent stop -- never speaks for the pane
        return 0

    text = last_assistant_text(payload.get("transcript_path") or "")
    if text is None:
        return 0
    # A turn that ended without a closing block still ended, and a full-lifecycle
    # source that stays silent leaves its last report standing forever -- a pane
    # that reported a gate last turn would keep showing it with nothing able to
    # clear it. Absent counts are zero counts: nobody is waiting on a human.
    # An absent block parses to zero counts, which is the honest reading.
    block = parse(text)

    outcome = report(
        agent="claude",
        blocking=block.blocking,
        agents=block.agents_running,
        gates=block.wire_gates(),
        items=block.wire_items(),
        decisions=block.wire_decisions(),
        agent_names=block.agents,
        session_id=payload.get("session_id"),
        session_path=payload.get("transcript_path"),
    )
    if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
