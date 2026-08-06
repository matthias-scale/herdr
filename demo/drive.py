#!/usr/bin/env python3
"""Drive the demo: feed fixtures through the real Stop hook, read herdr back.

Proves the full chain on a running herdr server:

    assistant closing block
      -> Stop hook (herdr-closing-block.py)
      -> pane.report_agent / pane.report_metadata over $HERDR_SOCKET_PATH
      -> herdr pane state

Nothing here fakes the middle: the hook binary under test is the same file that
would be wired into ~/.claude/settings.json.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import test_closing_block as fx  # noqa: E402

SOCK = os.environ["HERDR_SOCKET_PATH"]


def rpc(method: str, params: dict | None = None) -> dict:
    req = {"id": f"drive:{time.time_ns()}", "method": method, "params": params or {}}
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(3.0)
    client.connect(SOCK)
    try:
        client.sendall((json.dumps(req) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            buf += chunk
    finally:
        client.close()
    return json.loads(buf.decode() or "{}")


def first_pane_id() -> str:
    """Return a pane id, bootstrapping a workspace/tab if the server is empty."""
    for attempt in range(2):
        resp = rpc("pane.list")
        panes = (resp.get("result") or resp).get("panes") or []
        if panes:
            return panes[0].get("id") or panes[0].get("pane_id")
        if attempt:
            break
        ws = rpc("workspace.create", {})
        print("workspace.create ->", json.dumps(ws)[:300])
        tab = rpc("tab.create", {})
        print("tab.create ->", json.dumps(tab)[:300])
        time.sleep(0.5)
    raise SystemExit("no panes in the isolated server")


def run_hook(pane_id: str, transcript_text: str) -> str:
    """Write a one-row transcript, then invoke the hook exactly as Claude would."""
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as tf:
        tf.write(
            json.dumps(
                {
                    "type": "assistant",
                    "isSidechain": False,
                    "message": {"content": [{"type": "text", "text": transcript_text}]},
                }
            )
            + "\n"
        )
        path = tf.name

    payload = {
        "hook_event_name": "Stop",
        "session_id": "demo-session",
        "transcript_path": path,
    }
    env = dict(os.environ, HERDR_ENV="1", HERDR_PANE_ID=pane_id,
               HERDR_CLOSING_BLOCK_DEBUG="1")
    proc = subprocess.run(
        [sys.executable, os.path.join(HERE, "herdr-closing-block.py")],
        input=json.dumps(payload), capture_output=True, text=True, env=env, timeout=10,
    )
    os.unlink(path)
    if proc.returncode != 0:
        raise SystemExit(f"hook failed rc={proc.returncode}: {proc.stderr}")
    return proc.stderr.strip()


def read_state(pane_id: str) -> dict:
    resp = rpc("pane.get", {"pane_id": pane_id})
    result = resp.get("result") or resp
    pane = result.get("pane") if isinstance(result.get("pane"), dict) else result
    if os.environ.get("DRIVE_DUMP"):
        print("  raw pane.get ->", json.dumps(pane)[:1200])
    agent = pane.get("agent") if isinstance(pane.get("agent"), dict) else {}
    return {
        "agent_state": pane.get("agent_state") or agent.get("state"),
        "agent_status": pane.get("agent_status") or agent.get("status"),
        "message": pane.get("agent_message") or agent.get("message"),
        "tokens": pane.get("tokens") or agent.get("tokens"),
    }


# (name, fixture, expected herdr status, expected tokens subset)
CASES = [
    ("nothing to act on + 3 agents running", fx.NOTHING_BUT_AGENTS, "working",
     {"closing_blocking": "0", "closing_agents": "3", "closing_idle": "0"}),
    ("2 gates blocking (+2 agents)", fx.BLOCKING_GATE, "blocked",
     {"closing_blocking": "2", "closing_agents": "2", "closing_idle": "0"}),
    # Regression: agent names must not survive from the previous turn.
    ("nothing to act on + done here", fx.FULLY_IDLE, "idle",
     {"closing_blocking": "0", "closing_agents": "0", "closing_idle": "1",
      "closing_agent_names": ""}),
]


def main() -> int:
    pane_id = sys.argv[2] if len(sys.argv) > 2 else first_pane_id()
    print(f"pane: {pane_id}\n")
    failures = 0
    for name, text, want, want_tokens in CASES:
        dbg = run_hook(pane_id, text)
        time.sleep(0.4)
        got = read_state(pane_id)
        state_ok = want in (got["agent_state"], got["agent_status"])
        tokens = got["tokens"] or {}
        bad = {k: (tokens.get(k), v) for k, v in want_tokens.items()
               if tokens.get(k, "") != v}
        ok = state_ok and not bad
        failures += 0 if ok else 1
        print(f"{'PASS' if ok else 'FAIL'}  {name}")
        print(f"      hook emitted : {dbg or '(nothing)'}")
        print(f"      herdr status : {got['agent_status']}  (expected {want})")
        print(f"      herdr tokens : {tokens}")
        if bad:
            print(f"      TOKEN MISMATCH (got, want): {bad}")
        print()
    print("ALL PASS" if not failures else f"{failures} FAILED")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
