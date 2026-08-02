import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "pr-gate.yml"


class PullRequestGateWorkflowTests(unittest.TestCase):
    def test_forks_skip_job_before_maintainer_token_resolution(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

        job_start = workflow.index("  check-contributor:\n")
        guard = workflow.index(
            "    if: ${{ github.repository == 'ogulcancelik/herdr' }}\n",
            job_start,
        )
        runs_on = workflow.index("    runs-on:", job_start)
        steps = workflow.index("    steps:", job_start)

        self.assertLess(job_start, guard)
        self.assertLess(guard, runs_on)
        self.assertLess(runs_on, steps)
        self.assertIn(
            "github-token: ${{ secrets.KANGAL_GITHUB_TOKEN }}",
            workflow,
        )


if __name__ == "__main__":
    unittest.main()
