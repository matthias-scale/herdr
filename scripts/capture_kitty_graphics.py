#!/usr/bin/env python3
"""Isolated Kitty graphics pixel check for two Herdr clients.

This helper owns a private headless Sway compositor, standalone WezTerm
processes, named Herdr sessions, and every artifact it creates. It never
connects to an inherited Herdr or WezTerm endpoint.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid


SIZE = 64
DISPLAY_SIZE = (480, 320)
PIXEL_MIN_COMPONENT = 16
COLOR_TOLERANCE = 75
FIXTURE_COLORS = (
    (240, 32, 24),
    (24, 232, 48),
    (32, 48, 240),
    (248, 224, 24),
)
REPLACEMENT_COLORS = (
    (24, 224, 232),
    (232, 32, 208),
    (248, 112, 24),
    (248, 248, 248),
)
REQUIRED_TOOLS = ("sway", "swaymsg", "grim", "magick", "wezterm")
INHERITED_SOCKET_NAMES = ("HERDR_SOCKET_PATH", "HERDR_CLIENT_SOCKET_PATH")


class CaptureError(RuntimeError):
    pass


def fixture_rgba(colors: tuple[tuple[int, int, int], ...]) -> bytes:
    if len(colors) != 4:
        raise ValueError("fixture needs four colors")
    pixels = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            color = colors[(2 if y >= SIZE // 2 else 0) + (1 if x >= SIZE // 2 else 0)]
            pixels.extend((*color, 255))
    return bytes(pixels)


def kitty_display_sequence(colors: tuple[tuple[int, int, int], ...]) -> bytes:
    payload = base64.b64encode(fixture_rgba(colors)).decode("ascii")
    control = "a=T,f=32,t=d,i=4242,p=1,s=64,v=64,c=8,r=4,q=2"
    return f"\x1b_G{control};{payload}\x1b\\".encode("ascii")


def kitty_delete_sequence() -> bytes:
    return b"\x1b_Ga=d,d=I,i=4242,q=2;\x1b\\"


def nearest_color(pixel: tuple[int, int, int], colors: tuple[tuple[int, int, int], ...]) -> int | None:
    distances = [max(abs(channel - expected) for channel, expected in zip(pixel, color)) for color in colors]
    nearest = min(range(len(distances)), key=distances.__getitem__)
    return nearest if distances[nearest] <= COLOR_TOLERANCE else None


def fixture_pattern_present(
    width: int,
    height: int,
    rgb: bytes,
    colors: tuple[tuple[int, int, int], ...] = FIXTURE_COLORS,
    roi: tuple[int, int, int, int] | None = None,
) -> bool:
    """Find the four asymmetric opaque color patches in their 2x2 order."""
    if width <= 0 or height <= 0 or len(rgb) != width * height * 3 or len(colors) != 4:
        return False
    if roi is None:
        left, top, roi_width, roi_height = 0, 0, width, height
    else:
        left, top, roi_width, roi_height = roi
        left = max(0, left)
        top = max(0, top)
        roi_width = min(width - left, roi_width)
        roi_height = min(height - top, roi_height)
    if roi_width <= 0 or roi_height <= 0:
        return False

    labels = bytearray([255]) * (roi_width * roi_height)
    for y in range(roi_height):
        source_row = (top + y) * width + left
        target_row = y * roi_width
        for x in range(roi_width):
            offset = (source_row + x) * 3
            color_id = nearest_color((rgb[offset], rgb[offset + 1], rgb[offset + 2]), colors)
            if color_id is not None:
                labels[target_row + x] = color_id

    seen = bytearray(len(labels))
    largest: dict[int, tuple[int, int, int, int, int]] = {}
    for start, label in enumerate(labels):
        if label == 255 or seen[start]:
            continue
        seen[start] = 1
        stack = [start]
        area = 0
        min_x = max_x = start % roi_width
        min_y = max_y = start // roi_width
        while stack:
            index = stack.pop()
            x = index % roi_width
            y = index // roi_width
            area += 1
            min_x = min(min_x, x)
            max_x = max(max_x, x)
            min_y = min(min_y, y)
            max_y = max(max_y, y)
            neighbors = []
            if x:
                neighbors.append(index - 1)
            if x + 1 < roi_width:
                neighbors.append(index + 1)
            if y:
                neighbors.append(index - roi_width)
            if y + 1 < roi_height:
                neighbors.append(index + roi_width)
            for neighbor in neighbors:
                if not seen[neighbor] and labels[neighbor] == label:
                    seen[neighbor] = 1
                    stack.append(neighbor)
        previous = largest.get(label)
        if area >= PIXEL_MIN_COMPONENT and (previous is None or area > previous[0]):
            largest[label] = (area, min_x, min_y, max_x, max_y)

    if len(largest) != 4:
        return False
    boxes = [largest[index] for index in range(4)]
    widths = [box[3] - box[1] + 1 for box in boxes]
    heights = [box[4] - box[2] + 1 for box in boxes]
    if min(widths) * 2 < max(widths) or min(heights) * 2 < max(heights):
        return False
    centers = [((box[1] + box[3]) / 2, (box[2] + box[4]) / 2) for box in boxes]
    if not (centers[0][0] < centers[1][0] and centers[2][0] < centers[3][0]):
        return False
    if not (centers[0][1] < centers[2][1] and centers[1][1] < centers[3][1]):
        return False
    horizontal_gap = centers[1][0] - centers[0][0]
    vertical_gap = centers[2][1] - centers[0][1]
    average_width = sum(widths) / 4
    average_height = sum(heights) / 4
    return (
        average_width * 0.8 <= horizontal_gap <= average_width * 1.8
        and average_height * 0.8 <= vertical_gap <= average_height * 1.8
    )


def private_socket_conflicts(env: dict[str, str], original_home: Path) -> list[str]:
    conflicts = []
    defaults = {
        (original_home / ".config" / "herdr" / "herdr.sock").resolve(),
        (original_home / ".config" / "herdr" / "herdr-client.sock").resolve(),
        (original_home / ".config" / "herdr-dev" / "herdr.sock").resolve(),
        (original_home / ".config" / "herdr-dev" / "herdr-client.sock").resolve(),
    }
    for name in INHERITED_SOCKET_NAMES:
        value = env.get(name)
        if not value:
            continue
        path = Path(value).expanduser().resolve()
        if path in defaults:
            conflicts.append(f"{name} points at a default socket")
    return conflicts


def private_environment(home: Path, runtime: Path, config_path: Path, display: str) -> dict[str, str]:
    env = {name: os.environ[name] for name in ("PATH", "LANG", "LC_ALL", "LC_CTYPE") if name in os.environ}
    env["HOME"] = str(home)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env["XDG_DATA_HOME"] = str(home / ".local" / "share")
    env["XDG_STATE_HOME"] = str(home / ".local" / "state")
    env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["HERDR_CONFIG_PATH"] = str(config_path)
    env["WAYLAND_DISPLAY"] = display
    return env


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def tool_version(executable: str | None) -> str | None:
    if not executable:
        return None
    for flag in ("--version", "-version"):
        try:
            result = subprocess.run(
                [executable, flag], capture_output=True, text=True, timeout=4, check=False
            )
        except (OSError, subprocess.TimeoutExpired):
            continue
        text = (result.stdout + result.stderr).strip()
        if text:
            return text.splitlines()[0][:300]
    return "version unavailable"


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def walk_nodes(node: dict) -> list[dict]:
    output = [node]
    for key in ("nodes", "floating_nodes"):
        for child in node.get(key, []):
            output.extend(walk_nodes(child))
    return output


class OwnedProcess:
    def __init__(self, name: str, command: list[str], env: dict[str, str], log_path: Path):
        self.name = name
        self.command = command
        self.log_path = log_path
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.log = log_path.open("wb")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            env=env,
            start_new_session=True,
        )
        self.pid = self.process.pid
        self.pgid = os.getpgid(self.pid)
        if self.pgid != self.pid:
            raise CaptureError(f"{name} did not start in an isolated process group")

    def alive(self) -> bool:
        return self.process.poll() is None

    def stop(self, timeout: float = 5.0) -> None:
        try:
            os.killpg(self.pgid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                break
            time.sleep(0.05)
        try:
            os.killpg(self.pgid, 0)
        except ProcessLookupError:
            pass
        else:
            os.killpg(self.pgid, signal.SIGKILL)
        try:
            self.process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            pass
        self.log.close()


class Sway:
    def __init__(self, process: OwnedProcess, executable: str, runtime: Path, env: dict[str, str]):
        self.process = process
        self.executable = executable
        self.runtime = runtime
        self.env = env
        self.socket_path: Path | None = None

    def call(self, *args: str, timeout: float = 5) -> subprocess.CompletedProcess[str]:
        if self.socket_path is None:
            self.socket_path = self.wait_for_socket()
        command = [shutil.which("swaymsg") or "swaymsg", "-s", str(self.socket_path), *args]
        result = subprocess.run(command, env=self.env, capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise CaptureError(f"swaymsg {args!r} failed: {result.stderr.strip()}")
        return result

    def wait_for_socket(self, timeout: float = 12) -> Path:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.process.alive():
                raise CaptureError("private Sway exited during startup")
            sockets = sorted(self.runtime.glob("sway-ipc.*.sock"))
            if len(sockets) == 1:
                self.socket_path = sockets[0]
                self.call("-t", "get_version", timeout=2)
                return sockets[0]
            time.sleep(0.1)
        raise CaptureError("private Sway did not publish exactly one IPC socket")

    def tree(self) -> dict:
        return json.loads(self.call("-t", "get_tree", "-r").stdout)

    def window(self, app_id: str, timeout: float = 12) -> dict:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.process.alive():
                raise CaptureError("private Sway exited while waiting for a window")
            matches = [
                node
                for node in walk_nodes(self.tree())
                if node.get("app_id") == app_id and node.get("type") == "con"
            ]
            if len(matches) == 1:
                rect = matches[0].get("rect", {})
                if rect.get("width", 0) > 0 and rect.get("height", 0) > 0:
                    return matches[0]
            if len(matches) > 1:
                raise CaptureError(f"multiple private windows claim app_id {app_id}")
            time.sleep(0.1)
        raise CaptureError(f"private window {app_id} did not appear before its deadline")


class CaptureRun:
    def __init__(self, root: Path, sway: Sway, magick: str, grim: str):
        self.root = root
        self.sway = sway
        self.magick = magick
        self.grim = grim
        self.capture_count = 0
        self.observations: list[dict] = []

    def capture(self, name: str, app_id: str, palette=FIXTURE_COLORS, roi=None) -> tuple[bool, dict]:
        self.capture_count += 1
        sequence = self.capture_count
        full_path = self.root / "captures" / f"{name}-{sequence:03d}-full.png"
        crop_path = self.root / "captures" / f"{name}-{sequence:03d}-window.png"
        full_path.parent.mkdir(parents=True, exist_ok=True)
        window = self.sway.window(app_id)
        rect = window["rect"]
        subprocess.run(
            [self.grim, str(full_path)],
            env=self.sway.env,
            check=True,
            timeout=8,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        geometry = f"{rect['width']}x{rect['height']}+{rect['x']}+{rect['y']}"
        subprocess.run(
            [self.magick, str(full_path), "-crop", geometry, "+repage", str(crop_path)],
            check=True,
            timeout=8,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        rgb_result = subprocess.run(
            [
                self.magick,
                str(crop_path),
                "-alpha",
                "off",
                "-colorspace",
                "sRGB",
                "-resize",
                f"{DISPLAY_SIZE[0]}x{DISPLAY_SIZE[1]}!",
                "-depth",
                "8",
                "rgb:-",
            ],
            check=True,
            timeout=8,
            capture_output=True,
        )
        scaled_roi = None
        if roi is not None:
            x_scale = DISPLAY_SIZE[0] / rect["width"]
            y_scale = DISPLAY_SIZE[1] / rect["height"]
            scaled_roi = (
                round(roi[0] * x_scale),
                round(roi[1] * y_scale),
                max(1, round(roi[2] * x_scale)),
                max(1, round(roi[3] * y_scale)),
            )
        matched = fixture_pattern_present(
            DISPLAY_SIZE[0], DISPLAY_SIZE[1], rgb_result.stdout, palette, scaled_roi
        )
        observation = {
            "name": name,
            "app_id": app_id,
            "matched": matched,
            "window_rect": rect,
            "roi": roi,
            "scaled_roi": scaled_roi,
            "full_capture": str(full_path.relative_to(self.root)),
            "window_capture": str(crop_path.relative_to(self.root)),
        }
        self.observations.append(observation)
        write_json(self.root / "observations.json", self.observations)
        return matched, observation

    def expect_stable(
        self,
        name: str,
        app_id: str,
        expected: bool,
        palette=FIXTURE_COLORS,
        roi=None,
        timeout: float = 18,
    ) -> dict:
        deadline = time.monotonic() + timeout
        consecutive = 0
        last = None
        while time.monotonic() < deadline:
            matched, last = self.capture(name, app_id, palette, roi)
            consecutive = consecutive + 1 if matched == expected else 0
            if consecutive >= 2:
                assert last is not None
                return {"status": "passed", "expected_match": expected, "last": last}
            time.sleep(0.35)
        raise CaptureError(
            f"{name} did not produce {2} consecutive captures with matched={expected}; "
            f"last observation={last}"
        )


def pane_region(window: dict, layout: dict, pane_id: str) -> tuple[int, int, int, int]:
    body = layout.get("result", {}).get("layout", layout.get("result", {}))
    area = body.get("area")
    panes = body.get("panes")
    pane = next((item for item in (panes or []) if item.get("pane_id") == pane_id), None)
    if not area or not pane or not area.get("width") or not area.get("height"):
        raise CaptureError("pane layout response lacks usable pane and area geometry")
    rect = pane["rect"]
    window_rect = window["rect"]
    cell_width = window_rect["width"] / area["width"]
    cell_height = window_rect["height"] / area["height"]
    left = max(0, round((rect["x"] + 1) * cell_width - cell_width))
    top = max(0, round((rect["y"] + 12) * cell_height - cell_height))
    width = min(window_rect["width"] - left, round(10 * cell_width))
    height = min(window_rect["height"] - top, round(6 * cell_height))
    if width <= 0 or height <= 0:
        raise CaptureError("expected image region is outside the receiving window")
    return left, top, width, height


class HerdrScenario:
    def __init__(self, run: "Harness", binary: Path, label: str):
        self.run = run
        self.binary = binary
        self.label = label
        self.session = f"mat354-{run.run_id[:10]}-{label}"
        self.a_id = f"herdr-{run.run_id[:7]}-{label}-a"
        self.d_id = f"herdr-{run.run_id[:7]}-{label}-d"
        self.processes: list[OwnedProcess] = []
        self.server_stopped = False
        self.home = run.temp_root / label / "home"
        self.runtime = run.temp_root / "runtime"
        self.config = self.home / ".config" / "herdr-dev" / "config.toml"
        self.home.mkdir(parents=True, exist_ok=True)
        self.runtime.mkdir(parents=True, exist_ok=True)
        os.chmod(self.runtime, 0o700)
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.config.write_text("onboarding = false\n", encoding="utf-8")
        self.env = private_environment(self.home, self.runtime, self.config, run.display)
        self.env["WAYLAND_DISPLAY"] = run.display
        self.workdir = run.temp_root / label / "work"
        self.workdir.mkdir(parents=True, exist_ok=True)
        self.client_ids: list[int] = []

    def cli(self, *args: str, timeout: float = 12, log_name: str = "cli") -> dict:
        log_path = self.run.root / "logs" / f"{self.label}-{log_name}-{uuid.uuid4().hex[:6]}.log"
        command = [str(self.binary), "--session", self.session, *args]
        result = subprocess.run(
            command,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.write_text(result.stdout + result.stderr, encoding="utf-8")
        if result.returncode:
            raise CaptureError(f"Herdr command {args!r} failed; see {log_path}")
        try:
            response = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise CaptureError(f"Herdr command {args!r} returned invalid JSON; see {log_path}") from error
        if response.get("error"):
            raise CaptureError(f"Herdr command {args!r} returned {response['error']}")
        return response

    def start_client(self, app_id: str, label: str) -> OwnedProcess:
        command = [
            self.run.tools["wezterm"],
            "--config-file",
            str(self.run.wezterm_config),
            "start",
            "--always-new-process",
            "--class",
            app_id,
            "--cwd",
            str(self.workdir),
            "--",
            str(self.binary),
            "--session",
            self.session,
        ]
        process = OwnedProcess(
            f"{self.label}-{label}",
            command,
            self.env,
            self.run.root / "logs" / f"{self.label}-{label}-wezterm.log",
        )
        self.processes.append(self.run.own(process))
        self.run.sway.window(app_id)
        return process

    def workspace_id(self, response: dict, focused: bool) -> str:
        workspaces = response.get("result", {}).get("workspaces", [])
        match = next((row for row in workspaces if row.get("focused") is focused), None)
        if match is None:
            raise CaptureError(f"workspace list did not identify focused={focused}")
        return match["workspace_id"]

    def create_pair(self) -> tuple[str, str, str]:
        self.start_client(self.a_id, "client-a")
        deadline = time.monotonic() + 20
        response = None
        while time.monotonic() < deadline:
            try:
                response = self.cli("workspace", "list", log_name="workspace-list-a")
                break
            except (CaptureError, subprocess.TimeoutExpired):
                if not any(process.alive() for process in self.processes):
                    raise CaptureError("initial Herdr client exited before its server became ready")
                time.sleep(0.25)
        if response is None:
            raise CaptureError("Herdr API did not become ready before its deadline")
        self.wait_for_client_ids(1)
        time.sleep(0.5)
        a_workspace = self.workspace_id(response, True)
        created = self.cli(
            "workspace", "create", "--cwd", str(self.workdir), "--label", "graphics D", "--focus",
            log_name="workspace-create-d",
        )
        d_workspace = created.get("result", {}).get("workspace", {}).get("workspace_id")
        if not d_workspace:
            raise CaptureError("workspace create response lacks workspace id")
        self.start_client(self.d_id, "client-d")
        panes = self.cli("pane", "list", "--workspace", d_workspace, log_name="pane-list-d")
        pane_rows = panes.get("result", {}).get("panes", [])
        if not pane_rows:
            raise CaptureError("workspace D has no pane to host the Kitty fixture")
        pane_id = pane_rows[0]["pane_id"]
        self.client_ids = self.wait_for_client_ids(2)
        time.sleep(0.5)
        self.cli("workspace", "focus", a_workspace, log_name="global-focus-a")
        state = self.cli("workspace", "list", log_name="workspace-list-mismatch")
        if self.workspace_id(state, True) != a_workspace:
            raise CaptureError("could not establish global A/client D mismatch precondition")
        return a_workspace, d_workspace, pane_id

    def wait_for_client_ids(self, count: int, timeout: float = 10) -> list[int]:
        log_paths = [
            self.home / ".config" / app_dir / "sessions" / self.session / "herdr-server.log"
            for app_dir in ("herdr-dev", "herdr")
        ]
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            contents = "\n".join(
                path.read_text(encoding="utf-8", errors="replace")
                for path in log_paths
                if path.exists()
            )
            ids = sorted(
                {
                    int(match.group(1))
                    for match in re.finditer(r"client_id=(\d+).*client connected", contents)
                }
            )
            if len(ids) >= count:
                return ids[:count]
            if not any(process.alive() for process in self.processes):
                raise CaptureError("Herdr clients exited before connection IDs were logged")
            time.sleep(0.1)
        raise CaptureError("server log did not record both connected client IDs")

    def emit(self, pane_id: str, colors: tuple[tuple[int, int, int], ...] | None) -> None:
        if colors is None:
            wire = kitty_delete_sequence()
        else:
            wire = kitty_display_sequence(colors)
        payload_code = (
            "import base64,os;os.write(1,base64.b64decode(\""
            + base64.b64encode(wire).decode("ascii")
            + "\"))"
        )
        shell_command = f"python3 -c '{payload_code}'"
        self.cli("pane", "run", pane_id, shell_command, log_name="fixture-inject")

    def capture_region(self, pane_id: str, app_id: str) -> tuple[int, int, int, int]:
        layout = self.cli("pane", "layout", "--pane", pane_id, log_name="pane-layout")
        return pane_region(self.run.sway.window(app_id), layout, pane_id)

    def stop_server(self) -> None:
        if self.server_stopped:
            return
        try:
            self.cli("server", "stop", log_name="server-stop")
            self.server_stopped = True
        except (CaptureError, subprocess.TimeoutExpired):
            pass


class Harness:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.run_id = uuid.uuid4().hex
        self.output = Path(args.output).expanduser().resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.root = self.output / f"capture-kitty-graphics-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-{self.run_id[:8]}"
        self.root.mkdir(parents=True, exist_ok=False)
        self.root = self.root.resolve()
        self.temp_root: Path | None = None
        self.display = f"wayland-herdr-{self.run_id[:8]}"
        self.processes: list[OwnedProcess] = []
        self.tools = {name: shutil.which(name) for name in REQUIRED_TOOLS}
        self.wezterm_config = self.root / "private-wezterm.lua"
        self.sway: Sway | None = None
        self.capture_run: CaptureRun | None = None
        self.scenarios: list[HerdrScenario] = []
        self.manifest = {
            "run_id": self.run_id,
            "status": "RUNNING",
            "requested_binary_paths": {
                "baseline": args.baseline_bin,
                "candidate": args.candidate_bin,
            },
            "binary_paths": {},
            "binary_sha256": {},
            "tools": {},
            "client_ids": {},
            "cases": [],
            "cleanup_errors": [],
            "pixel_evidence": "captured compositor pixels only",
            "fixture_rgba_sha256": hashlib.sha256(fixture_rgba(FIXTURE_COLORS)).hexdigest(),
            "replacement_rgba_sha256": hashlib.sha256(fixture_rgba(REPLACEMENT_COLORS)).hexdigest(),
        }
        self.ownership: list[dict] = []

    def own(self, process: OwnedProcess) -> OwnedProcess:
        self.processes.append(process)
        record = {
            "name": process.name,
            "pid": process.pid,
            "pgid": process.pgid,
            "command": process.command,
            "log": str(process.log_path.relative_to(self.root)),
            "state": "running",
        }
        self.ownership.append(record)
        write_json(self.root / "ownership.json", self.ownership)
        return process

    def stop_owned(self, process: OwnedProcess) -> None:
        process.stop()
        for record in self.ownership:
            if record["pid"] == process.pid:
                record["state"] = "stopped"
                break
        write_json(self.root / "ownership.json", self.ownership)

    def record_preflight(self) -> None:
        original_home = Path(os.environ.get("HOME", str(Path.home()))).expanduser().resolve()
        conflicts = private_socket_conflicts(dict(os.environ), original_home)
        if conflicts:
            raise CaptureError("; ".join(conflicts))
        for name, executable in self.tools.items():
            self.manifest["tools"][name] = {
                "path": executable,
                "version": tool_version(executable),
            }
        missing = [name for name, executable in self.tools.items() if not executable]
        if missing:
            self.manifest["status"] = "BLOCKED"
            self.manifest["blocked"] = {
                "reason": "required isolated pixel-capture tools are unavailable",
                "missing_tools": missing,
            }
            raise CaptureError("BLOCKED: missing required tools: " + ", ".join(missing))
        binaries = {"baseline": Path(self.args.baseline_bin), "candidate": Path(self.args.candidate_bin)}
        for label, raw_path in binaries.items():
            path = raw_path.expanduser().resolve(strict=True)
            if not path.is_file() or not os.access(path, os.X_OK):
                raise CaptureError(f"{label} binary is not executable: {path}")
            self.manifest["binary_paths"][label] = str(path)
            self.manifest["binary_sha256"][label] = sha256_file(path)

    def start_compositor(self) -> None:
        self.temp_root = Path(tempfile.mkdtemp(prefix="private-", dir=self.root))
        home = self.temp_root / "home"
        runtime = self.temp_root / "runtime"
        config_dir = home / ".config" / "herdr-dev"
        for directory in (home, runtime, config_dir, home / ".local" / "share", home / ".cache"):
            directory.mkdir(parents=True, exist_ok=True)
        os.chmod(runtime, 0o700)
        (config_dir / "config.toml").write_text("onboarding = false\n", encoding="utf-8")
        config_path = config_dir / "config.toml"
        env = private_environment(home, runtime, config_path, self.display)
        env.update(
            {
                "WLR_BACKENDS": "headless",
                "WLR_HEADLESS_OUTPUTS": "1",
                "WLR_LIBINPUT_NO_DEVICES": "1",
                "WLR_RENDERER": "pixman",
            }
        )
        sway_config = self.temp_root / "sway.conf"
        sway_config.write_text(
            "font pango:monospace 10\n"
            "default_border pixel 0\n"
            "default_floating_border pixel 0\n"
            "default_layout splith\n"
            "focus_follows_mouse no\n"
            "workspace_auto_back_and_forth yes\n"
            "output * mode 1600x1000\n",
            encoding="utf-8",
        )
        self.wezterm_config.write_text(
            "return { enable_wayland = true, window_decorations = 'NONE', "
            "window_background_opacity = 1.0, font_size = 14.0, "
            "warn_about_missing_glyphs = false }\n",
            encoding="utf-8",
        )
        compositor = OwnedProcess(
            "private-sway",
            [self.tools["sway"], "--config", str(sway_config), "--verbose"],
            env,
            self.root / "logs" / "sway.log",
        )
        self.own(compositor)
        self.sway = Sway(compositor, self.tools["sway"], runtime, env)
        self.sway.wait_for_socket()
        self.capture_run = CaptureRun(self.root, self.sway, self.tools["magick"], self.tools["grim"])

    def start_window(self, app_id: str, label: str, program: list[str]) -> OwnedProcess:
        if self.temp_root is None:
            raise CaptureError("private runtime was not initialized")
        home = self.temp_root / "home"
        runtime = self.temp_root / "runtime"
        config = home / ".config" / "herdr-dev" / "config.toml"
        env = private_environment(home, runtime, config, self.display)
        command = [
            self.tools["wezterm"],
            "--config-file",
            str(self.wezterm_config),
            "start",
            "--always-new-process",
            "--class",
            app_id,
            "--cwd",
            str(self.temp_root),
            "--",
            *program,
        ]
        process = OwnedProcess(
            label,
            command,
            env,
            self.root / "logs" / f"{label}.log",
        )
        self.own(process)
        self.sway.window(app_id)
        return process

    def direct_controls(self) -> None:
        assert self.sway and self.capture_run and self.temp_root
        blank_id = f"herdr-{self.run_id[:7]}-blank"
        blank = self.start_window(
            blank_id,
            "direct-blank",
            [sys.executable, "-c", "import time;time.sleep(180)"],
        )
        self.capture_run.expect_stable("blank-negative", blank_id, False)
        self.manifest["cases"].append(
            {"case": "blank-negative", "status": "passed", "expected_match": False}
        )
        self.stop_owned(blank)
        self.processes.remove(blank)

        positive_id = f"herdr-{self.run_id[:7]}-direct"
        positive = self.start_window(
            positive_id,
            "direct-positive",
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "--emit-fixture",
                "--hold-seconds",
                "180",
            ],
        )
        direct = self.capture_run.expect_stable("direct-terminal-positive", positive_id, True)
        self.manifest["cases"].append({"case": "direct-terminal-positive", **direct})
        self.stop_owned(positive)
        self.processes.remove(positive)

    def run_scenario(self, binary_label: str, binary: Path) -> None:
        assert self.sway and self.capture_run and self.temp_root
        scenario = HerdrScenario(self, binary, binary_label)
        self.scenarios.append(scenario)
        self.manifest.setdefault("owned_sessions", []).append(
            {"name": scenario.session, "binary": str(binary), "state": "running"}
        )
        a_workspace, d_workspace, pane_id = scenario.create_pair()
        self.manifest["client_ids"][binary_label] = {
            "server_client_ids": scenario.client_ids,
            "client_a_app_id": scenario.a_id,
            "client_d_app_id": scenario.d_id,
            "pane_id": pane_id,
            "workspace_a": a_workspace,
            "workspace_d": d_workspace,
        }
        scenario.emit(pane_id, FIXTURE_COLORS)
        region = scenario.capture_region(pane_id, scenario.d_id)
        if binary_label == "baseline":
            result = self.capture_run.expect_stable(
                "baseline-mismatched-client-d", scenario.d_id, False
            )
            self.manifest["cases"].append(
                {
                    "case": "baseline-mismatch-red",
                    "oracle_required_match": True,
                    "baseline_failure_reproduced": not result["last"]["matched"],
                    **result,
                }
            )
        else:
            result_d = self.capture_run.expect_stable(
                "candidate-client-d-image", scenario.d_id, True, FIXTURE_COLORS, region
            )
            result_a = self.capture_run.expect_stable(
                "candidate-client-a-no-leak", scenario.a_id, False
            )
            self.manifest["cases"].extend(
                [
                    {"case": "candidate-client-d-image", **result_d},
                    {"case": "candidate-client-a-no-leak", **result_a},
                ]
            )
            scenario.cli("workspace", "focus", d_workspace, log_name="global-focus-d-aligned")
            aligned = self.capture_run.expect_stable(
                "candidate-aligned-workspace", scenario.d_id, True, FIXTURE_COLORS, region
            )
            self.manifest["cases"].append({"case": "candidate-aligned-workspace", **aligned})
            scenario.cli("workspace", "focus", a_workspace, log_name="global-focus-a-recheck")

            scenario.emit(pane_id, REPLACEMENT_COLORS)
            replacement = self.capture_run.expect_stable(
                "candidate-image-replacement", scenario.d_id, True, REPLACEMENT_COLORS, region
            )
            old_gone = self.capture_run.expect_stable(
                "candidate-old-image-gone", scenario.d_id, False, FIXTURE_COLORS, region
            )
            self.manifest["cases"].extend(
                [
                    {"case": "candidate-image-replacement", **replacement},
                    {"case": "candidate-old-image-gone", **old_gone},
                ]
            )
            scenario.emit(pane_id, None)
            removed = self.capture_run.expect_stable(
                "candidate-image-removal", scenario.d_id, False, REPLACEMENT_COLORS, region
            )
            self.manifest["cases"].append({"case": "candidate-image-removal", **removed})
        scenario.stop_server()
        self.mark_session_stopped(scenario.session)
        for process in scenario.processes:
            self.stop_owned(process)
            if process in self.processes:
                self.processes.remove(process)

    def mark_session_stopped(self, session: str) -> None:
        for record in self.manifest.get("owned_sessions", []):
            if record["name"] == session:
                record["state"] = "stopped"


    def execute(self) -> int:
        try:
            self.record_preflight()
            self.start_compositor()
            self.direct_controls()
            baseline = Path(self.manifest["binary_paths"]["baseline"])
            candidate = Path(self.manifest["binary_paths"]["candidate"])
            self.run_scenario("baseline", baseline)
            self.run_scenario("candidate", candidate)
            self.manifest["status"] = "PASS"
            return 0
        except CaptureError as error:
            if str(error).startswith("BLOCKED:"):
                self.manifest["status"] = "BLOCKED"
                self.manifest.setdefault("blocked", {"reason": str(error)[9:].strip()})
                print(json.dumps({"status": "BLOCKED", "reason": str(error)[9:].strip()}))
                return 2
            self.manifest["status"] = "FAIL"
            self.manifest["error"] = str(error)
            print(json.dumps({"status": "FAIL", "reason": str(error)}))
            return 1
        except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
            self.manifest["status"] = "FAIL"
            self.manifest["error"] = f"{type(error).__name__}: {error}"
            print(json.dumps({"status": "FAIL", "reason": self.manifest["error"]}))
            return 1
        finally:
            self.cleanup()
            write_json(self.root / "manifest.json", self.manifest)

    def cleanup(self) -> None:
        for scenario in reversed(self.scenarios):
            try:
                scenario.stop_server()
                self.mark_session_stopped(scenario.session)
            except (OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(f"session {scenario.session}: {error}")
        for process in reversed(self.processes):
            try:
                self.stop_owned(process)
            except (OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(f"{process.name}: {error}")
        self.processes.clear()
        if self.sway and self.sway.socket_path and self.sway.socket_path.exists():
            try:
                self.sway.call("exit", timeout=3)
            except (CaptureError, OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(f"private Sway exit: {error}")
        if self.temp_root is not None:
            shutil.rmtree(self.temp_root, ignore_errors=True)


def emit_from_cli(args: argparse.Namespace) -> int:
    if args.emit_fixture and args.emit_replacement:
        raise CaptureError("select one fixture colorway")
    colors = REPLACEMENT_COLORS if args.emit_replacement else FIXTURE_COLORS
    os.write(sys.stdout.fileno(), kitty_display_sequence(colors))
    time.sleep(max(0.0, args.hold_seconds))
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-bin", help="absolute path to the frozen baseline Herdr binary")
    parser.add_argument("--candidate-bin", help="absolute path to the candidate Herdr binary")
    parser.add_argument("--output", help="existing parent directory for a new evidence directory")
    parser.add_argument("--emit-fixture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--emit-replacement", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--hold-seconds", type=float, default=0.0, help=argparse.SUPPRESS)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.emit_fixture or args.emit_replacement:
        try:
            return emit_from_cli(args)
        except (OSError, CaptureError) as error:
            print(f"capture_kitty_graphics: {error}", file=sys.stderr)
            return 1
    if not args.baseline_bin or not args.candidate_bin or not args.output:
        print(
            "usage: capture_kitty_graphics.py --baseline-bin PATH --candidate-bin PATH --output DIR",
            file=sys.stderr,
        )
        return 2
    try:
        return Harness(args).execute()
    except (OSError, CaptureError, ValueError) as error:
        print(json.dumps({"status": "FAIL", "reason": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
