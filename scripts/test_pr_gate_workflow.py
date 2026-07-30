import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "pr-gate.yml"


class PullRequestGateWorkflowTests(unittest.TestCase):
    def test_forks_pass_without_resolving_the_maintainer_token(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

        job_start = workflow.index("  check-contributor:\n")
        fork_step = workflow.index(
            "      - name: Confirm canonical intake policy is not applicable\n",
            job_start,
        )
        fork_guard = workflow.index(
            "        if: ${{ github.repository != 'ogulcancelik/herdr' }}\n",
            fork_step,
        )
        canonical_step = workflow.index(
            "      - name: Check pull request intake policy\n",
            fork_guard,
        )
        canonical_guard = workflow.index(
            "        if: ${{ github.repository == 'ogulcancelik/herdr' }}\n",
            canonical_step,
        )
        token = workflow.index(
            "          github-token: ${{ secrets.KANGAL_GITHUB_TOKEN }}\n",
            canonical_guard,
        )

        self.assertLess(fork_step, fork_guard)
        self.assertLess(fork_guard, canonical_step)
        self.assertLess(canonical_step, canonical_guard)
        self.assertLess(canonical_guard, token)


if __name__ == "__main__":
    unittest.main()
