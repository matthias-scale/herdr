#!/usr/bin/env python3
"""Rebuild conservative pane observations and ask Herdr's Rust watchdog to replay them."""

from __future__ import annotations

import argparse
import datetime as dt
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

UTC = dt.timezone.utc
STATE = Path.home() / ".local/state/herdr"


def epoch(value: str | None) -> int | None:
    if not value:
        return None
    try:
        return int(dt.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp())
    except (TypeError, ValueError):
        return None


def text_content(value: object) -> str:
    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    parts = []
    for block in value:
        if not isinstance(block, dict):
            continue
        if block.get("type") in ("text", "output_text", "input_text"):
            content = block.get("text")
            if isinstance(content, str):
                parts.append(content)
    return "\n".join(parts).strip()


def transcript_paths() -> tuple[dict[str, Path], dict[str, Path]]:
    claude = {}
    for path in glob.glob(str(Path.home() / ".claude/projects/*/*.jsonl")):
        claude[Path(path).stem] = Path(path)
    codex = {}
    for path in glob.glob(str(Path.home() / ".codex*/sessions/**/rollout-*"), recursive=True):
        name = Path(path).name
        match = re.search(r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$", name)
        if match:
            session = match.group(1)
            codex[session] = Path(path)
    return claude, codex


def transcript_events(agent: str, session_id: str, files: tuple[dict[str, Path], dict[str, Path]]):
    claude, codex = files
    path = claude.get(session_id) if agent == "claude" else codex.get(session_id)
    if path is None or not path.is_file():
        return None
    events = []
    try:
        with path.open() as source:
            for line in source:
                try:
                    row = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                when = epoch(row.get("timestamp"))
                if when is None:
                    continue
                event = {"ts": when, "role": "", "text": "", "call": None, "result": None}
                if agent == "claude":
                    message = row.get("message", {})
                    if row.get("type") == "assistant":
                        event["role"] = "assistant"
                        content = message.get("content", [])
                        event["text"] = text_content(content)
                        calls = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
                        event["call"] = calls
                    elif row.get("type") == "user":
                        results = [b for b in message.get("content", []) if isinstance(b, dict) and b.get("type") == "tool_result"]
                        if results:
                            event["result"] = results
                        else:
                            event["role"] = "user"
                elif row.get("type") == "response_item":
                    payload = row.get("payload", {})
                    kind = payload.get("type")
                    if kind == "message" and payload.get("role") == "assistant":
                        event["role"] = "assistant"
                        event["text"] = text_content(payload.get("content", []))
                    elif kind == "function_call":
                        event["call"] = [{"id": payload.get("call_id"), "name": payload.get("name"), "input": payload.get("arguments", "{}") }]
                    elif kind == "function_call_output":
                        event["result"] = [{"tool_use_id": payload.get("call_id"), "content": payload.get("output", "") }]
                if event["role"] or event["call"] or event["result"]:
                    events.append(event)
    except OSError:
        return None
    events.sort(key=lambda item: item["ts"])
    return events


def call_input(call: dict) -> dict:
    value = call.get("input", {})
    if isinstance(value, dict):
        return value
    try:
        parsed = json.loads(value)
        return parsed if isinstance(parsed, dict) else {}
    except (TypeError, json.JSONDecodeError):
        return {}


def tool_name(call: dict) -> str:
    return str(call.get("name", "")).lower()


def tool_result_text(result: dict) -> str:
    value = result.get("content", "")
    if isinstance(value, list):
        return " ".join(str(part.get("text", "")) if isinstance(part, dict) else str(part) for part in value)
    return str(value)


def external_wait(text: str) -> bool:
    """Treat a visible wait on an outside event as unknown background work."""
    for line in text.splitlines():
        line = line.strip()
        match = re.match(r"(?i)^\*{0,2}now:\*{0,2}\s*(.*)$", line)
        if not match:
            continue
        value = match.group(1).strip().strip("*").lower()
        if value.startswith(("waiting on you", "waiting for your", "waiting for human", "awaiting your")):
            return True
        if "worker" in value and re.search(r"\b(?:building|running|working|starts|finish|finishes)\b", value):
            return True
        if value.startswith(("wait", "waiting", "awaiting")) and re.search(
            r"\b(?:ci|qa|github|review|approval|decision|worker|ub1|ub2|free up|notification|result)\b",
            value,
        ):
            return True
    return False


def read_jsonl(path: Path) -> list[dict]:
    rows = []
    try:
        with path.open() as source:
            for line in source:
                try:
                    row = json.loads(line)
                    if isinstance(row, dict):
                        rows.append(row)
                except json.JSONDecodeError:
                    continue
    except FileNotFoundError:
        pass
    return rows


def status_value(row: dict) -> str:
    value = str(row.get("to_state") or "unknown").lower()
    if value in {"idle", "working", "blocked", "done", "stale", "unknown"}:
        return value
    if row.get("completion") == "complete":
        return "done"
    if row.get("idle") is True:
        return "idle"
    return "unknown"


def old_nudge_count(cutoff: int, end: int) -> int:
    count = 0
    for path in (STATE / "watchdog-status.log", STATE / "watchdog-status-log.jsonl"):
        for row in read_jsonl(path):
            try:
                when = int(row.get("timestamp", 0))
            except (TypeError, ValueError):
                continue
            if cutoff <= when <= end and row.get("action") == "nudge":
                count += 1
    return count


def load_data(cutoff: int, end: int, files):
    by_pair = defaultdict(list)
    for row in read_jsonl(STATE / "status-changes.jsonl"):
        when = epoch(row.get("ts"))
        session = row.get("session_id")
        pane = row.get("pane_id")
        if when is None or not pane or when > end:
            continue
        by_pair[(pane, session or "")].append((when, row))

    active = {}
    for key, records in by_pair.items():
        records.sort(key=lambda item: item[0])
        recent = [record for record in records if record[0] >= cutoff]
        if not recent:
            continue
        latest = max(recent, key=lambda item: item[0])[1]
        agent = str(latest.get("agent") or "unknown").lower()
        active[key] = {
            "records": records,
            "agent": agent,
            "events": transcript_events(agent, key[1], files) if key[1] else None,
        }
    return active


def iter_observations(active, cutoff: int, end: int, host: str):
    step = ((cutoff + 59) // 60) * 60
    quiet_periods = 0
    current = {}
    for timestamp in range(step, end + 1, 60):
        for (pane, session), item in active.items():
            status_records = item["records"]
            status_index = next((i for i in range(len(status_records) - 1, -1, -1) if status_records[i][0] <= timestamp), None)
            if status_index is None:
                continue
            status_time, status_row = status_records[status_index]
            events = item["events"]
            status = status_value(status_row)
            tail = ""
            background = "unknown" if events is None else "none"
            last_entry = None
            assistant_texts = []
            pending = {}
            background_shells = set()
            event_index = current.get((pane, session), {}).get("event_index", 0)
            state = current.get((pane, session), {})
            previous_entry = state.get("last_entry")
            assistant_texts = state.get("assistant_texts", [])
            pending = state.get("pending", {})
            background_shells = state.get("background_shells", set())
            last_entry = state.get("last_entry")
            if events is None:
                last_entry = state.get("last_entry", status_time)
            if events is not None:
                while event_index < len(events) and events[event_index]["ts"] <= timestamp:
                    event = events[event_index]
                    last_entry = event["ts"]
                    for call in event["call"] or []:
                        key = str(call.get("id") or "")
                        if key:
                            pending[key] = call
                    for result in event["result"] or []:
                        key = str(result.get("tool_use_id") or "")
                        call = pending.pop(key, None)
                        if call and tool_name(call) in {"bash", "exec_command", "agent", "task"} and call_input(call).get("run_in_background"):
                            result_text = tool_result_text(result).lower()
                            completed = any(marker in result_text for marker in ("completed successfully", "task completed", "process exited", "command finished", "no shell"))
                            if not completed:
                                background_shells.add(key)
                        elif call and tool_name(call) in {"bashoutput", "monitor"}:
                            if any(token in tool_result_text(result).lower() for token in ("completed", "exited", "finished", "no shell")):
                                background_shells.clear()
                    if event["role"] == "assistant" and event["text"]:
                        assistant_texts.append(event["text"])
                        assistant_texts = assistant_texts[-6:]
                    event_index += 1
                if pending or background_shells:
                    background = "live"
                if assistant_texts:
                    tail = "\n".join(assistant_texts)[-7000:] + "\n❯ \n"
                    if background == "none" and external_wait(tail):
                        background = "unknown"
                if last_entry != previous_entry:
                    state["in_quiet"] = False
                state.update({
                    "event_index": event_index,
                    "assistant_texts": assistant_texts,
                    "pending": pending,
                    "background_shells": background_shells,
                    "last_entry": last_entry,
                })
            else:
                state.setdefault("last_entry", last_entry)
            quiet_age = timestamp - last_entry if last_entry is not None else 0
            if quiet_age < 60:
                state["in_quiet"] = False
                current[(pane, session)] = state
                continue
            if not state.get("in_quiet", False):
                quiet_periods += 1
                state["in_quiet"] = True
            current[(pane, session)] = state
            hook_age = max(0, timestamp - status_time)
            yield {
                "timestamp": timestamp,
                "host": host,
                "pane_id": pane,
                "session_id": session,
                "agent": item["agent"],
                "status": status,
                "reported_at": str(status_time),
                "hook_age_secs": hook_age,
                "tail": tail,
                "background": background,
            }, quiet_periods


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="PR build of herdr")
    parser.add_argument("--host", default=os.uname().nodename.split(".")[0])
    parser.add_argument("--hours", type=int, default=24)
    parser.add_argument("--out", default=f"/tmp/wd-replay-{os.uname().nodename.split('.')[0]}")
    args = parser.parse_args()
    end = int(dt.datetime.now(UTC).timestamp())
    cutoff = end - args.hours * 3600
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    rows_path = out / "input.jsonl"
    decisions_path = out / "decisions.jsonl"
    attempts_path = Path(f"/tmp/wd-replay-{args.host}.jsonl")
    summary_path = out / "summary.json"
    report_path = out / "result.md"
    files = transcript_paths()
    active = load_data(cutoff, end, files)
    quiet_periods = 0
    observation_count = 0
    with rows_path.open("w") as target:
        for row, quiet_periods in iter_observations(active, cutoff, end, args.host):
            target.write(json.dumps(row, ensure_ascii=False) + "\n")
            observation_count += 1
    cmd = [args.binary, "watchdog", "--replay", str(rows_path), "--json"]
    with decisions_path.open("w") as output:
        result = subprocess.run(cmd, stdout=output, stderr=subprocess.PIPE, text=True, check=False)
    if result.returncode:
        print(result.stderr, file=sys.stderr)
        return result.returncode

    attempts = []
    with decisions_path.open() as source:
        for line in source:
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            if row.get("would_nudge"):
                attempts.append(row)
    with attempts_path.open("w") as target:
        for row in attempts:
            target.write(json.dumps({"host": args.host, **row}, ensure_ascii=False) + "\n")
    episode_counts = defaultdict(int)
    for row in attempts:
        episode_counts[(row["pane_id"], row["session_id"], row["tail_hash"])] += 1
    summary = {
        "host": args.host,
        "window_start_utc": dt.datetime.fromtimestamp(cutoff, UTC).isoformat(),
        "window_end_utc": dt.datetime.fromtimestamp(end, UTC).isoformat(),
        "panes_scanned": len({pane for pane, _ in active}),
        "pane_sessions_scanned": len(active),
        "sessions_scanned": len({session for _, session in active if session}),
        "quiet_periods": quiet_periods,
        "observations": observation_count,
        "would_nudge_attempts": len(attempts),
        "distinct_panes_episodes_nudged": len(episode_counts),
        "max_attempts_per_episode": max(episode_counts.values(), default=0),
        "old_watchdog_nudges_same_window": old_nudge_count(cutoff, end),
        "pane_sessions_without_transcript": sum(item["events"] is None for item in active.values()),
        "binary": str(Path(args.binary).resolve()),
        "binary_sha256": hashlib.sha256(Path(args.binary).read_bytes()).hexdigest(),
        "attempt_rows": str(attempts_path),
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    lines = [
        f"# Watchdog 24-hour replay ({args.host})",
        "",
        f"Window: {summary['window_start_utc']} to {summary['window_end_utc']}.",
        f"PR build: `{summary['binary']}` (`{summary['binary_sha256']}`).",
        "",
        "## Summary",
        "",
        f"- Panes scanned: {summary['panes_scanned']} across {summary['sessions_scanned']} sessions.",
        f"- Quiet periods: {quiet_periods}; synthesized observations: {observation_count}.",
        f"- Would-nudge attempts: {len(attempts)}; distinct pane/episodes: {len(episode_counts)}; max attempts per episode: {summary['max_attempts_per_episode']}.",
        f"- Old watchdog nudge records in the same window: {summary['old_watchdog_nudges_same_window']}.",
        f"- Pane/session entries without a matching transcript: {summary['pane_sessions_without_transcript']} (conservatively ineligible).",
        "",
        "## Would-nudge rows for hand check",
        "",
    ]
    if not attempts:
        lines.append("None.")
    for index, row in enumerate(attempts, 1):
        when = dt.datetime.fromtimestamp(row["timestamp"], UTC).isoformat()
        lines.extend([
            f"### {index}. {row['pane_id']} / {row['session_id']} — attempt {row['attempt']}",
            "",
            f"- Time: {when}; idle: {row['idle_secs'] // 60} min; hook age: {(row.get('hook_age_secs') or 0) // 60} min.",
            f"- Evidence: {row['evidence']}",
            f"- Background: {row['background']}; simulated delivered: {str(row['delivered']).lower()}.",
            "- Last 12 screen lines used:",
            "```text",
            *row["tail"],
            "```",
            "",
        ])
    report_path.write_text("\n".join(lines) + "\n")
    print(json.dumps(summary, indent=2))
    print(f"REPORT {report_path}")
    print(f"ATTEMPTS {attempts_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
