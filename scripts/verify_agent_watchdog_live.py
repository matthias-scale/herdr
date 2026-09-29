#!/usr/bin/env python3
"""Run an isolated, replayable worker-watchdog smoke matrix on this host."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def run(command: list[str], env: dict[str, str], timeout: int = 30) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, env=env, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout, check=False)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--host-label", required=True)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--peer")
    parser.add_argument("--peer-binary", type=Path)
    parser.add_argument("--gemini-bin")
    parser.add_argument("--confirm-secs", type=int, default=5)
    args = parser.parse_args()

    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary does not exist: {binary}")
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="wdl.", dir="/tmp"))
    cfg = root / "cfg"
    state = root / "state"
    app_dir = "herdr-dev" if "/debug/" in str(binary) else "herdr"
    config_dir = cfg / app_dir
    config_dir.mkdir(parents=True)
    (config_dir / "config.toml").write_text("allow_nested = true\n")
    env = os.environ.copy()
    for name in list(env):
        if name.startswith("HERDR_"):
            env.pop(name)
    env.update({"XDG_CONFIG_HOME": str(cfg), "XDG_STATE_HOME": str(state)})
    runs = root / "runs"
    claude = root / "claude-projects"
    runs.mkdir()
    claude.mkdir()
    now = int(time.time())
    fixtures = [
        ("b-working", "active", now, "Compiling target\n"),
        ("b-silent-stall", "active", now - 900, "Running cargo test\n"),
        ("b-finished", "complete", now - 900, "Finished\n"),
    ]
    for worker_id, status, progress, trace in fixtures:
        run_dir = runs / worker_id
        turn = run_dir / "turns" / f"turn-{uuid.uuid4().hex[:8]}"
        turn.mkdir(parents=True)
        parent = {"host": args.host_label, "session": f"wdl-missing-{uuid.uuid4()}"}
        (run_dir / "state.json").write_text(json.dumps({
            "run_id": worker_id, "host": args.host_label, "agent": "codex", "pid": None,
            "started_at": progress, "progress_at": progress, "state": status,
            "exit_code": 0 if status == "complete" else None, "parent": parent,
        }) + "\n")
        (turn / "start.json").write_text(json.dumps({"started_at": progress}) + "\n")
        (turn / "trace.log").write_text(trace)
        os.utime(turn / "trace.log", (progress, progress))
        if status == "complete":
            (turn / "receipt.json").write_text('{"status":"ended-unverified","exit_code":0}\n')

    server_log = (args.out / "server.log").open("w")
    server: subprocess.Popen[str] | None = None
    timeline: list[dict[str, object]] = []
    try:
        server = subprocess.Popen([str(binary), "server"], env=env, stdout=server_log,
                                  stderr=subprocess.STDOUT, text=True)
        command = [str(binary), "watchdog", "workers", "--once", "--dry-run", "--json",
                   "--confirm-secs", str(args.confirm_secs), "--stall-minutes", "1",
                   "--runs-dir", str(runs), "--claude-projects-dir", str(claude),
                   "--state-file", str(root / "memory.json"), "--log-file", str(root / "incidents.jsonl"),
                   "--local-host", args.host_label]
        deadline = time.monotonic() + 30
        result = None
        while time.monotonic() < deadline:
            if server.poll() is not None:
                raise RuntimeError(f"isolated Herdr server exited {server.returncode}; see {args.out / 'server.log'}")
            result = run(command, env)
            if result.returncode == 0:
                break
            time.sleep(0.25)
        if result is None or result.returncode != 0:
            detail = result.stderr[-2000:] if result else "server did not become ready"
            raise RuntimeError(f"worker scan failed: {detail}")
        payload = json.loads(result.stdout)
        timeline.append({"command": command, "stdout": result.stdout, "stderr": result.stderr,
                         "returncode": result.returncode})
        actual = {item["worker_id"]: item for item in payload.get("decisions", [])}
        expected = {"b-working": "working", "b-silent-stall": "suspected_stall", "b-finished": "finished"}
        matrix = []
        for worker_id, wanted in expected.items():
            decision = actual.get(worker_id, {})
            found = decision.get("class", "missing")
            matrix.append({"id": worker_id, "watchdog": "B", "expected": wanted,
                           "actual": found, "match": found == wanted,
                           "evidence": decision.get("evidence", "no decision emitted")})
        manifest = {
            "host": args.host_label,
            "binary": str(binary),
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "source_sha": subprocess.check_output(["git", "-C", str(Path(__file__).resolve().parents[1]), "rev-parse", "HEAD"], text=True).strip(),
            "peer_requested": args.peer,
            "peer_binary": str(args.peer_binary.resolve()) if args.peer_binary else None,
            "cases": matrix,
        }
        (args.out / "matrix.json").write_text(json.dumps(manifest, indent=2) + "\n")
        (args.out / "matrix.md").write_text("| Case | Watchdog | Expected | Actual | Match | Evidence |\n|---|---|---|---|---|---|\n" + "".join(
            f"| {row['id']} | {row['watchdog']} | {row['expected']} | {row['actual']} | {'yes' if row['match'] else 'no'} | {row['evidence']} |\n" for row in matrix))
        (args.out / "timeline.jsonl").write_text("".join(json.dumps(item) + "\n" for item in timeline))
        return 0 if all(row["match"] for row in matrix) else 1
    except (OSError, RuntimeError, json.JSONDecodeError, subprocess.TimeoutExpired) as error:
        (args.out / "denial.txt").write_text(f"{type(error).__name__}: {error}\n")
        return 2
    finally:
        if server is not None and server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait()
        server_log.close()
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
