import json
import unittest
from unittest.mock import patch

from scripts.parallel_check import ROOT, compiler_error_locations_under_src


def compiler_message(file_name: str, *, level: str = "error", primary: bool = True) -> str:
    return json.dumps(
        {
            "reason": "compiler-message",
            "message": {
                "level": level,
                "spans": [
                    {
                        "file_name": file_name,
                        "is_primary": primary,
                        "line_start": 12,
                        "column_start": 7,
                    }
                ],
            },
        }
    )


class ParallelCheckTests(unittest.TestCase):
    def test_reports_primary_compiler_errors_under_src(self):
        output = "\n".join(
            (
                compiler_message("src/app.rs"),
                compiler_message(str(ROOT / "src/platform/windows.rs")),
            )
        )

        self.assertEqual(
            compiler_error_locations_under_src(output),
            ["src/app.rs:12:7", "src/platform/windows.rs:12:7"],
        )

    def test_ignores_integration_targets_and_non_primary_diagnostics(self):
        output = "\n".join(
            (
                compiler_message("tests/live_handoff.rs"),
                compiler_message("src/app.rs", primary=False),
                compiler_message("src/app.rs", level="warning"),
                "not json",
            )
        )

        self.assertEqual(compiler_error_locations_under_src(output), [])

    def test_rejects_src_prefix_outside_repository_source_directory(self):
        with patch("scripts.parallel_check.ROOT", ROOT / "fixture"):
            output = compiler_message(str(ROOT / "src/app.rs"))

            self.assertEqual(compiler_error_locations_under_src(output), [])


if __name__ == "__main__":
    unittest.main()
