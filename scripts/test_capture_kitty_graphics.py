import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

from scripts import capture_kitty_graphics as capture


def raster_with_pattern(colors, width=120, height=100, origin=(30, 20), patch_size=32):
    pixels = bytearray([16, 16, 16] * width * height)
    x0, y0 = origin
    for y in range(patch_size):
        for x in range(patch_size):
            color = colors[(2 if y >= patch_size // 2 else 0) + (1 if x >= patch_size // 2 else 0)]
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
            capture.fixture_pattern_present(
                120, 100, raster_with_pattern(swapped)
            )
        )
        patterned = raster_with_pattern(capture.FIXTURE_COLORS)
        self.assertFalse(
            capture.fixture_pattern_present(
                120, 100, patterned, roi=(0, 0, 20, 20)
            )
        )
        self.assertTrue(
            capture.fixture_pattern_present(
                120, 100, patterned, roi=(20, 10, 60, 60)
            )
        )

    def test_fixture_payload_is_opaque_and_kitty_encoded(self):
        rgba = capture.fixture_rgba(capture.FIXTURE_COLORS)
        self.assertEqual(len(rgba), capture.SIZE * capture.SIZE * 4)
        self.assertEqual(set(rgba[3::4]), {255})
        sequence = capture.kitty_display_sequence(capture.FIXTURE_COLORS)
        self.assertTrue(sequence.startswith(b"\x1b_Ga=T,f=32,t=d,i=4242"))
        self.assertTrue(sequence.endswith(b"\x1b\\"))
        self.assertEqual(capture.kitty_delete_sequence(), b"\x1b_Ga=d,d=I,i=4242,q=2;\x1b\\")


class IsolationTests(unittest.TestCase):
    def test_missing_compositor_is_explicitly_blocked_before_binary_validation(self):
        with tempfile.TemporaryDirectory() as temp:
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
            env = {"HERDR_SOCKET_PATH": str(home / ".config" / "herdr-dev" / "herdr.sock")}
            self.assertEqual(
                capture.private_socket_conflicts(env, home),
                ["HERDR_SOCKET_PATH points at a default socket"],
            )

    def test_private_environment_drops_inherited_runtime_routing(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            home = root / "home"
            runtime = root / "runtime"
            config = home / ".config" / "herdr-dev" / "config.toml"
            with mock.patch.dict(
                os.environ,
                {
                    "HERDR_SOCKET_PATH": "/tmp/parent-herdr.sock",
                    "HERDR_CLIENT_SOCKET_PATH": "/tmp/parent-client.sock",
                    "HERDR_ENV": "1",
                    "DISPLAY": ":0",
                    "SWAYSOCK": "/tmp/parent-sway.sock",
                    "WEZTERM_PANE": "99",
                    "TMUX": "/tmp/parent-tmux",
                },
                clear=False,
            ):
                env = capture.private_environment(home, runtime, config, "wayland-private")
            self.assertEqual(env["HOME"], str(home))
            self.assertEqual(env["XDG_RUNTIME_DIR"], str(runtime))
            self.assertEqual(env["WAYLAND_DISPLAY"], "wayland-private")
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

    def test_pane_region_uses_window_and_terminal_grid_geometry(self):
        window = {"rect": {"x": 0, "y": 0, "width": 800, "height": 600}}
        layout = {
            "result": {
                "layout": {
                    "area": {"width": 80, "height": 30},
                    "panes": [{"pane_id": "wD:p1", "rect": {"x": 5, "y": 2}}],
                }
            }
        }
        self.assertEqual(
            capture.pane_region(window, layout, "wD:p1"),
            (50, 260, 100, 120),
        )


if __name__ == "__main__":
    unittest.main()
