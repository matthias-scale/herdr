"""Fixtures are real closing blocks lifted from live transcripts."""

from closing_block import parse

# Verbatim from the 2026-08-06 session checkpoint: the exact case herdr is
# currently blind to -- nothing for the human, but three agents still working.
NOTHING_BUT_AGENTS = """\
Reviewer B failed on the `scalablehq` profile — codex usage limit, resets Aug 8.

**Nothing to act on.**

3 agents running: reviewer A — codex `gpt-5.6-sol` round 4; reviewer B — codex \
`gpt-5.6-sol` round 4 on `extra` profile; independent reviewer — Claude \
`opus·xhigh` round 4.
"""

FULLY_IDLE = """\
CI is green on `030f8b00`.

**Nothing to act on.**

Done here.
"""

BLOCKING_GATE = """\
Round 4 found two P1s. I verified both and dispatched the repair.

**Critical action points (2 blocking)**

1. **Gate** — Merge #30 into `fork/pr-base` once reviewer B reports.
2. **Gate** — Approve rotating the `extra` profile credential.
3. **Answer** — Should the PRIO panel fold sibling panes, or stay first-pane?

2 agents running: repair worker — P1 watermark fix; reviewer A — round 4.
"""

NO_HEADER_COUNT = """\
**Critical action points**

1. **Answer** — Prefer teal or green for the done dot?

Done here.
"""


def test_nothing_to_act_but_agents_running():
    b = parse(NOTHING_BUT_AGENTS)
    assert b.present
    assert b.blocking == 0
    assert b.agents_running == 3
    assert not b.done_here
    assert b.herdr_state == "working"
    assert b.message() == "3 agents running"
    assert "reviewer A" in b.agents[0]


def test_fully_idle():
    b = parse(FULLY_IDLE)
    assert b.blocking == 0
    assert b.agents_running == 0
    assert b.done_here
    assert b.herdr_state == "idle"
    assert b.message() is None


def test_blocking_gates_win_over_running_agents():
    b = parse(BLOCKING_GATE)
    assert b.blocking == 2
    assert b.agents_running == 2
    assert b.herdr_state == "blocked"
    assert len(b.items) == 3
    assert [i.blocking for i in b.items] == [True, True, False]
    assert b.message() == "Merge #30 into `fork/pr-base` once reviewer B reports. (+1)"


def test_header_without_declared_count_falls_back_to_labels():
    b = parse(NO_HEADER_COUNT)
    assert b.declared_blocking is None
    assert b.blocking == 0  # one Answer, no Gate
    assert b.herdr_state == "idle"


def test_absent_block_is_inert():
    b = parse("Just some prose with no closing block at all.")
    assert not b.present
    assert b.herdr_state == "idle"


def test_absent_block_reports_zero_counts_so_the_hook_can_send_it():
    """Both turn-end hooks now report a blockless turn instead of staying
    silent, which is only safe because an absent block reads as zero."""
    b = parse("Just some prose with no closing block at all.")
    assert b.blocking == 0
    assert b.agents_running == 0
    assert b.items == []
    assert b.agents == []


def test_last_marker_wins_when_body_quotes_an_earlier_one():
    text = "Earlier I wrote:\n\n**Nothing to act on.**\n\n" + BLOCKING_GATE
    b = parse(text)
    assert b.blocking == 2
    assert b.herdr_state == "blocked"
