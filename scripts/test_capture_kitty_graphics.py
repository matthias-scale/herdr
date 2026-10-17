import json
import os
import socket
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from scripts import capture_kitty_graphics as capture


def raster_with_pattern(colors, width=120, height=100, origin=(30, 20), patch_size=32):
    pixels = bytearray([16, 16, 16] * width * height)
    x0, y0 = origin
    for y in range(patch_size):
        for x in range(patch_size):
            color = colors[
                (2 if y >= patch_size // 2 else 0) + (1 if x >= patch_size // 2 else 0)
            ]
            offset = ((y0 + y) * width + x0 + x) * 3
            pixels[offset : offset + 3] = bytes(color)
    return bytes(pixels)


class FixtureOracleTests(unittest.TestCase):
    def test_finds_opaque_asymmetric_fixture_and_replacement(self):
        fixture = raster_with_pattern(capture.FIXTURE_COLORS)
        replacement = raster_with_pattern(capture.REPLACEMENT_COLORS)
        self.assertTrue(capture.fixture_pattern_present(120, 100, fixture))
        self.assertTrue(
            capture.fixture_pattern_present(
                120, 100, replacement, capture.REPLACEMENT_COLORS
            )
        )
        self.assertFalse(
            capture.fixture_pattern_present(
                120, 100, replacement, capture.FIXTURE_COLORS
            )
        )

    def test_blank_wrong_order_and_out_of_region_are_negative(self):
        blank = bytes([16, 16, 16] * 120 * 100)
        swapped = (
            capture.FIXTURE_COLORS[1],
            capture.FIXTURE_COLORS[0],
            capture.FIXTURE_COLORS[2],
            capture.FIXTURE_COLORS[3],
        )
        self.assertFalse(capture.fixture_pattern_present(120, 100, blank))
        self.assertFalse(
            capture.fixture_pattern_present(120, 100, raster_with_pattern(swapped))
        )
        patterned = raster_with_pattern(capture.FIXTURE_COLORS)
        self.assertFalse(
            capture.fixture_pattern_present(120, 100, patterned, roi=(0, 0, 20, 20))
        )
        self.assertTrue(
            capture.fixture_pattern_present(120, 100, patterned, roi=(20, 10, 60, 60))
        )

    def test_fixture_payload_is_opaque_and_kitty_encoded(self):
        rgba = capture.fixture_rgba(capture.FIXTURE_COLORS)
        self.assertEqual(len(rgba), capture.SIZE * capture.SIZE * 4)
        self.assertEqual(set(rgba[3::4]), {255})
        sequence = capture.kitty_display_sequence(capture.FIXTURE_COLORS)
        self.assertTrue(sequence.startswith(b"\x1b[2J\x1b[12;1H"))
        self.assertIn(b"\x1b_Ga=T,f=32,t=d,i=4242", sequence)
        self.assertTrue(sequence.endswith(b"\x1b\\"))
        self.assertEqual(
            capture.kitty_delete_sequence(), b"\x1b_Ga=d,d=I,i=4242,q=2;\x1b\\"
        )

    def test_fixture_positions_cursor_and_symbols_only_control_is_negative(self):
        sequence = capture.kitty_display_sequence(capture.FIXTURE_COLORS)
        self.assertTrue(sequence.startswith(b"\x1b[2J\x1b[12;1H"))
        symbols = capture.kitty_symbols_sequence(capture.FIXTURE_COLORS)
        self.assertNotIn(b"\x1b", symbols)
        self.assertIn(b"\\x1b_G", symbols)
        monochrome = bytes((248, 248, 248) * 120 * 100)
        self.assertFalse(capture.fixture_pattern_present(120, 100, monochrome))


class IsolationTests(unittest.TestCase):
    def test_pane_run_accepts_empty_success_but_json_queries_require_json(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            scenario = capture.HerdrScenario.__new__(capture.HerdrScenario)
            scenario.run = SimpleNamespace(root=root)
            scenario.label = "cli-contract"
            scenario.binary = root / "herdr"
            scenario.session = "capture-test"
            scenario.env = {}
            scenario.server_identity = object()

            with mock.patch.object(
                capture.subprocess,
                "run",
                side_effect=[
                    SimpleNamespace(returncode=0, stdout="", stderr=""),
                    SimpleNamespace(
                        returncode=0, stdout='{"result": {}}', stderr=""
                    ),
                    SimpleNamespace(returncode=0, stdout="", stderr=""),
                ],
            ):
                self.assertEqual(scenario.cli("pane", "run", "p1", "true"), {})
                self.assertEqual(scenario.cli("workspace", "list"), {"result": {}})
                with self.assertRaisesRegex(capture.CaptureError, "invalid JSON"):
                    scenario.cli("workspace", "list")

            with mock.patch.object(
                capture.subprocess,
                "run",
                return_value=SimpleNamespace(returncode=1, stdout="", stderr="failed"),
            ):
                with self.assertRaisesRegex(capture.CaptureError, "failed"):
                    scenario.cli("pane", "run", "p1", "false")

    def test_missing_compositor_is_explicitly_blocked_before_binary_validation(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temp:
            root = Path(temp)
            args = SimpleNamespace(
                baseline_bin=str(root / "missing-frozen-baseline"),
                candidate_bin=sys.executable,
                output=str(root),
            )

            def locate(name):
                return None if name in {"sway", "swaymsg"} else "/bin/true"

            with mock.patch.dict(
                os.environ,
                {"HERDR_SOCKET_PATH": "", "HERDR_CLIENT_SOCKET_PATH": ""},
            ):
                with mock.patch.object(capture.shutil, "which", side_effect=locate):
                    harness = capture.Harness(args)
                with self.assertRaisesRegex(capture.CaptureError, "BLOCKED:.*sway"):
                    harness.record_preflight()

            self.assertEqual(harness.manifest["status"], "BLOCKED")
            self.assertEqual(
                harness.manifest["blocked"]["missing_tools"], ["sway", "swaymsg"]
            )
            self.assertEqual(
                harness.manifest["requested_binary_paths"]["baseline"],
                str(root / "missing-frozen-baseline"),
            )

    def test_default_socket_overrides_are_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            env = {
                "HERDR_SOCKET_PATH": str(home / ".config" / "herdr-dev" / "herdr.sock")
            }
            self.assertEqual(
                capture.private_socket_conflicts(env, home),
                ["HERDR_SOCKET_PATH points at a default socket"],
            )

    def test_private_environment_drops_inherited_runtime_routing(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temp:
            root = Path(temp)
            home = root / "home"
            runtime = root / "runtime"
            config = home / ".config" / "herdr-dev" / "config.toml"
            runtime.mkdir()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wayland:
                wayland.bind(str(runtime / "wayland-private"))
                wayland.listen(1)
                with mock.patch.dict(
                    os.environ,
                    {
                        "HERDR_SOCKET_PATH": "/tmp/parent-herdr.sock",
                        "HERDR_CLIENT_SOCKET_PATH": "/tmp/parent-client.sock",
                        "HERDR_ENV": "1",
                        "DISPLAY": ":0",
                        "WAYLAND_DISPLAY": "wayland-live",
                        "SWAYSOCK": "/tmp/parent-sway.sock",
                        "WEZTERM_PANE": "99",
                        "WEZTERM_UNIX_SOCKET": "/tmp/parent-wezterm.sock",
                        "TMUX": "/tmp/parent-tmux",
                    },
                    clear=False,
                ):
                    env = capture.private_environment(
                        home, runtime, config, "wayland-private"
                    )
                    no_display = capture.private_environment(home, runtime, config)
            self.assertEqual(env["HOME"], str(home))
            self.assertEqual(env["XDG_RUNTIME_DIR"], str(runtime))
            self.assertEqual(env["WAYLAND_DISPLAY"], "wayland-private")
            self.assertNotIn("WAYLAND_DISPLAY", no_display)
            for name in (
                "HERDR_SOCKET_PATH",
                "HERDR_CLIENT_SOCKET_PATH",
                "HERDR_ENV",
                "DISPLAY",
                "SWAYSOCK",
                "WEZTERM_PANE",
                "WEZTERM_UNIX_SOCKET",
                "TMUX",
            ):
                self.assertNotIn(name, env)

    def test_wayland_display_is_discovered_as_a_private_listening_socket(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            runtime = Path(temporary)
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(str(runtime / "wayland-7"))
            listener.listen(1)
            try:
                self.assertEqual(capture.discover_wayland_display(runtime), "wayland-7")
            finally:
                listener.close()

    def test_capture_uses_a_fresh_grim_frame_as_its_oracle_input(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            pixels = bytes(
                (16, 16, 16) * capture.DISPLAY_SIZE[0] * capture.DISPLAY_SIZE[1]
            )

            class FakeSway:
                def __init__(self):
                    self.env = {}

                @staticmethod
                def window(_app_id):
                    return {"rect": {"x": 0, "y": 0, "width": 480, "height": 320}}

            def fake_run(command, **_kwargs):
                if command[0] == "grim":
                    Path(command[1]).write_bytes(b"current grim frame")
                    return SimpleNamespace(stdout=b"")
                if "-crop" in command:
                    Path(command[-1]).write_bytes(b"fresh receiving-window crop")
                    return SimpleNamespace(stdout=b"")
                return SimpleNamespace(stdout=pixels)

            runner = capture.CaptureRun(root, FakeSway(), "magick", "grim")
            with mock.patch.object(capture.subprocess, "run", side_effect=fake_run):
                matched, observation = runner.capture("blank", "herdr-test")
            self.assertFalse(matched)
            self.assertEqual(
                observation["capture_source"], "grim-live-compositor-frame"
            )
            self.assertEqual(
                observation["capture_sha256"],
                capture.sha256_file(root / observation["full_capture"]),
            )

    def test_build_provenance_rejects_identical_binaries_and_mismatched_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            baseline = root / "baseline"
            candidate = root / "candidate"
            baseline.write_bytes(b"same executable bytes")
            candidate.write_bytes(b"same executable bytes")
            baseline_record = root / "baseline.json"
            candidate_record = root / "candidate.json"

            def write_record(path, source_commit, source_tree, binary):
                path.write_text(
                    json.dumps(
                        {
                            "schema": capture.BUILD_RECORD_SCHEMA,
                            "source_commit": source_commit,
                            "source_tree": source_tree,
                            "binary_sha256": capture.sha256_file(binary),
                        }
                    ),
                    encoding="utf-8",
                )

            write_record(
                baseline_record,
                capture.EXPECTED_BASELINE_COMMIT,
                capture.EXPECTED_BASELINE_TREE,
                baseline,
            )
            write_record(
                candidate_record,
                capture.EXPECTED_CANDIDATE_COMMIT,
                capture.EXPECTED_CANDIDATE_TREE,
                candidate,
            )
            with self.assertRaisesRegex(capture.CaptureError, "distinct binary"):
                capture.validate_build_provenance(
                    {"baseline": baseline, "candidate": candidate},
                    {"baseline": baseline_record, "candidate": candidate_record},
                )

            candidate.write_bytes(b"different executable bytes")
            write_record(
                candidate_record,
                capture.EXPECTED_CANDIDATE_COMMIT,
                "2" * 40,
                candidate,
            )
            with self.assertRaisesRegex(capture.CaptureError, "candidate.*tree"):
                capture.validate_build_provenance(
                    {"baseline": baseline, "candidate": candidate},
                    {"baseline": baseline_record, "candidate": candidate_record},
                )

    def test_owned_process_group_is_stopped_and_recordable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            process = capture.OwnedProcess(
                "owned-sleep",
                [sys.executable, "-c", "import time;time.sleep(30)"],
                dict(os.environ),
                root / "owned.log",
            )
            self.assertTrue(process.alive())
            self.assertEqual(process.pgid, process.pid)
            process.stop(timeout=1)
            self.assertFalse(process.alive())

    def test_server_stop_refuses_identity_mismatch_and_reports_orphan(self):
        expected = capture.ProcessIdentity(
            pid=2345,
            start_ticks=100,
            executable="/usr/bin/herdr",
            pgid=2345,
            session_id=2345,
            home="/tmp/private-home",
            herdr_session="capture-a",
            command_line=("herdr", "server"),
        )
        replaced = capture.dataclasses.replace(expected, executable="/usr/bin/other")
        signals = []
        with self.assertRaisesRegex(capture.CaptureError, "identity mismatch"):
            capture.stop_server_with_identity(
                expected,
                lambda: capture.StopResult(0, "", ""),
                lambda _pid: replaced,
                lambda identity, sig: signals.append((identity, sig)),
                lambda _identity, _timeout: False,
                timeout=0,
            )
        self.assertEqual(signals, [])

        signals.clear()
        with self.assertRaisesRegex(capture.CaptureError, "orphaned"):
            capture.stop_server_with_identity(
                expected,
                lambda: capture.StopResult(0, "", ""),
                lambda _pid: expected,
                lambda identity, sig: signals.append((identity, sig)),
                lambda _identity, _timeout: False,
                timeout=0,
            )
        self.assertEqual(
            [sig for _identity, sig in signals],
            [capture.signal.SIGTERM, capture.signal.SIGKILL],
        )

    def test_server_stop_failure_falls_back_only_to_the_owned_identity(self):
        expected = capture.ProcessIdentity(
            pid=3456,
            start_ticks=200,
            executable="/usr/bin/herdr",
            pgid=3456,
            session_id=3456,
            home="/tmp/private-home",
            herdr_session="capture-b",
            command_line=("herdr", "server"),
        )
        alive = [True]
        signals = []

        def lookup(_pid):
            return expected if alive[0] else None

        def terminate(identity, sig):
            self.assertEqual(identity, expected)
            signals.append(sig)
            alive[0] = False

        with self.assertRaisesRegex(capture.CaptureError, "server stop exited 1"):
            capture.stop_server_with_identity(
                expected,
                lambda: capture.StopResult(1, "", "offline"),
                lookup,
                terminate,
                lambda _identity, _timeout: not alive[0],
                timeout=0,
            )
        self.assertEqual(signals, [capture.signal.SIGTERM])
        self.assertFalse(alive[0])

    def test_pane_region_uses_window_and_terminal_grid_geometry(self):
        window = {"rect": {"x": 0, "y": 0, "width": 800, "height": 600}}
        layout = {
            "result": {
                "layout": {
                    "area": {"width": 80, "height": 30},
                    "panes": [
                        {
                            "pane_id": "wD:p1",
                            "rect": {"x": 5, "y": 2, "width": 30, "height": 20},
                        }
                    ],
                }
            }
        }
        self.assertEqual(
            capture.pane_region(window, layout, "wD:p1", {"cols": 80, "rows": 30}),
            (50, 260, 80, 80),
        )
        smaller_client = {"rect": {"x": 0, "y": 0, "width": 480, "height": 320}}
        smaller_layout = {
            "result": {
                "layout": {
                    "area": {"width": 60, "height": 20},
                    "panes": [
                        {
                            "pane_id": "wD:p1",
                            "rect": {"x": 2, "y": 1, "width": 30, "height": 19},
                        }
                    ],
                }
            }
        }
        self.assertEqual(
            capture.pane_region(
                smaller_client, smaller_layout, "wD:p1", {"cols": 60, "rows": 20}
            ),
            (16, 192, 64, 64),
        )


class WezTermEvidenceTests(unittest.TestCase):
    """Ownership threats: wrong live GUI, stale PID, foreign socket, and PTY child.

    A same-binary/home/session lookalike is not owned unless its live ancestry
    reaches the exact launched GUI identity. Process-group and session changes
    inside the GUI's PTY are expected, while socket path/runtime ownership and
    the client's cwd, TTY, and foreground checks remain required.
    """

    def pane(self, pane_id=7):
        return {
            "pane_id": pane_id,
            "window_id": 2,
            "tab_id": 3,
            "tty_name": "/dev/pts/42",
            "rows": 30,
            "cols": 80,
            "pixel_width": 800,
            "pixel_height": 600,
        }

    def layout(self):
        return {
            "result": {
                "layout": {
                    "area": {"width": 80, "height": 30},
                    "panes": [
                        {
                            "pane_id": "herdr:D:p1",
                            "rect": {"x": 10, "y": 4, "width": 60, "height": 20},
                        }
                    ],
                }
            }
        }

    def screen(self, marker, layout=None, *, at=None):
        layout = layout or self.layout()
        body = layout["result"]["layout"]
        pane = body["panes"][0]["rect"]
        x = pane["x"] if at is None else at[0]
        y = pane["y"] + capture.MARKER_ROW - 1 if at is None else at[1]
        rows = [" " * 80 for _ in range(body["area"]["height"])]
        rows[y] = rows[y][:x] + marker + rows[y][x + len(marker) :]
        return "\n".join(rows)

    def test_wezterm_list_requires_one_complete_outer_pane(self):
        pane = self.pane()
        selected = capture.select_wezterm_pane(json.dumps([pane]))
        self.assertEqual(selected["pane_id"], 7)
        pane["size"] = {
            field: pane.pop(field)
            for field in ("rows", "cols", "pixel_width", "pixel_height")
        }
        self.assertEqual(capture.select_wezterm_pane(json.dumps([pane]))["rows"], 30)
        with self.assertRaisesRegex(capture.CaptureError, "exactly one"):
            capture.select_wezterm_pane(json.dumps([]))
        with self.assertRaisesRegex(capture.CaptureError, "exactly one"):
            capture.select_wezterm_pane(json.dumps([pane, self.pane(8)]))
        incomplete = self.pane()
        del incomplete["tty_name"]
        with self.assertRaisesRegex(capture.CaptureError, "tty_name"):
            capture.select_wezterm_pane(json.dumps([incomplete]))

    def test_outer_pane_wait_retries_startup_geometry_only(self):
        incomplete = self.pane()
        incomplete["rows"] = 0
        query = mock.Mock(
            side_effect=[json.dumps([incomplete]), json.dumps([self.pane()])]
        )
        selected = capture.wait_for_wezterm_pane(query, timeout=1)
        self.assertEqual(selected["pane_id"], 7)
        self.assertEqual(query.call_count, 2)
        self.assertGreater(query.call_args_list[0].args[0], 0)

    def test_outer_pane_wait_does_not_retry_identity_errors(self):
        query = mock.Mock(return_value=json.dumps([]))
        with self.assertRaisesRegex(capture.CaptureError, "exactly one"):
            capture.wait_for_wezterm_pane(query, timeout=1)
        self.assertEqual(query.call_count, 1)

    def test_live_socket_must_match_exact_launched_gui_identity(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            socket_dir = runtime / "wezterm"
            socket_dir.mkdir(parents=True)
            os.chmod(runtime, 0o700)
            expected_home = root / "home"
            expected_home.mkdir()
            gui = capture.ProcessIdentity(
                pid=200,
                start_ticks=30,
                executable="/usr/bin/wezterm-gui",
                pgid=200,
                session_id=200,
                home=str(expected_home),
                herdr_session="",
                command_line=("wezterm-gui", "--config-file", "private.lua"),
            )
            endpoint = socket_dir / "gui-sock-200"
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(str(endpoint))
            listener.listen(1)
            try:
                validated, gui_identity = capture.validate_owned_wezterm_socket(
                    str(endpoint),
                    runtime,
                    gui,
                    lambda pid: gui if pid == 200 else None,
                )
                self.assertEqual(validated, endpoint)
                self.assertEqual(gui_identity, gui)
                with self.assertRaisesRegex(capture.CaptureError, "private runtime"):
                    capture.validate_owned_wezterm_socket(
                        str(endpoint), root, gui, lambda _pid: gui
                    )
                stale_gui = capture.dataclasses.replace(gui, start_ticks=31)
                with self.assertRaisesRegex(capture.CaptureError, "GUI identity"):
                    capture.validate_owned_wezterm_socket(
                        str(endpoint), runtime, gui, lambda _pid: stale_gui
                    )
                another_live_gui = capture.dataclasses.replace(gui, pid=201)
                with self.assertRaisesRegex(capture.CaptureError, "GUI identity"):
                    capture.validate_owned_wezterm_socket(
                        str(endpoint),
                        runtime,
                        another_live_gui,
                        lambda _pid: gui,
                    )
                wrong_gui = capture.dataclasses.replace(gui, pgid=201)
                with self.assertRaisesRegex(capture.CaptureError, "GUI identity"):
                    capture.validate_owned_wezterm_socket(
                        str(endpoint), runtime, gui, lambda _pid: wrong_gui
                    )
                with self.assertRaisesRegex(capture.CaptureError, "socket path"):
                    capture.validate_owned_wezterm_socket(
                        "", runtime, gui, lambda _pid: gui
                    )
                external_dir = root / "external"
                external_dir.mkdir()
                external = external_dir / "gui-sock-201"
                external_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                external_listener.bind(str(external))
                external_listener.listen(1)
                try:
                    with self.assertRaisesRegex(
                        capture.CaptureError, "private runtime"
                    ):
                        capture.validate_owned_wezterm_socket(
                            str(external), runtime, gui, lambda _pid: gui
                        )
                finally:
                    external_listener.close()
            finally:
                listener.close()

    def test_only_descendant_client_is_matched_across_pty_process_group(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            binary = root / "herdr"
            binary.write_text("fixture")
            home = root / "home"
            home.mkdir()
            proc_root = root / "proc"
            proc_root.mkdir()
            for pid in (200, 201):
                (proc_root / str(pid)).mkdir()

            gui = capture.ProcessIdentity(
                pid=100,
                start_ticks=10,
                executable="/usr/bin/wezterm-gui",
                pgid=100,
                session_id=100,
                home=str(home),
                herdr_session="",
                command_line=("wezterm-gui",),
                parent_pid=1,
            )
            client = capture.ProcessIdentity(
                pid=200,
                start_ticks=20,
                executable=str(binary),
                pgid=200,
                session_id=200,
                home=str(home),
                herdr_session="",
                command_line=(str(binary), "--session", "capture-a", "client"),
                parent_pid=100,
            )
            lookalike = capture.dataclasses.replace(
                client, pid=201, start_ticks=21, parent_pid=300
            )
            identities = {100: gui, 200: client, 201: lookalike}
            matches = capture.matching_processes(
                binary,
                "capture-a",
                home,
                owned_gui=gui,
                process_root=proc_root,
                process_lookup=identities.get,
            )
            self.assertEqual(matches, [client])
            self.assertNotEqual(client.pgid, gui.pgid)
            self.assertNotEqual(client.session_id, gui.session_id)

    def test_stale_launched_gui_pid_does_not_prove_client_ancestry(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            binary = root / "herdr"
            binary.write_text("fixture")
            home = root / "home"
            home.mkdir()
            proc_root = root / "proc"
            proc_root.mkdir()
            (proc_root / "200").mkdir()
            expected_gui = capture.ProcessIdentity(
                pid=100,
                start_ticks=10,
                executable="/usr/bin/wezterm-gui",
                pgid=100,
                session_id=100,
                home=str(home),
                herdr_session="",
                command_line=("wezterm-gui",),
                parent_pid=1,
            )
            reused_gui = capture.dataclasses.replace(expected_gui, start_ticks=11)
            client = capture.ProcessIdentity(
                pid=200,
                start_ticks=20,
                executable=str(binary),
                pgid=200,
                session_id=200,
                home=str(home),
                herdr_session="",
                command_line=(str(binary), "--session", "capture-a", "client"),
                parent_pid=100,
            )
            matches = capture.matching_processes(
                binary,
                "capture-a",
                home,
                owned_gui=expected_gui,
                process_root=proc_root,
                process_lookup={100: reused_gui, 200: client}.get,
            )
            self.assertEqual(matches, [])

    def test_input_ownership_checks_user_private_cwd_tty_and_foreground_group(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as temporary:
            root = Path(temporary)
            proc_root = root / "proc"
            proc = proc_root / "3456"
            workdir = root / "work"
            workdir.mkdir()
            (proc / "fd").mkdir(parents=True)
            (proc / "cwd").symlink_to(workdir, target_is_directory=True)
            (proc / "fd" / "0").symlink_to("/dev/pts/42")
            identity = capture.ProcessIdentity(
                pid=3456,
                start_ticks=200,
                executable="/usr/bin/herdr",
                pgid=3456,
                session_id=3456,
                home=str(root),
                herdr_session="capture-b",
                command_line=("herdr", "--session", "capture-b", "client"),
                state="S",
                tty_nr=34816,
                foreground_pgid=3456,
            )
            pane = {"tty_name": "/dev/pts/42"}
            evidence = capture.validate_client_process(
                identity, workdir, pane, proc_root, lambda _pid: identity
            )
            self.assertEqual(evidence["uid"], os.getuid())
            self.assertEqual(evidence["cwd"], str(workdir))
            self.assertEqual(evidence["tty_name"], "/dev/pts/42")
            self.assertEqual(evidence["foreground_pgid"], identity.pgid)
            with self.assertRaisesRegex(capture.CaptureError, "private workdir"):
                capture.validate_client_process(
                    identity, root, pane, proc_root, lambda _pid: identity
                )
            with (
                mock.patch.object(capture.os, "getuid", return_value=os.getuid() + 1),
                self.assertRaisesRegex(capture.CaptureError, "different user"),
            ):
                capture.validate_client_process(
                    identity, workdir, pane, proc_root, lambda _pid: identity
                )
            with self.assertRaisesRegex(capture.CaptureError, "TTY"):
                capture.validate_client_process(
                    identity,
                    workdir,
                    {"tty_name": "/dev/pts/43"},
                    proc_root,
                    lambda _pid: identity,
                )
            wrong_foreground = capture.dataclasses.replace(
                identity, foreground_pgid=999
            )
            with self.assertRaisesRegex(capture.CaptureError, "not foreground"):
                capture.validate_client_process(
                    wrong_foreground,
                    workdir,
                    pane,
                    proc_root,
                    lambda _pid: wrong_foreground,
                )
            replaced = capture.dataclasses.replace(
                identity, executable="/usr/bin/other"
            )
            with self.assertRaisesRegex(capture.CaptureError, "identity changed"):
                capture.validate_client_process(
                    identity,
                    workdir,
                    pane,
                    proc_root,
                    lambda _pid: replaced,
                )

    def test_wezterm_cli_is_explicit_and_failure_has_no_fallback(self):
        env = {"HOME": "/tmp/private-home", "WEZTERM_PANE": "parent-pane"}
        failed = SimpleNamespace(returncode=1, stdout="", stderr="socket rejected")
        with (
            mock.patch.object(capture.subprocess, "run", return_value=failed) as run,
            self.assertRaisesRegex(capture.CaptureError, "no fallback"),
        ):
            capture.wezterm_cli(
                "/usr/bin/wezterm",
                Path("/tmp/private.lua"),
                env,
                Path("/tmp/private/wezterm/gui-sock-200"),
                ["list", "--format", "json"],
            )
        self.assertEqual(run.call_count, 1)
        command = run.call_args.args[0]
        self.assertEqual(
            command[1:5],
            ["--config-file", "/tmp/private.lua", "cli", "--no-auto-start"],
        )
        self.assertEqual(command[-3:], ["list", "--format", "json"])
        self.assertEqual(
            run.call_args.kwargs["env"]["WEZTERM_UNIX_SOCKET"],
            "/tmp/private/wezterm/gui-sock-200",
        )
        self.assertNotIn("WEZTERM_PANE", run.call_args.kwargs["env"])

    def test_start_and_config_disable_auto_connect_and_inherit_no_tabs_or_padding(self):
        command = capture.wezterm_start_command(
            "/usr/bin/wezterm",
            Path("/tmp/private.lua"),
            "herdr-private-d",
            Path("/tmp/private-work"),
            Path("/tmp/herdr"),
            "owned-session",
        )
        self.assertEqual(command[0], "/usr/bin/wezterm-gui")
        self.assertIn("--no-auto-connect", command)
        self.assertIn("--always-new-process", command)
        self.assertIn("herdr-private-d", command)
        config = capture.private_wezterm_config()
        for setting in (
            "unix_domains = {}",
            "ssh_domains = {}",
            "tls_clients = {}",
            "default_gui_startup_args = {'start'}",
            "enable_tab_bar = false",
            "window_decorations = 'NONE'",
            "left = 0",
            "right = 0",
        ):
            self.assertIn(setting, config)

    def test_visible_marker_must_be_unique_in_selected_pane_content(self):
        layout = self.layout()
        marker_a = "MAT354-A-ABC123"
        marker_d = "MAT354-D-ABC123"
        valid_a = capture.require_visible_marker(
            self.screen(marker_a, layout), marker_a, marker_d, layout, "herdr:D:p1"
        )
        valid_d = capture.require_visible_marker(
            self.screen(marker_d, layout), marker_d, marker_a, layout, "herdr:D:p1"
        )
        self.assertEqual(valid_a["source"], "wezterm-visible-terminal-lines")
        self.assertTrue(valid_d["screen_sha256"])
        for bad_screen in (
            self.screen(marker_d, layout),
            self.screen("", layout),
            self.screen(marker_a, layout, at=(2, 2)),
            self.screen(marker_a + " " + marker_a, layout),
        ):
            with self.assertRaises(capture.CaptureError):
                capture.require_visible_marker(
                    bad_screen, marker_a, marker_d, layout, "herdr:D:p1"
                )
        with self.assertRaisesRegex(capture.CaptureError, "wrong workspace marker"):
            capture.require_visible_marker(
                self.screen(marker_a + marker_d, layout),
                marker_a,
                marker_d,
                layout,
                "herdr:D:p1",
            )

    def test_a_marker_remains_visible_when_d_switches_and_d_is_excluded_from_a(self):
        layout = self.layout()
        a = "MAT354-A-ABC123"
        d = "MAT354-D-ABC123"
        before = self.screen(a, layout)
        after = self.screen(a, layout)
        after_d = self.screen(d, layout)
        before_evidence = capture.require_visible_marker(
            before, a, d, layout, "herdr:D:p1"
        )
        after_evidence = capture.require_marker_stays_visible(
            before, after, a, d, layout, "herdr:D:p1"
        )
        self.assertEqual(
            before_evidence["marker_position"], after_evidence["marker_position"]
        )
        d_evidence = capture.require_visible_marker(after_d, d, a, layout, "herdr:D:p1")
        self.assertNotEqual(
            before_evidence["screen_sha256"], d_evidence["screen_sha256"]
        )
        with self.assertRaises(capture.CaptureError):
            capture.require_marker_stays_visible(
                before, after_d, a, d, layout, "herdr:D:p1"
            )

    def test_client_geometry_must_match_current_outer_terminal_size(self):
        pane = self.pane()
        layout = self.layout()
        capture.validate_wezterm_geometry(pane, layout, "herdr:D:p1")
        pane["cols"] = 79
        with self.assertRaisesRegex(capture.CaptureError, "geometry mismatch"):
            capture.validate_wezterm_geometry(pane, layout, "herdr:D:p1")

    def test_chrome_offset_content_area_scales_by_outer_terminal_grid(self):
        # `pane layout` reports view.terminal_area (content after sidebar, tab bar
        # and dock) and absolute pane rects; the screenshot spans the outer grid.
        pane = self.pane()
        pane.update(cols=120, rows=40, pixel_width=1200, pixel_height=800)
        window = {"rect": {"x": 0, "y": 0, "width": 1200, "height": 800}}
        layout = {
            "result": {
                "layout": {
                    "area": {"x": 26, "y": 1, "width": 94, "height": 36},
                    "panes": [
                        {
                            "pane_id": "herdr:D:p1",
                            "rect": {"x": 26, "y": 1, "width": 94, "height": 36},
                        }
                    ],
                }
            }
        }
        capture.validate_wezterm_geometry(pane, layout, "herdr:D:p1")
        self.assertEqual(
            capture.pane_region(window, layout, "herdr:D:p1", pane),
            (
                (26 + capture.FIXTURE_COLUMN - 1) * 10,
                (1 + capture.FIXTURE_ROW - 1) * 20,
                capture.FIXTURE_COLUMNS * 10,
                capture.FIXTURE_ROWS * 20,
            ),
        )
        layout["result"]["layout"]["area"]["width"] = 95
        with self.assertRaisesRegex(capture.CaptureError, "geometry mismatch"):
            capture.validate_wezterm_geometry(pane, layout, "herdr:D:p1")

    def test_compositor_geometry_must_match_the_owned_app_id(self):
        window = {
            "app_id": "herdr-private-d",
            "type": "con",
            "rect": {"x": 10, "y": 20, "width": 800, "height": 600},
        }
        self.assertEqual(
            capture.validate_owned_window(window, "herdr-private-d"), window["rect"]
        )
        with self.assertRaisesRegex(capture.CaptureError, "owned app id"):
            capture.validate_owned_window(window, "another-window")
        window["rect"]["width"] = 0
        with self.assertRaisesRegex(capture.CaptureError, "invalid geometry"):
            capture.validate_owned_window(window, "herdr-private-d")

    def test_marker_that_would_wrap_out_of_the_fixture_pane_is_refused(self):
        layout = self.layout()
        layout["result"]["layout"]["panes"][0]["rect"]["width"] = 12
        with self.assertRaisesRegex(capture.CaptureError, "does not fit"):
            capture.require_visible_marker(
                self.screen("MAT354-D-ABC123", self.layout()),
                "MAT354-D-ABC123",
                "MAT354-A-ABC123",
                layout,
                "herdr:D:p1",
            )

    def test_marker_survives_fixture_replacement_and_removal_sequences(self):
        marker = "MAT354-D-ABC123"
        for sequence in (
            capture.kitty_display_sequence(capture.FIXTURE_COLORS, marker),
            capture.kitty_display_sequence(capture.REPLACEMENT_COLORS, marker),
            capture.kitty_delete_sequence(marker),
        ):
            self.assertIn(marker.encode("ascii"), sequence)
            self.assertIn(
                f"\x1b[{capture.MARKER_ROW};{capture.MARKER_COLUMN}H".encode(), sequence
            )


class ScenarioContractTests(unittest.TestCase):
    def test_aligned_d_preserves_client_a_while_d_and_shared_focus_are_d(self):
        scenario = capture.HerdrScenario.__new__(capture.HerdrScenario)
        scenario.workspace_a = "workspace-a"
        scenario.workspace_d = "workspace-d"
        scenario.pane_a = "pane-a"
        scenario.pane_d = "pane-d"
        scenario.a_id = "client-a"
        scenario.d_id = "client-d"
        scenario.workspace_markers = {"a": "marker-a", "d": "marker-d"}
        scenario.workspace_layouts = {"a": {"layout": "a"}, "d": {"layout": "d"}}
        scenario.session = "session"
        scenario.run = SimpleNamespace(manifest={})
        scenario.cli = mock.Mock(return_value={"focused": "workspace-d"})
        scenario.workspace_id = mock.Mock(return_value="workspace-d")
        evidence_by_client = {
            "client-a": {"marker": "marker-a"},
            "client-d": {"marker": "marker-d"},
        }
        scenario.wait_for_marker = mock.Mock(
            side_effect=lambda client, *_args: (None, evidence_by_client[client])
        )

        evidence = scenario.align_and_verify_d()

        self.assertEqual(
            scenario.wait_for_marker.call_args_list,
            [
                mock.call(
                    "client-d", "marker-d", "marker-a", {"layout": "d"}, "pane-d"
                ),
                mock.call(
                    "client-a", "marker-a", "marker-d", {"layout": "a"}, "pane-a"
                ),
            ],
        )
        self.assertEqual(
            evidence["predicates"],
            {
                "shared_workspace_d_focused": True,
                "client_a_a_marker_visible": True,
                "client_d_d_marker_visible": True,
            },
        )

    def default_config_keys(self):
        source = (Path(__file__).resolve().parents[1] / "src" / "main.rs").read_text(
            encoding="utf-8"
        )
        body = source.split('const DEFAULT_CONFIG: &str = r##"', 1)[1].split('"##', 1)[
            0
        ]
        section, keys = "", set()
        for line in body.splitlines():
            if line.startswith("["):
                section = line.strip("[]")
            elif line.startswith("# ") and " = " in line:
                keys.add((section, line[2:].split(" = ", 1)[0]))
        return keys

    def test_scenario_config_keys_exist_in_the_source_default_config(self):
        config = tomllib.loads(capture.scenario_config())
        known = self.default_config_keys()
        for name, value in config.items():
            pairs = value.items() if isinstance(value, dict) else [(name, value)]
            for key, _ in pairs:
                section = name if isinstance(value, dict) else ""
                self.assertIn((section, key), known)
        self.assertIs(config["onboarding"], False)
        self.assertIs(config["experimental"]["kitty_graphics"], True)
        self.assertIs(config["ui"]["show_home_on_start"], False)
        # Direct F6 dispatches in Terminal mode; a fresh server without Home
        # starts in Navigate mode, which matches prefix-form bindings.
        self.assertEqual(config["keys"]["next_workspace"], ["f6", "prefix+f6"])

    def test_pair_starts_owned_server_before_any_client(self):
        scenario = mock.Mock()
        scenario.start_client.side_effect = RuntimeError("stop after first client")
        with self.assertRaisesRegex(RuntimeError, "stop after first client"):
            capture.HerdrScenario.create_pair(scenario)
        names = [call[0] for call in scenario.method_calls]
        self.assertEqual(names[:3], ["start_server", "cli", "start_client"])

    def test_owned_server_is_launched_in_foreground_and_bound_to_its_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            run = mock.Mock(root=root)
            run.own.side_effect = lambda process: process
            scenario = SimpleNamespace(
                run=run,
                binary=Path("/opt/herdr"),
                session="mat354-x-baseline",
                label="baseline",
                env={"HOME": str(root)},
                home=root,
                workdir=root / "work",
                processes=[],
                server_identity=None,
            )
            process = SimpleNamespace(pid=41)
            with (
                mock.patch.object(
                    capture, "OwnedProcess", return_value=process
                ) as owned,
                mock.patch.object(
                    capture,
                    "wait_for_owned_server",
                    return_value=SimpleNamespace(pid=41),
                ),
            ):
                capture.HerdrScenario.start_server(scenario)
            command, env = owned.call_args.args[1:3]
            self.assertEqual(
                command, ["/opt/herdr", "server", "--session", "mat354-x-baseline"]
            )
            self.assertEqual(env["HERDR_STARTUP_CWD"], str(root / "work"))
            self.assertEqual(scenario.processes, [process])
            self.assertEqual(scenario.server_identity.pid, 41)
            run.record_server_process.assert_called_once()
            scenario.server_identity = None
            with (
                mock.patch.object(capture, "OwnedProcess", return_value=process),
                mock.patch.object(
                    capture,
                    "wait_for_owned_server",
                    return_value=SimpleNamespace(pid=42),
                ),
                self.assertRaisesRegex(capture.CaptureError, "owned server"),
            ):
                capture.HerdrScenario.start_server(scenario)


if __name__ == "__main__":
    unittest.main()
