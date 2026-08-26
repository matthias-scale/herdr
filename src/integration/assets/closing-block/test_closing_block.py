import contextlib
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

    def test_declared_blocking_without_items_still_blocks(self):
        block = closing_block.parse(DECLARED_BLOCKING_WITHOUT_ITEMS)

        self.assertEqual(block.blocking, 2)
        self.assertEqual(block.herdr_state, "blocked")
        # No parseable items means no gate detail, only the declared count.
        self.assertEqual(block.wire_gates(), [])
        self.assertEqual(block.message(), "2 blocking")

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

    def test_labeled_items_outrank_the_declared_count_and_keep_their_labels(self):
        # Reverses the previous "declared count floors blocking" contract.
        # Answer and Verify are non-blocking by definition, so a header that
        # over-declares above them is a miscounted header, not a hidden gate.
        # Latching it parked panes as blocked whose agent was free to proceed,
        # which is the whole point of the non-blocking labels.
        for count, text in DECLARED_COUNT_WITH_ONLY_NONBLOCKING_ITEMS.items():
            with self.subTest(count=count):
                block = closing_block.parse(text)

                self.assertEqual(block.blocking, 0)
                self.assertEqual(block.wire_gates(), [])
                self.assertEqual(
                    [item["label"] for item in block.wire_items()],
                    ["Answer", "Verify"],
                )

    def test_declared_count_still_floors_blocking_with_nothing_labeled(self):
        # The header remains the only evidence when no label parsed at all, so
        # it keeps flooring the count there: under-reporting a gate is the
        # failure mode that guard exists for.
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n"
            "\n"
            "1. Approve the production rollout.\n"
            "2. Approve the secret rotation.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 2)
        self.assertEqual(
            [item["label"] for item in block.wire_gates()], ["Gate", "Gate"]
        )

    def test_an_unlabeled_line_beside_a_label_is_prose_not_a_silent_gate(self):
        # Mixed block: the author did label their gate, so the trailing
        # unlabeled line must not be promoted to make the header's count.
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n"
            "\n"
            "1. **Answer** — Post the summary after CI settles?\n"
            "2. Some trailing prose that is not an item.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.wire_gates(), [])

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
        # The state label names the state; the gate body stays in the token.
        self.assertEqual(params["state_labels"]["blocked"], "gate")
        self.assertNotIn("Approve PR #2606", params["state_labels"]["blocked"])
        self.assertIn("Approve PR #2606", params["tokens"]["closing_gates"])

    def test_emit_forces_legacy_default_fields_to_null(self):
        with self._isolated():
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
                pane_id="w9:p12",
                sock_path="/tmp/herdr-test.sock",
            )

        gate = outcome["payload"]["gates"][0]
        self.assertIsNone(gate["default"])
        self.assertIsNone(gate["default_at"])

    def test_report_emits_reported_at_and_declared_wait_for_working_state(self):
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            outcome = herdr_status.report(
                agent="claude",
                blocking=0,
                agents=1,
                wait="CI run 4123",
                eta_s=720,
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )

        payload = outcome["payload"]
        self.assertRegex(payload["reported_at"], r"^20\d{2}-\d{2}-\d{2}T.*Z$")
        self.assertEqual(payload["wait"], "CI run 4123")
        self.assertEqual(payload["eta_s"], 720)
        report_params = rpc.call_args_list[1].args[3]
        self.assertEqual(report_params["reported_at"], payload["reported_at"])
        self.assertEqual(report_params["wait"], "CI run 4123")
        self.assertEqual(report_params["eta_s"], 720)

    def test_declared_wait_is_omitted_when_state_is_not_working(self):
        with self._isolated():
            outcome = herdr_status.report(
                agent="claude",
                blocking=1,
                agents=0,
                wait="human approval",
                eta_s=720,
                pane_id="w9:p14",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertIn("reported_at", outcome["payload"])
        self.assertNotIn("wait", outcome["payload"])
        self.assertNotIn("eta_s", outcome["payload"])

    def test_blocked_state_label_never_carries_gate_text(self):
        self.assertEqual(herdr_status.blocked_state_label(0), "blocked")
        self.assertEqual(herdr_status.blocked_state_label(1), "gate")
        self.assertEqual(herdr_status.blocked_state_label(2), "2 gates")
        self.assertEqual(herdr_status.blocked_state_label(9), "9 gates")
        # A negative count is nonsense but must not render as a gate.
        self.assertEqual(herdr_status.blocked_state_label(-1), "blocked")

    def test_blocked_state_label_is_independent_of_gate_bodies(self):
        long_gate = [{"text": "x" * 300, "label": "Gate", "n": 1}]
        with self._isolated():
            outcome = herdr_status.report(
                agent="claude",
                blocking=1,
                agents=0,
                gates=long_gate,
                pane_id="w9:p13",
                sock_path="/tmp/herdr-test.sock",
            )
        self.assertNotIn("x" * 20, outcome["payload"].get("message", ""))

    def test_report_never_falls_back_to_the_ambient_pane(self):
        # `report()` defaults pane and socket from HERDR_PANE_ID /
        # HERDR_SOCKET_PATH, so a test that omits them talks to whatever server
        # owns the shell running the suite. That is how this suite once posted
        # the fixture gate "Gate text" onto a live daily-driver pane. Every
        # `report()` call in this file must name both, and this test fails if
        # the ambient values would have been reachable.
        import inspect

        source = inspect.getsource(type(self))
        calls = source.count("herdr_status.report(")
        self.assertEqual(calls, source.count('sock_path="/tmp/herdr-test.sock"'))
        self.assertEqual(calls, source.count('pane_id="w'))

    @contextlib.contextmanager
    def _isolated(self):
        """No ambient pane, no ambient socket, no live state directory."""
        with mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            herdr_status.os.environ.pop("HERDR_PANE_ID", None)
            herdr_status.os.environ.pop("HERDR_SOCKET_PATH", None)
            yield

    @staticmethod
    def _state_dir():
        import tempfile

        return tempfile.mkdtemp(prefix="herdr-closing-block-test-")


PLAIN_FORM_CAP = """\
Critical action points (1 blocking)

1. n8n production apply — still held per your instruction
  - (a-rec) Green-light after both PRs merge, so the apply runs through the new --base guard rather than around it
  - (b) Apply now under the current unguarded script
  - (c) You apply by hand in the n8n UI
"""

BOLD_FORM_CAP = """\
**Critical action points (1 blocking)**

1. **Gate** — n8n production apply — still held per your instruction
  - (a-rec) Green-light after both PRs merge, so the apply runs through the new --base guard rather than around it
  - (b) Apply now under the current unguarded script
  - (c) You apply by hand in the n8n UI
"""


SUFFIXED_PLAIN_FORM_CAP = """\
Reinstatement details prepared.

Critical action points (1 blocking) — unchanged, still waiting on your answer:

1. noopnutrition: execute the reinstatement now and then create the Missive draft?
  - (a-rec) yes — reinstate now, then I create the draft with name/address resolved
  - (b) hold both — send the offer-phrased email first, reinstate only on his reply
  - (c) reinstate now, draft stays un-created until you say so

Waiting on you — item 1.
"""

UNLABELED_BOLD_CAP_WITH_TRAILER = """\
I'll bring you the peer's report the moment it lands, then kill it.

**Critical action points (1 blocking)**

1. `proposals/copy-approval-linear-lane.md` in PR #921
   - (a-rec) Keep it in #921 — docs-only, inert, zero risk
   - (b) Split it out into its own PR
   - (c) Drop it entirely

Waiting on you — item 1.
"""

UNLABELED_CAP_BEFORE_AGENTS_LINE = """\
**Critical action points (1 blocking)**

1. PR #921 — ship-critical-skill exception class, your approval only:
   - (a-rec) approve https://github.com/scalable-so/scalable-agent-fleet/pull/921
   - (b) request changes — name them and I'll route a repair
   - (c) hold until the Linear approval-lane slices are also implemented

3 agents running: intake-reuse-fix — REJECT_DUPLICATE fix; fleet-vendor-sync — vendored fleet bump; symphony-skill-intake — SKILL.md intake docs.
"""


class PlainFormTests(unittest.TestCase):
    """The observed unformatted authoring must latch exactly like the strict
    form -- these fixtures pin the two shapes against drifting apart."""

    def test_plain_form_latches_a_gate(self):
        block = closing_block.parse(PLAIN_FORM_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        gates = block.wire_gates()
        self.assertEqual(len(gates), 1)
        self.assertIn("n8n production apply", gates[0]["text"])
        self.assertIn("(a-rec)", gates[0]["text"])

    def test_plain_and_bold_forms_do_not_drift(self):
        plain = closing_block.parse(PLAIN_FORM_CAP)
        bold = closing_block.parse(BOLD_FORM_CAP)

        self.assertEqual(plain.blocking, bold.blocking)
        self.assertEqual(plain.herdr_state, bold.herdr_state)
        self.assertEqual(
            [gate["text"] for gate in plain.wire_gates()],
            [gate["text"] for gate in bold.wire_gates()],
        )

    def test_header_line_alone_is_enough_to_block(self):
        for header in (
            "Critical action points (3 blocking)",
            "## Critical action points (3 blocking)",
            "critical action points (3 blocking):",
            "**Critical action points (3 blocking)**",
        ):
            with self.subTest(header=header):
                block = closing_block.parse(header + "\n")
                self.assertEqual(block.blocking, 3)
                self.assertEqual(block.herdr_state, "blocked")

    def test_counted_header_with_trailing_suffix_latches(self):
        block = closing_block.parse(SUFFIXED_PLAIN_FORM_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        gates = block.wire_gates()
        self.assertEqual(len(gates), 1)
        self.assertIn("noopnutrition", gates[0]["text"])
        self.assertIn("(a-rec)", gates[0]["text"])

    def test_counted_header_suffix_variants_latch(self):
        for header in (
            "Critical action points (2 blocking) — unchanged, still waiting on your answer:",
            "Critical action points (2 blocking): still waiting on item 1",
            "**Critical action points (2 blocking)** — carried from last turn",
            "## Critical action points (2 blocking) - both from the review",
        ):
            with self.subTest(header=header):
                block = closing_block.parse(header + "\n")
                self.assertEqual(block.blocking, 2)
                self.assertEqual(block.herdr_state, "blocked")

    def test_countless_header_stays_full_line_only(self):
        # Without an explicit count the anchor keeps its strict form so prose
        # mentions ("the Critical action points above were resolved") never
        # latch a phantom block.
        block = closing_block.parse(
            "Critical action points were all addressed earlier.\n\nDone here.\n"
        )
        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.herdr_state, "idle")

    def test_unlabeled_bold_cap_with_waiting_trailer_latches(self):
        block = closing_block.parse(UNLABELED_BOLD_CAP_WITH_TRAILER)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        gates = block.wire_gates()
        self.assertEqual(len(gates), 1)
        self.assertIn("copy-approval-linear-lane", gates[0]["text"])
        self.assertEqual(gates[0]["pr"], 921)

    def test_unlabeled_cap_before_agents_line_stays_blocked(self):
        block = closing_block.parse(UNLABELED_CAP_BEFORE_AGENTS_LINE)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.agents_running, 3)
        gates = block.wire_gates()
        self.assertEqual(len(gates), 1)
        self.assertIn("ship-critical-skill", gates[0]["text"])
        self.assertNotIn("agents running", gates[0]["text"])

    def test_bare_header_without_count_stays_zero(self):
        block = closing_block.parse("Critical action points\n\nDone here.\n")

        self.assertTrue(block.present)
        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.herdr_state, "idle")

    def test_plain_labels_parse_and_word_prefixes_do_not(self):
        block = closing_block.parse(
            "Critical action points (1 blocking)\n\n"
            "1. Gate — approve the rollout\n"
            "2. Verify — check the deploy log\n"
            "3. Gate-keeping doc update shipped\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.wire_gates()[0]["text"], "approve the rollout")
        labels = [item["label"] for item in block.wire_items()]
        self.assertEqual(labels, ["Verify"])

    def test_promotion_stops_at_the_declared_count(self):
        block = closing_block.parse(
            "Critical action points (1 blocking)\n\n"
            "1. first unlabeled item\n"
            "2. second unlabeled item\n"
            "3. third unlabeled item\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        gates = block.wire_gates()
        self.assertEqual(len(gates), 1)
        self.assertEqual(gates[0]["text"], "first unlabeled item")
        self.assertEqual(block.wire_items(), [])


class StopHookTranscriptTests(unittest.TestCase):
    @staticmethod
    def _hook_module():
        import importlib.util
        import os

        path = os.path.join(os.path.dirname(__file__), "herdr-closing-block.py")
        spec = importlib.util.spec_from_file_location("herdr_closing_block_hook", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def _write_transcript(self, lines):
        import os
        import tempfile

        fd, path = tempfile.mkstemp(prefix="herdr-transcript-", suffix=".jsonl")
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
        self.addCleanup(os.unlink, path)
        return path

    _ASSISTANT_ROW = (
        '{"type": "assistant", "message": {"content": '
        '[{"type": "text", "text": "Done here."}]}}'
    )

    def test_torn_trailing_line_does_not_discard_transcript(self):
        hook = self._hook_module()
        path = self._write_transcript(
            [
                '{"type": "user"}',
                self._ASSISTANT_ROW,
                '{"type": "system", "subtype": "stop_hook_su',
            ]
        )
        self.assertEqual(hook.last_assistant_text(path), "Done here.")

    def test_torn_line_mid_file_is_skipped(self):
        hook = self._hook_module()
        path = self._write_transcript(['{"broken', '{"type": "user"}', self._ASSISTANT_ROW])
        self.assertEqual(hook.last_assistant_text(path), "Done here.")

    def test_waits_for_the_assistant_row_to_flush(self):
        hook = self._hook_module()
        path = self._write_transcript(['{"type": "user"}'])

        def append_reply():
            import time

            time.sleep(0.3)
            with open(path, "a", encoding="utf-8") as fh:
                fh.write(self._ASSISTANT_ROW + "\n")

        writer = threading.Thread(target=append_reply)
        writer.start()
        try:
            self.assertEqual(hook.last_assistant_text(path), "Done here.")
        finally:
            writer.join()

    def test_stale_text_is_a_timeout_fallback_not_a_fresh_read(self):
        hook = self._hook_module()
        hook.FLUSH_WAIT_SECONDS = 0.3
        path = self._write_transcript([self._ASSISTANT_ROW, '{"type": "user"}'])
        import time

        started = time.monotonic()
        self.assertEqual(hook.last_assistant_text(path), "Done here.")
        self.assertGreaterEqual(time.monotonic() - started, 0.3)

    def test_missing_file_returns_none(self):
        hook = self._hook_module()
        hook.FLUSH_WAIT_SECONDS = 0.2
        self.assertIsNone(hook.last_assistant_text("/nonexistent/transcript.jsonl"))


class QuestionGateHookTests(unittest.TestCase):
    """`AskUserQuestion` opens a gate mid-turn, where no `Stop` ever fires."""

    @staticmethod
    def _hook_module():
        import importlib.util
        import os

        path = os.path.join(os.path.dirname(__file__), "herdr-question-gate.py")
        spec = importlib.util.spec_from_file_location("herdr_question_gate_hook", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def setUp(self):
        import tempfile

        self.hook = self._hook_module()
        self.pane_id = "w1:p1"
        state_dir = tempfile.mkdtemp(prefix="herdr-question-gate-test-")
        patch = mock.patch.dict(
            herdr_status.os.environ,
            {
                "XDG_STATE_HOME": state_dir,
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": self.pane_id,
                "HERDR_SOCKET_PATH": "/tmp/herdr-question-gate-test.sock",
            },
            clear=False,
        )
        patch.start()
        self.addCleanup(patch.stop)
        rpc = mock.patch.object(herdr_status, "_rpc")
        self.rpc = rpc.start()
        self.addCleanup(rpc.stop)

    # Captured verbatim from a live `AskUserQuestion` PreToolUse payload.
    _PRE = {
        "session_id": "sess-1",
        "transcript_path": "/tmp/sess-1.jsonl",
        "hook_event_name": "PreToolUse",
        "tool_name": "AskUserQuestion",
        "tool_use_id": "toolu_1",
        "tool_input": {
            "questions": [
                {
                    "question": "Which color do you prefer?",
                    "header": "Color",
                    "options": [
                        {"label": "Red", "description": "Warm."},
                        {"label": "Green", "description": "Calm."},
                    ],
                    "multiSelect": False,
                }
            ]
        },
    }
    _POST = {
        "session_id": "sess-1",
        "transcript_path": "/tmp/sess-1.jsonl",
        "hook_event_name": "PostToolUse",
        "tool_name": "AskUserQuestion",
        "tool_use_id": "toolu_1",
        "tool_input": _PRE["tool_input"],
        "tool_response": {"answers": {"Which color do you prefer?": "Red"}},
    }

    def _run(self, payload):
        import io
        import json

        stdin = io.StringIO(json.dumps(payload))
        with mock.patch.object(self.hook.sys, "stdin", stdin):
            self.assertEqual(self.hook.main(), 0)

    def _reports(self):
        return [
            call.args[3]
            for call in self.rpc.call_args_list
            if call.args[2] == "pane.report_agent"
        ]

    def test_opening_the_dialog_reports_blocked_with_the_question(self):
        self._run(self._PRE)

        reports = self._reports()
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]["state"], "blocked")
        self.assertEqual(reports[0]["source"], "herdr:claude-closing-block")
        self.assertIn("Which color do you prefer?", reports[0]["gates"][0]["text"])
        self.assertIn("Red / Green", reports[0]["gates"][0]["text"])
        self.assertIn("Which color do you prefer?", reports[0]["message"])

    def test_the_gate_binds_to_the_reporting_session(self):
        self._run(self._PRE)

        session = [
            call.args[3]
            for call in self.rpc.call_args_list
            if call.args[2] == "pane.report_agent_session"
        ]
        self.assertEqual(session[0]["agent_session_id"], "sess-1")
        self.assertEqual(session[0]["agent_session_path"], "/tmp/sess-1.jsonl")

    def test_answering_returns_the_pane_to_working_not_idle(self):
        self._run(self._PRE)
        self._run(self._POST)

        reports = self._reports()
        self.assertEqual(len(reports), 2)
        # Zero counts alone would read as `idle` and publish a finished turn.
        self.assertEqual(reports[1]["state"], "working")
        self.assertEqual(reports[1]["gates"], [])
        self.assertGreater(reports[1]["seq"], reports[0]["seq"])

    def test_a_prompt_submit_clears_a_dialog_cancelled_with_escape(self):
        self._run(self._PRE)
        self._run(
            {
                "session_id": "sess-1",
                "transcript_path": "/tmp/sess-1.jsonl",
                "hook_event_name": "UserPromptSubmit",
                "prompt": "never mind",
            }
        )

        reports = self._reports()
        self.assertEqual(len(reports), 2)
        self.assertEqual(reports[1]["state"], "working")

    def test_a_prompt_submit_without_an_open_gate_reports_nothing(self):
        self._run(
            {
                "session_id": "sess-1",
                "hook_event_name": "UserPromptSubmit",
                "prompt": "ordinary work",
            }
        )

        self.assertEqual(self._reports(), [])

    def test_a_gate_is_only_cleared_by_the_session_that_opened_it(self):
        self._run(self._PRE)
        stale = dict(self._POST, session_id="sess-2", hook_event_name="UserPromptSubmit")
        self._run(stale)

        self.assertEqual(len(self._reports()), 1)
        # The original session can still clear its own gate.
        self._run(self._POST)
        self.assertEqual(len(self._reports()), 2)

    def test_other_tools_and_subagents_are_ignored(self):
        self._run(dict(self._PRE, tool_name="Bash"))
        self._run(dict(self._PRE, agent_id="sub-1"))

        self.assertEqual(self._reports(), [])

    def test_outside_herdr_nothing_is_reported(self):
        with mock.patch.dict(herdr_status.os.environ, {"HERDR_ENV": "0"}, clear=False):
            self._run(self._PRE)
        self.assertEqual(self._reports(), [])

    def test_multiple_questions_report_one_gate_each(self):
        payload = dict(self._PRE)
        payload["tool_input"] = {
            "questions": [
                {"question": "First?", "options": [{"label": "A"}]},
                {"question": "Second?", "options": [{"label": "B"}]},
            ]
        }
        self._run(payload)

        reports = self._reports()
        self.assertEqual(len(reports[0]["gates"]), 2)
        self.assertEqual(reports[0]["state"], "blocked")

    def test_a_malformed_tool_input_still_opens_a_gate(self):
        self._run(dict(self._PRE, tool_input={"questions": "not a list"}))

        reports = self._reports()
        self.assertEqual(reports[0]["state"], "blocked")
        self.assertEqual(reports[0]["gates"][0]["text"], "Question waiting")


class ExplicitStateTests(unittest.TestCase):
    def test_an_override_names_a_state_the_counts_cannot(self):
        self.assertEqual(herdr_status.resolve_state(0, 0, "working"), "working")
        self.assertEqual(herdr_status.resolve_state(0, 0, None), "idle")
        # Junk must degrade to the counts, never reach the server verbatim.
        self.assertEqual(herdr_status.resolve_state(1, 0, "nonsense"), "blocked")
        self.assertEqual(herdr_status.resolve_state(0, 1, 7), "working")


if __name__ == "__main__":
    unittest.main()
