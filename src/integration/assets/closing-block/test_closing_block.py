import sys
import unittest
from unittest import mock

sys.path.insert(0, __import__("os").path.dirname(__file__))

import closing_block
import herdr_status


REALISTIC_CAP = """\
**Critical action points (1 blocking)**

1. **Gate** — Approve PR #2606 for MAT-125 before production rollout.
2. **Answer** — Keep the blocked list compact.

**What to test — Gate 1 · #2606 (MAT-125)**

https://mat125-gates-v2.vercel.app/preview
1. Open the branch preview.
2. Confirm the gate text is visible.

**Auto-proceeded decisions**

1. Proceed with the compact list; recommendation: compact list at 14:25.

1 agent running: reviewer — v2 payload.
"""


class ClosingBlockV2Tests(unittest.TestCase):
    def test_cap_gate_becomes_nonempty_object_gate(self):
        block = closing_block.parse(REALISTIC_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(len(block.wire_gates()), 1)
        self.assertEqual(
            block.wire_gates()[0],
            {
                "n": 1,
                "label": "Gate",
                "text": "Approve PR #2606 for MAT-125 before production rollout.",
                "pr": 2606,
                "ticket": "MAT-125",
                "url": None,
                "default": None,
                "default_at": None,
            },
        )

    def test_answer_and_what_to_test_are_nonblocking_items(self):
        block = closing_block.parse(REALISTIC_CAP)
        items = block.wire_items()

        self.assertEqual([item["label"] for item in items], ["Answer", "What to test"])
        self.assertEqual(items[1]["pr"], 2606)
        self.assertEqual(items[1]["ticket"], "MAT-125")
        self.assertEqual(
            items[1]["url"],
            "https://mat125-gates-v2.vercel.app/preview",
        )
        self.assertIn("Confirm the gate text is visible.", items[1]["text"])

    def test_decisions_are_separate_and_reversible(self):
        decisions = closing_block.parse(REALISTIC_CAP).wire_decisions()

        self.assertEqual(len(decisions), 1)
        self.assertEqual(decisions[0]["text"], "Proceed with the compact list; recommendation: compact list at 14:25.")
        self.assertEqual(decisions[0]["recommendation"], "compact list")
        self.assertEqual(decisions[0]["decided_at"], "14:25")
        self.assertTrue(decisions[0]["reversible"])

    def test_v1_payload_is_skipped_without_error(self):
        self.assertFalse(herdr_status.accepts_payload({"v": 1}))
        self.assertTrue(herdr_status.accepts_payload({"v": 2}))
        self.assertFalse(herdr_status.accepts_payload({"v": 3}))

    def test_report_emits_v2_arrays_and_existing_blocked_channel(self):
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            outcome = herdr_status.report(
                agent="claude",
                blocking=1,
                agents=1,
                gates=closing_block.parse(REALISTIC_CAP).wire_gates(),
                items=closing_block.parse(REALISTIC_CAP).wire_items(),
                decisions=closing_block.parse(REALISTIC_CAP).wire_decisions(),
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertEqual(outcome["payload"]["v"], 2)
        self.assertIsInstance(outcome["payload"]["gates"][0], dict)
        self.assertEqual(len(outcome["payload"]["items"]), 2)
        self.assertTrue(outcome["payload"]["decisions"][0]["reversible"])
        report = rpc.call_args_list[1].args
        self.assertEqual(report[2], "pane.report_agent")
        self.assertEqual(report[3]["gates"], outcome["payload"]["gates"])
        self.assertEqual(report[3]["items"], outcome["payload"]["items"])
        self.assertEqual(report[3]["decisions"], outcome["payload"]["decisions"])
        metadata = rpc.call_args_list[-1].args
        self.assertEqual(metadata[1], "herdr:claude-closing-block")
        self.assertEqual(metadata[2], "pane.report_metadata")
        params = metadata[3]
        self.assertIn("Approve PR #2606", params["state_labels"]["blocked"])

    @staticmethod
    def _state_dir():
        import tempfile

        return tempfile.mkdtemp(prefix="herdr-closing-block-test-")


if __name__ == "__main__":
    unittest.main()
