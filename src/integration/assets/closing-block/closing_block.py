"""Parse the existing closing block into herdr status channels.

The adapter deliberately keeps the authoring format unchanged. Critical action
points retain their Gate/Answer/Verify labels; only Gate items block. The
optional What to test section is non-blocking context, and auto-proceeded
decisions are a separate delimited list.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any

# HERDR_INTEGRATION_VERSION=1
_HEADER_RE = re.compile(
    r"^\s*\*\*Critical action points(?:\s*\((?P<n>\d+)\s+blocking\))?\*\*\s*$",
    re.MULTILINE,
)
_NOTHING_RE = re.compile(r"^\s*\*\*Nothing to act on\.?\*\*\s*$", re.MULTILINE)
_AGENTS_RE = re.compile(
    r"^\s*(?P<n>\d+)\s+agents?\s+running:\s*(?P<rest>.+?)\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_DONE_RE = re.compile(r"^\s*Done here\.\s*$", re.MULTILINE)

_ITEM_START = r"^\d+[.)]\s*\*\*(?:Gate|Answer|Verify)\*\*"
_ITEM_RE = re.compile(
    rf"^(?P<idx>\d+)[.)]\s*\*\*(?P<label>Gate|Answer|Verify)\*\*"
    rf"\s*(?P<body>.*?)(?={_ITEM_START}|^\*\*What to test\b|"
    r"^\*\*Auto-proceeded decisions\*\*|^\d+\s+agents?\s+running:|"
    r"^Done here\.|\Z)",
    re.MULTILINE | re.IGNORECASE | re.DOTALL,
)
_DECISIONS_RE = re.compile(
    r"^\s*\*\*Auto-proceeded decisions\*\*\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_DECISION_ITEM_RE = re.compile(
    r"^(?P<idx>\d+)[.)]\s*(?P<body>.*?)(?=^\d+[.)]\s+|"
    r"^\*\*What to test\b|^\d+\s+agents?\s+running:|"
    r"^Done here\.|\Z)",
    re.MULTILINE | re.DOTALL,
)
_WHAT_RE = re.compile(
    r"^\s*\*\*What to test(?P<meta>.*?)\*\*\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_PR_RE = re.compile(r"(?<![A-Za-z0-9])#(?P<pr>\d+)\b")
_TICKET_RE = re.compile(r"\b[A-Z][A-Z0-9]+-\d+\b")
_URL_RE = re.compile(r"https?://[^\s\]>]+", re.IGNORECASE)
_RECOMMENDATION_RE = re.compile(
    r"\b(?:recommend(?:ation|ed)?|proceed(?:ed)?\s+with)\s*[:\-]?\s*"
    r"(?P<value>.+?)(?=\s+(?:because|at\s+\d{1,2}:\d{2}|decided\s+at)|$)",
    re.IGNORECASE,
)
_DECIDED_AT_RE = re.compile(
    r"\b(?:decided\s+at|at)\s+(?P<value>\d{1,2}:\d{2}(?:\s*[A-Z]{2})?)",
    re.IGNORECASE,
)


def _clean_body(body: str) -> str:
    # Remove only the Markdown list separator. Keep the item wording and
    # internal whitespace verbatim.
    return body.strip().lstrip("-—–:").strip()


def _metadata(text: str) -> dict[str, Any]:
    url_match = _URL_RE.search(text)
    url = url_match.group(0).rstrip(".,;:") if url_match else None
    while url and url.endswith(")") and url.count(")") > url.count("("):
        url = url[:-1]
    pr_match = _PR_RE.search(text)
    ticket_match = _TICKET_RE.search(text)
    return {
        "pr": int(pr_match.group("pr")) if pr_match else None,
        "ticket": ticket_match.group(0) if ticket_match else None,
        "url": url,
    }


@dataclass
class Item:
    index: int
    label: str  # Gate | Answer | Verify | What to test
    text: str
    metadata: dict[str, Any] | None = None

    @property
    def blocking(self) -> bool:
        return self.label == "Gate"

    def wire(self) -> dict[str, Any]:
        metadata = _metadata(self.text)
        if self.metadata:
            metadata.update(self.metadata)
        return {
            "n": self.index,
            "label": self.label,
            "text": self.text,
            **metadata,
            "default": None,
            "default_at": None,
        }


@dataclass
class Decision:
    index: int
    text: str
    recommendation: str
    decided_at: str | None = None

    def wire(self) -> dict[str, Any]:
        decided_at = self.decided_at
        if decided_at is None:
            decided_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
        return {
            "n": self.index,
            "text": self.text,
            "recommendation": self.recommendation,
            "reversible": True,
            "decided_at": decided_at,
        }


@dataclass
class ClosingBlock:
    present: bool = False
    items: list[Item] = field(default_factory=list)
    decisions: list[Decision] = field(default_factory=list)
    declared_blocking: int | None = None
    agents: list[str] = field(default_factory=list)
    declared_agents: int | None = None
    done_here: bool = False

    @property
    def gates(self) -> list[Item]:
        return [item for item in self.items if item.blocking]

    @property
    def nonblocking_items(self) -> list[Item]:
        return [item for item in self.items if not item.blocking]

    @property
    def blocking(self) -> int:
        # Item labels are authoritative. Keep the header only as a fallback for
        # an incomplete block, never as a source of phantom gates.
        if self.items:
            return len(self.gates)
        return self.declared_blocking or 0

    @property
    def agents_running(self) -> int:
        if self.declared_agents is not None:
            return self.declared_agents
        return len(self.agents)

    @property
    def herdr_state(self) -> str:
        if self.blocking > 0:
            return "blocked"
        if self.agents_running > 0:
            return "working"
        return "idle"

    def message(self) -> str | None:
        if self.blocking > 0:
            head = self.gates[0].text if self.gates else ""
            extra = f" (+{self.blocking - 1})" if self.blocking > 1 else ""
            return ((head or f"{self.blocking} blocking")[:80]) + extra
        if self.agents_running > 0:
            n = self.agents_running
            return f"{n} agent{'s' if n != 1 else ''} running"
        return None

    def wire_gates(self) -> list[dict[str, Any]]:
        return [item.wire() for item in self.gates]

    def wire_items(self) -> list[dict[str, Any]]:
        return [item.wire() for item in self.nonblocking_items]

    def wire_decisions(self) -> list[dict[str, Any]]:
        return [decision.wire() for decision in self.decisions]


def _section_end(text: str, start: int) -> int:
    candidates = [
        match.start()
        for pattern in (_DECISIONS_RE, _WHAT_RE)
        for match in pattern.finditer(text, start)
    ]
    return min(candidates, default=len(text))


def _decisions_end(text: str, start: int) -> int:
    # The decisions list may sit before the critical-action-points block (the
    # authoring rules put the closing block last), so the item scan must stop
    # at whichever section opens next or the CAP items parse as decisions.
    candidates = [
        match.start()
        for pattern in (_HEADER_RE, _NOTHING_RE, _WHAT_RE, _AGENTS_RE, _DONE_RE)
        for match in pattern.finditer(text, start)
    ]
    return min(candidates, default=len(text))


def _parse_what_to_test(text: str, start: int, items: list[Item]) -> list[Item]:
    headings = list(_WHAT_RE.finditer(text, start))
    parsed: list[Item] = []
    for position, heading in enumerate(headings):
        end_candidates = [
            match.start()
            for pattern in (_DECISIONS_RE, _AGENTS_RE, _DONE_RE)
            for match in pattern.finditer(text, heading.end())
        ]
        if position + 1 < len(headings):
            end_candidates.append(headings[position + 1].start())
        end = min(end_candidates, default=len(text))
        body = text[heading.end() : end].strip()
        if not body:
            continue
        gate_number = re.search(r"\bGate\s+(?P<n>\d+)\b", heading.group("meta"), re.I)
        index = int(gate_number.group("n")) if gate_number else (
            max((item.index for item in [*items, *parsed]), default=0) + 1
        )
        parsed.append(
            Item(index, "What to test", body, _metadata(heading.group(0) + " " + body))
        )
    return parsed


def _decision_fields(body: str) -> tuple[str, str | None]:
    recommendation_match = re.search(
        r"\brecommendation\s*[:\-]\s*(?P<value>.+?)(?=\s+at\s+\d{1,2}:\d{2}|$)",
        body,
        re.IGNORECASE,
    ) or _RECOMMENDATION_RE.search(body)
    recommendation = (
        recommendation_match.group("value").strip(" .")
        if recommendation_match
        else body
    )
    decided_match = _DECIDED_AT_RE.search(body)
    decided_at = decided_match.group("value").strip() if decided_match else None
    return recommendation, decided_at


def parse(text: str) -> ClosingBlock:
    """Parse the last closing block out of a full assistant message."""
    block = ClosingBlock()

    headers = list(_HEADER_RE.finditer(text))
    nothings = list(_NOTHING_RE.finditer(text))
    header = headers[-1] if headers else None
    nothing = nothings[-1] if nothings else None

    start = None
    if header and (nothing is None or header.start() > nothing.start()):
        block.present = True
        declared = header.group("n")
        block.declared_blocking = int(declared) if declared is not None else None
        start = header.end()
    elif nothing:
        block.present = True
        block.declared_blocking = 0
        start = nothing.end()

    if start is not None:
        cap_end = _section_end(text, start)
        for match in _ITEM_RE.finditer(text, start, cap_end):
            block.items.append(
                Item(
                    int(match.group("idx")),
                    match.group("label").capitalize(),
                    _clean_body(match.group("body")),
                )
            )
        block.items.extend(_parse_what_to_test(text, start, block.items))

    closing_markers = sorted([*headers, *nothings], key=lambda match: match.start())
    decisions_start = closing_markers[-1].end() if len(closing_markers) > 1 else 0
    decisions_heading = None
    for candidate in _DECISIONS_RE.finditer(text, decisions_start):
        decisions_heading = candidate
    if decisions_heading:
        block.present = True
        decisions_end = _decisions_end(text, decisions_heading.end())
        for match in _DECISION_ITEM_RE.finditer(
            text, decisions_heading.end(), decisions_end
        ):
            body = _clean_body(match.group("body"))
            if not body:
                continue
            recommendation, decided_at = _decision_fields(body)
            block.decisions.append(
                Decision(int(match.group("idx")), body, recommendation, decided_at)
            )

    agents = None
    for candidate in _AGENTS_RE.finditer(text):
        agents = candidate
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
