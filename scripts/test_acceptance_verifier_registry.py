import json
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
REGISTRY_PATH = REPO_ROOT / ".github" / "acceptance-verifiers.json"
CI_WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "ci.yml"


class AcceptanceVerifierRegistryTests(unittest.TestCase):
    def test_registry_allows_the_trusted_ubuntu_herdr_check(self) -> None:
        registry = json.loads(REGISTRY_PATH.read_text(encoding="utf-8"))

        self.assertEqual(registry["schema"], "acceptance-verifiers/v1")
        self.assertEqual(
            registry["verifiers"],
            [{
                "id": "herdr-check-ubuntu",
                "check_name": "check (ubuntu-latest)",
            }],
        )

        workflow = CI_WORKFLOW_PATH.read_text(encoding="utf-8")
        self.assertIn("name: check (${{ matrix.os }})", workflow)
        self.assertIn("- os: ubuntu-latest", workflow)
        self.assertIn("run: just ci '${{ matrix.nextest_filter }}'", workflow)


if __name__ == "__main__":
    unittest.main()
