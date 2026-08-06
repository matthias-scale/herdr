"""Agent-agnostic turn-end status contract for herdr.

One payload, one transport, any coding agent.

    {"v": 1, "agent": "claude", "blocking": 2, "agents": 3,
     "gates": ["Merge #30"], "agent_names": ["reviewer A — round 4"]}

`blocking` is the count of items only a human can clear (CLAUDE.md **Gate**
items). `agents` is the count still working. They are independent: "nothing for
you, but 3 agents are still running" is `blocking=0, agents=3`, which is exactly
the state herdr could not previously see.

Transport is the herdr socket at $HERDR_SOCKET_PATH -- already injected into
every pane, already proven. A JSON mirror is written atomically to
$XDG_STATE_HOME/herdr/agent-status/<pane_id>.json so the last known status
survives herdr restarts and is inspectable without the socket.

Any agent can report in one line from its own turn-end hook:

    herdr-status --agent codex --blocking 1 --agents 2
    echo '{"blocking":0,"agents":3}' | herdr-status --agent opencode
"""

from __future__ import annotations

import json
import os
import random
import socket
import tempfile
import time

VERSION = 1


def state_for(blocking: int, agents: int) -> str:
    """Collapse the channels to herdr's AgentState.

    Only a human gate is genuinely *blocked*. Agents still working is *working*
    -- keeping those apart is the whole point.
    """
    if blocking > 0:
        return "blocked"
    if agents > 0:
        return "working"
    return "idle"


def message_for(blocking: int, agents: int, gates: list[str]) -> str | None:
    if blocking > 0:
        head = (gates[0] if gates else "").strip()
        extra = f" (+{blocking - 1})" if blocking > 1 else ""
        return ((head or f"{blocking} blocking")[:80]) + extra
    if agents > 0:
        return f"{agents} agent{'s' if agents != 1 else ''} running"
    return None


def mirror_path(pane_id: str) -> str:
    root = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    d = os.path.join(root, "herdr", "agent-status")
    os.makedirs(d, exist_ok=True)
    return os.path.join(d, f"{pane_id.replace(':', '_')}.json")


def write_mirror(pane_id: str, payload: dict) -> str | None:
    """Atomic write -- a torn status file is worse than a stale one."""
    path = mirror_path(pane_id)
    try:
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(payload, fh)
        os.replace(tmp, path)
        return path
    except OSError:
        return None


def _rpc(sock_path: str, source: str, method: str, params: dict) -> None:
    req = {
        "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(10**6):06d}",
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


def report(
    *,
    agent: str,
    blocking: int,
    agents: int,
    gates: list[str] | None = None,
    agent_names: list[str] | None = None,
    session_id: str | None = None,
    session_path: str | None = None,
    pane_id: str | None = None,
    sock_path: str | None = None,
) -> dict:
    """Push one turn-end status. Never raises; returns what it did."""
    gates = gates or []
    agent_names = agent_names or []
    pane_id = pane_id or os.environ.get("HERDR_PANE_ID") or ""
    sock_path = sock_path or os.environ.get("HERDR_SOCKET_PATH") or ""

    seq = time.time_ns()
    state = state_for(blocking, agents)
    payload = {
        "v": VERSION,
        "agent": agent,
        "seq": seq,
        "state": state,
        "blocking": blocking,
        "agents": agents,
        "gates": gates,
        "agent_names": agent_names,
    }

    outcome = {"payload": payload, "mirror": None, "socket": False}
    if pane_id:
        outcome["mirror"] = write_mirror(pane_id, payload)
    if not (pane_id and sock_path):
        return outcome

    source = f"herdr:{agent}-closing-block"
    # A full-lifecycle source must announce its own session before its state
    # reports are honoured; sequences are tracked per source, so herdr's managed
    # hook (which announces under `herdr:<agent>`) does not cover us. Without
    # this the report is buffered and dropped with no error.
    session_params = {"pane_id": pane_id, "source": source, "agent": agent,
                      "seq": seq - 1}
    if session_id:
        session_params["agent_session_id"] = session_id
    if session_path:
        session_params["agent_session_path"] = session_path

    agent_params = {"pane_id": pane_id, "source": source, "agent": agent,
                    "state": state, "seq": seq}
    message = message_for(blocking, agents, gates)
    if message:
        agent_params["message"] = message

    # Always write every token. Tokens persist across reports, so omitting one
    # leaves the previous turn's value on screen -- a finished pane would keep
    # advertising agents that already exited.
    tokens = {
        "closing_blocking": str(blocking),
        "closing_agents": str(agents),
        "closing_idle": "1" if state == "idle" else "0",
        "closing_agent_names": "; ".join(agent_names)[:200],
        "closing_gates": "; ".join(gates)[:200],
    }
    meta_params = {
        "pane_id": pane_id,
        "source": source,
        "applies_to_source": source,
        "tokens": tokens,
        "state_labels": {
            "blocked": f"gate ×{blocking}" if blocking else "blocked",
            "working": f"{agents} agents" if agents else "working",
        },
        "seq": seq,
    }

    try:
        _rpc(sock_path, source, "pane.report_agent_session", session_params)
        _rpc(sock_path, source, "pane.report_agent", agent_params)
        _rpc(sock_path, source, "pane.report_metadata", meta_params)
        outcome["socket"] = True
    except OSError:
        pass  # mirror already written; a hook must never wedge a turn
    return outcome
