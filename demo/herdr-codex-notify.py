#!/usr/bin/env python3
"""Codex `notify` handler -> herdr turn-end status.

Codex invokes the notify program with a single JSON argument. For a finished
turn it carries the assistant's final message, which is all we need:

    notify = ["python3", "/path/to/herdr-codex-notify.py"]

Codex already has a notify entry on this machine. Chain rather than replace:

    notify = ["/path/to/chain.sh"]     # calls the existing handler, then this

Fails silent and non-blocking in every path.
"""

from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse  # noqa: E402
from herdr_status import mirror_path, report  # noqa: E402

# Codex has used both spellings across versions.
TURN_DONE = {"agent-turn-complete", "agent_turn_complete", "turn-ended", "turn_ended"}
MESSAGE_KEYS = ("last-assistant-message", "last_assistant_message", "message")


def load_payload(argv: list[str]) -> dict:
    for arg in argv:
        try:
            body = json.loads(arg)
        except ValueError:
            continue
        if isinstance(body, dict):
            return body
    if not sys.stdin.isatty():
        try:
            body = json.load(sys.stdin)
            if isinstance(body, dict):
                return body
        except (ValueError, OSError):
            pass
    return {}


def title_from(payload: dict, pane_id: str) -> str | None:
    """A stable session name for the pane, since codex ships none.

    Codex's own resume picker previews the first user message, so use the same
    thing -- but only the *first* one. `input-messages` is per turn, so
    re-deriving every turn would rename the pane on every prompt. The first
    title wins and is kept in the mirror for the rest of the session.
    """
    try:
        with open(mirror_path(pane_id), encoding="utf-8") as fh:
            existing = json.load(fh).get("title")
        if isinstance(existing, str) and existing:
            return existing
    except (OSError, ValueError, AttributeError):
        pass

    messages = payload.get("input-messages") or payload.get("input_messages") or []
    if not isinstance(messages, list):
        return None
    # Last message, not first: the injected AGENTS.md / permissions preamble is
    # prepended, so the human's actual prompt is at the end.
    for msg in reversed(messages):
        text = msg if isinstance(msg, str) else ""
        for line in text.splitlines():
            line = line.strip().lstrip("#").strip()
            if not line or line.startswith("<") or line.lower().startswith("agents.md"):
                continue
            return line[:80]
    return None


def main() -> int:
    if os.environ.get("HERDR_ENV") != "1" or not os.environ.get("HERDR_PANE_ID"):
        return 0
    payload = load_payload(sys.argv[1:])
    kind = str(payload.get("type") or payload.get("event") or "")
    if kind and kind not in TURN_DONE:
        return 0

    text = next(
        (payload[k] for k in MESSAGE_KEYS if isinstance(payload.get(k), str)), None
    )
    if not text:
        return 0
    block = parse(text)
    if not block.present:
        return 0

    outcome = report(
        agent="codex",
        blocking=block.blocking,
        agents=block.agents_running,
        gates=[i.text for i in block.items if i.blocking],
        agent_names=block.agents,
        session_id=payload.get("turn-id") or payload.get("turn_id"),
        title=title_from(payload, os.environ["HERDR_PANE_ID"]),
    )
    if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except Exception:  # noqa: BLE001 -- never wedge a turn
        raise SystemExit(0)
