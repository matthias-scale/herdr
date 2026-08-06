"""Parse the CLAUDE.md closing block into orthogonal herdr signals.

The closing-block spec (global CLAUDE.md, "## Closing block") guarantees two
independent fields at the end of every interactive response:

  1. an action-item header -- either
         **Critical action points (N blocking)**
     or  **Nothing to act on.**
  2. a liveness line -- either
         Done here.
     or  N agents running: <name -- topic>; <name -- topic>
     optionally followed by named waits.

They are orthogonal: "Nothing to act on." only replaces the *item list*, never
the liveness line. So "no human action needed, but 3 agents are still working"
is a legal and common terminal state -- and today herdr cannot see it, because
Claude/Codex lifecycle is inferred from screen scraping alone.

This module turns the block into a ClosingBlock with three independent counts so
herdr can render them as three channels instead of one collapsed status.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

# **Critical action points (3 blocking)** / (0 blocking) / bare header.
_HEADER_RE = re.compile(
    r"^\s*\*\*Critical action points(?:\s*\((?P<n>\d+)\s+blocking\))?\*\*\s*$",
    re.MULTILINE,
)
_NOTHING_RE = re.compile(r"^\s*\*\*Nothing to act on\.?\*\*\s*$", re.MULTILINE)

# "3 agents running: reviewer A -- round 4; reviewer B -- extra profile"
_AGENTS_RE = re.compile(
    r"^\s*(?P<n>\d+)\s+agents?\s+running:\s*(?P<rest>.+?)\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_DONE_RE = re.compile(r"^\s*Done here\.\s*$", re.MULTILINE)

# Numbered item carrying a bold label: "1. **Gate** -- ..." / "2. **Answer** ..."
_ITEM_RE = re.compile(
    r"^\s*(?P<idx>\d+)[.)]\s*\*\*(?P<label>Gate|Answer|Verify)\*\*\s*(?P<body>.*?)\s*$",
    re.MULTILINE,
)

_LABEL_SEP = re.compile(r"\s+[-—–]\s+")


@dataclass
class Item:
    index: int
    label: str  # Gate | Answer | Verify
    text: str

    @property
    def blocking(self) -> bool:
        # Spec: "Gates block."
        return self.label == "Gate"


@dataclass
class ClosingBlock:
    present: bool = False
    items: list[Item] = field(default_factory=list)
    # Count declared in the header, when the header declares one.
    declared_blocking: int | None = None
    agents: list[str] = field(default_factory=list)
    declared_agents: int | None = None
    done_here: bool = False

    @property
    def blocking(self) -> int:
        """Number of blocking items. Header wins; item labels are the fallback."""
        if self.declared_blocking is not None:
            return self.declared_blocking
        return sum(1 for i in self.items if i.blocking)

    @property
    def agents_running(self) -> int:
        if self.declared_agents is not None:
            return self.declared_agents
        return len(self.agents)

    @property
    def herdr_state(self) -> str:
        """Collapse to herdr's AgentState for `pane.report_agent`.

        Only blocking items are a genuine human gate. Agents still running is
        *working*, not blocked -- that distinction is the whole point.
        """
        if self.blocking > 0:
            return "blocked"
        if self.agents_running > 0:
            return "working"
        return "idle"

    def message(self) -> str | None:
        """Short human string for the herdr status row."""
        if self.blocking > 0:
            gates = [i for i in self.items if i.blocking]
            head = gates[0].text if gates else ""
            head = _LABEL_SEP.split(head, 1)[0].strip(" -—:") if head else ""
            extra = f" (+{self.blocking - 1})" if self.blocking > 1 else ""
            return (head or f"{self.blocking} blocking")[:80] + extra
        if self.agents_running > 0:
            n = self.agents_running
            return f"{n} agent{'s' if n != 1 else ''} running"
        return None


def parse(text: str) -> ClosingBlock:
    """Parse the closing block out of a full assistant message."""
    block = ClosingBlock()

    header = None
    for header in _HEADER_RE.finditer(text):
        pass  # keep the last -- the closing block is at the end
    nothing = None
    for nothing in _NOTHING_RE.finditer(text):
        pass

    # Whichever marker appears last is the operative one.
    start = None
    if header and (not nothing or header.start() > nothing.start()):
        block.present = True
        n = header.group("n")
        block.declared_blocking = int(n) if n is not None else None
        start = header.end()
    elif nothing:
        block.present = True
        block.declared_blocking = 0
        start = nothing.end()

    if start is not None and block.declared_blocking != 0:
        for m in _ITEM_RE.finditer(text, start):
            # Items read "1. **Gate** — <text>"; drop the leading separator so
            # the gate text stands alone as a status line.
            body = m.group("body").strip().lstrip("-—–:").strip()
            block.items.append(Item(int(m.group("idx")), m.group("label"), body))

    agents = None
    for agents in _AGENTS_RE.finditer(text):
        pass
    if agents:
        block.declared_agents = int(agents.group("n"))
        block.agents = [
            part.strip() for part in agents.group("rest").split(";") if part.strip()
        ]
        block.present = True
    elif _DONE_RE.search(text):
        block.done_here = True
        block.declared_agents = 0
        block.present = True

    return block
