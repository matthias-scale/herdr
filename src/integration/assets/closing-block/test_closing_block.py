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

ZERO_COUNT_GATE = """\
**Critical action points (0 blocking)**

1. **Gate** — Approve the zero-count correction.

Done here.
"""

MULTI_WHAT = """\
**Critical action points (2 blocking)**

1. **Gate** — First gate.
2. **Gate** — Second gate.

**What to test — Gate 1 · #1 (MAT-1)**

https://example.test/path_(a)
1. Verify the first gate.

**What to test — Gate 2 · #2 (MAT-2)**

1. Verify the second gate.

**Auto-proceeded decisions**

1. Proceed with the correction.
   1. Nested detail remains part of the decision.
"""


DECISIONS_BEFORE_CAP = """\
Work summary goes here.

**Auto-proceeded decisions**

1. Proceed with the compact list; recommendation: compact list at 14:25.
2. Kept the existing socket transport.

**Critical action points (1 blocking)**

1. **Gate** — Approve PR #2606 for MAT-125 before production rollout.

Done here.
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

    def test_zero_declared_count_does_not_hide_gate_item(self):
        block = closing_block.parse(ZERO_COUNT_GATE)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.wire_gates()[0]["text"], "Approve the zero-count correction.")

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

    def test_decisions_before_cap_block_are_parsed(self):
        block = closing_block.parse(DECISIONS_BEFORE_CAP)

        decisions = block.wire_decisions()
        self.assertEqual(len(decisions), 2)
        self.assertEqual(decisions[0]["recommendation"], "compact list")
        self.assertIn("socket transport", decisions[1]["text"])
        self.assertNotIn("Critical action points", decisions[1]["text"])
        self.assertEqual(block.blocking, 1)
        self.assertEqual(len(block.wire_gates()), 1)

    def test_mirror_write_keeps_newest_seq(self):
        with mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            newer = {"v": 2, "seq": 200, "gates": [{"text": "newer"}]}
            stale = {"v": 2, "seq": 100, "gates": [{"text": "stale"}]}
            path = herdr_status.write_mirror("w9:p9", newer)
            self.assertIsNotNone(path)
            self.assertIsNone(herdr_status.write_mirror("w9:p9", stale))
            self.assertIsNone(herdr_status.write_mirror("w9:p9", dict(newer)))
            with open(path, encoding="utf-8") as fh:
                kept = herdr_status.json.load(fh)
            self.assertEqual(kept["seq"], 200)
            self.assertEqual(kept["gates"][0]["text"], "newer")
            fresher = {"v": 2, "seq": 300, "gates": [{"text": "fresher"}]}
            self.assertIsNotNone(herdr_status.write_mirror("w9:p9", fresher))

    def test_v1_payload_is_skipped_without_error(self):
        self.assertFalse(herdr_status.accepts_payload({"v": 1}))
        self.assertTrue(herdr_status.accepts_payload({"v": 2}))
        self.assertFalse(herdr_status.accepts_payload({"v": 3}))

    def test_what_to_test_urls_and_nested_decisions_are_not_collapsed(self):
        block = closing_block.parse(MULTI_WHAT)

        items = block.wire_items()
        self.assertEqual(len(items), 2)
        self.assertEqual(items[0]["url"], "https://example.test/path_(a)")
        self.assertIn("Verify the second gate.", items[1]["text"])
        decisions = block.wire_decisions()
        self.assertEqual(len(decisions), 1)
        self.assertIn("Nested detail remains part of the decision.", decisions[0]["text"])

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
        self.assertEqual(report[3]["v"], 2)
        self.assertEqual(report[3]["gates"], outcome["payload"]["gates"])
        self.assertEqual(report[3]["items"], outcome["payload"]["items"])
        self.assertEqual(report[3]["decisions"], outcome["payload"]["decisions"])
        metadata = rpc.call_args_list[-1].args
        self.assertEqual(metadata[1], "herdr:claude-closing-block")
        self.assertEqual(metadata[2], "pane.report_metadata")
        params = metadata[3]
        self.assertIn("Approve PR #2606", params["state_labels"]["blocked"])

    def test_emit_forces_legacy_default_fields_to_null(self):
        outcome = herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[
                {
                    "text": "Gate text",
                    "default": "approve",
                    "default_at": "2026-08-09T12:00:00Z",
                }
            ],
        )

        gate = outcome["payload"]["gates"][0]
        self.assertIsNone(gate["default"])
        self.assertIsNone(gate["default_at"])

    @staticmethod
    def _state_dir():
        import tempfile

        return tempfile.mkdtemp(prefix="herdr-closing-block-test-")


if __name__ == "__main__":
    unittest.main()
