import json
import os
import subprocess
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ADAPTER = ROOT / "scripts" / "qa-preview-adapter.sh"
JOURNEY = ROOT / "scripts" / "qa-sidebar-lifecycle-journey.sh"
DECLARATION = ROOT / ".qa" / "preview-adapter.json"
JOURNEYS = ROOT / "docs" / "qa" / "user-journeys.md"


class QaPreviewAdapterTests(unittest.TestCase):
    def setUp(self) -> None:
        self.head = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip()

    def adapter(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(ADAPTER), *args],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_declaration_points_to_executable_terminal_adapter_and_journeys(self) -> None:
        declaration = json.loads(DECLARATION.read_text(encoding="utf-8"))
        self.assertEqual(declaration["schema"], "preview-adapter/v1")
        self.assertEqual(declaration["command"], ["./scripts/qa-preview-adapter.sh"])
        self.assertEqual(declaration["journeys_document"], "docs/qa/user-journeys.md")
        self.assertTrue(ADAPTER.is_file())
        self.assertTrue(os.access(ADAPTER, os.X_OK))
        self.assertTrue(JOURNEY.is_file())
        self.assertTrue(os.access(JOURNEY, os.X_OK))
        self.assertTrue(JOURNEYS.is_file())

    def test_card_is_exact_head_terminal_contract_with_named_nonvisual_flows(self) -> None:
        with mock.patch.dict(os.environ, {"QA_SECRET_TOKEN": "never-print"}):
            result = self.adapter(
                "--repo-dir", str(ROOT), "--pr", "13", "--head", self.head,
                "--mode", "qa", "--format", "json",
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        card = json.loads(result.stdout)
        self.assertEqual(card["schema"], "preview-card/v1")
        self.assertEqual(card["head_sha"], self.head)
        self.assertEqual(card["preview_url"], "")
        self.assertNotIn("http", card["card_markdown"].lower())
        self.assertNotIn("never-print", result.stdout)
        self.assertEqual(card["artifacts"], [])
        self.assertEqual(
            [flow["id"] for flow in card["required_flows"]],
            [
                "sidebar-all-tab-retention",
                "space-first-single-line-sidebar",
                "same-session-title-replacement",
                "reopen-clears-done-without-reorder",
                "working-latches-until-genuine-completion",
            ],
        )
        for flow in card["required_flows"]:
            self.assertFalse(flow["visual_required"])
            self.assertEqual(flow["automation"]["schema"], "qa-automation/v1")
            self.assertEqual(
                flow["automation"]["command"],
                ["./scripts/qa-sidebar-lifecycle-journey.sh", "--flow", flow["id"]],
            )

    def test_declared_command_uses_its_process_working_directory(self) -> None:
        result = subprocess.run(
            [
                str(ADAPTER), "--pr", "13", "--head", self.head,
                "--mode", "qa", "--format", "json",
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        card = json.loads(result.stdout)
        self.assertEqual(card["head_sha"], self.head)

    def test_adapter_rejects_invalid_arguments_and_checkout_binding(self) -> None:
        short_head = self.adapter(
            "--repo-dir", str(ROOT), "--pr", "13", "--head", self.head[:12],
            "--mode", "qa", "--format", "json",
        )
        self.assertEqual(short_head.returncode, 64)
        self.assertIn("full --head", short_head.stderr)

        wrong_head = self.adapter(
            "--repo-dir", str(ROOT), "--pr", "13", "--head", "0" * 40,
            "--mode", "qa", "--format", "json",
        )
        self.assertEqual(wrong_head.returncode, 66)
        self.assertIn("not the requested exact head", wrong_head.stderr)

        invalid_pr = self.adapter(
            "--repo-dir", str(ROOT), "--pr", "0", "--head", self.head,
            "--mode", "qa", "--format", "json",
        )
        self.assertEqual(invalid_pr.returncode, 64)

        invalid_mode = self.adapter(
            "--repo-dir", str(ROOT), "--pr", "13", "--head", self.head,
            "--mode", "preview", "--format", "json",
        )
        self.assertEqual(invalid_mode.returncode, 64)

        invalid_format = self.adapter(
            "--repo-dir", str(ROOT), "--pr", "13", "--head", self.head,
            "--mode", "qa", "--format", "text",
        )
        self.assertEqual(invalid_format.returncode, 64)

    def test_journey_emits_head_bound_nonvisual_artifacts_and_fails_closed(self) -> None:
        artifact_root = ROOT / ".local" / "qa-preview-adapter-test"
        result = subprocess.run(
            [str(JOURNEY), "--repo-dir", str(ROOT), "--head", self.head,
             "--flow", "sidebar-all-tab-retention", "--artifact-dir", str(artifact_root)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["schema"], "qa-journey-result/v1")
        self.assertEqual(payload["status"], "PASS")
        self.assertEqual(payload["head_sha"], self.head)
        self.assertEqual(len(payload["artifacts"]), 1)
        artifact = Path(payload["artifacts"][0]["path"])
        self.assertTrue(artifact.is_file())
        self.assertTrue(artifact.is_relative_to(artifact_root.resolve()))
        self.assertTrue((artifact_root / "cargo-target").is_dir())
        self.assertNotIn("token", artifact.read_text(encoding="utf-8").lower())

        failed = subprocess.run(
            [str(JOURNEY), "--repo-dir", str(ROOT), "--head", "0" * 40,
             "--flow", "sidebar-all-tab-retention", "--artifact-dir", str(artifact_root)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(failed.returncode, 66)
        self.assertIn("not the requested exact head", failed.stderr)


if __name__ == "__main__":
    unittest.main()
