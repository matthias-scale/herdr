"""Agent-agnostic v2 turn-end status contract for herdr.

The closing-block adapter writes one payload:

    {"v": 2, "agent": "claude", "blocking": 1, "agents": 0,
     "gates": [{"n": 1, "label": "Gate", "text": "...", "pr": null,
                "ticket": null, "url": null, "default": null,
                "default_at": null}],
     "items": [], "decisions": [], "agent_names": []}

The arrays are sent through the existing agent-report channel. The blocked state
label carries the first gate so the existing sidebar/detail view is answerable
in place. A turn-end hook never raises.
"""

from __future__ import annotations

import json
import os
import random
import socket
import tempfile
import time
from datetime import datetime, timezone
from typing import Any

# HERDR_INTEGRATION_VERSION=2
VERSION = 2


def state_for(blocking: int, agents: int) -> str:
    if blocking > 0:
        return "blocked"
    if agents > 0:
        return "working"
    return "idle"


def _item_text(item: dict[str, Any]) -> str:
    return str(item.get("text") or "").strip()


def _normalize_item(
    value: dict[str, Any] | str,
    *,
    index: int,
    label: str,
) -> dict[str, Any]:
    if isinstance(value, str):
        text = value
        return {
            "n": index,
            "label": label,
            "text": text,
            "pr": None,
            "ticket": None,
            "url": None,
            "default": None,
            "default_at": None,
        }
    item = dict(value)
    item.setdefault("n", index)
    item.setdefault("label", label)
    item.setdefault("text", "")
    item.setdefault("pr", None)
    item.setdefault("ticket", None)
    item.setdefault("url", None)
    item.setdefault("default", None)
    item.setdefault("default_at", None)
    return item


def _normalize_decision(
    value: dict[str, Any],
    *,
    index: int,
) -> dict[str, Any]:
    decision = dict(value)
    decision.setdefault("n", index)
    decision.setdefault("text", "")
    decision.setdefault("recommendation", decision["text"])
    decision["reversible"] = True
    if decision.get("decided_at") is None:
        decision["decided_at"] = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    return decision


def message_for(
    blocking: int,
    agents: int,
    gates: list[dict[str, Any]],
) -> str | None:
    if blocking > 0:
        head = _item_text(gates[0]) if gates else ""
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


def accepts_payload(payload: object) -> bool:
    """Return false for an old/new wire version without raising or reporting."""
    if not isinstance(payload, dict):
        return False
    version = payload.get("v")
    return version is None or version == VERSION


def report(
    *,
    agent: str,
    blocking: int,
    agents: int,
    gates: list[dict[str, Any] | str] | None = None,
    items: list[dict[str, Any] | str] | None = None,
    decisions: list[dict[str, Any]] | None = None,
    agent_names: list[str] | None = None,
    session_id: str | None = None,
    session_path: str | None = None,
    title: str | None = None,
    pane_id: str | None = None,
    sock_path: str | None = None,
) -> dict:
    """Push one v2 turn-end status. Never raises; returns what it did."""
    gate_objects = [
        _normalize_item(value, index=index, label="Gate")
        for index, value in enumerate(gates or [], start=1)
    ]
    item_objects = [
        _normalize_item(value, index=index, label="Answer")
        for index, value in enumerate(items or [], start=1)
    ]
    decision_objects = [
        _normalize_decision(value, index=index)
        for index, value in enumerate(decisions or [], start=1)
    ]
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
        "gates": gate_objects,
        "items": item_objects,
        "decisions": decision_objects,
        "agent_names": agent_names,
    }
    if title:
        payload["title"] = title

    outcome = {"payload": payload, "mirror": None, "socket": False}
    if pane_id:
        outcome["mirror"] = write_mirror(pane_id, payload)
    if not (pane_id and sock_path):
        return outcome

    source = f"herdr:{agent}-closing-block"
    session_params = {
        "pane_id": pane_id,
        "source": source,
        "agent": agent,
        "seq": seq - 1,
    }
    if session_id:
        session_params["agent_session_id"] = session_id
    if session_path:
        session_params["agent_session_path"] = session_path

    agent_params = {"pane_id": pane_id, "source": source, "agent": agent,
                    "state": state, "seq": seq,
                    "gates": gate_objects, "items": item_objects,
                    "decisions": decision_objects}
    message = message_for(blocking, agents, gate_objects)
    if message:
        agent_params["message"] = message

    gate_texts = [_item_text(gate) for gate in gate_objects]
    tokens = {
        "closing_blocking": str(blocking),
        "closing_agents": str(agents),
        "closing_idle": "1" if state == "idle" else "0",
        "closing_agent_names": "; ".join(agent_names)[:200],
        "closing_gates": "; ".join(gate_texts)[:200],
        "session_title": (title or "")[:120],
    }
    meta_params = {
        "pane_id": pane_id,
        "source": source,
        "applies_to_source": source,
        "tokens": tokens,
        "state_labels": {
            "blocked": message if blocking else "blocked",
            "working": "working",
        },
        "seq": seq,
    }

    try:
        _rpc(sock_path, source, "pane.report_agent_session", session_params)
        _rpc(sock_path, source, "pane.report_agent", agent_params)
        _rpc(sock_path, source, "pane.report_metadata", meta_params)
        outcome["socket"] = True
    except OSError:
        pass
    return outcome
