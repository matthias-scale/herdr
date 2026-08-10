import sys
import threading
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

DECLARED_BLOCKING_WITHOUT_ITEMS = """\
**Critical action points (2 blocking)**

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

DECISIONS_BEFORE_EMPTY_CAP = """\
Work summary goes here.

**Auto-proceeded decisions**

1. Proceed with the compact list; recommendation: compact list at 14:25.
2. Kept the existing socket transport.

**Critical action points (0 blocking)**

**Nothing to act on.**

Done here.
"""

STALE_DECISIONS_BEFORE_OPERATIVE_CAP = """\
**Auto-proceeded decisions**

1. Stale earlier decision.

**Critical action points (1 blocking)**

1. **Gate** — Operative gate.

**Auto-proceeded decisions**

1. Live later decision.

Done here.
"""

FENCED_ONLY_CAP = """\
```markdown
**Critical action points (1 blocking)**

1. **Gate** — Example only.
```

Done here.
"""

FENCED_CAP_BETWEEN_DECISIONS_AND_REAL_CAP = """\
**Auto-proceeded decisions**

1. Live pre-CAP decision.

````markdown
**Critical action points (99 blocking)**

1. **Gate** — Example only.
````

**Critical action points (0 blocking)**

Done here.
"""

FENCED_NOTHING_BETWEEN_DECISIONS_AND_REAL_CAP = """\
**Auto-proceeded decisions**

1. Live pre-CAP decision.

~~~markdown
**Nothing to act on.**
~~~

**Critical action points (0 blocking)**

Done here.
"""

UNCLOSED_FENCE = """\
```markdown
**Critical action points (1 blocking)**

1. **Gate** — Incomplete example.
"""

BOM_PREFIXED_CAP = """\
\ufeff**Critical action points (1 blocking)**

1. **Gate** — BOM-prefixed gate.

Done here.
"""

STALE_DECISIONS_BEFORE_FINAL_CAP = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. Proceed with the stale decision.

**Critical action points (0 blocking)**

Done here.
"""

STALE_DECISIONS_AFTER_TERMINATION = """\
**Auto-proceeded decisions**

1. Stale decision.

**Nothing to act on.**

Intervening prose.

**Critical action points (0 blocking)**

Done here.
"""

DECISIONS_AFTER_FINAL_NOTHING = """\
**Critical action points (0 blocking)**

**Nothing to act on.**

**Auto-proceeded decisions**

1. Stale decision after termination.
"""

ADVERSARIAL_MULTIPLE_DECISIONS_ONE_CAP = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. First decision in this block.

**Auto-proceeded decisions**

2. Second decision in this block.

Done here.
"""

ADVERSARIAL_MULTIPLE_DECISIONS_BEFORE_EMPTY_CAP = """\
Work summary goes here.

**Auto-proceeded decisions**

1. First pre-CAP decision.

**Auto-proceeded decisions**

2. Second pre-CAP decision.

**Critical action points (0 blocking)**

**Nothing to act on.**

Done here.
"""

ADVERSARIAL_REPEATED_NOTHING = """\
**Critical action points (0 blocking)**

**Nothing to act on.**

**Auto-proceeded decisions**

1. Decision between repeated Nothing markers.

**Nothing to act on.**
"""

ADVERSARIAL_DECISION_BETWEEN_CAPS = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. Decision belonging to the earlier CAP.

**Critical action points (0 blocking)**

Done here.
"""

ADVERSARIAL_INDENTED_CAP_IN_DECISION = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. The explanation quotes this example:
   **Critical action points (99 blocking)**
   and the quoted marker is content.

Done here.
"""

ADVERSARIAL_INDENTED_NOTHING_IN_DECISION = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. The explanation quotes this example:
   **Nothing to act on.**
   and the decision continues after it.

Done here.
"""

ADVERSARIAL_REPEATED_CAPS_FINAL_DECISIONS = """\
**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. Stale decision from the earlier CAP.

**Critical action points (0 blocking)**

**Auto-proceeded decisions**

1. Decision from the final CAP.

Done here.
"""

ADVERSARIAL_CRLF = (
    "**Critical action points (0 blocking)**\r\n"
    "\r\n"
    "**Auto-proceeded decisions**\r\n"
    "\r\n"
    "1. CRLF decision; recommendation: retain at 10:00.\r\n"
    "\r\n"
    "Done here.\r\n"
)

ADVERSARIAL_DECISIONS_WITHOUT_BLOCK = """\
**Auto-proceeded decisions**

1. Orphan decision without a closing-block marker.

Done here.
"""

ADVERSARIAL_STALE_BEFORE_INTERVENING_NOTHING = """\
**Auto-proceeded decisions**

1. Stale decision before the intervening Nothing marker.

**Nothing to act on.**

Intervening prose.

**Critical action points (0 blocking)**

Done here.
"""

ADVERSARIAL_DECISION_AFTER_FINAL_NOTHING = """\
**Critical action points (0 blocking)**

**Nothing to act on.**

**Auto-proceeded decisions**

1. Decision after the final Nothing marker.
"""

DECLARED_COUNT_WITH_ONLY_NONBLOCKING_ITEMS = {
    count: f"""**Critical action points ({count} blocking)**

1. **Answer** — A nonblocking answer.
2. **Verify** — A nonblocking verification.

Done here.
"""
    for count in (0, 1, 2, 99)
}


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

    def test_declared_blocking_without_items_does_not_create_phantom_gates(self):
        block = closing_block.parse(DECLARED_BLOCKING_WITHOUT_ITEMS)

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.wire_gates(), [])

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

    def test_decisions_before_empty_cap_block_are_parsed(self):
        block = closing_block.parse(DECISIONS_BEFORE_EMPTY_CAP)

        decisions = block.wire_decisions()
        self.assertEqual(len(decisions), 2)
        self.assertEqual(decisions[0]["recommendation"], "compact list")
        self.assertIn("socket transport", decisions[1]["text"])
        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.wire_gates(), [])

    def test_stale_pre_cap_decisions_are_superseded_by_operative_cap(self):
        block = closing_block.parse(STALE_DECISIONS_BEFORE_OPERATIVE_CAP)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            ["Live later decision."],
        )

    def test_fenced_cap_is_not_a_live_gate(self):
        block = closing_block.parse(FENCED_ONLY_CAP)

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.wire_gates(), [])

    def test_fenced_cap_does_not_drop_pre_cap_decisions(self):
        block = closing_block.parse(FENCED_CAP_BETWEEN_DECISIONS_AND_REAL_CAP)

        decisions = block.wire_decisions()
        self.assertEqual(len(decisions), 1)
        self.assertIn("Live pre-CAP decision.", decisions[0]["text"])

    def test_fenced_nothing_does_not_drop_pre_cap_decisions(self):
        block = closing_block.parse(FENCED_NOTHING_BETWEEN_DECISIONS_AND_REAL_CAP)

        decisions = block.wire_decisions()
        self.assertEqual(len(decisions), 1)
        self.assertIn("Live pre-CAP decision.", decisions[0]["text"])

    def test_unclosed_fence_suppresses_tail_markers(self):
        block = closing_block.parse(UNCLOSED_FENCE)

        self.assertFalse(block.present)
        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.wire_gates(), [])

    def test_bom_prefixed_cap_is_recognized(self):
        block = closing_block.parse(BOM_PREFIXED_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.wire_gates()[0]["text"], "BOM-prefixed gate.")

    def test_decisions_from_earlier_cap_block_are_not_attached(self):
        block = closing_block.parse(STALE_DECISIONS_BEFORE_FINAL_CAP)

        self.assertEqual(block.wire_decisions(), [])

    def test_decisions_after_nothing_are_not_attached_to_final_cap(self):
        block = closing_block.parse(STALE_DECISIONS_AFTER_TERMINATION)

        self.assertEqual(block.wire_decisions(), [])

    def test_decisions_after_final_nothing_are_not_parsed_as_payload(self):
        block = closing_block.parse(DECISIONS_AFTER_FINAL_NOTHING)

        self.assertEqual(block.wire_decisions(), [])

    def test_adversarial_valid_decisions_before_empty_cap(self):
        block = closing_block.parse(DECISIONS_BEFORE_EMPTY_CAP)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            [
                "Proceed with the compact list; recommendation: compact list at 14:25.",
                "Kept the existing socket transport.",
            ],
        )

    def test_adversarial_stale_decisions_before_intervening_nothing(self):
        block = closing_block.parse(ADVERSARIAL_STALE_BEFORE_INTERVENING_NOTHING)

        self.assertEqual(block.wire_decisions(), [])

    def test_adversarial_decision_heading_after_final_nothing(self):
        block = closing_block.parse(ADVERSARIAL_DECISION_AFTER_FINAL_NOTHING)

        self.assertEqual(block.wire_decisions(), [])

    def test_adversarial_repeated_nothing_markers_keep_the_final_section(self):
        block = closing_block.parse(ADVERSARIAL_REPEATED_NOTHING)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            ["Decision between repeated Nothing markers."],
        )

    def test_adversarial_decision_between_caps_belongs_to_earlier_block(self):
        block = closing_block.parse(ADVERSARIAL_DECISION_BETWEEN_CAPS)

        self.assertEqual(block.wire_decisions(), [])

    def test_adversarial_multiple_decision_sections_in_one_cap(self):
        block = closing_block.parse(ADVERSARIAL_MULTIPLE_DECISIONS_ONE_CAP)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            [
                "First decision in this block.",
                "Second decision in this block.",
            ],
        )

    def test_adversarial_multiple_decision_sections_before_empty_cap(self):
        block = closing_block.parse(ADVERSARIAL_MULTIPLE_DECISIONS_BEFORE_EMPTY_CAP)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            ["First pre-CAP decision.", "Second pre-CAP decision."],
        )

    def test_adversarial_indented_cap_header_is_decision_content(self):
        block = closing_block.parse(ADVERSARIAL_INDENTED_CAP_IN_DECISION)

        self.assertEqual(block.blocking, 0)
        self.assertEqual(len(block.wire_decisions()), 1)
        self.assertIn(
            "**Critical action points (99 blocking)**",
            block.wire_decisions()[0]["text"],
        )
        self.assertIn("and the quoted marker is content.", block.wire_decisions()[0]["text"])

    def test_adversarial_indented_nothing_marker_is_decision_content(self):
        block = closing_block.parse(ADVERSARIAL_INDENTED_NOTHING_IN_DECISION)

        self.assertEqual(len(block.wire_decisions()), 1)
        self.assertIn("**Nothing to act on.**", block.wire_decisions()[0]["text"])
        self.assertIn("and the decision continues after it.", block.wire_decisions()[0]["text"])

    def test_adversarial_repeated_caps_keep_final_block_decisions(self):
        block = closing_block.parse(ADVERSARIAL_REPEATED_CAPS_FINAL_DECISIONS)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            ["Decision from the final CAP."],
        )

    def test_adversarial_crlf_closing_block(self):
        block = closing_block.parse(ADVERSARIAL_CRLF)

        self.assertEqual(
            [decision["text"] for decision in block.wire_decisions()],
            ["CRLF decision; recommendation: retain at 10:00."],
        )

    def test_adversarial_decisions_without_cap_or_nothing_are_ignored(self):
        block = closing_block.parse(ADVERSARIAL_DECISIONS_WITHOUT_BLOCK)

        self.assertEqual(block.wire_decisions(), [])

    def test_adversarial_declared_counts_do_not_create_gates(self):
        for count, text in DECLARED_COUNT_WITH_ONLY_NONBLOCKING_ITEMS.items():
            with self.subTest(count=count):
                block = closing_block.parse(text)

                self.assertEqual(block.blocking, 0)
                self.assertEqual(block.wire_gates(), [])
                self.assertEqual(
                    [item["label"] for item in block.wire_items()],
                    ["Answer", "Verify"],
                )

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

    def test_concurrent_mirror_writes_keep_newest_seq(self):
        stale_inside_replace = threading.Event()
        release_stale = threading.Event()
        original_replace = herdr_status.os.replace

        def ordered_replace(source, destination):
            with open(source, encoding="utf-8") as fh:
                seq = herdr_status.json.load(fh)["seq"]
            if seq == 100:
                stale_inside_replace.set()
                self.assertTrue(release_stale.wait(2))
            original_replace(source, destination)

        with mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ), mock.patch.object(herdr_status.os, "replace", side_effect=ordered_replace):
            stale = threading.Thread(
                target=herdr_status.write_mirror,
                args=("w9:p10", {"v": 2, "seq": 100}),
            )
            newer = threading.Thread(
                target=herdr_status.write_mirror,
                args=("w9:p10", {"v": 2, "seq": 200}),
            )
            stale.start()
            self.assertTrue(stale_inside_replace.wait(2))
            newer.start()
            release_stale.set()
            stale.join(2)
            newer.join(2)
            self.assertFalse(stale.is_alive())
            self.assertFalse(newer.is_alive())

            with open(herdr_status.mirror_path("w9:p10"), encoding="utf-8") as fh:
                self.assertEqual(herdr_status.json.load(fh)["seq"], 200)

    def test_mirror_write_replaces_non_dict_json(self):
        with mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            path = herdr_status.mirror_path("w9:p11")
            with open(path, "w", encoding="utf-8") as fh:
                herdr_status.json.dump([1, 2], fh)

            self.assertEqual(
                herdr_status.write_mirror("w9:p11", {"v": 2, "seq": 100}),
                path,
            )
            with open(path, encoding="utf-8") as fh:
                self.assertEqual(herdr_status.json.load(fh)["seq"], 100)

    def test_v1_payload_is_skipped_without_error(self):
        for payload in (None, [], "malformed", {"v": 1}, {"v": 3}):
            with self.subTest(payload=payload):
                self.assertFalse(herdr_status.accepts_payload(payload))
        self.assertTrue(herdr_status.accepts_payload({"v": 2}))

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
