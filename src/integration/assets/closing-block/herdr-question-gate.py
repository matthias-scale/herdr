#!/usr/bin/env python3
# HERDR_INTEGRATION_VERSION=1
"""Claude Code `AskUserQuestion` hooks -> herdr blocked/working status.

Installed beside `herdr-closing-block.py` and reporting through the same
`herdr:claude-closing-block` source, so the two never fight over authority --
they are one full-lifecycle source reporting at different points of a turn.

    "PreToolUse":  [{"matcher": "AskUserQuestion", "hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-question-gate.py"}]}],
    "PostToolUse": [{"matcher": "AskUserQuestion", "hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-question-gate.py"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-question-gate.py"}]}]

Why a hook and not screen detection: once any closing-block report lands, the
pane is under full-lifecycle hook authority and herdr stops reading the screen
for state, so the `live_blocked_form` manifest rule that does match the
AskUserQuestion dialog is never consulted. The turn-end `Stop` hook cannot
cover it either -- the dialog opens *mid*-turn, and the turn only ends after a
human answers. Without this hook the pane shows `working` for the entire time
it is actually waiting on a human.

`UserPromptSubmit` is a recovery path only. Escape cancels the dialog and the
whole turn without firing `PostToolUse`, so an open gate is cleared by the next
thing the human does. It reports nothing when no gate is outstanding, so it
never claims authority on an ordinary prompt.

Fails silent and non-blocking in every path.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from herdr_status import mirror_path, report  # noqa: E402

TOOL_NAME = "AskUserQuestion"
GATE_TEXT_LIMIT = 160


def marker_path(pane_id: str) -> str:
    """Sidecar beside the status mirror; same directory, same lifetime."""
    return f"{mirror_path(pane_id)}.question-gate"


def read_marker(pane_id: str) -> dict | None:
    try:
        with open(marker_path(pane_id), encoding="utf-8") as fh:
            marker = json.load(fh)
    except (OSError, ValueError):
        return None
    return marker if isinstance(marker, dict) else None


def write_marker(pane_id: str, marker: dict) -> None:
    path = marker_path(pane_id)
    try:
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(marker, fh)
        os.replace(tmp, path)
    except OSError:
        pass


def clear_marker(pane_id: str) -> None:
    try:
        os.remove(marker_path(pane_id))
    except OSError:
        pass


def questions_of(payload: dict) -> list[dict]:
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return []
    questions = tool_input.get("questions")
    if not isinstance(questions, list):
        return []
    return [question for question in questions if isinstance(question, dict)]


def gate_text(question: dict) -> str:
    """The question, with its options, as one line a human can answer from."""
    text = str(question.get("question") or question.get("header") or "").strip()
    labels = [
        str(option.get("label") or "").strip()
        for option in question.get("options") or []
        if isinstance(option, dict) and str(option.get("label") or "").strip()
    ]
    if labels:
        joined = " / ".join(labels)
        text = f"{text} — {joined}" if text else joined
    return (text or "Question waiting")[:GATE_TEXT_LIMIT]


def gates_for(payload: dict) -> list[dict]:
    return [{"text": gate_text(question)} for question in questions_of(payload)] or [
        {"text": "Question waiting"}
    ]


def open_gate(payload: dict, pane_id: str) -> dict:
    gates = gates_for(payload)
    outcome = report(
        agent="claude",
        blocking=len(gates),
        agents=0,
        gates=gates,
        session_id=payload.get("session_id"),
        session_path=payload.get("transcript_path"),
    )
    write_marker(
        pane_id,
        {
            "session_id": payload.get("session_id"),
            "tool_use_id": payload.get("tool_use_id"),
            "seq": outcome["payload"]["seq"],
        },
    )
    return outcome


def close_gate(payload: dict, pane_id: str, *, require_marker: bool) -> dict | None:
    """Hand the pane back to `working`; the turn resumed, it did not end.

    Zero counts alone would resolve to `idle` and publish a finished turn while
    the model is still replying, so the state is named explicitly. The next
    `Stop` report supersedes this one with the real turn-end reading.

    `PostToolUse` for this tool means a human just answered, which is true
    whether or not the marker survived, so it reports unconditionally. A prompt
    submit only means the turn resumed when a gate was actually outstanding.
    """
    session_id = payload.get("session_id")
    marker = read_marker(pane_id)
    if marker is not None:
        # A gate opened by a different session must not be answered for by this
        # one; leave it to that session's own hooks.
        marked_session = marker.get("session_id")
        if marked_session and session_id and marked_session != session_id:
            return None
        clear_marker(pane_id)
    elif require_marker:
        return None
    return report(
        agent="claude",
        blocking=0,
        agents=0,
        state="working",
        session_id=session_id,
        session_path=payload.get("transcript_path"),
    )


def main() -> int:
    if os.environ.get("HERDR_ENV") != "1":
        return 0
    pane_id = os.environ.get("HERDR_PANE_ID")
    if not pane_id:
        return 0
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        return 0
    if not isinstance(payload, dict):
        return 0
    if payload.get("agent_id"):  # subagent -- never speaks for the pane
        return 0

    event = payload.get("hook_event_name")
    tool_name = payload.get("tool_name")
    if event in ("PreToolUse", "PostToolUse") and tool_name != TOOL_NAME:
        # Installed without a matcher, or matched loosely: only this tool blocks.
        return 0

    if event == "PreToolUse":
        outcome = open_gate(payload, pane_id)
    elif event == "PostToolUse":
        outcome = close_gate(payload, pane_id, require_marker=False)
    elif event == "UserPromptSubmit":
        outcome = close_gate(payload, pane_id, require_marker=True)
    else:
        return 0

    if outcome and os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps(outcome["payload"]), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
