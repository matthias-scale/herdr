#!/usr/bin/env python3
"""Isolated Kitty graphics pixel check for two Herdr clients.

This helper owns a private headless Sway compositor, standalone WezTerm
processes, named Herdr sessions, and every artifact it creates. It never
connects to an inherited Herdr or WezTerm endpoint.
"""

from __future__ import annotations

import argparse
import base64
import dataclasses
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Callable
from pathlib import Path

SIZE = 64
DISPLAY_SIZE = (480, 320)
FIXTURE_ROW = 12
FIXTURE_COLUMN = 1
FIXTURE_COLUMNS = 8
FIXTURE_ROWS = 4
MARKER_ROW = 6
MARKER_COLUMN = 1
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
BUILD_RECORD_SCHEMA = "herdr-capture-build-record-v1"
EXPECTED_BASELINE_COMMIT = "92df57ed8079a53fb28f8f667179e4c81ccdbfbb"
EXPECTED_BASELINE_TREE = "2f12fd3804e33469a2a5b53a66f33fd2d7bb34e6"
EXPECTED_CANDIDATE_COMMIT = "5f760b1de0e666e91b88f7ac7aec5435889318d7"
EXPECTED_CANDIDATE_TREE = "6f83077f7c00ba18d3419506a58bb845660b4ee6"


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


def kitty_marker_sequence(marker: str) -> bytes:
    if not re.fullmatch(r"[A-Z0-9-]{8,48}", marker):
        raise ValueError("fixture marker must be unique printable ASCII")
    return (
        f"\x1b[2J\x1b[{MARKER_ROW};{MARKER_COLUMN}H{marker}"
        f"\x1b[{FIXTURE_ROW};{FIXTURE_COLUMN}H"
    ).encode("ascii")


def kitty_display_sequence(
    colors: tuple[tuple[int, int, int], ...], marker: str | None = None
) -> bytes:
    payload = base64.b64encode(fixture_rgba(colors)).decode("ascii")
    control = "a=T,f=32,t=d,i=4242,p=1,s=64,v=64,c=8,r=4,q=2"
    prefix = (
        kitty_marker_sequence(marker)
        if marker is not None
        else f"\x1b[2J\x1b[{FIXTURE_ROW};{FIXTURE_COLUMN}H".encode("ascii")
    )
    return prefix + f"\x1b_G{control};{payload}\x1b\\".encode("ascii")


def kitty_symbols_sequence(colors: tuple[tuple[int, int, int], ...]) -> bytes:
    """Return visible escape notation as text without sending terminal controls."""
    return repr(kitty_display_sequence(colors)).encode("ascii")


def kitty_delete_sequence(marker: str | None = None) -> bytes:
    prefix = kitty_marker_sequence(marker) if marker is not None else b""
    return prefix + b"\x1b_Ga=d,d=I,i=4242,q=2;\x1b\\"


def nearest_color(
    pixel: tuple[int, int, int], colors: tuple[tuple[int, int, int], ...]
) -> int | None:
    distances = [
        max(abs(channel - expected) for channel, expected in zip(pixel, color))
        for color in colors
    ]
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
            color_id = nearest_color(
                (rgb[offset], rgb[offset + 1], rgb[offset + 2]), colors
            )
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


def private_environment(
    home: Path, runtime: Path, config_path: Path, display: str | None = None
) -> dict[str, str]:
    env = {
        name: os.environ[name]
        for name in ("PATH", "LANG", "LC_ALL", "LC_CTYPE")
        if name in os.environ
    }
    env["HOME"] = str(home)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env["XDG_DATA_HOME"] = str(home / ".local" / "share")
    env["XDG_STATE_HOME"] = str(home / ".local" / "state")
    env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["HERDR_CONFIG_PATH"] = str(config_path)
    if display is not None:
        if (
            Path(display).name != display
            or discover_wayland_display(runtime) != display
        ):
            raise CaptureError(
                "WAYLAND_DISPLAY must name the validated socket in the private runtime"
            )
        env["WAYLAND_DISPLAY"] = display
    return env


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def discover_wayland_display(runtime: Path) -> str:
    """Return the one live Wayland socket published inside the private runtime."""
    runtime = runtime.resolve(strict=True)
    listening = []
    for path in sorted(runtime.glob("wayland-*")):
        try:
            metadata = path.lstat()
        except OSError:
            continue
        if not stat.S_ISSOCK(metadata.st_mode) or path.parent.resolve() != runtime:
            continue
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.settimeout(0.5)
        try:
            probe.connect(str(path))
        except OSError:
            continue
        else:
            listening.append(path.name)
        finally:
            probe.close()
    if len(listening) != 1:
        raise CaptureError(
            "private runtime must contain exactly one listening Wayland socket; "
            f"found {len(listening)}"
        )
    return listening[0]


def validate_build_provenance(
    binaries: dict[str, Path], records: dict[str, Path]
) -> dict[str, dict[str, str]]:
    """Validate parent-provided source/build records before launching either binary."""
    validated = {}
    for label in ("baseline", "candidate"):
        binary = binaries[label].expanduser().resolve(strict=True)
        record_path = records[label].expanduser().resolve(strict=True)
        try:
            record = json.loads(record_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise CaptureError(
                f"{label} build record is not readable JSON: {record_path}"
            ) from error
        if not isinstance(record, dict) or record.get("schema") != BUILD_RECORD_SCHEMA:
            raise CaptureError(f"{label} build record has an unsupported schema")
        commit = record.get("source_commit")
        tree = record.get("source_tree")
        binary_hash = record.get("binary_sha256")
        if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
            raise CaptureError(f"{label} build record lacks a full source commit")
        if not isinstance(tree, str) or not re.fullmatch(r"[0-9a-f]{40}", tree):
            raise CaptureError(f"{label} build record lacks a full source tree")
        if label == "baseline" and commit != EXPECTED_BASELINE_COMMIT:
            raise CaptureError(
                f"baseline build record source commit must equal {EXPECTED_BASELINE_COMMIT}"
            )
        if label == "baseline" and tree != EXPECTED_BASELINE_TREE:
            raise CaptureError(
                f"baseline build record source tree must equal {EXPECTED_BASELINE_TREE}"
            )
        if label == "candidate" and commit != EXPECTED_CANDIDATE_COMMIT:
            raise CaptureError(
                f"candidate build record source commit must equal {EXPECTED_CANDIDATE_COMMIT}"
            )
        if label == "candidate" and tree != EXPECTED_CANDIDATE_TREE:
            raise CaptureError(
                f"candidate build record source tree must equal {EXPECTED_CANDIDATE_TREE}"
            )
        actual_hash = sha256_file(binary)
        if binary_hash != actual_hash:
            raise CaptureError(f"{label} binary hash does not match its build record")
        validated[label] = {
            "source_commit": commit,
            "source_tree": tree,
            "binary_sha256": actual_hash,
            "record_path": str(record_path),
        }
    if (
        validated["baseline"]["binary_sha256"]
        == validated["candidate"]["binary_sha256"]
    ):
        raise CaptureError("baseline and candidate must be distinct binary artifacts")
    return validated


def select_wezterm_pane(output: str) -> dict:
    try:
        panes = json.loads(output)
    except json.JSONDecodeError as error:
        raise CaptureError("WezTerm list returned invalid JSON") from error
    if not isinstance(panes, list) or len(panes) != 1 or not isinstance(panes[0], dict):
        raise CaptureError("private WezTerm GUI must expose exactly one outer pane")
    pane = panes[0]
    for field in ("pane_id", "window_id", "tab_id", "tty_name"):
        value = pane.get(field)
        if value is None or not str(value).strip():
            raise CaptureError(f"WezTerm outer pane lacks {field}")
    for field in ("rows", "cols", "pixel_width", "pixel_height"):
        value = pane.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise CaptureError(f"WezTerm outer pane has invalid {field}")
    return pane


def single_herdr_pane_id(response: dict, workspace_id: str) -> str:
    panes = response.get("result", {}).get("panes", [])
    if not isinstance(panes, list) or len(panes) != 1 or not isinstance(panes[0], dict):
        raise CaptureError(
            f"workspace {workspace_id} must have exactly one fixture pane"
        )
    pane_id = panes[0].get("pane_id")
    if not isinstance(pane_id, str) or not pane_id:
        raise CaptureError(f"workspace {workspace_id} pane list has no pane id")
    return pane_id


def validate_wezterm_geometry(pane: dict, layout: dict, pane_id: str) -> None:
    body = layout.get("result", {}).get("layout", layout.get("result", {}))
    area = body.get("area")
    selected = next(
        (item for item in body.get("panes", []) if item.get("pane_id") == pane_id),
        None,
    )
    rect = selected.get("rect") if selected else None
    if not area or not rect:
        raise CaptureError("Herdr pane layout lacks geometry for the selected pane")
    if pane["cols"] != area.get("width") or pane["rows"] != area.get("height"):
        raise CaptureError(
            "WezTerm outer pane and Herdr layout geometry mismatch; refusing stale coordinates"
        )
    if (
        rect.get("x", -1) < 0
        or rect.get("y", -1) < 0
        or rect.get("width", 0) <= 0
        or rect.get("height", 0) <= 0
        or rect["x"] + rect["width"] > pane["cols"]
        or rect["y"] + rect["height"] > pane["rows"]
    ):
        raise CaptureError("Herdr pane rectangle is outside the current outer geometry")


def validate_owned_window(window: dict, app_id: str) -> dict:
    if window.get("app_id") != app_id or window.get("type") != "con":
        raise CaptureError("compositor window does not match the owned app id")
    rect = window.get("rect", {})
    if rect.get("width", 0) <= 0 or rect.get("height", 0) <= 0:
        raise CaptureError("owned compositor window has invalid geometry")
    return rect


def require_visible_marker(
    screen: str,
    marker: str,
    excluded_marker: str,
    layout: dict,
    pane_id: str,
) -> dict:
    if excluded_marker in screen:
        raise CaptureError("wrong workspace marker is visible in this client")
    body = layout.get("result", {}).get("layout", layout.get("result", {}))
    selected = next(
        (item for item in body.get("panes", []) if item.get("pane_id") == pane_id),
        None,
    )
    if not selected:
        raise CaptureError("marker evidence has no selected Herdr pane geometry")
    rect = selected.get("rect", {})
    x = rect.get("x", -1) + MARKER_COLUMN - 1
    y = rect.get("y", -1) + MARKER_ROW - 1
    if x + len(marker) > rect.get("x", 0) + rect.get("width", 0):
        raise CaptureError(
            "fixture marker does not fit in the selected pane content region"
        )
    if screen.count(marker) != 1:
        raise CaptureError("expected fixture marker is absent or ambiguous")
    lines = screen.splitlines()
    if y < 0 or y >= len(lines) or lines[y].find(marker) != x:
        raise CaptureError("fixture marker is outside the selected pane content region")
    return {
        "source": "wezterm-visible-terminal-lines",
        "marker": marker,
        "marker_position": {"x": x, "y": y},
        "screen_sha256": hashlib.sha256(screen.encode("utf-8")).hexdigest(),
    }


def require_marker_stays_visible(
    before: str,
    after: str,
    marker: str,
    excluded_marker: str,
    layout: dict,
    pane_id: str,
) -> dict:
    before_evidence = require_visible_marker(
        before, marker, excluded_marker, layout, pane_id
    )
    after_evidence = require_visible_marker(
        after, marker, excluded_marker, layout, pane_id
    )
    if before_evidence["marker_position"] != after_evidence["marker_position"]:
        raise CaptureError(
            "client content marker moved during the other client's navigation"
        )
    return after_evidence


def validate_owned_wezterm_socket(
    value: str | None,
    private_runtime: Path,
    owner: ProcessIdentity,
    process_lookup: Callable[[int], ProcessIdentity | None] | None = None,
) -> tuple[Path, ProcessIdentity]:
    if not value or not Path(value).is_absolute():
        raise CaptureError("owned WezTerm child has no explicit absolute socket path")
    runtime = private_runtime.resolve(strict=True)
    runtime_metadata = runtime.stat()
    if (
        runtime_metadata.st_uid != os.getuid()
        or stat.S_IMODE(runtime_metadata.st_mode) & 0o077
    ):
        raise CaptureError("WezTerm runtime is not private to the current user")
    supplied = Path(value)
    try:
        socket_path = supplied.resolve(strict=True)
        parent = socket_path.parent.resolve(strict=True)
    except OSError as error:
        raise CaptureError("owned WezTerm socket path is unavailable") from error
    if supplied != socket_path or parent != runtime / "wezterm":
        raise CaptureError("WezTerm socket is outside the task-private runtime")
    match = re.fullmatch(r"gui-sock-(\d+)", socket_path.name)
    if match is None:
        raise CaptureError("WezTerm socket path does not identify its GUI process")
    try:
        metadata = socket_path.lstat()
    except OSError as error:
        raise CaptureError("WezTerm socket path is unavailable") from error
    if not stat.S_ISSOCK(metadata.st_mode) or metadata.st_uid != os.getuid():
        raise CaptureError("WezTerm endpoint is not an owned Unix socket")
    if process_lookup is None:
        process_lookup = read_process_identity
    gui = process_lookup(int(match.group(1)))
    if (
        gui is None
        or gui.state == "Z"
        or Path(gui.executable).name != "wezterm-gui"
        or gui.home != owner.home
        or gui.pgid != owner.pgid
        or gui.session_id != owner.session_id
    ):
        raise CaptureError(
            "WezTerm GUI does not match the recorded owned process group"
        )
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    probe.settimeout(0.5)
    try:
        probe.connect(str(socket_path))
    except OSError as error:
        raise CaptureError("owned WezTerm socket is not listening") from error
    finally:
        probe.close()
    return socket_path, gui


def wezterm_cli(
    executable: str,
    config: Path,
    base_env: dict[str, str],
    socket_path: Path,
    args: list[str],
    timeout: float = 5,
) -> str:
    if not str(socket_path) or not socket_path.is_absolute():
        raise CaptureError("WezTerm CLI requires an explicit private socket")
    env = dict(base_env)
    env.pop("WEZTERM_PANE", None)
    env["WEZTERM_UNIX_SOCKET"] = str(socket_path)
    command = [
        executable,
        "--config-file",
        str(config),
        "cli",
        "--no-auto-start",
        *args,
    ]
    try:
        result = subprocess.run(
            command,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise CaptureError(f"WezTerm CLI failed with no fallback: {error}") from error
    if result.returncode:
        raise CaptureError(
            f"WezTerm CLI failed with no fallback (exit {result.returncode})"
        )
    return result.stdout


def wezterm_start_command(
    executable: str,
    config: Path,
    app_id: str,
    cwd: Path,
    binary: Path,
    session: str,
) -> list[str]:
    return [
        executable,
        "--config-file",
        str(config),
        "start",
        "--always-new-process",
        "--no-auto-connect",
        "--class",
        app_id,
        "--cwd",
        str(cwd),
        "--",
        str(binary),
        "--session",
        session,
        "client",
    ]


def private_wezterm_config() -> str:
    return (
        "return { enable_wayland = true, unix_domains = {}, ssh_domains = {}, "
        "tls_clients = {}, default_gui_startup_args = {'start'}, "
        "enable_tab_bar = false, window_decorations = 'NONE', "
        "window_padding = { left = 0, right = 0, top = 0, bottom = 0 }, "
        "window_background_opacity = 1.0, font_size = 14.0, "
        "warn_about_missing_glyphs = false }\n"
    )


def tool_version(executable: str | None) -> str | None:
    if not executable:
        return None
    for flag in ("--version", "-version"):
        try:
            result = subprocess.run(
                [executable, flag],
                capture_output=True,
                text=True,
                timeout=4,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            continue
        text = (result.stdout + result.stderr).strip()
        if text:
            return text.splitlines()[0][:300]
    return "version unavailable"


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def walk_nodes(node: dict) -> list[dict]:
    output = [node]
    for key in ("nodes", "floating_nodes"):
        for child in node.get(key, []):
            output.extend(walk_nodes(child))
    return output


@dataclasses.dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    start_ticks: int
    executable: str
    pgid: int
    session_id: int
    home: str
    herdr_session: str
    command_line: tuple[str, ...]
    state: str = "?"
    tty_nr: int = 0
    foreground_pgid: int = 0


@dataclasses.dataclass(frozen=True)
class StopResult:
    returncode: int
    stdout: str
    stderr: str


def read_process_environment_value(pid: int, name: str) -> str | None:
    """Read one named Linux process environment value without exposing its siblings."""
    prefix = name.encode("ascii") + b"="
    try:
        values = (Path("/proc") / str(pid) / "environ").read_bytes().split(b"\0")
        for value in values:
            if value.startswith(prefix):
                return value[len(prefix) :].decode("utf-8", "replace")
    except OSError:
        return None
    return None


def read_process_identity(pid: int) -> ProcessIdentity | None:
    """Read Linux process identity fields used to bind cleanup to an owned task."""
    proc = Path("/proc") / str(pid)
    try:
        raw_stat = (proc / "stat").read_text(encoding="ascii")
        fields = raw_stat[raw_stat.rfind(")") + 2 :].split()
        if len(fields) <= 19:
            return None
        process_env = {}
        for field in (proc / "environ").read_bytes().split(b"\0"):
            if b"=" in field:
                key, value = field.split(b"=", 1)
                if key in (b"HOME", b"HERDR_SESSION"):
                    process_env[key.decode("ascii")] = value.decode("utf-8", "replace")
        command_line = tuple(
            part.decode("utf-8", "replace")
            for part in (proc / "cmdline").read_bytes().split(b"\0")
            if part
        )
        return ProcessIdentity(
            pid=pid,
            start_ticks=int(fields[19]),
            executable=str((proc / "exe").resolve(strict=True)),
            pgid=int(fields[2]),
            session_id=int(fields[3]),
            home=process_env.get("HOME", ""),
            herdr_session=process_env.get("HERDR_SESSION", ""),
            command_line=command_line,
            state=fields[0],
            tty_nr=int(fields[4]),
            foreground_pgid=int(fields[5]),
        )
    except (OSError, ValueError, IndexError):
        return None


def validate_client_process(
    identity: ProcessIdentity,
    private_cwd: Path,
    pane: dict,
    proc_root: Path = Path("/proc"),
    process_lookup: Callable[[int], ProcessIdentity | None] | None = None,
) -> dict:
    if process_lookup is None:
        process_lookup = read_process_identity
    current = process_lookup(identity.pid)
    if (
        current is None
        or current.state == "Z"
        or not identity_matches(identity, current)
    ):
        raise CaptureError(
            "owned Herdr client identity changed before input or capture"
        )
    proc = proc_root / str(identity.pid)
    try:
        uid = proc.stat().st_uid
        cwd = (proc / "cwd").resolve(strict=True)
        tty_name = os.readlink(proc / "fd" / "0")
    except OSError as error:
        raise CaptureError("owned Herdr client cwd or TTY is unavailable") from error
    if uid != os.getuid():
        raise CaptureError("owned Herdr client belongs to a different user")
    if cwd != private_cwd.resolve(strict=True):
        raise CaptureError("owned Herdr client cwd is outside its private workdir")
    if tty_name != pane.get("tty_name") or current.tty_nr == 0:
        raise CaptureError("WezTerm pane TTY does not match the owned Herdr client")
    if current.foreground_pgid != identity.pgid:
        raise CaptureError("owned Herdr client is not foreground on its WezTerm TTY")
    return {
        "host": os.uname().nodename,
        "uid": uid,
        "cwd": str(cwd),
        "tty_name": tty_name,
        "foreground_pgid": current.foreground_pgid,
    }


def identity_matches(
    expected: ProcessIdentity, current: ProcessIdentity | None
) -> bool:
    return (
        current is not None
        and dataclasses.replace(current, state=expected.state) == expected
    )


def process_group_snapshot(pgid: int) -> list[tuple[int, int, int]]:
    members = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal():
            continue
        try:
            raw_stat = (proc / "stat").read_text(encoding="ascii")
            fields = raw_stat[raw_stat.rfind(")") + 2 :].split()
            if len(fields) > 3 and fields[0] != "Z" and int(fields[2]) == pgid:
                members.append((int(proc.name), int(fields[2]), int(fields[3])))
        except (OSError, ValueError):
            continue
    return members


def matching_processes(
    binary: Path,
    session: str,
    home: Path,
    *,
    pgid: int | None = None,
    detached_server: bool = False,
) -> list[ProcessIdentity]:
    executable = str(binary.resolve(strict=True))
    expected_home = str(home.resolve(strict=True))
    matches = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdecimal():
            continue
        identity = read_process_identity(int(proc.name))
        if (
            identity is None
            or identity.state == "Z"
            or identity.executable != executable
            or identity.home != expected_home
            or (pgid is not None and identity.pgid != pgid)
        ):
            continue
        is_server_command = (
            len(identity.command_line) > 1 and identity.command_line[1] == "server"
        )
        if detached_server:
            if identity.session_id == identity.pid and is_server_command:
                matches.append(identity)
        else:
            args = identity.command_line[1:]
            session_arg = identity.herdr_session == session or any(
                arg == f"--session={session}"
                or (arg == "--session" and args[index + 1 : index + 2] == (session,))
                for index, arg in enumerate(args)
            )
            if identity.session_id == pgid and not is_server_command and session_arg:
                matches.append(identity)
    return matches


def wait_for_owned_server(
    binary: Path, session: str, home: Path, timeout: float = 10.0
) -> ProcessIdentity:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        matches = matching_processes(binary, session, home, detached_server=True)
        if len(matches) == 1:
            return matches[0]
        if len(matches) > 1:
            raise CaptureError(
                f"multiple detached Herdr servers match private session {session}"
            )
        time.sleep(0.05)
    raise CaptureError(
        f"could not identify detached Herdr server for private session {session}"
    )


def wait_for_process_exit(expected: ProcessIdentity, timeout: float) -> bool:
    deadline = time.monotonic() + max(0.0, timeout)
    while True:
        current = read_process_identity(expected.pid)
        if (
            current is None
            or current.start_ticks != expected.start_ticks
            or current.state == "Z"
        ):
            return True
        if not identity_matches(expected, current):
            return False
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def terminate_exact_process(expected: ProcessIdentity, sig: int) -> None:
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise CaptureError(
            "safe PID-bound signaling is unavailable; refusing fallback termination"
        )
    try:
        pidfd = os.pidfd_open(expected.pid, 0)
    except ProcessLookupError:
        return
    try:
        current = read_process_identity(expected.pid)
        if not identity_matches(expected, current):
            raise CaptureError(
                f"PID {expected.pid} identity mismatch; refusing signal {sig}"
            )
        try:
            signal.pidfd_send_signal(pidfd, sig)
        except ProcessLookupError:
            return
    finally:
        os.close(pidfd)


def stop_process_with_identity(
    expected: ProcessIdentity,
    terminate: Callable[[ProcessIdentity, int], None],
    wait_for_exit: Callable[[ProcessIdentity, float], bool],
    timeout: float = 3.0,
) -> None:
    if wait_for_exit(expected, 0):
        return
    current = read_process_identity(expected.pid)
    if current is None or current.start_ticks != expected.start_ticks:
        return
    if not identity_matches(expected, current):
        raise CaptureError(
            f"task-owned PID {expected.pid} identity mismatch; refusing termination"
        )
    terminate(expected, signal.SIGTERM)
    if wait_for_exit(expected, timeout):
        return
    current = read_process_identity(expected.pid)
    if not identity_matches(expected, current):
        raise CaptureError(
            f"task-owned PID {expected.pid} identity changed during termination"
        )
    terminate(expected, signal.SIGKILL)
    if not wait_for_exit(expected, timeout):
        raise CaptureError(
            f"orphaned task-owned PID {expected.pid} survived verified SIGKILL"
        )


def stop_server_with_identity(
    expected: ProcessIdentity,
    stop_command: Callable[[], StopResult],
    lookup: Callable[[int], ProcessIdentity | None],
    terminate: Callable[[ProcessIdentity, int], None],
    wait_for_exit: Callable[[ProcessIdentity, float], bool],
    timeout: float = 5.0,
) -> None:
    """Stop one recorded daemon; fallback signals require the same PID/start identity."""
    command_error = None
    try:
        result = stop_command()
        if result.returncode:
            command_error = (
                f"server stop exited {result.returncode}: {result.stderr.strip()}"
            )
    except (OSError, subprocess.SubprocessError) as error:
        command_error = f"server stop command failed: {error}"

    if not wait_for_exit(expected, timeout):
        current = lookup(expected.pid)
        if current is None or current.start_ticks != expected.start_ticks:
            if command_error:
                raise CaptureError(command_error)
            return
        if not identity_matches(expected, current):
            raise CaptureError(
                f"server PID {expected.pid} identity mismatch; refusing fallback termination"
            )
        terminate(expected, signal.SIGTERM)
        if not wait_for_exit(expected, min(timeout, 3.0)):
            current = lookup(expected.pid)
            if not identity_matches(expected, current):
                raise CaptureError(
                    f"server PID {expected.pid} identity changed during fallback termination"
                )
            terminate(expected, signal.SIGKILL)
            if not wait_for_exit(expected, min(timeout, 3.0)):
                raise CaptureError(
                    f"orphaned task-owned server PID {expected.pid} survived verified SIGKILL"
                )
    if command_error:
        raise CaptureError(command_error)


class OwnedProcess:
    def __init__(
        self, name: str, command: list[str], env: dict[str, str], log_path: Path
    ):
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
        try:
            self.pgid = os.getpgid(self.pid)
        except OSError as error:
            self._stop_unverified_launch()
            raise CaptureError(f"{name} process group could not be verified") from error
        if self.pgid != self.pid:
            self._stop_unverified_launch()
            raise CaptureError(f"{name} did not start in an isolated process group")
        self.identity = read_process_identity(self.pid)
        if (
            self.identity is None
            or self.identity.pgid != self.pid
            or self.identity.session_id != self.pid
        ):
            self._stop_unverified_launch()
            raise CaptureError(f"{name} lacks a verifiable isolated process identity")

    def _stop_unverified_launch(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=1)
        self.log.close()

    def alive(self) -> bool:
        return self.process.poll() is None

    def stop(self, timeout: float = 5.0) -> None:
        if self.process.poll() is None or process_group_snapshot(self.pgid):
            self._signal_group(signal.SIGTERM)
            if self.process.poll() is None:
                try:
                    self.process.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    self._signal_group(signal.SIGKILL)
                    try:
                        self.process.wait(timeout=1)
                    except subprocess.TimeoutExpired as error:
                        raise CaptureError(
                            f"{self.name} survived verified SIGKILL"
                        ) from error
            if process_group_snapshot(self.pgid):
                self._signal_group(signal.SIGKILL)
                deadline = time.monotonic() + 1
                while process_group_snapshot(self.pgid) and time.monotonic() < deadline:
                    time.sleep(0.05)
        survivors = process_group_snapshot(self.pgid)
        if survivors:
            raise CaptureError(
                f"{self.name} exited but task-owned process group {self.pgid} has live members "
                f"{[pid for pid, _pgid, _sid in survivors]}; refusing unverified group termination"
            )
        self.log.close()

    def _signal_group(self, sig: int) -> None:
        current = read_process_identity(self.pid)
        if identity_matches(self.identity, current):
            if current.pgid != self.pgid or current.session_id != self.pid:
                raise CaptureError(
                    f"{self.name} no longer owns its recorded process group"
                )
        else:
            if self.process.poll() is None:
                raise CaptureError(
                    f"{self.name} PID {self.pid} identity mismatch; refusing process-group signal"
                )
            members = process_group_snapshot(self.pgid)
            if not members or any(
                sid != self.identity.session_id for _pid, _pgid, sid in members
            ):
                raise CaptureError(
                    f"{self.name} process group {self.pgid} lacks its verified task session"
                )
        try:
            os.killpg(self.pgid, sig)
        except ProcessLookupError:
            if self.process.poll() is None or process_group_snapshot(self.pgid):
                raise CaptureError(
                    f"{self.name} process group vanished while its leader is alive"
                )


class Sway:
    def __init__(
        self, process: OwnedProcess, executable: str, runtime: Path, env: dict[str, str]
    ):
        self.process = process
        self.executable = executable
        self.runtime = runtime
        self.env = env
        self.socket_path: Path | None = None
        self.display: str | None = None

    def call(self, *args: str, timeout: float = 5) -> subprocess.CompletedProcess[str]:
        if self.socket_path is None:
            self.socket_path = self.wait_for_socket()
        command = [
            shutil.which("swaymsg") or "swaymsg",
            "-s",
            str(self.socket_path),
            *args,
        ]
        result = subprocess.run(
            command,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        if result.returncode:
            raise CaptureError(f"swaymsg {args!r} failed: {result.stderr.strip()}")
        return result

    def wait_for_socket(self, timeout: float = 12) -> Path:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.process.alive():
                raise CaptureError("private Sway exited during startup")
            sockets = sorted(self.runtime.glob("sway-ipc.*.sock"))
            socket_ready = False
            if len(sockets) == 1:
                try:
                    socket_ready = stat.S_ISSOCK(sockets[0].lstat().st_mode)
                except OSError:
                    pass
            if socket_ready:
                self.socket_path = sockets[0]
                try:
                    self.call("-t", "get_version", timeout=2)
                    self.display = discover_wayland_display(self.runtime)
                except (CaptureError, OSError, subprocess.SubprocessError):
                    self.socket_path = None
                    time.sleep(0.1)
                    continue
                self.env["WAYLAND_DISPLAY"] = self.display
                return sockets[0]
            time.sleep(0.1)
        raise CaptureError(
            "private Sway did not publish one listening IPC socket and one listening "
            "Wayland socket under its private runtime"
        )

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
        raise CaptureError(
            f"private window {app_id} did not appear before its deadline"
        )


class CaptureRun:
    def __init__(self, root: Path, sway: Sway, magick: str, grim: str):
        self.root = root
        self.sway = sway
        self.magick = magick
        self.grim = grim
        self.capture_count = 0
        self.observations: list[dict] = []

    def capture(
        self, name: str, app_id: str, palette=FIXTURE_COLORS, roi=None
    ) -> tuple[bool, dict]:
        self.capture_count += 1
        sequence = self.capture_count
        full_path = self.root / "captures" / f"{name}-{sequence:03d}-full.png"
        crop_path = self.root / "captures" / f"{name}-{sequence:03d}-window.png"
        full_path.parent.mkdir(parents=True, exist_ok=True)
        if full_path.exists() or crop_path.exists():
            raise CaptureError(
                f"capture path already exists; refusing stale frame reuse: {full_path}"
            )
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
        if not full_path.is_file() or full_path.stat().st_size == 0:
            raise CaptureError("grim did not create a fresh compositor frame")
        geometry = f"{rect['width']}x{rect['height']}+{rect['x']}+{rect['y']}"
        subprocess.run(
            [self.magick, str(full_path), "-crop", geometry, "+repage", str(crop_path)],
            check=True,
            timeout=8,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        if not crop_path.is_file() or crop_path.stat().st_size == 0:
            raise CaptureError(
                "ImageMagick did not create the fresh receiving-window crop"
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
            "capture_source": "grim-live-compositor-frame",
            "capture_sha256": sha256_file(full_path),
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
    pane = next(
        (item for item in (panes or []) if item.get("pane_id") == pane_id), None
    )
    if not area or not pane or not area.get("width") or not area.get("height"):
        raise CaptureError("pane layout response lacks usable pane and area geometry")
    rect = pane["rect"]
    window_rect = window["rect"]
    if (
        rect.get("width", 0) < FIXTURE_COLUMN - 1 + FIXTURE_COLUMNS
        or rect.get("height", 0) < FIXTURE_ROW - 1 + FIXTURE_ROWS
    ):
        raise CaptureError(
            "receiving client pane is too small for the positioned Kitty fixture"
        )
    cell_width = window_rect["width"] / area["width"]
    cell_height = window_rect["height"] / area["height"]
    left = round((rect["x"] + FIXTURE_COLUMN - 1) * cell_width)
    top = round((rect["y"] + FIXTURE_ROW - 1) * cell_height)
    width = min(window_rect["width"] - left, round(FIXTURE_COLUMNS * cell_width))
    height = min(window_rect["height"] - top, round(FIXTURE_ROWS * cell_height))
    if width <= 0 or height <= 0:
        raise CaptureError("expected image region is outside the receiving window")
    return left, top, width, height


class HerdrScenario:
    def __init__(self, run: Harness, binary: Path, label: str):
        self.run = run
        self.binary = binary
        self.label = label
        self.session = f"mat354-{run.run_id[:10]}-{label}"
        self.a_id = f"herdr-{run.run_id[:7]}-{label}-a"
        self.d_id = f"herdr-{run.run_id[:7]}-{label}-d"
        self.processes: list[OwnedProcess] = []
        self.server_stopped = False
        self.server_identity: ProcessIdentity | None = None
        self.home = run.temp_root / label / "home"
        self.runtime = run.temp_root / "runtime"
        self.config = self.home / ".config" / "herdr-dev" / "config.toml"
        self.home.mkdir(parents=True, exist_ok=True)
        self.runtime.mkdir(parents=True, exist_ok=True)
        os.chmod(self.runtime, 0o700)
        self.config.parent.mkdir(parents=True, exist_ok=True)
        self.config.write_text("onboarding = false\n", encoding="utf-8")
        if not run.display:
            raise CaptureError(
                "private Wayland display was not discovered after Sway startup"
            )
        self.env = private_environment(
            self.home, self.runtime, self.config, run.display
        )
        self.workdir = run.temp_root / label / "work"
        self.workdir.mkdir(parents=True, exist_ok=True)
        self.client_processes: dict[str, ProcessIdentity] = {}
        self.outer_clients: dict[str, dict] = {}
        self.workspace_markers = {
            "a": f"MAT354-A-{run.run_id[:10].upper()}",
            "d": f"MAT354-D-{run.run_id[:10].upper()}",
        }
        self.workspace_layouts: dict[str, dict] = {}
        self.workspace_a: str | None = None
        self.workspace_d: str | None = None
        self.pane_a: str | None = None
        self.pane_d: str | None = None

    def cli(self, *args: str, timeout: float = 12, log_name: str = "cli") -> dict:
        log_path = (
            self.run.root
            / "logs"
            / f"{self.label}-{log_name}-{uuid.uuid4().hex[:6]}.log"
        )
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
            raise CaptureError(
                f"Herdr command {args!r} returned invalid JSON; see {log_path}"
            ) from error
        if response.get("error"):
            raise CaptureError(f"Herdr command {args!r} returned {response['error']}")
        if self.server_identity is None:
            self.server_identity = wait_for_owned_server(
                self.binary, self.session, self.home
            )
            self.run.record_server_process(self, self.server_identity)
        return response

    def start_client(self, app_id: str, label: str) -> OwnedProcess:
        command = wezterm_start_command(
            self.run.tools["wezterm"],
            self.run.wezterm_config,
            app_id,
            self.workdir,
            self.binary,
            self.session,
        )
        process = OwnedProcess(
            f"{self.label}-{label}",
            command,
            self.env,
            self.run.root / "logs" / f"{self.label}-{label}-wezterm.log",
        )
        self.processes.append(self.run.own(process))
        self.run.sway.window(app_id)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            clients = matching_processes(
                self.binary,
                self.session,
                self.home,
                pgid=process.pgid,
            )
            if len(clients) == 1:
                identity = clients[0]
                current = read_process_identity(identity.pid)
                if not identity_matches(identity, current):
                    raise CaptureError(
                        f"Herdr client identity changed during startup for {app_id}"
                    )
                socket_value = read_process_environment_value(
                    identity.pid, "WEZTERM_UNIX_SOCKET"
                )
                socket_path, gui_identity = validate_owned_wezterm_socket(
                    socket_value, self.runtime, identity
                )
                self.client_processes[app_id] = identity
                self.run.record_client_process(self, app_id, identity)
                pane = select_wezterm_pane(
                    wezterm_cli(
                        self.run.tools["wezterm"],
                        self.run.wezterm_config,
                        self.env,
                        socket_path,
                        ["list", "--format", "json"],
                    )
                )
                process_ownership = validate_client_process(
                    identity, self.workdir, pane
                )
                window = self.run.sway.window(app_id)
                self.outer_clients[app_id] = {
                    "socket_path": socket_path,
                    "gui_identity": gui_identity,
                    "pane": pane,
                    "window_rect": validate_owned_window(window, app_id),
                    "process_ownership": process_ownership,
                }
                return process
            if len(clients) > 1:
                raise CaptureError(
                    f"multiple Herdr clients match isolated window {app_id}"
                )
            if not process.alive():
                raise CaptureError(
                    f"isolated Herdr client window {app_id} exited before startup"
                )
            time.sleep(0.1)
        raise CaptureError(
            f"isolated Herdr client process for {app_id} was not observable"
        )

    def workspace_id(self, response: dict, focused: bool) -> str:
        workspaces = response.get("result", {}).get("workspaces", [])
        matches = [row for row in workspaces if row.get("focused") is focused]
        if len(matches) != 1:
            raise CaptureError(f"workspace list did not identify focused={focused}")
        workspace = matches[0].get("workspace_id")
        if not isinstance(workspace, str) or not workspace:
            raise CaptureError("focused workspace row has no workspace id")
        return workspace

    def outer_pane(self, app_id: str) -> dict:
        client = self.outer_clients.get(app_id)
        if client is None:
            raise CaptureError(f"no owned WezTerm endpoint recorded for {app_id}")
        current = select_wezterm_pane(
            self.wezterm(app_id, ["list", "--format", "json"])
        )
        previous = client["pane"]
        if any(
            current[field] != previous[field]
            for field in ("pane_id", "window_id", "tab_id", "tty_name")
        ):
            raise CaptureError("owned WezTerm pane identity changed during capture")
        rect = validate_owned_window(self.run.sway.window(app_id), app_id)
        client["pane"] = current
        client["window_rect"] = rect
        return current

    def wezterm(self, app_id: str, args: list[str]) -> str:
        client = self.outer_clients.get(app_id)
        expected = self.client_processes.get(app_id)
        if client is None or expected is None:
            raise CaptureError(f"no recorded WezTerm process identity for {app_id}")
        client["process_ownership"] = validate_client_process(
            expected, self.workdir, client["pane"]
        )
        socket_path, gui = validate_owned_wezterm_socket(
            read_process_environment_value(expected.pid, "WEZTERM_UNIX_SOCKET"),
            self.runtime,
            expected,
        )
        if socket_path != client["socket_path"] or not identity_matches(
            client["gui_identity"], gui
        ):
            raise CaptureError("owned WezTerm socket or GUI identity changed")
        return wezterm_cli(
            self.run.tools["wezterm"],
            self.run.wezterm_config,
            self.env,
            socket_path,
            args,
        )

    def visible_screen(
        self,
        app_id: str,
        marker: str,
        excluded_marker: str,
        layout: dict,
        inner_pane_id: str,
    ) -> tuple[str, dict]:
        client = self.outer_clients[app_id]
        pane = self.outer_pane(app_id)
        validate_wezterm_geometry(pane, layout, inner_pane_id)
        screen = self.wezterm(
            app_id,
            [
                "get-text",
                "--pane-id",
                str(pane["pane_id"]),
                "--start-line",
                "0",
                "--end-line",
                str(pane["rows"] - 1),
            ],
        )
        evidence = require_visible_marker(
            screen, marker, excluded_marker, layout, inner_pane_id
        )
        evidence.update(
            {
                "app_id": app_id,
                "socket_path": str(client["socket_path"]),
                "gui_pid": client["gui_identity"].pid,
                "outer_pane_id": pane["pane_id"],
                "outer_window_id": pane["window_id"],
                "outer_tab_id": pane["tab_id"],
                "tty_name": pane["tty_name"],
                "rows": pane["rows"],
                "cols": pane["cols"],
                "pixel_width": pane["pixel_width"],
                "pixel_height": pane["pixel_height"],
                "window_rect": client["window_rect"],
                "process_ownership": client["process_ownership"],
                "inner_pane_id": inner_pane_id,
            }
        )
        self.run.manifest.setdefault("client_screen_evidence", []).append(evidence)
        return screen, evidence

    def wait_for_marker(
        self,
        app_id: str,
        marker: str,
        excluded_marker: str,
        layout: dict,
        inner_pane_id: str,
        timeout: float = 10,
    ) -> tuple[str, dict]:
        deadline = time.monotonic() + timeout
        last_error = "marker not observed"
        while time.monotonic() < deadline:
            try:
                return self.visible_screen(
                    app_id, marker, excluded_marker, layout, inner_pane_id
                )
            except CaptureError as error:
                last_error = str(error)
                time.sleep(0.1)
        raise CaptureError(
            f"client {app_id} did not show its expected content marker: {last_error}"
        )

    def inject_wire(self, pane_id: str, wire: bytes, log_name: str) -> None:
        payload_code = (
            'import base64,os;os.write(1,base64.b64decode("'
            + base64.b64encode(wire).decode("ascii")
            + '"))'
        )
        self.cli(
            "pane",
            "run",
            pane_id,
            f"python3 -c '{payload_code}'",
            log_name=log_name,
        )

    def emit_marker(self, pane_id: str, marker: str) -> None:
        self.inject_wire(pane_id, kitty_marker_sequence(marker), "fixture-marker")

    def verify_pair_markers(self, stage: str) -> dict:
        if not all((self.workspace_a, self.workspace_d, self.pane_a, self.pane_d)):
            raise CaptureError("workspace marker verification lacks pair identities")
        _d_screen, d_evidence = self.wait_for_marker(
            self.d_id,
            self.workspace_markers["d"],
            self.workspace_markers["a"],
            self.workspace_layouts["d"],
            self.pane_d,
        )
        _a_screen, a_evidence = self.visible_screen(
            self.a_id,
            self.workspace_markers["a"],
            self.workspace_markers["d"],
            self.workspace_layouts["a"],
            self.pane_a,
        )
        focused = self.workspace_id(self.cli("workspace", "list"), True)
        if focused != self.workspace_a:
            raise CaptureError("client D activity changed the shared workspace focus")
        evidence = {
            "session": self.session,
            "stage": stage,
            "shared_focused_workspace": focused,
            "client_a": a_evidence,
            "client_d": d_evidence,
            "predicates": {
                "a_content_marker_visible": a_evidence["marker"]
                == self.workspace_markers["a"],
                "d_content_marker_visible": d_evidence["marker"]
                == self.workspace_markers["d"],
                "other_workspace_markers_excluded": True,
                "shared_workspace_focus_stayed_a": True,
            },
        }
        self.run.manifest.setdefault("workspace_content_checks", []).append(evidence)
        return evidence

    def create_pair(self) -> tuple[str, str, str, str]:
        self.start_client(self.a_id, "client-a")
        deadline = time.monotonic() + 20
        response = None
        while time.monotonic() < deadline:
            try:
                response = self.cli("workspace", "list", log_name="workspace-list-a")
                break
            except (CaptureError, subprocess.TimeoutExpired):
                if not any(process.alive() for process in self.processes):
                    raise CaptureError(
                        "initial Herdr client exited before its server became ready"
                    )
                time.sleep(0.25)
        if response is None:
            raise CaptureError("Herdr API did not become ready before its deadline")
        a_workspace = self.workspace_id(response, True)
        a_pane = single_herdr_pane_id(
            self.cli("pane", "list", "--workspace", a_workspace), a_workspace
        )
        created = self.cli(
            "workspace",
            "create",
            "--cwd",
            str(self.workdir),
            "--label",
            "graphics D",
            "--no-focus",
            log_name="workspace-create-d",
        )
        d_workspace = created.get("result", {}).get("workspace", {}).get("workspace_id")
        if not d_workspace:
            raise CaptureError("workspace create response lacks workspace id")
        if self.workspace_id(self.cli("workspace", "list"), True) != a_workspace:
            raise CaptureError("workspace D creation unexpectedly moved shared focus")
        d_pane = single_herdr_pane_id(
            self.cli("pane", "list", "--workspace", d_workspace), d_workspace
        )
        self.workspace_a = a_workspace
        self.workspace_d = d_workspace
        self.pane_a = a_pane
        self.pane_d = d_pane
        self.start_client(self.d_id, "client-d")
        a_layout = self.cli("pane", "layout", "--pane", a_pane)
        d_layout = self.cli("pane", "layout", "--pane", d_pane)
        self.workspace_layouts = {"a": a_layout, "d": d_layout}
        self.emit_marker(a_pane, self.workspace_markers["a"])
        self.emit_marker(d_pane, self.workspace_markers["d"])
        a_before_screen, a_before_evidence = self.wait_for_marker(
            self.a_id,
            self.workspace_markers["a"],
            self.workspace_markers["d"],
            a_layout,
            a_pane,
        )
        _d_before_screen, d_before_evidence = self.wait_for_marker(
            self.d_id,
            self.workspace_markers["a"],
            self.workspace_markers["d"],
            a_layout,
            a_pane,
        )
        self.wezterm(
            self.d_id,
            [
                "send-text",
                "--pane-id",
                str(self.outer_pane(self.d_id)["pane_id"]),
                "--no-paste",
                "\x1b[17~",
            ],
        )
        _d_screen, d_evidence = self.wait_for_marker(
            self.d_id,
            self.workspace_markers["d"],
            self.workspace_markers["a"],
            d_layout,
            d_pane,
        )
        a_after_screen, a_evidence = self.wait_for_marker(
            self.a_id,
            self.workspace_markers["a"],
            self.workspace_markers["d"],
            a_layout,
            a_pane,
        )
        require_marker_stays_visible(
            a_before_screen,
            a_after_screen,
            self.workspace_markers["a"],
            self.workspace_markers["d"],
            a_layout,
            a_pane,
        )
        if self.workspace_id(self.cli("workspace", "list"), True) != a_workspace:
            raise CaptureError("client D navigation changed the shared workspace focus")
        self.run.manifest.setdefault("workspace_navigation", []).append(
            {
                "session": self.session,
                "shared_focused_workspace": a_workspace,
                "workspace_markers": {
                    a_workspace: self.workspace_markers["a"],
                    d_workspace: self.workspace_markers["d"],
                },
                "navigation_input": {
                    "source": "wezterm-cli-send-text",
                    "app_id": self.d_id,
                    "outer_pane_id": self.outer_clients[self.d_id]["pane"]["pane_id"],
                    "key": "F6",
                    "bytes_hex": "1b5b31377e",
                },
                "client_a_still_shows_a": a_evidence,
                "client_a_before_f6": a_before_evidence,
                "client_d_before_f6": d_before_evidence,
                "client_d_shows_d": d_evidence,
                "predicates": {
                    "both_clients_attached_to_a_before_d_f6": True,
                    "only_d_received_f6": True,
                    "a_marker_unchanged": a_evidence["marker"]
                    == a_before_evidence["marker"]
                    == self.workspace_markers["a"],
                    "a_marker_position_unchanged": a_evidence["marker_position"]
                    == a_before_evidence["marker_position"],
                    "d_was_on_a_before_f6": d_before_evidence["marker"]
                    == self.workspace_markers["a"],
                    "d_content_marker_visible": d_evidence["marker"]
                    == self.workspace_markers["d"],
                    "shared_workspace_focus_stayed_a": True,
                },
            }
        )
        return a_workspace, d_workspace, a_pane, d_pane

    def emit(
        self,
        pane_id: str,
        marker: str,
        colors: tuple[tuple[int, int, int], ...] | None,
    ) -> None:
        if colors is None:
            wire = kitty_delete_sequence(marker)
        else:
            wire = kitty_display_sequence(colors, marker)
        self.inject_wire(pane_id, wire, "fixture-inject")

    def capture_region(self, pane_id: str, app_id: str) -> tuple[int, int, int, int]:
        layout = self.cli("pane", "layout", "--pane", pane_id, log_name="pane-layout")
        validate_wezterm_geometry(self.outer_pane(app_id), layout, pane_id)
        return pane_region(self.run.sway.window(app_id), layout, pane_id)

    def stop_server(self) -> None:
        if self.server_stopped:
            return
        if self.server_identity is None:
            raise CaptureError(
                f"no verified detached-server identity for session {self.session}"
            )
        log_path = self.run.root / "logs" / f"{self.label}-server-stop.log"

        def stop_command() -> StopResult:
            result = subprocess.run(
                [str(self.binary), "--session", self.session, "server", "stop"],
                env=self.env,
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
            log_path.parent.mkdir(parents=True, exist_ok=True)
            log_path.write_text(result.stdout + result.stderr, encoding="utf-8")
            return StopResult(result.returncode, result.stdout, result.stderr)

        stop_server_with_identity(
            self.server_identity,
            stop_command,
            read_process_identity,
            terminate_exact_process,
            wait_for_process_exit,
        )
        self.server_stopped = True

    def stop_clients(self) -> None:
        for identity in self.client_processes.values():
            stop_process_with_identity(
                identity,
                terminate_exact_process,
                wait_for_process_exit,
            )
            self.run.mark_process_identity_stopped(identity)


class Harness:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.run_id = uuid.uuid4().hex
        self.output = Path(args.output).expanduser().resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.root = (
            self.output
            / f"capture-kitty-graphics-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-{self.run_id[:8]}"
        )
        self.root.mkdir(parents=True, exist_ok=False)
        self.root = self.root.resolve()
        self.temp_root: Path | None = None
        self.display: str | None = None
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
            "build_provenance": {},
            "tools": {},
            "client_ids": {},
            "cases": [],
            "cleanup_errors": [],
            "pixel_evidence": {"source": "grim-live-compositor-frame", "captures": 0},
            "fixture_rgba_sha256": hashlib.sha256(
                fixture_rgba(FIXTURE_COLORS)
            ).hexdigest(),
            "replacement_rgba_sha256": hashlib.sha256(
                fixture_rgba(REPLACEMENT_COLORS)
            ).hexdigest(),
        }
        self.ownership: list[dict] = []

    def own(self, process: OwnedProcess) -> OwnedProcess:
        self.processes.append(process)
        record = {
            "name": process.name,
            "pid": process.pid,
            "pgid": process.pgid,
            "identity": dataclasses.asdict(process.identity),
            "command": process.command,
            "log": str(process.log_path.relative_to(self.root)),
            "state": "running",
        }
        self.ownership.append(record)
        write_json(self.root / "ownership.json", self.ownership)
        return process

    def record_server_process(
        self, scenario: HerdrScenario, identity: ProcessIdentity
    ) -> None:
        record = {
            "name": f"{scenario.label}-detached-server",
            "pid": identity.pid,
            "pgid": identity.pgid,
            "session_id": identity.session_id,
            "start_ticks": identity.start_ticks,
            "executable": identity.executable,
            "home": identity.home,
            "herdr_session": scenario.session,
            "observed_session_env": identity.herdr_session,
            "command_line": identity.command_line,
            "state": "running",
            "detached": True,
        }
        self.ownership.append(record)
        write_json(self.root / "ownership.json", self.ownership)

    def record_client_process(
        self, scenario: HerdrScenario, app_id: str, identity: ProcessIdentity
    ) -> None:
        self.ownership.append(
            {
                "name": f"{scenario.label}-client-{app_id}",
                "pid": identity.pid,
                "pgid": identity.pgid,
                "session_id": identity.session_id,
                "start_ticks": identity.start_ticks,
                "executable": identity.executable,
                "home": identity.home,
                "herdr_session": identity.herdr_session,
                "command_line": identity.command_line,
                "state": "running",
                "detached": False,
            }
        )
        write_json(self.root / "ownership.json", self.ownership)

    def mark_process_identity_stopped(self, identity: ProcessIdentity) -> None:
        for record in self.ownership:
            if (
                record.get("pid") == identity.pid
                and record.get("start_ticks") == identity.start_ticks
            ):
                record["state"] = "stopped"
        write_json(self.root / "ownership.json", self.ownership)

    def stop_owned(self, process: OwnedProcess) -> None:
        process.stop()
        for record in self.ownership:
            if record["pid"] == process.pid:
                record["state"] = "stopped"
                break
        write_json(self.root / "ownership.json", self.ownership)

    def record_preflight(self) -> None:
        original_home = (
            Path(os.environ.get("HOME", str(Path.home()))).expanduser().resolve()
        )
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
        if not self.args.baseline_build_record or not self.args.candidate_build_record:
            self.manifest["status"] = "BLOCKED"
            self.manifest["blocked"] = {
                "reason": "parent-provided source/build records are required for both binaries"
            }
            raise CaptureError(
                "BLOCKED: parent-provided baseline and candidate build records are required"
            )
        binaries = {
            "baseline": Path(self.args.baseline_bin),
            "candidate": Path(self.args.candidate_bin),
        }
        for label, raw_path in binaries.items():
            path = raw_path.expanduser().resolve(strict=True)
            if not path.is_file() or not os.access(path, os.X_OK):
                raise CaptureError(f"{label} binary is not executable: {path}")
            self.manifest["binary_paths"][label] = str(path)
        records = {
            "baseline": Path(self.args.baseline_build_record),
            "candidate": Path(self.args.candidate_build_record),
        }
        self.manifest["build_provenance"] = validate_build_provenance(binaries, records)
        self.manifest["binary_sha256"] = {
            label: record["binary_sha256"]
            for label, record in self.manifest["build_provenance"].items()
        }

    def start_compositor(self) -> None:
        self.temp_root = Path(tempfile.mkdtemp(prefix="mat354-", dir="/tmp"))
        self.manifest["private_runtime"] = str(self.temp_root)
        home = self.temp_root / "home"
        runtime = self.temp_root / "runtime"
        config_dir = home / ".config" / "herdr-dev"
        for directory in (
            home,
            runtime,
            config_dir,
            home / ".local" / "share",
            home / ".cache",
        ):
            directory.mkdir(parents=True, exist_ok=True)
        os.chmod(runtime, 0o700)
        (config_dir / "config.toml").write_text(
            "onboarding = false\n", encoding="utf-8"
        )
        config_path = config_dir / "config.toml"
        env = private_environment(home, runtime, config_path)
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
        self.wezterm_config.write_text(private_wezterm_config(), encoding="utf-8")
        compositor = OwnedProcess(
            "private-sway",
            [self.tools["sway"], "--config", str(sway_config), "--verbose"],
            env,
            self.root / "logs" / "sway.log",
        )
        self.own(compositor)
        self.sway = Sway(compositor, self.tools["sway"], runtime, env)
        self.sway.wait_for_socket()
        self.display = self.sway.display
        self.capture_run = CaptureRun(
            self.root, self.sway, self.tools["magick"], self.tools["grim"]
        )

    def start_window(self, app_id: str, label: str, program: list[str]) -> OwnedProcess:
        if self.temp_root is None:
            raise CaptureError("private runtime was not initialized")
        if not self.display:
            raise CaptureError("private Wayland display was not discovered")
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
            "--no-auto-connect",
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

        symbols_id = f"herdr-{self.run_id[:7]}-symbols"
        symbols = self.start_window(
            symbols_id,
            "direct-symbols-only",
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "--emit-symbols",
                "--hold-seconds",
                "180",
            ],
        )
        self.capture_run.expect_stable("symbols-only-negative", symbols_id, False)
        self.manifest["cases"].append(
            {
                "case": "symbols-only-negative",
                "status": "passed",
                "expected_match": False,
            }
        )
        self.stop_owned(symbols)
        self.processes.remove(symbols)

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
        direct = self.capture_run.expect_stable(
            "direct-terminal-positive", positive_id, True
        )
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
        a_workspace, d_workspace, pane_a, pane_d = scenario.create_pair()
        self.manifest.setdefault("client_processes", {})[binary_label] = {
            "client_a": dataclasses.asdict(scenario.client_processes[scenario.a_id]),
            "client_d": dataclasses.asdict(scenario.client_processes[scenario.d_id]),
            "client_a_app_id": scenario.a_id,
            "client_d_app_id": scenario.d_id,
            "outer_client_a": {
                "socket_path": str(
                    scenario.outer_clients[scenario.a_id]["socket_path"]
                ),
                "gui_identity": dataclasses.asdict(
                    scenario.outer_clients[scenario.a_id]["gui_identity"]
                ),
                "process_ownership": scenario.outer_clients[scenario.a_id][
                    "process_ownership"
                ],
                "pane": scenario.outer_clients[scenario.a_id]["pane"],
                "window_rect": scenario.outer_clients[scenario.a_id]["window_rect"],
            },
            "outer_client_d": {
                "socket_path": str(
                    scenario.outer_clients[scenario.d_id]["socket_path"]
                ),
                "gui_identity": dataclasses.asdict(
                    scenario.outer_clients[scenario.d_id]["gui_identity"]
                ),
                "process_ownership": scenario.outer_clients[scenario.d_id][
                    "process_ownership"
                ],
                "pane": scenario.outer_clients[scenario.d_id]["pane"],
                "window_rect": scenario.outer_clients[scenario.d_id]["window_rect"],
            },
            "pane_a": pane_a,
            "pane_d": pane_d,
            "workspace_a": a_workspace,
            "workspace_d": d_workspace,
            "workspace_markers": scenario.workspace_markers,
        }
        marker_d = scenario.workspace_markers["d"]
        scenario.emit(pane_d, marker_d, FIXTURE_COLORS)
        scenario.verify_pair_markers("fixture-injected")
        region = scenario.capture_region(pane_d, scenario.d_id)
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
            scenario.emit(pane_d, marker_d, REPLACEMENT_COLORS)
            scenario.verify_pair_markers("fixture-replaced")
            replacement = self.capture_run.expect_stable(
                "candidate-image-replacement",
                scenario.d_id,
                True,
                REPLACEMENT_COLORS,
                region,
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
            scenario.emit(pane_d, marker_d, None)
            scenario.verify_pair_markers("fixture-removed")
            removed = self.capture_run.expect_stable(
                "candidate-image-removal",
                scenario.d_id,
                False,
                REPLACEMENT_COLORS,
                region,
            )
            self.manifest["cases"].append(
                {"case": "candidate-image-removal", **removed}
            )
        scenario.stop_server()
        self.mark_session_stopped(scenario.session)
        for process in scenario.processes:
            self.stop_owned(process)
            if process in self.processes:
                self.processes.remove(process)
        scenario.stop_clients()

    def mark_session_stopped(self, session: str) -> None:
        for record in self.manifest.get("owned_sessions", []):
            if record["name"] == session:
                record["state"] = "stopped"
        for record in self.ownership:
            if record.get("herdr_session") == session:
                record["state"] = "stopped"
        write_json(self.root / "ownership.json", self.ownership)

    def execute(self) -> int:
        exit_code = 1
        try:
            self.record_preflight()
            self.start_compositor()
            self.direct_controls()
            baseline = Path(self.manifest["binary_paths"]["baseline"])
            candidate = Path(self.manifest["binary_paths"]["candidate"])
            self.run_scenario("baseline", baseline)
            self.run_scenario("candidate", candidate)
            self.manifest["status"] = "PASS"
            exit_code = 0
        except CaptureError as error:
            if str(error).startswith("BLOCKED:"):
                self.manifest["status"] = "BLOCKED"
                self.manifest.setdefault("blocked", {"reason": str(error)[9:].strip()})
                exit_code = 2
            else:
                self.manifest["status"] = "FAIL"
                self.manifest["error"] = str(error)
                exit_code = 1
        except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
            self.manifest["status"] = "FAIL"
            self.manifest["error"] = f"{type(error).__name__}: {error}"
            exit_code = 1
        finally:
            self.cleanup()
            if self.capture_run:
                self.manifest["pixel_evidence"]["captures"] = (
                    self.capture_run.capture_count
                )
            if self.manifest["cleanup_errors"]:
                self.manifest["status"] = "FAIL"
                self.manifest["error"] = "cleanup failed; private runtime retained"
                exit_code = 1
            write_json(self.root / "manifest.json", self.manifest)
        print(
            json.dumps(
                {
                    "status": self.manifest["status"],
                    "reason": self.manifest.get("error")
                    or self.manifest.get("blocked", {}).get("reason"),
                    "evidence_dir": str(self.root),
                }
            )
        )
        return exit_code

    def cleanup(self) -> None:
        for scenario in reversed(self.scenarios):
            try:
                scenario.stop_server()
                self.mark_session_stopped(scenario.session)
            except (CaptureError, OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(
                    f"session {scenario.session}: {error}"
                )
                if scenario.server_identity and wait_for_process_exit(
                    scenario.server_identity, 0
                ):
                    self.mark_session_stopped(scenario.session)
        for process in reversed(self.processes):
            try:
                self.stop_owned(process)
            except (CaptureError, OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(f"{process.name}: {error}")
        for scenario in reversed(self.scenarios):
            try:
                scenario.stop_clients()
            except (CaptureError, OSError, subprocess.SubprocessError) as error:
                self.manifest["cleanup_errors"].append(
                    f"session {scenario.session} clients: {error}"
                )
        if not self.manifest["cleanup_errors"]:
            self.processes.clear()
            if self.temp_root is not None:
                try:
                    shutil.rmtree(self.temp_root)
                except OSError as error:
                    self.manifest["cleanup_errors"].append(
                        f"private runtime removal failed: {error}"
                    )


def emit_from_cli(args: argparse.Namespace) -> int:
    selected = sum((args.emit_fixture, args.emit_replacement, args.emit_symbols))
    if selected != 1:
        raise CaptureError("select exactly one fixture output mode")
    if args.emit_symbols:
        os.write(sys.stdout.fileno(), kitty_symbols_sequence(FIXTURE_COLORS))
        time.sleep(max(0.0, args.hold_seconds))
        return 0
    colors = REPLACEMENT_COLORS if args.emit_replacement else FIXTURE_COLORS
    os.write(sys.stdout.fileno(), kitty_display_sequence(colors))
    time.sleep(max(0.0, args.hold_seconds))
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--baseline-bin", help="absolute path to the frozen baseline Herdr binary"
    )
    parser.add_argument(
        "--candidate-bin", help="absolute path to the candidate Herdr binary"
    )
    parser.add_argument(
        "--output", help="existing parent directory for a new evidence directory"
    )
    parser.add_argument(
        "--baseline-build-record", help="parent-provided JSON build record for baseline"
    )
    parser.add_argument(
        "--candidate-build-record",
        help="parent-provided JSON build record for candidate",
    )
    parser.add_argument("--emit-fixture", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument(
        "--emit-replacement", action="store_true", help=argparse.SUPPRESS
    )
    parser.add_argument("--emit-symbols", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument(
        "--hold-seconds", type=float, default=0.0, help=argparse.SUPPRESS
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.emit_fixture or args.emit_replacement or args.emit_symbols:
        try:
            return emit_from_cli(args)
        except (OSError, CaptureError) as error:
            print(f"capture_kitty_graphics: {error}", file=sys.stderr)
            return 1
    if (
        not args.baseline_bin
        or not args.candidate_bin
        or not args.output
        or not args.baseline_build_record
        or not args.candidate_build_record
    ):
        print(
            "usage: capture_kitty_graphics.py --baseline-bin PATH --candidate-bin PATH "
            "--baseline-build-record JSON --candidate-build-record JSON --output DIR",
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
