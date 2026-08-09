#!/usr/bin/env python3
"""Does a hook-reported state survive later screen-detection ticks?

This is the question that decides whether the fork needs to touch
`full_lifecycle_hook_authority` at all. Report `blocked`, then re-read the pane
over ~8s (several detection intervals) and see whether screen scraping --
which sees a shell prompt and concludes "idle" -- takes the pane back.
"""

from __future__ import annotations

import json
import os
import socket
import sys
import time

SOCK = os.environ["HERDR_SOCKET_PATH"]
SOURCE = "herdr:claude-closing-block"


def rpc(method: str, params: dict) -> dict:
    req = {"id": f"pc:{time.time_ns()}", "method": method, "params": params}
    c = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    c.settimeout(3.0)
    c.connect(SOCK)
    try:
        c.sendall((json.dumps(req) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = c.recv(65536)
            if not chunk:
                break
            buf += chunk
    finally:
        c.close()
    return json.loads(buf.decode() or "{}")


def status(pane_id: str, dump: bool = False) -> str:
    resp = rpc("pane.get", {"pane_id": pane_id})
    result = resp.get("result") or resp
    pane = result.get("pane") if isinstance(result.get("pane"), dict) else result
    if dump:
        print("  raw:", json.dumps(pane)[:600])
    return pane.get("agent_status")


def main() -> int:
    panes = rpc("pane.list", {}).get("result", {}).get("panes") or []
    if not panes:
        rpc("workspace.create", {})
        time.sleep(0.5)
        panes = rpc("pane.list", {}).get("result", {}).get("panes") or []
    pane_id = panes[0]["pane_id"]
    print(f"pane: {pane_id}")

    resp = rpc("pane.report_agent", {
        "pane_id": pane_id, "source": SOURCE, "agent": "claude",
        "state": "blocked", "message": "Gate 1: merge #30", "seq": time.time_ns(),
    })
    print("  report_agent ->", json.dumps(resp)[:400])

    seen = []
    for i in range(17):
        s = status(pane_id, dump=(i == 0))
        seen.append(s)
        print(f"  t+{i * 0.5:4.1f}s  {s}")
        time.sleep(0.5)

    held = all(s == "blocked" for s in seen)
    print(f"\n{'HELD' if held else 'OVERRIDDEN'}: blocked survived = {held}")
    if not held:
        print(f"  first non-blocked at index {next(i for i, s in enumerate(seen) if s != 'blocked')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
