import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "pr-gate.yml"


class PullRequestGateWorkflowTests(unittest.TestCase):
    def test_noncanonical_repository_passes_without_resolving_token(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

        job_start = workflow.index("  check-contributor:\n")
        fork_step = workflow.index(
            "      - name: Confirm canonical intake policy is not applicable to this repository\n",
            job_start,
        )
        canonical_step = workflow.index(
            "      - name: Check pull request intake policy\n",
            fork_step,
        )
        fork_block = workflow[fork_step:canonical_step]
        canonical_block = workflow[canonical_step:]

        self.assertIn(
            "        if: ${{ github.repository != 'ogulcancelik/herdr' }}\n",
            fork_block,
        )
        self.assertNotIn("KANGAL_GITHUB_TOKEN", fork_block)
        self.assertIn(
            "        if: ${{ github.repository == 'ogulcancelik/herdr' }}\n",
            canonical_block,
        )
        self.assertIn(
            "          github-token: ${{ secrets.KANGAL_GITHUB_TOKEN }}\n",
            canonical_block,
        )
        self.assertEqual(workflow.count("KANGAL_GITHUB_TOKEN"), 1)


if __name__ == "__main__":
    unittest.main()
