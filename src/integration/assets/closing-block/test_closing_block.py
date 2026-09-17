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

CONTRACT_MET = """\
CONTRACT: Both required verification commands exit 0. — met

**Critical action points (0 blocking)**

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

NONBLOCKING_ONLY = """\
**Critical action points (0 blocking)**

1. **Answer** · non-blocking: Share any preference if useful.
2. **Verify** · non-blocking: Confirm the optional visual detail.

Done here.
"""

SUFFIX_NONBLOCKING_ITEMS = """\
**Critical action points (0 blocking)**

1. **Answer** — Choose the release lane · non-blocking
2. **Verify** — Confirm the optional visual detail · non-blocking.

Done here.
"""

MID_SENTENCE_NONBLOCKING_WORD = """\
**Critical action points (0 blocking)**

1. **Answer** — This is non-blocking until we hear from QA.

Done here.
"""

BLOCKING_ANSWER = """\
**Critical action points (0 blocking)**

1. **Answer** — Choose the release lane.

Done here.
"""

UNMARKED_VERIFY_BLOCKING_ANSWER = """\
**Critical action points (0 blocking)**

1. **Verify**: check the log

Done here.
"""

MOVE_AND_RESIZE_CAP = """\
**Critical action points (0 blocking)**

1. https://github.com/scalable-so/scalablev2/pull/3401
   In plain words: LaoZhang routing is restored.

   Answer · non-blocking — What next for placement accuracy?

Waiting on you — 1 item (1), 0 blocking.
"""


class ClosingBlockV2Tests(unittest.TestCase):
    def test_url_first_later_label_retains_the_pending_decision(self):
        block = closing_block.parse(MOVE_AND_RESIZE_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.parse_status, "malformed")
        self.assertEqual(block.completion, "incomplete")
        self.assertEqual(
            [(item["label"], item["blocking"]) for item in block.wire_items()],
            [("Answer", True)],
        )
        self.assertIn("What next for placement accuracy?", block.wire_items()[0]["text"])
        self.assertIn("What next for placement accuracy?", block.message())
        self.assertEqual(
            block.wire_items()[0]["url"],
            "https://github.com/scalable-so/scalablev2/pull/3401",
        )

    def test_bold_plain_and_url_first_labels_have_identical_decision_state(self):
        presentations = (
            "1. **Answer** — Choose the placement strategy.\n",
            "1. Answer · non-blocking — Choose the placement strategy.\n",
            "1. https://example.test/preview\n"
            "   **Answer · non-blocking** — Choose the placement strategy.\n",
        )

        parsed = [
            closing_block.parse(
                "**Critical action points (1 blocking)**\n\n"
                + presentation
                + "\nWaiting on you — 1 item (1), 1 blocking.\n"
            )
            for presentation in presentations
        ]

        self.assertEqual([block.blocking for block in parsed], [1, 1, 1])
        self.assertEqual([block.herdr_state for block in parsed], ["blocked"] * 3)
        self.assertEqual(
            [block.wire_items()[0]["label"] for block in parsed],
            ["Answer"] * 3,
        )
        self.assertTrue(all(block.wire_items()[0]["blocking"] for block in parsed))

    def test_final_done_outranks_an_earlier_agent_claim(self):
        block = closing_block.parse(
            "1 agent running: reviewer — checking the patch.\n\n"
            "**Critical action points (0 blocking)**\n\n"
            "Done here.\n"
        )

        self.assertEqual(block.agents_running, 0)
        self.assertEqual(block.completion, "complete")
        self.assertEqual(block.herdr_state, "idle")

    def test_later_worker_claim_reopens_an_earlier_done_footer(self):
        block = closing_block.parse(
            "Done here.\n\n"
            "**Critical action points (0 blocking)**\n\n"
            "1 agent running: reviewer — checking the new turn.\n"
        )

        self.assertFalse(block.done_here)
        self.assertEqual(block.completion, "incomplete")
        self.assertTrue(block.workers_unknown)

    def test_fenced_and_quoted_status_examples_are_not_authoritative(self):
        block = closing_block.parse(
            "> **Critical action points (1 blocking)**\n"
            "> 1. **Answer** — Example question.\n\n"
            "```markdown\n"
            "Waiting on you — 1 item (1), 1 blocking.\n"
            "1 agent running: example — not live.\n"
            "```\n\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.agents_running, 0)
        self.assertEqual(block.completion, "complete")

    def test_named_external_wait_is_in_progress_without_becoming_a_human_blocker(self):
        block = closing_block.parse(
            "**Critical action points (0 blocking)**\n\n"
            "Waiting for CI run 4123 via github-ci-watch.\n"
        )

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.external_wait, "CI run 4123 via github-ci-watch")
        self.assertEqual(block.herdr_state, "working")
        self.assertEqual(block.completion, "incomplete")
        self.assertEqual(block.parse_status, "ok")

    def test_short_reply_has_missing_task_evidence_instead_of_clearing_state(self):
        block = closing_block.parse("Progressing.\n")

        self.assertFalse(block.present)
        self.assertEqual(block.completion, "missing")
        self.assertEqual(block.parse_status, "missing")
        self.assertEqual(block.herdr_state, "idle")

    def test_combined_worker_and_human_wait_footer_keeps_each_fact_separate(self):
        block = closing_block.parse(
            "**Critical action points (1 blocking)**\n\n"
            "1. **Answer** — Choose the release lane.\n\n"
            "1 agent running: reviewer — checking another lane · "
            "Waiting on you — 1 item (1), 1 blocking.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertTrue(block.waiting_on_you)
        self.assertEqual(block.agents_running, 0)
        self.assertTrue(block.workers_unknown)
        self.assertEqual(block.agents, ["reviewer — checking another lane"])
        self.assertIsNone(block.external_wait)

    def test_combined_worker_and_external_wait_footer_parses_named_wait(self):
        block = closing_block.parse(
            "**Critical action points (0 blocking)**\n\n"
            "1 agent running: monitor — CI watcher · "
            "Waiting for CI run 4123 via github-ci-watch.\n"
        )

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.agents_running, 0)
        self.assertTrue(block.workers_unknown)
        self.assertEqual(block.agents, ["monitor — CI watcher"])
        self.assertEqual(block.external_wait, "CI run 4123 via github-ci-watch")
        self.assertEqual(block.herdr_state, "working")

    def test_canonical_registered_external_wait_footer(self):
        block = closing_block.parse(
            "The CI watcher will resume this task when the required checks finish.\n\n"
            "**Nothing to act on.**\n\n"
            "Waiting: required CI checks, completion watcher registered\n"
        )

        self.assertEqual(
            block.external_wait,
            "required CI checks, completion watcher registered",
        )
        self.assertEqual(block.herdr_state, "working")
        self.assertEqual(block.completion, "incomplete")

    def test_canonical_mixed_label_footer(self):
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n\n"
            "1. **Gate** — Should the agent publish the synthetic changelog post "
            "to the public site? `y/n`\n"
            "2. **Verify** — Approve the tested mobile layout for publication, as "
            "the release requires your visual sign-off? `y/n`\n\n"
            "1 agent running: next-slice — dashboard filters · "
            "Waiting on you — 2 items (1, 2), 2 blocking.\n"
        )

        self.assertEqual(block.blocking, 2)
        self.assertEqual([item.label for item in block.items], ["Gate", "Verify"])
        self.assertEqual(block.agents, ["next-slice — dashboard filters"])
        self.assertEqual(block.agents_running, 0)
        self.assertTrue(block.workers_unknown)
        self.assertTrue(block.waiting_on_you)

    def test_combined_agents_external_wait_and_human_wait_split_cleanly(self):
        block = closing_block.parse(
            "**Critical action points (1 blocking)**\n\n"
            "1. **Answer** — Choose the next lane.\n\n"
            "2 agents running: reviewer — patch review · verifier — focused tests · "
            "Waiting: required CI checks, completion watcher registered · "
            "Waiting on you — 1 item (1), 1 blocking.\n"
        )

        self.assertEqual(block.agents, ["reviewer — patch review", "verifier — focused tests"])
        self.assertEqual(
            block.external_wait,
            "required CI checks, completion watcher registered",
        )
        self.assertTrue(block.waiting_on_you)
        self.assertEqual(block.blocking, 1)

    def test_legacy_suffix_nonblocking_marker_does_not_exempt_decisions(self):
        block = closing_block.parse(SUFFIX_NONBLOCKING_ITEMS)

        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.blocking, 2)
        self.assertEqual(
            [(item["label"], item["text"], item["blocking"]) for item in block.wire_items()],
            [
                ("Answer", "Choose the release lane", True),
                ("Verify", "Confirm the optional visual detail", True),
            ],
        )
        with mock.patch.object(
            herdr_status, "write_mirror", return_value=None
        ), mock.patch.object(herdr_status, "_rpc") as rpc:
            outcome = herdr_status.report(
                agent="claude",
                blocking=block.blocking,
                agents=block.agents_running,
                gates=block.wire_gates(),
                items=block.wire_items(),
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )
        self.assertEqual(outcome["payload"]["state"], "blocked")
        self.assertEqual(
            [item["blocking"] for item in outcome["payload"]["items"]],
            [True, True],
        )
        metadata = rpc.call_args_list[-1].args[3]
        self.assertEqual(metadata["tokens"]["closing_idle"], "0")

    def test_nonblocking_word_in_middle_is_blocking(self):
        block = closing_block.parse(MID_SENTENCE_NONBLOCKING_WORD)

        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.wire_items()[0]["blocking"], True)
        self.assertEqual(block.wire_items()[0]["text"], "This is non-blocking until we hear from QA.")

    def test_legacy_prefix_nonblocking_marker_does_not_exempt_decisions(self):
        block = closing_block.parse(NONBLOCKING_ONLY)

        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.blocking, 2)
        self.assertEqual(
            [(item["label"], item["text"], item["blocking"]) for item in block.wire_items()],
            [
                ("Answer", "Share any preference if useful.", True),
                ("Verify", "Confirm the optional visual detail.", True),
            ],
        )
        with mock.patch.object(
            herdr_status, "write_mirror", return_value=None
        ), mock.patch.object(herdr_status, "_rpc") as rpc:
            outcome = herdr_status.report(
                agent="claude",
                blocking=block.blocking,
                agents=block.agents_running,
                gates=block.wire_gates(),
                items=block.wire_items(),
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )
        self.assertEqual(outcome["payload"]["state"], "blocked")
        self.assertEqual(
            [item["blocking"] for item in outcome["payload"]["items"]],
            [True, True],
        )
        metadata = rpc.call_args_list[-1].args[3]
        self.assertEqual(metadata["tokens"]["closing_idle"], "0")

    def test_blocking_answer(self):
        block = closing_block.parse(BLOCKING_ANSWER)

        self.assertEqual(block.herdr_state, "blocked")
        self.assertTrue(block.wire_items()[0]["blocking"])
        with mock.patch.object(
            herdr_status, "write_mirror", return_value=None
        ), mock.patch.object(herdr_status, "_rpc"):
            outcome = herdr_status.report(
                agent="claude",
                blocking=block.blocking,
                agents=block.agents_running,
                gates=block.wire_gates(),
                items=block.wire_items(),
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
        )
        self.assertEqual(outcome["payload"]["state"], "blocked")

    # MAT-147 AC2
    def test_verify_not_marked_as_nonblocking(self):
        block = closing_block.parse(UNMARKED_VERIFY_BLOCKING_ANSWER)

        self.assertEqual(block.herdr_state, "blocked")
        self.assertTrue(block.wire_items()[0]["blocking"])
        self.assertEqual(block.wire_items()[0]["label"], "Verify")
        with mock.patch.object(
            herdr_status, "write_mirror", return_value=None
        ), mock.patch.object(herdr_status, "_rpc") as rpc:
            outcome = herdr_status.report(
                agent="claude",
                blocking=block.blocking,
                agents=block.agents_running,
                gates=block.wire_gates(),
                items=block.wire_items(),
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )
        self.assertEqual(outcome["payload"]["state"], "blocked")
        self.assertEqual(outcome["payload"]["items"][0]["blocking"], True)

    def test_contract_line_parses_met_and_unmet_states(self):
        met = closing_block.parse(CONTRACT_MET)
        unmet = closing_block.parse(CONTRACT_MET.replace("— met", "— unmet"))

        self.assertEqual(met.contract, "Both required verification commands exit 0.")
        self.assertTrue(met.contract_met)
        self.assertFalse(unmet.contract_met)

    def test_contract_is_absent_without_a_valid_contract_line(self):
        for text in (
            ZERO_COUNT_GATE,
            ZERO_COUNT_GATE.replace(
                "Done here.", "CONTRACT: missing state\n\nDone here."
            ),
            ZERO_COUNT_GATE.replace(
                "Done here.", "CONTRACT: condition — unknown\n\nDone here."
            ),
            ZERO_COUNT_GATE.replace(
                "Done here.", "```\nCONTRACT: fenced — met\n```\n\nDone here."
            ),
        ):
            with self.subTest(text=text):
                block = closing_block.parse(text)
                self.assertIsNone(block.contract)
                self.assertIsNone(block.contract_met)

    def test_cap_gate_becomes_nonempty_object_gate(self):
        block = closing_block.parse(REALISTIC_CAP)

        self.assertEqual(block.blocking, 2)
        self.assertEqual(len(block.wire_gates()), 1)
        self.assertEqual(
            block.wire_gates()[0],
            {
                "n": 1,
                "label": "Gate",
                "text": "Approve PR #2606 for MAT-125 before production rollout.",
                "blocking": True,
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
        self.assertEqual(block.completion, "incomplete")
        self.assertEqual(block.parse_status, "malformed")
        self.assertEqual(block.wire_gates()[0]["text"], "Approve the zero-count correction.")

    def test_declared_blocking_without_items_is_malformed_attention(self):
        block = closing_block.parse(DECLARED_BLOCKING_WITHOUT_ITEMS)

        self.assertEqual(block.blocking, 0)
        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.parse_status, "malformed")
        self.assertEqual(block.wire_gates(), [])
        self.assertIsNone(block.message())

    def test_answer_and_informational_notes_remain_separate_items(self):
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

    def test_what_to_test_alone_stays_idle_because_it_is_not_an_action_point(self):
        block = closing_block.parse(
            "**Critical action points (0 blocking)**\n\n"
            "**What to test**\n\n"
            "1. Confirm the green state remains visible.\n\n"
            "Done here.\n"
        )

        self.assertEqual([item["label"] for item in block.wire_items()], ["What to test"])
        self.assertEqual(block.herdr_state, "idle")

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

        self.assertEqual(block.wire_gates(), [])
        self.assertEqual(block.wire_items(), [])
        self.assertEqual(block.herdr_state, "idle")
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

    def test_retained_items_define_the_count_and_keep_their_labels(self):
        for count, text in DECLARED_COUNT_WITH_ONLY_NONBLOCKING_ITEMS.items():
            with self.subTest(count=count):
                block = closing_block.parse(text)

                self.assertEqual(block.blocking, 2)
                self.assertEqual(block.wire_gates(), [])
                self.assertEqual(
                    [item["label"] for item in block.wire_items()],
                    ["Answer", "Verify"],
                )

    def test_an_item_marker_the_terminator_missed_is_still_its_own_item(self):
        # The body terminator used to be stricter than the item opener, so an
        # item it did not recognise was absorbed into the previous item's body.
        # A gate lost that way left nothing discarded, so the count could not
        # notice it either -- the decision simply disappeared.
        for second in ("2)**Gate** — Approve the rollout.",
                       "  2. **Gate** — Approve the rollout."):
            with self.subTest(second=second):
                block = closing_block.parse(
                    "**Critical action points (1 blocking)**\n"
                    "\n"
                    "1. Answer — which lane?\n"
                    + second + "\n"
                    "\n"
                    "Done here.\n"
                )

                self.assertEqual(block.blocking, 2)
                self.assertEqual(
                    [item["label"] for item in block.wire_gates()], ["Gate"]
                )
                self.assertEqual(block.herdr_state, "blocked")

    def test_a_decimal_opening_a_body_line_is_not_an_item_marker(self):
        # Relaxing the terminator must not let prose split into items.
        block = closing_block.parse(
            "**Critical action points (1 blocking)**\n"
            "\n"
            "1. Answer — which lane?\n"
            "1.5x faster than before.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertEqual(
            [item["label"] for item in block.wire_items()], ["Answer"]
        )

    def test_an_incomplete_unlabeled_parse_reports_only_retained_items(self):
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n"
            "\n"
            "1. Approve the production rollout.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.parse_status, "malformed")

    def test_a_discarded_line_beside_a_label_marks_the_parse_malformed(self):
        for body in (
            "1. Approve the production rollout.\n2. Answer — which lane?\n",
            "1. **Gate:** Merge approval for PR #3078\n2. Answer — which lane?\n",
        ):
            with self.subTest(body=body):
                block = closing_block.parse(
                    "**Critical action points (1 blocking)**\n"
                    "\n" + body + "\n"
                    "Done here.\n"
                )

                self.assertEqual(block.blocking, 1)
                self.assertEqual(block.herdr_state, "blocked")
                self.assertEqual(block.parse_status, "malformed")

    def test_a_complete_labeled_answer_blocks_because_no_agent_is_running(self):
        # The Part 2 fix itself: nothing was discarded and the author labeled
        # every line, so a miscounted header loses to the labels.
        block = closing_block.parse(
            "**Critical action points (1 blocking)**\n"
            "\n"
            "1. Answer — which lane?\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")

        # A real gate beside an answer is unaffected.
        mixed = closing_block.parse(
            "**Critical action points (1 blocking)**\n"
            "\n"
            "1. Gate — merge approval for PR #3078\n"
            "2. Answer — which lane?\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(mixed.blocking, 2)
        self.assertEqual(
            [item["label"] for item in mixed.wire_gates()], ["Gate"]
        )

    def test_declared_count_promotes_matching_unlabeled_items(self):
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n"
            "\n"
            "1. Approve the production rollout.\n"
            "2. Approve the secret rotation.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 2)
        self.assertEqual(block.parse_status, "malformed")
        self.assertEqual(
            [item["label"] for item in block.wire_gates()], ["Gate", "Gate"]
        )

    def test_an_unlabeled_line_beside_a_label_is_never_promoted(self):
        # Mixed block: the author did label their item, so the trailing
        # unlabeled line is not promoted into a gate -- its text is prose and
        # must never be published as a decision the human owes.
        #
        # The dropped line makes the parse malformed, while the displayed count
        # still derives only from the retained pending decision.
        block = closing_block.parse(
            "**Critical action points (2 blocking)**\n"
            "\n"
            "1. **Answer** — Post the summary after CI settles?\n"
            "2. Some trailing prose that is not an item.\n"
            "\n"
            "Done here.\n"
        )

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.parse_status, "malformed")
        self.assertEqual(block.wire_gates(), [])
        self.assertEqual(
            [item["label"] for item in block.wire_items()], ["Answer"]
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
        self.assertEqual(outcome["payload"]["state"], "working")
        self.assertIsInstance(outcome["payload"]["gates"][0], dict)
        self.assertEqual(len(outcome["payload"]["items"]), 2)
        self.assertTrue(outcome["payload"]["decisions"][0]["reversible"])
        report = rpc.call_args_list[1].args
        self.assertEqual(report[2], "pane.report_agent")
        self.assertEqual(report[3]["v"], 2)
        self.assertEqual(report[3]["agents"], 1)
        self.assertEqual(report[3]["gates"], outcome["payload"]["gates"])
        self.assertEqual(report[3]["items"], outcome["payload"]["items"])
        self.assertEqual(report[3]["decisions"], outcome["payload"]["decisions"])
        metadata = rpc.call_args_list[-1].args
        self.assertEqual(metadata[1], "herdr:claude-closing-block")
        self.assertEqual(metadata[2], "pane.report_metadata")
        params = metadata[3]
        # The state label names the state; the gate body stays in the token.
        self.assertEqual(params["state_labels"]["blocked"], "2 action points")
        self.assertNotIn("Approve PR #2606", params["state_labels"]["blocked"])
        self.assertIn("Approve PR #2606", params["tokens"]["closing_gates"])

    def test_report_binds_every_rpc_to_the_provider_session(self):
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            herdr_status.report(
                agent="codex",
                blocking=0,
                agents=0,
                session_id="thread-native-1",
                pane_id="w1:p2",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertEqual(
            [call.args[2] for call in rpc.call_args_list],
            ["pane.report_agent_session", "pane.report_agent", "pane.report_metadata"],
        )
        self.assertTrue(
            all(
                call.args[3]["agent_session_id"] == "thread-native-1"
                for call in rpc.call_args_list
            )
        )
        metadata = rpc.call_args_list[-1].args[3]
        self.assertEqual(metadata["agent"], "codex")
        self.assertEqual(
            metadata["applies_to_source"], "herdr:codex-closing-block"
        )

    def test_report_uses_a_sequence_reserved_by_the_caller(self):
        with self._isolated():
            outcome = herdr_status.report(
                agent="claude",
                blocking=0,
                agents=0,
                seq=123456,
                pane_id="w1:p3",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertEqual(outcome["payload"]["seq"], 123456)

    def test_non_gate_action_points_block_only_when_no_agent_is_running(self):
        cases = [
            ([{"label": "Answer", "text": "Choose the release lane"}], 0, "answer"),
            ([{"label": "Verify", "text": "Confirm the deployed build"}], 0, "verify"),
            (
                [
                    {"label": "Answer", "text": "Choose the release lane"},
                    {"label": "Verify", "text": "Confirm the deployed build"},
                ],
                0,
                "2 action points",
            ),
            ([{"label": "Answer", "text": "Choose the release lane"}], 2, "working"),
        ]
        for items, agents, expected_label in cases:
            with self.subTest(agents=agents, expected_label=expected_label), mock.patch.object(
                herdr_status, "_rpc"
            ) as rpc, mock.patch.dict(
                herdr_status.os.environ,
                {"XDG_STATE_HOME": self._state_dir()},
                clear=False,
            ):
                outcome = herdr_status.report(
                    agent="claude",
                    blocking=0,
                    agents=agents,
                    items=items,
                    pane_id="w9:p17",
                    sock_path="/tmp/herdr-test.sock",
                )

            expected_state = "working" if agents else "blocked"
            self.assertEqual(outcome["payload"]["state"], expected_state)
            self.assertEqual(outcome["payload"]["items"][0]["text"], items[0]["text"])
            report_params = rpc.call_args_list[1].args[3]
            self.assertEqual(report_params["state"], expected_state)
            metadata_params = rpc.call_args_list[-1].args[3]
            if agents:
                self.assertEqual(metadata_params["state_labels"]["working"], expected_label)
            else:
                self.assertEqual(metadata_params["state_labels"]["blocked"], expected_label)

    def test_report_emits_contract_tokens_together_and_truncates_text(self):
        contract = "x" * 220
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            herdr_status.report(
                agent="claude",
                blocking=0,
                agents=0,
                contract=contract,
                contract_met=True,
                pane_id="w9:p15",
                sock_path="/tmp/herdr-test.sock",
            )

        tokens = rpc.call_args_list[-1].args[3]["tokens"]
        self.assertEqual(tokens["closing_contract"], "x" * 200)
        self.assertEqual(tokens["closing_contract_met"], "1")

    def test_report_omits_both_contract_tokens_without_a_valid_contract(self):
        for contract, contract_met in ((None, None), ("", True), ("condition", None)):
            with self.subTest(contract=contract, contract_met=contract_met), mock.patch.object(
                herdr_status, "_rpc"
            ) as rpc, mock.patch.dict(
                herdr_status.os.environ,
                {"XDG_STATE_HOME": self._state_dir()},
                clear=False,
            ):
                herdr_status.report(
                    agent="claude",
                    blocking=0,
                    agents=0,
                    contract=contract,
                    contract_met=contract_met,
                    pane_id="w9:p16",
                    sock_path="/tmp/herdr-test.sock",
                )

            tokens = rpc.call_args_list[-1].args[3]["tokens"]
            self.assertNotIn("closing_contract", tokens)
            self.assertNotIn("closing_contract_met", tokens)

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

    def test_report_emits_additive_lifecycle_fields_and_clearing_tokens(self):
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            outcome = herdr_status.report(
                agent="claude",
                blocking=1,
                agents=2,
                items=[{"label": "Answer", "text": "Choose the lane."}],
                completion="incomplete",
                external_wait="CI run 4123 via watcher",
                parse_status="ok",
                workers_unknown=False,
                pane_id="w1:p1",
                sock_path="/tmp/herdr-test.sock",
            )

        payload = outcome["payload"]
        self.assertEqual(payload["state"], "working")
        self.assertEqual(payload["completion"], "incomplete")
        self.assertEqual(payload["external_wait"], "CI run 4123 via watcher")
        self.assertEqual(payload["parse_status"], "ok")
        self.assertFalse(payload["workers_unknown"])
        report_params = rpc.call_args_list[1].args[3]
        self.assertEqual(report_params["completion"], "incomplete")
        self.assertEqual(report_params["external_wait"], "CI run 4123 via watcher")
        tokens = rpc.call_args_list[-1].args[3]["tokens"]
        self.assertEqual(tokens["closing_completion"], "incomplete")
        self.assertEqual(tokens["closing_wait"], "CI run 4123 via watcher")
        self.assertEqual(tokens["closing_parse"], "ok")
        self.assertEqual(tokens["closing_workers_unknown"], "0")

    def test_report_always_clears_absent_lifecycle_values(self):
        with mock.patch.object(herdr_status, "_rpc") as rpc, mock.patch.dict(
            herdr_status.os.environ,
            {"XDG_STATE_HOME": self._state_dir()},
            clear=False,
        ):
            outcome = herdr_status.report(
                agent="codex",
                blocking=0,
                agents=0,
                completion="missing",
                parse_status="missing",
                pane_id="w9:p18",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertIsNone(outcome["payload"]["external_wait"])
        self.assertFalse(outcome["payload"]["workers_unknown"])
        tokens = rpc.call_args_list[-1].args[3]["tokens"]
        self.assertEqual(tokens["closing_completion"], "missing")
        self.assertEqual(tokens["closing_wait"], "")
        self.assertEqual(tokens["closing_parse"], "missing")
        self.assertEqual(tokens["closing_workers_unknown"], "0")

    def test_short_reply_payload_cannot_clear_merge_base_dependencies(self):
        block = closing_block.parse("Progressing.")
        with self._isolated():
            outcome = herdr_status.report(
                agent="codex",
                blocking=block.blocking,
                agents=block.agents_running,
                gates=block.wire_gates(),
                items=block.wire_items(),
                decisions=block.wire_decisions(),
                agent_names=block.agents,
                completion=block.completion,
                parse_status=block.parse_status,
                workers_unknown=block.workers_unknown,
                pane_id="w9:p18-missing",
                sock_path="/tmp/herdr-test.sock",
            )

        payload = outcome["payload"]
        self.assertEqual(payload["completion"], "missing")
        self.assertEqual(payload["parse_status"], "missing")
        for key in ("agents", "agent_names", "gates", "items", "decisions"):
            self.assertNotIn(key, payload)

        # The merge-base server only replaced dependencies when all three CAP
        # arrays were present. Replaying this payload must leave both facts intact.
        merge_base_state = {"gates": ["pending gate"], "agents": 2}
        if all(key in payload for key in ("gates", "items", "decisions")):
            merge_base_state = {
                "gates": payload["gates"],
                "agents": payload.get("agents"),
            }
        self.assertEqual(merge_base_state, {"gates": ["pending gate"], "agents": 2})

    def test_report_does_not_honor_legacy_nonblocking_item_flags(self):
        with self._isolated():
            outcome = herdr_status.report(
                agent="claude",
                blocking=0,
                agents=0,
                items=[
                    {
                        "label": "Answer",
                        "text": "Choose the release lane.",
                        "blocking": False,
                    }
                ],
                completion="incomplete",
                parse_status="ok",
                pane_id="w9:p19",
                sock_path="/tmp/herdr-test.sock",
            )

        self.assertEqual(outcome["payload"]["blocking"], 1)
        self.assertTrue(outcome["payload"]["items"][0]["blocking"])
        self.assertEqual(outcome["payload"]["state"], "blocked")

    def test_missing_report_mirror_retains_prior_structured_obligations(self):
        with self._isolated():
            herdr_status.report(
                agent="claude",
                blocking=1,
                agents=0,
                gates=[{"label": "Gate", "text": "Gate A"}],
                completion="incomplete",
                external_wait="CI watcher",
                parse_status="ok",
                pane_id="w9:p20",
                sock_path="/tmp/herdr-test.sock",
            )
            herdr_status.report(
                agent="claude",
                blocking=0,
                agents=0,
                completion="missing",
                parse_status="missing",
                pane_id="w9:p20",
                sock_path="/tmp/herdr-test.sock",
            )
            with open(herdr_status.mirror_path("w9:p20"), encoding="utf-8") as fh:
                mirrored = herdr_status.json.load(fh)

        self.assertEqual([gate["text"] for gate in mirrored["gates"]], ["Gate A"])
        self.assertEqual(mirrored["blocking"], 1)
        self.assertEqual(mirrored["external_wait"], "CI watcher")
        self.assertEqual(mirrored["completion"], "missing")
        self.assertEqual(mirrored["parse_status"], "missing")

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
                self.assertEqual(block.blocking, 0)
                self.assertEqual(block.herdr_state, "blocked")
                self.assertEqual(block.parse_status, "malformed")

    def test_counted_header_with_trailing_suffix_latches(self):
        block = closing_block.parse(SUFFIXED_PLAIN_FORM_CAP)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        items = block.wire_items()
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["label"], "Answer")
        self.assertIn("noopnutrition", items[0]["text"])
        self.assertIn("(a-rec)", items[0]["text"])

    def test_counted_header_suffix_variants_latch(self):
        for header in (
            "Critical action points (2 blocking) — unchanged, still waiting on your answer:",
            "Critical action points (2 blocking): still waiting on item 1",
            "**Critical action points (2 blocking)** — carried from last turn",
            "## Critical action points (2 blocking) - both from the review",
        ):
            with self.subTest(header=header):
                block = closing_block.parse(header + "\n")
                self.assertEqual(block.blocking, 0)
                self.assertEqual(block.herdr_state, "blocked")
                self.assertEqual(block.parse_status, "malformed")

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
        items = block.wire_items()
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["label"], "Answer")
        self.assertIn("copy-approval-linear-lane", items[0]["text"])
        self.assertEqual(items[0]["pr"], 921)

    def test_unlabeled_cap_before_agents_line_stays_blocked(self):
        block = closing_block.parse(UNLABELED_CAP_BEFORE_AGENTS_LINE)

        self.assertEqual(block.blocking, 1)
        self.assertEqual(block.herdr_state, "blocked")
        self.assertEqual(block.agents_running, 0)
        self.assertTrue(block.workers_unknown)
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

        self.assertEqual(block.blocking, 2)
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

    def test_short_reply_reports_missing_lifecycle_evidence(self):
        import io

        hook = self._hook_module()
        with mock.patch.object(
            hook, "last_assistant_text", return_value="Progressing."
        ), mock.patch.object(
            hook,
            "report",
            return_value={"payload": {}, "mirror": None, "socket": False},
        ) as report, mock.patch.dict(
            hook.os.environ,
            {"HERDR_ENV": "1", "HERDR_PANE_ID": "w1:p1"},
            clear=False,
        ), mock.patch.object(
            hook.sys, "stdin", io.StringIO('{"transcript_path":"/tmp/t.jsonl"}')
        ):
            self.assertEqual(hook.main(), 0)

        kwargs = report.call_args.kwargs
        self.assertEqual(kwargs["completion"], "missing")
        self.assertEqual(kwargs["parse_status"], "missing")
        self.assertIsNone(kwargs["external_wait"])
        self.assertFalse(kwargs["workers_unknown"])

    def test_reserves_report_sequence_before_reading_the_transcript(self):
        import io

        hook = self._hook_module()
        calls = []

        def reserve():
            calls.append("reserve")
            return 101

        def read(_path):
            calls.append("read")
            return "Done here."

        with mock.patch.object(hook, "reserve_sequence", side_effect=reserve), mock.patch.object(
            hook, "last_assistant_text", side_effect=read
        ), mock.patch.object(
            hook,
            "report",
            return_value={"payload": {}, "mirror": None, "socket": False},
        ) as report, mock.patch.dict(
            hook.os.environ,
            {"HERDR_ENV": "1", "HERDR_PANE_ID": "w1:p1"},
            clear=False,
        ), mock.patch.object(
            hook.sys,
            "stdin",
            io.StringIO(
                '{"session_id":"claude-session-1",'
                '"transcript_path":"/tmp/native-transcript.jsonl"}'
            ),
        ):
            self.assertEqual(hook.main(), 0)

        self.assertEqual(calls, ["reserve", "read"])
        self.assertEqual(report.call_args.kwargs["seq"], 101)

    def test_reserved_old_stop_cannot_restore_a_newer_done_report(self):
        gate = closing_block.parse(
            "**Critical action points (1 blocking)**\n\n"
            "1. **Gate** — Approve A.\n\nDone here."
        )
        done = closing_block.parse(
            "**Critical action points (0 blocking)**\n\nDone here."
        )
        import tempfile

        state_dir = tempfile.mkdtemp(prefix="herdr-stop-order-test-")
        with mock.patch.dict(
            herdr_status.os.environ, {"XDG_STATE_HOME": state_dir}, clear=False
        ), mock.patch.object(herdr_status, "_rpc"):
            for block, seq in ((done, 202), (gate, 101)):
                herdr_status.report(
                    agent="claude",
                    blocking=block.blocking,
                    agents=block.agents_running,
                    gates=block.wire_gates(),
                    items=block.wire_items(),
                    decisions=block.wire_decisions(),
                    completion=block.completion,
                    parse_status=block.parse_status,
                    seq=seq,
                    pane_id="w1:p1",
                    sock_path="/tmp/herdr-test.sock",
                )
            mirror = herdr_status.mirror_path("w1:p1")

        with open(mirror, encoding="utf-8") as fh:
            latest = herdr_status.json.load(fh)
        self.assertEqual(latest["seq"], 202)
        self.assertEqual(latest["completion"], "complete")
        self.assertEqual(latest["gates"], [])


class CodexNotifyHookTests(unittest.TestCase):
    @staticmethod
    def _state_dir():
        import tempfile

        return tempfile.mkdtemp(prefix="herdr-codex-notify-test-")

    @staticmethod
    def _hook_module():
        import importlib.util
        import os

        path = os.path.join(os.path.dirname(__file__), "herdr-codex-notify.py")
        spec = importlib.util.spec_from_file_location("herdr_codex_notify_hook", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_short_reply_reports_missing_lifecycle_evidence(self):
        import json

        hook = self._hook_module()
        payload = json.dumps(
            {
                "v": 2,
                "type": "agent-turn-complete",
                "last-assistant-message": "Progressing.",
            }
        )
        with mock.patch.object(
            hook,
            "report",
            return_value={"payload": {}, "mirror": None, "socket": False},
        ) as report, mock.patch.object(
            hook, "title_from", return_value="Task title"
        ), mock.patch.dict(
            hook.os.environ,
            {"HERDR_ENV": "1", "HERDR_PANE_ID": "w1:p1"},
            clear=False,
        ), mock.patch.object(
            hook.sys, "argv", ["herdr-codex-notify.py", payload]
        ):
            self.assertEqual(hook.main(), 0)

        kwargs = report.call_args.kwargs
        self.assertEqual(kwargs["completion"], "missing")
        self.assertEqual(kwargs["parse_status"], "missing")
        self.assertIsNone(kwargs["external_wait"])
        self.assertFalse(kwargs["workers_unknown"])

    def test_documented_thread_id_is_the_session_identity(self):
        import json

        hook = self._hook_module()
        payload = json.dumps(
            {
                "type": "agent-turn-complete",
                "thread-id": "01a0a5ef-d376-7630-ab2e-1df07874247c",
                "turn-id": "01a0a5ef-d39c-7bc2-a807-265792464411",
                "last-assistant-message": "Done here.",
                "input-messages": ["Finish the adapter."],
            }
        )
        with mock.patch.object(
            hook,
            "report",
            return_value={"payload": {}, "mirror": "/tmp/mirror", "socket": False},
        ) as report, mock.patch.dict(
            hook.os.environ,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": "w1:p1",
                "XDG_STATE_HOME": self._state_dir(),
            },
            clear=False,
        ), mock.patch.object(
            hook.sys, "argv", ["herdr-codex-notify.py", payload]
        ):
            self.assertEqual(hook.main(), 0)

        self.assertEqual(
            report.call_args.kwargs["session_id"],
            "01a0a5ef-d376-7630-ab2e-1df07874247c",
        )

    def test_reserves_sequence_before_loading_and_parsing_the_notification(self):
        hook = self._hook_module()
        calls = []
        payload = {
            "type": "agent-turn-complete",
            "thread-id": "thread-1",
            "turn-id": "turn-1",
            "last-assistant-message": "Done here.",
        }

        def reserve():
            calls.append("reserve")
            return 303

        def load(_argv):
            calls.append("load")
            return payload

        with mock.patch.object(hook, "reserve_sequence", side_effect=reserve), mock.patch.object(
            hook, "load_payload", side_effect=load
        ), mock.patch.object(
            hook, "title_from", return_value="Task title"
        ), mock.patch.object(
            hook,
            "report",
            return_value={"payload": {}, "mirror": "/tmp/mirror", "socket": False},
        ) as report, mock.patch.dict(
            hook.os.environ,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": "w1:p1",
                "XDG_STATE_HOME": self._state_dir(),
            },
            clear=False,
        ):
            self.assertEqual(hook.main(), 0)

        self.assertEqual(calls, ["reserve", "load"])
        self.assertEqual(report.call_args.kwargs["seq"], 303)

    def test_empty_notification_does_not_consume_the_replay_key(self):
        hook = self._hook_module()
        payload = {
            "type": "agent-turn-complete",
            "thread-id": "thread-1",
            "turn-id": "turn-1",
        }
        with mock.patch.object(hook, "load_payload", return_value=payload), mock.patch.object(
            hook, "report_unreported_turn"
        ) as claim, mock.patch.dict(
            hook.os.environ,
            {"HERDR_ENV": "1", "HERDR_PANE_ID": "w1:p1"},
            clear=False,
        ):
            self.assertEqual(hook.main(), 0)

        claim.assert_not_called()

    def test_failed_delivery_is_retried_for_the_same_notification(self):
        import json
        import tempfile

        hook = self._hook_module()
        payload = {
            "type": "agent-turn-complete",
            "thread-id": "thread-retry",
            "turn-id": "turn-retry",
            "last-assistant-message": "Done here.",
        }
        with mock.patch.object(
            hook,
            "report",
            side_effect=[
                {"payload": {}, "mirror": None, "socket": False},
                {"payload": {}, "mirror": "/tmp/mirror", "socket": True},
            ],
        ) as report, mock.patch.object(
            hook, "title_from", return_value="Task title"
        ), mock.patch.dict(
            hook.os.environ,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": "w1:p1",
                "XDG_STATE_HOME": tempfile.mkdtemp(prefix="herdr-codex-retry-test-"),
            },
            clear=False,
        ):
            for _ in range(2):
                with mock.patch.object(
                    hook.sys,
                    "argv",
                    ["herdr-codex-notify.py", json.dumps(payload)],
                ):
                    self.assertEqual(hook.main(), 0)

        self.assertEqual(report.call_count, 2)

    def test_duplicate_old_turn_cannot_restore_a_resolved_gate(self):
        import json
        import tempfile

        hook = self._hook_module()
        old = {
            "type": "agent-turn-complete",
            "thread-id": "01a0a5ef-d376-7630-ab2e-1df07874247c",
            "turn-id": "01a0a5ef-d39c-7bc2-a807-265792464411",
            "last-assistant-message": (
                "**Critical action points (1 blocking)**\n\n"
                "1. **Gate** — Approve A.\n\nDone here."
            ),
        }
        done = {
            **old,
            "turn-id": "01a0a5f4-9f04-7101-843e-d46c286d65f4",
            "last-assistant-message": (
                "**Critical action points (0 blocking)**\n\nDone here."
            ),
        }
        state_dir = tempfile.mkdtemp(prefix="herdr-codex-replay-test-")
        reports = []

        def report(**kwargs):
            reports.append(kwargs)
            return {"payload": {}, "mirror": "/tmp/mirror", "socket": True}

        with mock.patch.object(hook, "report", side_effect=report), mock.patch.object(
            hook, "title_from", return_value="Task title"
        ), mock.patch.dict(
            hook.os.environ,
            {
                "HERDR_ENV": "1",
                "HERDR_PANE_ID": "w1:p1",
                "XDG_STATE_HOME": state_dir,
            },
            clear=False,
        ):
            for native in (old, done, old):
                with mock.patch.object(
                    hook.sys,
                    "argv",
                    ["herdr-codex-notify.py", json.dumps(native)],
                ):
                    self.assertEqual(hook.main(), 0)

        self.assertEqual(len(reports), 2)
        self.assertEqual(reports[0]["blocking"], 1)
        self.assertEqual(reports[1]["blocking"], 0)

    def test_competing_processes_deliver_one_turn_only_once(self):
        import multiprocessing
        import tempfile

        hook = self._hook_module()
        state_dir = tempfile.mkdtemp(prefix="herdr-codex-claim-test-")
        context = multiprocessing.get_context("fork")
        ready = context.Event()
        results = context.Queue()

        def deliver():
            ready.wait(2)
            with mock.patch.dict(
                hook.os.environ, {"XDG_STATE_HOME": state_dir}, clear=False
            ):
                outcome = hook.report_unreported_turn(
                    "w1:p1",
                    "thread-1",
                    "turn-1",
                    lambda: {"socket": True},
                )
                results.put(outcome is not None)

        processes = [context.Process(target=deliver) for _ in range(6)]
        for process in processes:
            process.start()
        ready.set()
        for process in processes:
            process.join(2)
            self.assertFalse(process.is_alive())

        claimed = [results.get(timeout=1) for _ in processes]
        self.assertEqual(claimed.count(True), 1)


class BundleInstallerTests(unittest.TestCase):
    RUNTIME_FILES = (
        "closing_block.py",
        "herdr_status.py",
        "herdr-closing-block.py",
        "herdr-codex-notify.py",
        "herdr-question-gate.py",
    )

    @staticmethod
    def _installer_module():
        import importlib.util
        import os

        path = os.path.join(os.path.dirname(__file__), "install.py")
        spec = importlib.util.spec_from_file_location("closing_block_installer", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def setUp(self):
        import pathlib
        import shutil
        import tempfile

        self.root = pathlib.Path(tempfile.mkdtemp(prefix="herdr-closing-install-test-"))
        self.addCleanup(shutil.rmtree, self.root)
        self.source = pathlib.Path(__file__).parent
        self.target = self.root / "share" / "herdr-closing-block"
        self.target.mkdir(parents=True)
        for name in self.RUNTIME_FILES:
            (self.target / name).write_text(
                "# HERDR_INTEGRATION_VERSION=1\nSTALE = True\n",
                encoding="utf-8",
            )
        (self.target / "codex-notify-chain.sh").write_text(
            "preserve existing notify wiring\n", encoding="utf-8"
        )

    def test_install_replaces_the_five_modules_as_one_verified_bundle(self):
        installer = self._installer_module()

        result = installer.install_bundle(self.source, self.target, dry_run=False)

        self.assertEqual(result["mode"], "installed")
        self.assertEqual(set(result["files"]), set(self.RUNTIME_FILES))
        for name in self.RUNTIME_FILES:
            self.assertEqual(
                (self.target / name).read_bytes(),
                (self.source / name).read_bytes(),
            )
            self.assertEqual(result["files"][name]["version"], 2)
            self.assertRegex(result["files"][name]["sha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(
            (self.target / "codex-notify-chain.sh").read_text(encoding="utf-8"),
            "preserve existing notify wiring\n",
        )
        backup = self.root / "share" / result["backup"]
        self.assertTrue(backup.is_dir())
        self.assertIn("STALE = True", (backup / "closing_block.py").read_text())

    def test_dry_run_validates_without_writing_target_or_backup(self):
        installer = self._installer_module()
        before = {
            path.relative_to(self.target): path.read_bytes()
            for path in self.target.iterdir()
        }

        result = installer.install_bundle(self.source, self.target, dry_run=True)

        after = {
            path.relative_to(self.target): path.read_bytes()
            for path in self.target.iterdir()
        }
        self.assertEqual(result["mode"], "dry-run")
        self.assertEqual(after, before)
        self.assertEqual(list(self.target.parent.glob("herdr-closing-block.backup-*")), [])
        self.assertEqual(list(self.target.parent.glob(".herdr-closing-block.stage-*")), [])

    def test_invalid_source_bundle_leaves_existing_install_untouched(self):
        import shutil

        installer = self._installer_module()
        invalid_source = self.root / "invalid-source"
        invalid_source.mkdir()
        for name in self.RUNTIME_FILES:
            shutil.copy2(self.source / name, invalid_source / name)
        (invalid_source / "closing_block.py").write_text(
            "# HERDR_INTEGRATION_VERSION=2\nthis is not valid python !\n",
            encoding="utf-8",
        )
        before = (self.target / "closing_block.py").read_bytes()

        with self.assertRaises(installer.BundleValidationError):
            installer.install_bundle(invalid_source, self.target, dry_run=False)

        self.assertEqual((self.target / "closing_block.py").read_bytes(), before)
        self.assertEqual(list(self.target.parent.glob("herdr-closing-block.backup-*")), [])


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

    def test_answering_tool_question_restores_unrelated_pending_gate(self):
        herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[{"n": 7, "label": "Gate", "text": "Gate A"}],
            completion="incomplete",
            parse_status="ok",
            session_id="sess-1",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        self.rpc.reset_mock()

        self._run(self._PRE)
        self._run(self._POST)

        reports = self._reports()
        self.assertEqual(
            [[gate["text"] for gate in report["gates"]] for report in reports],
            [["Gate A", "Which color do you prefer? — Red / Green"], ["Gate A"]],
        )
        self.assertEqual(reports[1]["state"], "working")
        self.assertEqual(reports[1]["completion"], "incomplete")
        self.assertEqual(reports[1]["parse_status"], "ok")
        self.assertTrue(all(report["agents"] == 0 for report in reports))
        self.assertTrue(all(report["workers_unknown"] is False for report in reports))

    def test_new_session_question_ignores_old_session_status(self):
        herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[{"n": 7, "label": "Gate", "text": "Old session gate"}],
            completion="incomplete",
            parse_status="ok",
            session_id="sess-old",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        herdr_status.report(
            agent="claude",
            blocking=0,
            agents=0,
            completion="missing",
            parse_status="missing",
            session_id="sess-new",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        self.rpc.reset_mock()

        self._run(dict(self._PRE, session_id="sess-new"))

        reports = self._reports()
        self.assertEqual(len(reports), 1)
        self.assertEqual(
            [gate["text"] for gate in reports[0]["gates"]],
            ["Which color do you prefer? — Red / Green"],
        )
        self.assertNotIn("agents", reports[0])
        self.assertTrue(reports[0]["workers_unknown"])

    def test_post_without_a_marker_cannot_clear_an_unrelated_pending_gate(self):
        import json

        herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[{"n": 7, "label": "Gate", "text": "Gate A"}],
            completion="incomplete",
            parse_status="ok",
            session_id="sess-1",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        self.rpc.reset_mock()

        self._run(self._POST)

        reports = self._reports()
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]["state"], "working")
        self.assertEqual(reports[0]["parse_status"], "missing")
        with open(self.hook.mirror_path(self.pane_id), encoding="utf-8") as fh:
            mirrored = json.load(fh)
        self.assertEqual([gate["text"] for gate in mirrored["gates"]], ["Gate A"])

    def test_late_post_cannot_restore_over_a_newer_authoritative_report(self):
        import json

        herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[{"n": 7, "label": "Gate", "text": "Gate A"}],
            completion="incomplete",
            parse_status="ok",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        self._run(self._PRE)
        herdr_status.report(
            agent="claude",
            blocking=1,
            agents=0,
            gates=[{"n": 8, "label": "Gate", "text": "Gate B"}],
            completion="incomplete",
            parse_status="ok",
            pane_id=self.pane_id,
            sock_path="/tmp/herdr-question-gate-test.sock",
        )
        self.rpc.reset_mock()

        self._run(self._POST)

        self.assertEqual(self._reports(), [])
        self.assertIsNone(self.hook.read_marker(self.pane_id))
        with open(self.hook.mirror_path(self.pane_id), encoding="utf-8") as fh:
            mirrored = json.load(fh)
        self.assertEqual([gate["text"] for gate in mirrored["gates"]], ["Gate B"])

    def test_mismatched_post_cannot_clear_another_tool_question(self):
        self._run(self._PRE)
        self.rpc.reset_mock()

        self._run(dict(self._POST, tool_use_id="toolu_other"))

        self.assertEqual(self._reports(), [])
        self.assertIsNotNone(self.hook.read_marker(self.pane_id))
        self._run(self._POST)
        self.assertEqual(len(self._reports()), 1)
        self.assertIsNone(self.hook.read_marker(self.pane_id))

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

    def test_an_ordinary_prompt_submit_starts_a_working_turn(self):
        self._run(
            {
                "session_id": "sess-1",
                "transcript_path": "/tmp/sess-1.jsonl",
                "hook_event_name": "UserPromptSubmit",
                "prompt": "ordinary work",
            }
        )

        reports = self._reports()
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]["state"], "working")
        self.assertNotIn("gates", reports[0])
        self.assertNotIn("items", reports[0])
        self.assertNotIn("decisions", reports[0])
        self.assertNotIn("agents", reports[0])
        self.assertTrue(reports[0]["workers_unknown"])

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
