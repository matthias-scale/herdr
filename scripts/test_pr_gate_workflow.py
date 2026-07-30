import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "pr-gate.yml"


class PullRequestGateWorkflowTests(unittest.TestCase):
    def test_forks_pass_job_without_maintainer_token_resolution(self) -> None:
        workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

        job_start = workflow.index("  check-contributor:\n")
        runs_on = workflow.index("    runs-on:", job_start)
        self.assertNotIn(
            "    if: ${{ github.repository == 'ogulcancelik/herdr' }}\n",
            workflow[job_start:runs_on],
        )

        no_op_step = workflow.index(
            "      - name: Confirm canonical intake policy is not applicable",
            runs_on,
        )
        no_op_condition = workflow.index(
            "        if: ${{ github.repository != 'ogulcancelik/herdr' }}\n",
            no_op_step,
        )
        policy_step = workflow.index(
            "      - name: Check pull request intake policy",
            no_op_condition,
        )
        policy_condition = workflow.index(
            "        if: ${{ github.repository == 'ogulcancelik/herdr' }}\n",
            policy_step,
        )
        token = workflow.index(
            "github-token: ${{ secrets.KANGAL_GITHUB_TOKEN }}",
            policy_condition,
        )

        self.assertLess(no_op_step, no_op_condition)
        self.assertLess(no_op_condition, policy_step)
        self.assertLess(policy_step, policy_condition)
        self.assertLess(policy_condition, token)
        self.assertIn(
            'run: echo "PR Gate is canonical-repository-only; fork check passed."',
            workflow[no_op_condition:policy_step],
        )


if __name__ == "__main__":
    unittest.main()
