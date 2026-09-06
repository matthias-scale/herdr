import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
SEED_SCRIPT = REPO_ROOT / "scripts" / "dev" / "t3-seed-session.sh"


FAKE_HERDR = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

state_path = Path(os.environ["FAKE_HERDR_STATE"])
log_path = Path(os.environ["FAKE_HERDR_LOG"])
if state_path.exists():
    state = json.loads(state_path.read_text())
else:
    state = {"workspaces": [], "panes": [], "next_pane": 1}

args = sys.argv[1:]
if args[:1] == ["--session"]:
    args = args[2:]
with log_path.open("a") as log:
    log.write(json.dumps(args) + "\n")

def save():
    state_path.write_text(json.dumps(state))

def emit(result):
    print(json.dumps({"result": result}))

def pane(pane_id):
    return next(item for item in state["panes"] if item["pane_id"] == pane_id)

command = tuple(args[:2])
if command == ("workspace", "list"):
    emit({"workspaces": state["workspaces"]})
elif command == ("workspace", "close"):
    workspace_id = args[2]
    state["workspaces"] = [item for item in state["workspaces"] if item["workspace_id"] != workspace_id]
    state["panes"] = [item for item in state["panes"] if item["workspace_id"] != workspace_id]
    save()
    emit({"type": "ok"})
elif command == ("workspace", "create"):
    workspace_id = "ws-1"
    pane_id = f"pane-{state['next_pane']}"
    state["next_pane"] += 1
    state["workspaces"].append({"workspace_id": workspace_id, "label": "t3-sample"})
    state["panes"].append({"pane_id": pane_id, "workspace_id": workspace_id,
                           "label": None, "agent": None, "agent_status": "unknown",
                           "settled_at": None, "work_context": {}, "metadata_titles": []})
    save()
    emit({"workspace": {"workspace_id": workspace_id}, "root_pane": {"pane_id": pane_id}})
elif command == ("tab", "create"):
    workspace_id = args[args.index("--workspace") + 1]
    pane_id = f"pane-{state['next_pane']}"
    state["next_pane"] += 1
    state["panes"].append({"pane_id": pane_id, "workspace_id": workspace_id,
                           "label": None, "agent": None, "agent_status": "unknown",
                           "settled_at": None, "work_context": {}, "metadata_titles": []})
    save()
    emit({"root_pane": {"pane_id": pane_id}})
elif command == ("pane", "rename"):
    pane(args[2])["label"] = args[3]
    save()
    emit({"type": "ok"})
elif args[:3] == ["pane", "work-context", "set"]:
    item = pane(args[3])
    if "--title" in args:
        item["work_context"]["work_title"] = args[args.index("--title") + 1]
    save()
    emit({"type": "ok"})
elif command == ("pane", "report-metadata"):
    item = pane(args[2])
    item["metadata_titles"].append(args[args.index("--title") + 1])
    save()
    emit({"type": "ok"})
elif command == ("pane", "settle"):
    pane(args[2])["settled_at"] = 1770000000
    save()
    emit({"type": "ok"})
elif command == ("pane", "get"):
    emit({"pane": pane(args[2])})
elif command == ("pane", "run"):
    item = pane(args[2])
    fixture = Path(args[3]).name
    if fixture == "claude":
        item["agent"] = "claude"
    elif fixture == "codex":
        item["agent"] = "codex"
    else:
        raise SystemExit(f"unexpected fixture command: {fixture}")
    item["agent_status"] = "idle"
    item["settled_at"] = None
    save()
    emit({"type": "ok"})
elif command == ("pane", "list"):
    workspace_id = args[args.index("--workspace") + 1]
    emit({"panes": [item for item in state["panes"] if item["workspace_id"] == workspace_id]})
else:
    raise SystemExit(f"unsupported fake herdr command: {args}")
'''


class T3SeedSessionTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Repos" / "herdr-worktrees" / "t3-integration").mkdir(parents=True)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.fake_herdr = self.bin_dir / "herdr-fixture"
        self.fake_herdr.write_text(FAKE_HERDR)
        self.fake_herdr.chmod(0o755)
        self.sleep_log = self.root / "sleep.log"
        fake_sleep = self.bin_dir / "sleep"
        fake_sleep.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_SLEEP_LOG\"\n")
        fake_sleep.chmod(0o755)
        self.claude_fixture = self.bin_dir / "claude"
        self.codex_fixture = self.bin_dir / "codex"
        for fixture, prompt in [(self.claude_fixture, "❯"), (self.codex_fixture, "›")]:
            fixture.write_text(textwrap.dedent(f"""\
                #!/bin/sh
                printf '{prompt}\\n'
                sleep 60
            """))
            fixture.chmod(0o755)
        self.state_path = self.root / "state.json"
        self.log_path = self.root / "herdr.log"
        self.env = {
            **os.environ,
            "HOME": str(self.root),
            "PATH": f"{self.bin_dir}:{os.environ['PATH']}",
            "HERDR_BIN": str(self.fake_herdr),
            "HERDR_SEED_CLAUDE": str(self.claude_fixture),
            "HERDR_SEED_CODEX": str(self.codex_fixture),
            "FAKE_HERDR_STATE": str(self.state_path),
            "FAKE_HERDR_LOG": str(self.log_path),
            "FAKE_SLEEP_LOG": str(self.sleep_log),
        }

    def tearDown(self):
        self.temp.cleanup()

    def run_seed(self, *options):
        return subprocess.run(
            [str(SEED_SCRIPT), "t3-fixture", *options],
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )

    def state(self):
        return json.loads(self.state_path.read_text())

    def commands(self):
        return [json.loads(line) for line in self.log_path.read_text().splitlines()]

    def test_agents_are_sequential_titled_reported_and_settled(self):
        result = self.run_seed("--agents")
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertEqual(len(lines), 5)
        self.assertRegex(lines[0], r"sample-linear: pane-\d+ · claude · idle")
        self.assertRegex(lines[1], r"sample-pr: pane-\d+ · codex · idle")
        self.assertRegex(lines[2], r"sample-missive: pane-\d+ · claude · idle")
        self.assertRegex(lines[3], r"sample-shell: pane-\d+ · - · unknown")
        self.assertRegex(lines[4], r"sample-settled: pane-\d+ · claude · idle")

        panes = {item["label"]: item for item in self.state()["panes"]}
        expected_titles = {
            "sample-linear": "image-edit-simple v3 reference addendum",
            "sample-pr": "t3: home screen and dock",
            "sample-missive": "Sample support conversation",
        }
        for label, title in expected_titles.items():
            self.assertEqual(panes[label]["work_context"]["work_title"], title)
            self.assertEqual(panes[label]["metadata_titles"], [title])
        self.assertIsNotNone(panes["sample-settled"]["settled_at"])

        runs = [command for command in self.commands() if command[:2] == ["pane", "run"]]
        self.assertEqual(
            [(command[2], command[3]) for command in runs],
            [
                (panes["sample-linear"]["pane_id"], str(self.claude_fixture)),
                (panes["sample-pr"]["pane_id"], str(self.codex_fixture)),
                (panes["sample-missive"]["pane_id"], str(self.claude_fixture)),
                (panes["sample-settled"]["pane_id"], str(self.claude_fixture)),
            ],
        )
        # Three 5 s gaps between agent starts, then the quiet wait before
        # settling polls twice more with the same 5 s pause.
        self.assertEqual(
            self.sleep_log.read_text().splitlines(), ["5", "5", "5", "5", "5"]
        )

        repeated = self.run_seed("--agents")
        self.assertEqual(repeated.returncode, 0, repeated.stderr)
        repeated_runs = [
            command for command in self.commands() if command[:2] == ["pane", "run"]
        ]
        self.assertEqual(repeated_runs, runs)

    def test_reset_closes_agent_panes_and_reseeds_cleanly(self):
        first = self.run_seed("--agents")
        self.assertEqual(first.returncode, 0, first.stderr)
        reset = self.run_seed("--reset")
        self.assertEqual(reset.returncode, 0, reset.stderr)
        state = self.state()
        self.assertEqual(len(state["workspaces"]), 1)
        self.assertEqual(len(state["panes"]), 5)
        self.assertTrue(all(item["agent"] is None for item in state["panes"]))
        self.assertTrue(any(command[:2] == ["workspace", "close"] for command in self.commands()))


if __name__ == "__main__":
    unittest.main()
