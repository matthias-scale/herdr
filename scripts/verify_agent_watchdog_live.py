#!/usr/bin/env python3
"""Run isolated live watchdog fixtures against a disposable Herdr server.

The harness uses only Python's standard library.  Every server, workspace,
worker process, run file and watchdog state file is created below a short-lived
directory and is removed on exit.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[1]


class Harness:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="wdl.", dir="/tmp"))
        self.cfg = self.root / "cfg"
        self.state = self.root / "state"
        self.sock = self.root / "s.sock"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
        self.env.update(XDG_CONFIG_HOME=str(self.cfg), XDG_STATE_HOME=str(self.state))
        self.server: subprocess.Popen[str] | None = None
        self.started: list[subprocess.Popen[Any]] = []
        self.panes: dict[str, str] = {}
        self.timeline = self.args.out / "timeline.jsonl"

    def start(self) -> None:
        app = "herdr-dev" if "debug" in str(self.args.binary) else "herdr"
        config = self.cfg / app / "config.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text("allow_nested = true\n", encoding="utf-8")
        self.state.mkdir(parents=True, exist_ok=True)
        self.env["HERDR_SOCKET_PATH"] = str(self.sock)
        self.server = subprocess.Popen([str(self.args.binary), "server"], cwd=self.root,
                                       env=self.env, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.PIPE, text=True, start_new_session=True)
        deadline = time.monotonic() + 30
        last = ""
        while time.monotonic() < deadline:
            if self.server.poll() is not None:
                last = self.server.stderr.read() if self.server.stderr else ""
                raise RuntimeError(f"disposable server exited {self.server.returncode}: {last}")
            if self.sock.exists():
                try:
                    self.call("ping", {})
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(.1)
        raise RuntimeError(f"server socket did not become ready: {last}")

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request = {"id": "watchdog-harness", "method": method, "params": params}
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(10)
            client.connect(str(self.sock))
            client.sendall((json.dumps(request) + "\n").encode())
            data = b""
            while b"\n" not in data:
                chunk = client.recv(65536)
                if not chunk:
                    break
                data += chunk
        try:
            result = json.loads(data.splitlines()[0])
        except (IndexError, json.JSONDecodeError) as exc:
            raise RuntimeError(f"invalid response for {method}: {data[:500]!r}") from exc
        if result.get("error"):
            raise RuntimeError(f"{method}: {result['error']}")
        return result.get("result", {})

    def cli(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        result = subprocess.run([str(self.args.binary), *args], cwd=self.root, env=self.env,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=45, check=False)
        if check and result.returncode:
            raise RuntimeError(f"herdr {' '.join(args)} exited {result.returncode}: "
                               f"{result.stdout[-800:]} {result.stderr[-800:]}")
        return result

    def workspace(self, label: str, command: str) -> str:
        result = self.cli("workspace", "create", "--cwd", str(self.root), "--label", label)
        payload = _last_json(result.stdout)
        pane = _find_value(payload, "pane_id")
        if not pane:
            raise RuntimeError(f"workspace create omitted pane_id: {result.stdout[-500:]}")
        self.panes[label] = str(pane)
        self.cli("pane", "run", str(pane), command)
        self.call("pane.report_agent", {"pane_id": str(pane), "source": "watchdog-harness",
                                        "agent": "codex", "state": "working"})
        return str(pane)

    def run_watchdog(self, family: str, options: list[str], dry: bool = True) -> dict[str, Any]:
        command = [str(self.args.binary), "watchdog"]
        if family == "B":
            command.append("workers")
        command += ["--once", "--json", "--confirm-secs", str(self.args.confirm_secs), *options]
        if dry:
            command.append("--dry-run")
        start = time.monotonic()
        result = subprocess.run(command, cwd=self.root, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=max(90, self.args.confirm_secs + 60), check=False)
        elapsed = round((time.monotonic() - start) * 1000)
        payload = _last_json(result.stdout) if result.stdout.strip() else {}
        if payload is None:
            payload = {"exit_code": result.returncode, "stdout": result.stdout[-1500:],
                       "stderr": result.stderr[-1000:]}
        payload["_wall_time_ms"] = elapsed
        payload["_model_latency_ms"] = (payload.get("summary") or {}).get("model_latency_ms")
        with self.timeline.open("a", encoding="utf-8") as output:
            output.write(json.dumps({"family": family, "command": command[1:],
                                     "wall_time_ms": elapsed,
                                     "model_latency_ms": payload["_model_latency_ms"],
                                     "result": payload}, sort_keys=True) + "\n")
        return payload

    def age_memory(self, path: Path, key: str, seconds: int, *, session: str | None = None) -> None:
        try:
            memory = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            memory = {}
        entry = memory.setdefault(key, {})
        aged = int(time.time()) - seconds
        for field in ("since", "retry_since", "op_since"):
            if field in entry:
                entry[field] = aged
        if "since" not in entry:
            entry["since"] = aged
        if session is not None:
            entry["agent_session"] = session
        path.write_text(json.dumps(memory) + "\n", encoding="utf-8")

    def cleanup(self) -> None:
        for proc in reversed(self.started):
            if proc.poll() is None:
                if os.name == "posix" and proc.pid != os.getpid():
                    try:
                        os.killpg(proc.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                else:
                    proc.terminate()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
        if self.server and self.server.poll() is None:
            # Ask the isolated server to shut down its own pane runtimes first.
            self.cli("server", "stop", check=False)
            try:
                self.server.wait(timeout=8)
            except subprocess.TimeoutExpired:
                # The dedicated process group contains only this harness server
                # and its descendants (never a user's existing Herdr session).
                try:
                    os.killpg(self.server.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                self.server.wait()
        shutil.rmtree(self.root, ignore_errors=True)


def _last_json(text: str) -> Any:
    for line in reversed(text.splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


def _find_value(value: Any, key: str) -> Any:
    if isinstance(value, dict):
        if key in value and value[key]:
            return value[key]
        for child in value.values():
            found = _find_value(child, key)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = _find_value(child, key)
            if found:
                return found
    return None


def _script(text: str, repeat: bool = False) -> str:
    literal = json.dumps(text)
    if repeat:
        code = ("import time\n" + f"print({literal}, flush=True)\n" +
                "i=0\nwhile True:\n time.sleep(2)\n i+=1\n " +
                f"print({literal} + ' ' + str(i), flush=True)\n")
    else:
        code = f"print({literal}, flush=True)\nimport time; time.sleep(3600)\n"
    return "python3 -u -c " + repr(code)


def setup_quiet(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Running cargo test"))


def setup_stall(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("static build output"))


def setup_prompt(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Overwrite generated snapshot? [y/n]"))


def setup_progress(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Compiling crate", repeat=True))


def setup_summary(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Build completed successfully"))


def setup_retry(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("API 429; retry in 120 seconds"))


def setup_account(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("You hit your usage limit; change account"))


def setup_spinner(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("◐ working", repeat=True))


def setup_quote(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script('> "Should I continue?"\nNew turn started\nWorking on files'))


def setup_dialog(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Do you want to allow this command?\n❯ 1. Yes\n  2. No\nEsc to cancel"))


def setup_prose_question(h: Harness, ident: str) -> str:
    return h.workspace(ident, _script("Which deployment option should I choose?"))


def setup_worker(h: Harness, ident: str) -> dict[str, str]:
    # Each run directory has its own live fixture process and aged trace.
    runs = h.root / "runs" / ident
    runs.mkdir(parents=True, exist_ok=True)
    worker = subprocess.Popen([sys.executable, "-c", "import subprocess,time; "
        "subprocess.Popen(['sleep','3600']); time.sleep(3600)"], cwd=h.root,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    h.started.append(worker)
    old = int(time.time()) - 3600
    terminal = ident == "b-finished"
    retry = "retry in 120 seconds\n" if "retry" in ident else ""
    prompt = "Overwrite? [y/n]\n" if "subprocess-yn" in ident else ""
    approval = ident == "b-approval"
    trace = runs / "trace.log"
    trace.write_text(retry + prompt + "codex\nexec\necho build\n")
    os.utime(trace, (old, old))
    parent: dict[str, Any] = {"host": h.args.host_label, "pane": "nonexistent",
                              "workspace": "missing", "session": "missing"}
    if ident == "p-unreachable":
        parent["host"] = "wd-unreachable.invalid"
    elif ident == "p-present-local":
        parent_pane = h.workspace("parent-" + ident, _script("Parent agent fixture"))
        parent_session = "parent-session-" + ident
        h.call("pane.report_agent_session", {"pane_id": parent_pane,
            "source": "watchdog-harness", "agent": "codex", "agent_session_id": parent_session})
        parent.update(pane=parent_pane, session=parent_session)
    (runs / "state.json").write_text(json.dumps({"schema": 1, "run_id": ident,
        "host": h.args.host_label, "agent": "codex", "pid": worker.pid,
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "state": "complete" if terminal else ("blocked" if approval else "active"),
        "blocked_reason": "approval required" if approval else None,
        "progress_at": None, "parent": parent}))
    os.utime(runs / "state.json", (old, old))
    turn = runs / "turns" / "turn-1"
    turn.mkdir(parents=True, exist_ok=True)
    (turn / "start.json").write_text(json.dumps({"started_at": time.time()}))
    (turn / "trace.log").write_text(trace.read_text())
    os.utime(turn / "trace.log", (old, old))
    if terminal:
        (turn / "receipt.json").write_text(json.dumps({"status": "ended-unverified", "exit_code": 0}))
    return {"runs": str(h.root / "runs")}


# Each row is deliberately self-contained: id, watchdog family, fixture setup,
# expected class. B and P records are created by their setup functions above.
CASES: list[tuple[str, str, Callable[[Harness, str], Any], str]] = [
    ("a-quiet-build", "A", setup_quiet, "working"),
    ("a-silent-stall", "A", setup_stall, "stalled"),
    ("a-quoted-question", "A", setup_quote, "working"),
    ("a-subprocess-yn", "A", setup_prompt, "waiting_tool_input"),
    ("a-finished-idle", "A", setup_summary, "finished_idle"),
    ("a-retry-backoff", "A", setup_retry, "waiting_retry"),
    ("a-retry-renewed", "A", setup_retry, "stalled"),
    ("a-account-limit", "A", setup_account, "waiting_human"),
    ("a-spinner-only", "A", setup_spinner, "stalled"),
    ("a-spinner-progress", "A", setup_progress, "working"),
    ("a-resumed", "A", setup_quote, "working"),
    ("a-approval-dialog", "A", setup_dialog, "waiting_human"),
    ("a-approval-hook", "A", setup_dialog, "waiting_human"),
    ("a-missing-parent", "A", setup_progress, "working"),
    ("a-prose-question", "A", setup_prose_question, "unknown"),
    ("b-quiet-build", "B", setup_worker, "tool_wait"),
    ("b-silent-stall", "B", setup_worker, "suspected_stall"),
    ("b-quoted-question", "B", setup_worker, "working"),
    ("b-subprocess-yn", "B", setup_worker, "waiting_tool_input"),
    ("b-finished", "B", setup_worker, "finished"),
    ("b-retry-backoff", "B", setup_worker, "waiting_retry"),
    ("b-retry-renewed", "B", setup_worker, "suspected_stall"),
    ("b-spinner-only", "B", setup_worker, "suspected_stall"),
    ("b-spinner-progress", "B", setup_worker, "working"),
    ("b-resumed", "B", setup_worker, "working"),
    ("b-resumed-same-op", "B", setup_worker, "suspected_stall"),
    ("b-approval", "B", setup_worker, "waiting_approval"),
    ("b-dead-pid", "B", setup_worker, "dead"),
    ("b-claude-subagent", "B", setup_worker, "working"),
    ("p-present-local", "P", setup_worker, "present"),
    ("p-missing-local", "P", setup_worker, "orphaned"),
    ("p-cross-host", "P", setup_worker, "present"),
    ("p-unreachable", "P", setup_worker, "parent_unknown"),
]


def self_test(args: argparse.Namespace) -> int:
    h = Harness(args)
    try:
        h.start()
        h.workspace("self-test", _script("harness fixture ready"))
        proc = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(3600)"],
                                cwd=h.root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        h.started.append(proc)
        (h.root / "self-test-runs").mkdir()
        (h.root / "timeline.jsonl").write_text(json.dumps({"self_test": True}) + "\n")
        print("SELF_TEST server_started=true workspace_created=true pane_created=true "
              f"worker_pid={proc.pid} timeline_written=true")
        return 0
    except Exception as exc:
        print(f"SELF_TEST_DENIED {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    finally:
        h.cleanup()
        print("SELF_TEST teardown=complete")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="herdr executable to test")
    parser.add_argument("--host-label", default=platform.node().split(".")[0])
    parser.add_argument("--out", type=Path, default=Path("watchdog-live-results"))
    parser.add_argument("--peer")
    parser.add_argument("--peer-binary", type=Path)
    parser.add_argument("--confirm-secs", type=int, default=5)
    parser.add_argument("--only", help="comma-separated case ids")
    parser.add_argument("--gemini-bin", help="pass through to the pane watchdog")
    parser.add_argument("--self-test", action="store_true", help="exercise fixture setup/teardown only")
    args = parser.parse_args()
    if args.binary is None:
        parser.error("--binary is required")
    args.binary = args.binary.resolve()
    if not args.binary.is_file():
        parser.error(f"binary not found: {args.binary}")
    if args.confirm_secs < 0:
        parser.error("--confirm-secs must be nonnegative")
    args.out.mkdir(parents=True, exist_ok=True)
    if args.self_test:
        return self_test(args)
    # Filter is validated before touching the server. CASES is the audited
    # source of ids and expected classes for every matrix entry.
    selected = {part.strip() for part in args.only.split(",")} if args.only else None
    known = {case[0] for case in CASES}
    if selected and selected - known:
        parser.error("unknown --only case(s): " + ", ".join(sorted(selected - known)))
    if args.gemini_bin:
        shutil.which(args.gemini_bin) or Path(args.gemini_bin).is_file() or parser.error(
            f"Gemini executable not found: {args.gemini_bin}")
    harness = Harness(args)
    rows: list[dict[str, Any]] = []
    try:
        harness.start()
        for ident, family, setup, expected in CASES:
            if selected is not None and ident not in selected:
                continue
            result = setup(harness, ident)
            if family == "A":
                pane_id = harness.panes[ident]
                session_id = "session-" + ident
                harness.call("pane.report_agent_session", {"pane_id": pane_id,
                    "source": "watchdog-harness", "agent": "codex",
                    "agent_session_id": session_id})
                status = "idle" if ident == "a-finished-idle" else (
                    "blocked" if ident == "a-approval-hook" else "working")
                report: dict[str, Any] = {"pane_id": pane_id, "source": "watchdog-harness",
                    "agent": "codex", "state": status}
                if ident.startswith("a-retry"):
                    report.update(wait="retry", eta_s=120, reported_at=int(time.time()))
                harness.call("pane.report_agent", report)
                state = harness.root / f"pane-{ident}.json"
                log = harness.root / f"pane-{ident}.jsonl"
                cmd_options = ["--dry-run", "--stall-secs", "600", "--confirm-secs",
                               str(args.confirm_secs), "--state-file", str(state),
                               "--status-log", str(log)]
                if args.gemini_bin:
                    cmd_options += ["--gemini-bin", args.gemini_bin]
                harness.run_watchdog("A", cmd_options)
                age = 900 if ident not in ("a-retry-backoff",) else 0
                if ident == "a-retry-renewed":
                    age = 1200
                if age:
                    harness.age_memory(state, pane_id, age,
                                       session="old-session" if ident == "a-resumed" else None)
                payload = harness.run_watchdog("A", cmd_options)
                decisions = payload.get("decisions", [])
                decision = next((d for d in decisions if d.get("pane_id") == pane_id), {})
                actual = decision.get("class", "missing")
                evidence = decision.get("evidence")
            elif family in ("B", "P"):
                opts = ["--dry-run", "--stall-minutes", "1", "--runs-dir",
                        str(Path(result["runs"]).parent), "--state-file", str(harness.root / f"worker-{ident}.json"),
                        "--log-file", str(harness.root / f"worker-{ident}.jsonl")]
                payload = harness.run_watchdog("B", opts)
                decision = next((d for d in payload.get("decisions", []) if d.get("worker_id") == ident), {})
                actual = decision.get("parent", "missing") if family == "P" else decision.get("class", "missing")
                evidence = decision.get("evidence")
            else:
                actual, evidence = "not_checked", "parent check included in worker result"
                payload = {"_wall_time_ms": 0, "_model_latency_ms": None}
            rows.append({"id": ident, "watchdog": family, "expected": expected,
                         "actual": actual, "match": actual == expected,
                         "evidence": evidence, "wall_time_ms": payload.get("_wall_time_ms"),
                         "model_latency_ms": payload.get("_model_latency_ms")})
        digest = hashlib.sha256(args.binary.read_bytes()).hexdigest()
        source = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout.strip()
        doc = {"host": args.host_label, "binary_sha256": digest, "source_sha": source, "cases": rows}
        (args.out / "matrix.json").write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
        lines = ["# Watchdog live matrix", "", f"Host: `{args.host_label}`  ",
                 f"Source: `{source}`  ", f"Binary SHA256: `{digest}`", "",
                 "| Case | Watchdog | Expected | Actual | Match |", "|---|---|---|---|---|"]
        lines.extend(f"| {r['id']} | {r['watchdog']} | {r['expected']} | {r['actual']} | {'yes' if r['match'] else 'no'} |" for r in rows)
        (args.out / "matrix.md").write_text("\n".join(lines) + "\n")
        print(f"MATRIX {args.out / 'matrix.md'} matched={sum(r['match'] for r in rows)}/{len(rows)}")
        return 0 if rows and all(r["match"] for r in rows) else 1
    finally:
        harness.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
