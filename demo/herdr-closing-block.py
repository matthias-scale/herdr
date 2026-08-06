#!/usr/bin/env python3
"""Claude Code `Stop` hook: report the closing block to herdr as real state.

Installed *beside* herdr's managed `herdr-agent-state.sh` (which herdr overwrites
on integration update, and which today reports session identity only -- never a
lifecycle state).

Wire into ~/.claude/settings.json:

    "Stop": [{"hooks": [{"type": "command",
      "command": "python3 /path/to/herdr-closing-block.py"}]}]

Reads the Stop payload on stdin, pulls the last main-chain assistant message out
of the transcript, parses its closing block, and emits two herdr RPCs:

  pane.report_agent     -- blocked (gates) / working (agents running) / idle
  pane.report_metadata  -- tokens so the three channels can render separately

Fails silent and non-blocking in every error path: a hook must never wedge a turn.
"""

from __future__ import annotations

import json
import os
import random
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from closing_block import parse  # noqa: E402

SOURCE = "herdr:claude-closing-block"


def rpc(sock_path: str, method: str, params: dict) -> None:
    req = {
        "id": f"{SOURCE}:{int(time.time() * 1000)}:{random.randrange(10**6):06d}",
        "method": method,
        "params": params,
    }
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    try:
        client.connect(sock_path)
        client.sendall((json.dumps(req) + "\n").encode())
        try:
            client.recv(4096)
        except OSError:
            pass
    finally:
        client.close()


def last_assistant_text(transcript_path: str) -> str | None:
    try:
        with open(transcript_path, encoding="utf-8") as fh:
            rows = [json.loads(line) for line in fh if line.strip()]
    except (OSError, ValueError):
        return None
    for row in reversed(rows):
        if row.get("type") != "assistant" or row.get("isSidechain"):
            continue
        chunks = [
            c.get("text", "")
            for c in row.get("message", {}).get("content", [])
            if c.get("type") == "text"
        ]
        text = "\n".join(c for c in chunks if c)
        if text.strip():
            return text
    return None


def main() -> int:
    pane_id = os.environ.get("HERDR_PANE_ID")
    sock_path = os.environ.get("HERDR_SOCKET_PATH")
    if os.environ.get("HERDR_ENV") != "1" or not pane_id or not sock_path:
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

    block = parse(text)
    if not block.present:
        return 0

    seq = time.time_ns()
    # A full-lifecycle source must announce its own session before its state
    # reports are honoured -- reports arriving without one are buffered and
    # silently dropped. Sequences are tracked per source, and herdr's managed
    # hook announces under `herdr:claude`, a different source, so that does not
    # cover us. Announce under ours, one tick earlier.
    session_ref = payload.get("session_id")
    session_params = {
        "pane_id": pane_id,
        "source": SOURCE,
        "agent": "claude",
        "seq": seq - 1,
    }
    if session_ref:
        session_params["agent_session_id"] = session_ref
    if payload.get("transcript_path"):
        session_params["agent_session_path"] = payload["transcript_path"]

    agent_params = {
        "pane_id": pane_id,
        "source": SOURCE,
        "agent": "claude",
        "state": block.herdr_state,
        "seq": seq,
    }
    message = block.message()
    if message:
        agent_params["message"] = message

    # The three channels, kept separate so the UI never has to collapse them.
    tokens = {
        "closing_blocking": str(block.blocking),
        "closing_agents": str(block.agents_running),
        "closing_idle": "1" if (block.done_here and not block.agents) else "0",
    }
    # Always write every key. Tokens persist across reports, so omitting one
    # leaves the previous turn's value on screen -- a finished pane would keep
    # advertising agents that already exited.
    tokens["closing_agent_names"] = "; ".join(block.agents)[:200]

    try:
        rpc(sock_path, "pane.report_agent_session", session_params)
        rpc(sock_path, "pane.report_agent", agent_params)
        rpc(
            sock_path,
            "pane.report_metadata",
            {
                "pane_id": pane_id,
                "source": SOURCE,
                "applies_to_source": SOURCE,
                "tokens": tokens,
                "state_labels": {
                    "blocked": f"gate ×{block.blocking}" if block.blocking else "blocked",
                    "working": (
                        f"{block.agents_running} agents"
                        if block.agents_running
                        else "working"
                    ),
                },
                "seq": seq,
            },
        )
    except OSError:
        return 0

    if os.environ.get("HERDR_CLOSING_BLOCK_DEBUG"):
        print(json.dumps({"state": block.herdr_state, "tokens": tokens}), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
