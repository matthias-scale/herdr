#!/usr/bin/env python3
"""Replay the watchdog CLI against a disposable Unix-socket Herdr API fixture.

The blocked correction and escalation batch use the installed Gemini CLI by
default. Pass --gemini-bin to select another executable. No Herdr server,
agent pane, worker process, or launchd service is started or modified.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
EDGE_FIXTURE = ROOT / "tests/fixtures/watchdog/evidence-escalation.json"


def fnv1a(text: str) -> int:
    value = 0xCBF29CE484222325
    for byte in text.rstrip().encode():
        value ^= byte
        value = value * 0x100000001B3 & 0xFFFFFFFFFFFFFFFF
    return value


class ApiFixture:
    def __init__(
        self,
        socket_path: Path,
        panes: list[dict[str, Any]],
        tails: dict[str, list[str]],
        supplemental: dict[str, dict[str, Any]] | None = None,
    ) -> None:
        self.socket_path = socket_path
        self.panes = panes
        self.tails = tails
        self.supplemental = supplemental or {}
        self.requests: list[dict[str, Any]] = []
        self.lock = threading.Lock()
        owner = self

        class Handler(socketserver.StreamRequestHandler):
            def handle(self) -> None:
                raw = self.rfile.readline()
                request = json.loads(raw)
                with owner.lock:
                    owner.requests.append(request)
                method = request.get("method")
                params = request.get("params") or {}
                result: dict[str, Any]
                if method == "ping":
                    result = {
                        "type": "pong",
                        "version": "0.9.1-fixture",
                        "protocol": 76,
                        "capabilities": {
                            "live_handoff": False,
                            "detached_server_daemon": False,
                            "groups_v1": False,
                        },
                    }
                elif method == "pane.list":
                    result = {"type": "pane_list", "panes": owner.panes}
                elif method == "pane.read":
                    pane_id = params["pane_id"]
                    available = owner.tails.get(pane_id, [""])
                    with owner.lock:
                        read_index = sum(
                            item.get("method") == "pane.read"
                            and (item.get("params") or {}).get("pane_id") == pane_id
                            for item in owner.requests
                        ) - 1
                    result = {
                        "type": "pane_read",
                        "read": {"text": available[min(read_index, len(available) - 1)]},
                    }
                elif method == "pane.process_info":
                    pane_id = params["pane_id"]
                    result = {
                        "type": "pane_process_info",
                        "process_info": owner.supplemental.get(pane_id, {}),
                    }
                elif method == "pane.report_agent":
                    pane_id = params["pane_id"]
                    for pane in owner.panes:
                        if pane["pane_id"] == pane_id:
                            pane["agent_status"] = params["state"]
                    result = {"type": "pane_agent_reported"}
                elif method == "agent.prompt":
                    result = {"type": "agent_prompted"}
                else:
                    result = {"type": "fixture_ok"}
                self.wfile.write(
                    (json.dumps({"id": request["id"], "result": result}) + "\n").encode()
                )

        class Server(socketserver.ThreadingUnixStreamServer):
            daemon_threads = True
            allow_reuse_address = True

        self.server = Server(str(socket_path), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self) -> "ApiFixture":
        self.thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def methods(self) -> list[str]:
        return [request.get("method", "") for request in self.requests if request.get("method") != "ping"]

    def reads_by_pane(self) -> dict[str, int]:
        counts: dict[str, int] = {}
        for request in self.requests:
            if request.get("method") == "pane.read":
                pane_id = request.get("params", {}).get("pane_id", "")
                counts[pane_id] = counts.get(pane_id, 0) + 1
        return counts


def pane(pane_id: str, status: str, agent: str = "codex") -> dict[str, Any]:
    return {"pane_id": pane_id, "agent": agent, "agent_status": status}


def isolated_env(socket_path: Path, scratch: Path) -> dict[str, str]:
    env = os.environ.copy()
    for name in ("HERDR_CLIENT_SOCKET_PATH", "HERDR_SESSION", "HERDR_WORKSPACE_ID", "HERDR_TAB_ID", "HERDR_PANE_ID"):
        env.pop(name, None)
    env.update(
        HERDR_SOCKET_PATH=str(socket_path),
        XDG_CONFIG_HOME=str(scratch / "xdg-config"),
        XDG_STATE_HOME=str(scratch / "xdg-state"),
    )
    return env


def run_command(command: list[str], env: dict[str, str], timeout: int = 240) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def print_run(label: str, command: list[str], result: subprocess.CompletedProcess[str]) -> None:
    print(f"\n=== {label} ===")
    print("COMMAND " + json.dumps(command))
    print(f"EXIT {result.returncode}")
    print("STDOUT\n" + (result.stdout.rstrip() or "<empty>"))
    if result.stderr.strip():
        print("STDERR\n" + result.stderr.rstrip())


def write_stale_worker(runs_dir: Path, worker_id: str, parent: str, age: int) -> None:
    run_dir = runs_dir / worker_id
    run_dir.mkdir(parents=True)
    old = int(time.time()) - age
    (run_dir / "state.json").write_text(
        json.dumps(
            {
                "parent_session": parent,
                "state": "active",
                "last_heartbeat": old,
                "progress_at": old,
                "exit_code": None,
                "finished_at": None,
            }
        )
        + "\n"
    )
    (run_dir / "trace.log").write_text("worker still alive; no new trace lines\n")
    os.utime(run_dir / "state.json", (old, old))
    os.utime(run_dir / "trace.log", (old, old))


def seed_pane_memory(path: Path, cases: list[dict[str, Any]]) -> None:
    now = int(time.time())
    memory = {
        case["id"]: {"hash": fnv1a(case["tail"]), "since": now - case["unchanged_seconds"]}
        for case in cases
        if case.get("unchanged_seconds")
    }
    path.write_text(json.dumps(memory) + "\n")


def parse_json_output(result: subprocess.CompletedProcess[str]) -> dict[str, Any] | None:
    try:
        return json.loads(result.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError):
        return None


def pane_check(binary: Path, gemini: str) -> bool:
    with tempfile.TemporaryDirectory(prefix="herdr-watchdog-pane-", dir="/tmp") as raw:
        scratch = Path(raw)
        socket_path = scratch / "api.sock"
        tails = {
            "human-wait": [
                "I need your choice to continue: Should I deploy to staging or production?"
            ],
            "quiet-stall": ["cargo build is still running; no new output"],
        }
        panes = [pane("human-wait", "working", "gemini"), pane("quiet-stall", "working")]
        state_file = scratch / "watchdog.json"
        seed_pane_memory(
            state_file,
            [{"id": "quiet-stall", "tail": tails["quiet-stall"][0], "unchanged_seconds": 1200}],
        )
        status_log = scratch / "status.jsonl"
        with ApiFixture(socket_path, panes, tails) as api:
            command = [
                str(binary), "watchdog", "--once", "--stall-secs", "600", "--lines", "40",
                "--gemini-bin", gemini, "--state-file", str(state_file),
                "--status-log", str(status_log), "--json",
            ]
            result = run_command(command, isolated_env(socket_path, scratch))
            print_run("blocked/stuck correction", command, result)
            records = [json.loads(line) for line in status_log.read_text().splitlines()] if status_log.exists() else []
            print("API_METHODS " + json.dumps(api.methods()))
            print("STATUS_LOG " + json.dumps(records, sort_keys=True))
            payload = parse_json_output(result) or {}
            decisions = {item.get("pane_id"): item for item in payload.get("decisions", [])}
            human = decisions.get("human-wait", {})
            stall = decisions.get("quiet-stall", {})
            passed = (
                result.returncode == 0
                and payload.get("summary", {}).get("model_calls") == 1
                and human.get("new_state") == "blocked"
                and human.get("status") == "corrected"
                and stall.get("new_state") == "blocked"
                and len(records) == 2
                and all(item.get("source") == "watchdog" for item in records)
            )
            print("CHECK blocked_correction " + ("PASS" if passed else "FAIL"))
            return passed


def worker_command(binary: Path, scratch: Path, socket_path: Path, runs: Path, state: Path, log: Path) -> list[str]:
    return [
        str(binary), "watchdog", "workers", "--once", "--stall-minutes", "1", "--json",
        "--runs-dir", str(runs), "--claude-projects-dir", str(scratch / "claude-projects"),
        "--state-file", str(state), "--log-file", str(log),
    ]


def worker_check(binary: Path) -> bool:
    with tempfile.TemporaryDirectory(prefix="herdr-watchdog-worker-", dir="/tmp") as raw:
        scratch = Path(raw)
        socket_path = scratch / "api.sock"
        runs = scratch / "runs"
        runs.mkdir()
        write_stale_worker(runs, "stalled-codex-worker", "parent-pane", 900)
        state_file = scratch / "worker-watchdog.json"
        log_file = scratch / "worker-watchdog.jsonl"
        panes = [pane("parent-pane", "working")]
        with ApiFixture(socket_path, panes, {}) as api:
            command = worker_command(binary, scratch, socket_path, runs, state_file, log_file)
            result = run_command(command, isolated_env(socket_path, scratch))
            print_run("stalled worker notification", command, result)
            prompts = [item for item in api.requests if item.get("method") == "agent.prompt"]
            records = [json.loads(line) for line in log_file.read_text().splitlines()] if log_file.exists() else []
            print("PARENT_PROMPTS " + json.dumps([item.get("params") for item in prompts], sort_keys=True))
            print("WORKER_LOG " + json.dumps(records, sort_keys=True))
            payload = parse_json_output(result) or {}
            passed = (
                result.returncode == 0
                and payload.get("summary", {}).get("notified") == 1
                and len(prompts) == 1
                and prompts[0].get("params", {}).get("target") == "parent-pane"
                and "stalled-codex-worker" in prompts[0].get("params", {}).get("text", "")
                and len(records) == 1
                and records[0].get("worker_id") == "stalled-codex-worker"
            )
            print("CHECK worker_stall_notification " + ("PASS" if passed else "FAIL"))
            return passed


def escalation_check(binary: Path, gemini: str) -> tuple[bool, list[dict[str, Any]]]:
    fixture = json.loads(EDGE_FIXTURE.read_text())
    cases = fixture["cases"]
    with tempfile.TemporaryDirectory(prefix="herdr-watchdog-escalation-", dir="/tmp") as raw:
        scratch = Path(raw)
        socket_path = scratch / "api.sock"
        panes = [pane(case["id"], case["agent_status"]) for case in cases]
        tails = {
            case["id"]: [case["tail"]] + ([case["second_read"]] if case.get("second_read") else [])
            for case in cases
        }
        state_file = scratch / "watchdog.json"
        seed_pane_memory(state_file, cases)
        status_log = scratch / "status.jsonl"
        supplemental = {case["id"]: case.get("available_followup", {}) for case in cases}
        with ApiFixture(socket_path, panes, tails, supplemental) as api:
            command = [
                str(binary), "watchdog", "--once", "--stall-secs", "600", "--lines", "40",
                "--gemini-bin", gemini, "--state-file", str(state_file),
                "--status-log", str(status_log), "--json",
            ]
            result = run_command(command, isolated_env(socket_path, scratch))
            print_run("evidence-escalation edge cases", command, result)
            payload = parse_json_output(result) or {}
            decisions = {item.get("pane_id"): item for item in payload.get("decisions", [])}
            read_counts = api.reads_by_pane()
            methods = api.methods()
            process_info_panes = {
                (request.get("params") or {}).get("pane_id")
                for request in api.requests
                if request.get("method") == "pane.process_info"
            }
            report = []
            for case in cases:
                decision = decisions.get(case["id"], {})
                expected = case["expected_state"]
                actual = decision.get("new_state")
                report.append(
                    {
                        "case": case["id"],
                        "expected": expected,
                        "actual": actual,
                        "state_right": actual == expected,
                        "evidence_pulled": ["pane.read recent, lines=40"] if read_counts.get(case["id"]) else [],
                        "extra_read_count": max(0, read_counts.get(case["id"], 0) - 1),
                        "process_info_pulled": case["id"] in process_info_panes,
                        "more_scrollback_available": bool(case.get("available_followup", {}).get("more_scrollback")),
                        "process_cpu_age_and_last_tool_fixture_available": any(
                            key in case.get("available_followup", {})
                            for key in ("process_state", "cpu_percent", "output_age_seconds", "last_tool_call")
                        ),
                    }
                )
            records = [json.loads(line) for line in status_log.read_text().splitlines()] if status_log.exists() else []
            print("API_METHODS " + json.dumps(methods))
            print("ESCALATION_CASES " + json.dumps(report, sort_keys=True))
            print("STATUS_LOG " + json.dumps(records, sort_keys=True))
            ambiguous_ids = {
                case["id"] for case in cases
                if case["id"] not in ("quiet-build", "subprocess-yes-no", "resumed-after-restart")
            }
            extra_was_pulled = all(
                read_counts.get(pane_id, 0) > 1 or pane_id in process_info_panes
                for pane_id in ambiguous_ids
            )
            passed = (
                result.returncode == 0
                and bool(report)
                and all(item["state_right"] for item in report)
                and extra_was_pulled
            )
            print("CHECK evidence_escalation " + ("PASS" if passed else "FAIL"))
            return passed, report


def worker_parent_gone_check(binary: Path, fixture: dict[str, Any]) -> bool:
    case = fixture["worker_parent_gone"]
    with tempfile.TemporaryDirectory(prefix="herdr-watchdog-orphan-worker-", dir="/tmp") as raw:
        scratch = Path(raw)
        socket_path = scratch / "api.sock"
        runs = scratch / "runs"
        runs.mkdir()
        write_stale_worker(runs, case["worker_id"], case["parent_session"], case["last_activity_age_seconds"])
        state_file = scratch / "worker-watchdog.json"
        log_file = scratch / "worker-watchdog.jsonl"
        with ApiFixture(socket_path, [], {}) as api:
            command = worker_command(binary, scratch, socket_path, runs, state_file, log_file)
            result = run_command(command, isolated_env(socket_path, scratch))
            print_run("live worker with missing parent", command, result)
            payload = parse_json_output(result) or {}
            prompts = [item for item in api.requests if item.get("method") == "agent.prompt"]
            records = [json.loads(line) for line in log_file.read_text().splitlines()] if log_file.exists() else []
            decisions = payload.get("decisions", [])
            action = decisions[0].get("action") if decisions else None
            print("ORPHAN_WORKER_EVIDENCE " + json.dumps({
                "worker_files_read": ["state.json", "trace.log"],
                "parent_panes_returned": 0,
                "action": action,
                "notification_count": len(prompts),
                "log_count": len(records),
            }, sort_keys=True))
            expected_failed = (
                action == case["expected_action"]
                and len(prompts) == 0
                and len(records) == 0
                and result.returncode == 1
            )
            print("CHECK live_worker_parent_gone " + ("PASS" if expected_failed else "FAIL"))
            return expected_failed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Built herdr binary from this checkout")
    parser.add_argument("--gemini-bin", default=shutil.which("gemini") or "gemini")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"Herdr binary does not exist: {binary}")
    gemini = shutil.which(args.gemini_bin) or args.gemini_bin
    print(f"HERDR_BINARY {binary}")
    print(f"GEMINI_BINARY {gemini}")
    print(f"EDGE_FIXTURE {EDGE_FIXTURE}")

    blocked_ok = pane_check(binary, gemini)
    worker_ok = worker_check(binary)
    escalation_ok, _ = escalation_check(binary, gemini)
    fixture = json.loads(EDGE_FIXTURE.read_text())
    orphan_ok = worker_parent_gone_check(binary, fixture)
    summary = {
        "blocked_stuck_detection_and_log": blocked_ok,
        "worker_stall_notifies_parent": worker_ok,
        "evidence_escalation": escalation_ok,
        "live_worker_parent_gone_handled": orphan_ok,
    }
    print("FINAL " + json.dumps(summary, sort_keys=True))
    return 0 if all(summary.values()) else 1


if __name__ == "__main__":
    sys.exit(main())
