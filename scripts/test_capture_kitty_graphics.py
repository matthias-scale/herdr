import json
import os
import socket
import sys
import tempfile
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

    def test_client_workspace_state_without_client_scoped_evidence_is_blocked(self):
        with self.assertRaisesRegex(capture.CaptureError, "BLOCKED:.*client-scoped"):
            capture.verify_client_workspace_evidence(
                None, {"a": "workspace-a", "d": "workspace-d"}
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
            capture.pane_region(window, layout, "wD:p1"),
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
            capture.pane_region(smaller_client, smaller_layout, "wD:p1"),
            (16, 192, 64, 64),
        )


if __name__ == "__main__":
    unittest.main()
