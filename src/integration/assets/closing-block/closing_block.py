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
# The header line alone is the trigger: agents drop the bold markers, add a
# heading marker or a colon, or vary the case often enough that any strictness
# here silently un-latches gates. The `(N blocking)` count on this line is what
# blocks; item parsing below it is best-effort detail. Only the full-line
# anchor is kept so prose mentions never match.
_HEADER_RE = re.compile(
    r"^(?:#{1,6}[ \t]*)?(?:\*\*)?Critical action points"
    r"(?:[ \t]*\((?P<n>\d+)[ \t]+blocking\))?(?:\*\*)?"
    # A separator-led suffix after an explicit count is authored trailing prose
    # ("— unchanged, still waiting on your answer:"); the count keeps prose
    # mentions from matching, so only the countless form stays full-line.
    r"(?(n)(?:[ \t]*[—–:-].*)?)[ \t]*:?[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_NOTHING_RE = re.compile(
    r"^(?:\*\*)?Nothing to act on\.?(?:\*\*)?[ \t]*\r?$", re.MULTILINE
)
_AGENTS_RE = re.compile(
    r"^(?P<n>\d+)[ \t]+agents?[ \t]+running:[ \t]*(?P<rest>.+?)[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_DONE_RE = re.compile(r"^Done here\.[ \t]*\r?$", re.MULTILINE)
_CONTRACT_RE = re.compile(
    r"^CONTRACT:[ \t]+(?P<text>.+?)[ \t]+—[ \t]+(?P<state>met|unmet)[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_FENCE_OPEN_RE = re.compile(
    r"^[ \t]{0,3}(?P<marker>`{3,}|~{3,})(?P<info>.*)$"
)
_FENCE_CLOSE_RE = re.compile(r"^[ \t]{0,3}(?P<marker>`{3,}|~{3,})[ \t]*$")

# A label is bold in the authored form; a plain `Gate — ...` prefix counts too,
# but only with a following separator so an item that merely *starts* with the
# word (`Verify the deploy...`) stays unlabeled instead of being half-eaten.
# The item opener and the body terminator must accept exactly the same shapes.
# When the terminator was stricter, a real item it failed to recognise was
# swallowed into the previous item's body instead: `2)**Gate**` and an indented
# `  2. **Gate**` both vanished into an `Answer` above them, taking a declared
# gate with them and leaving nothing discarded for the count to notice.
#
# The trailing lookahead keeps a decimal at the start of a body line (`1.5x
# faster`) from reading as an item marker, which the old `[ \t]` requirement
# had been doing incidentally.
_ITEM_START = r"^[ \t]*\d+[.)](?=[ \t]|\*\*|$)"
_ITEM_RE = re.compile(
    rf"^[ \t]*(?P<idx>\d+)[.)]\s*"
    rf"(?:\*\*(?P<label>Gate|Answer|Verify)\*\*"
    rf"|(?P<plain_label>Gate|Answer|Verify)(?=[ \t]*(?:[—–:]|-[ \t])))?"
    rf"\s*(?P<body>.*?)(?={_ITEM_START}|^\*\*What to test\b|"
    r"^\*\*Auto-proceeded decisions\*\*|^\d+\s+agents?\s+running:|"
    r"^Done here\.|\Z)",
    re.MULTILINE | re.IGNORECASE | re.DOTALL,
)
_DECISIONS_RE = re.compile(
    r"^\*\*Auto-proceeded decisions\*\*[ \t]*\r?$",
    re.MULTILINE | re.IGNORECASE,
)
_DECISION_ITEM_RE = re.compile(
    r"^(?P<idx>\d+)[.)]\s*(?P<body>.*?)(?=^\d+[.)]\s+|"
    r"^\*\*What to test\b|^\d+\s+agents?\s+running:|"
    r"^Done here\.|\Z)",
    re.MULTILINE | re.DOTALL,
)
_WHAT_RE = re.compile(
    r"^\*\*What to test(?P<meta>.*?)\*\*[ \t]*\r?$",
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


@dataclass(frozen=True)
class _ClosingBlock:
    start: int
    end: int
    header: re.Match[str] | None
    decisions_end: int


@dataclass
class ClosingBlock:
    present: bool = False
    items: list[Item] = field(default_factory=list)
    decisions: list[Decision] = field(default_factory=list)
    declared_blocking: int | None = None
    agents: list[str] = field(default_factory=list)
    declared_agents: int | None = None
    done_here: bool = False
    contract: str | None = None
    contract_met: bool | None = None
    # Whether the author labeled anything themselves, and whether any parsed
    # line was discarded for carrying no label. Both are needed to tell a
    # miscounted header apart from an incomplete parse.
    authored_labels: bool = False
    discarded_items: int = 0

    @property
    def gates(self) -> list[Item]:
        return [item for item in self.items if item.blocking]

    @property
    def nonblocking_items(self) -> list[Item]:
        return [item for item in self.items if not item.blocking]

    @property
    def blocking(self) -> int:
        # Labels are the authority on what blocks: Gate means authority is
        # owed before the agent may execute, while Answer and Verify are
        # non-blocking by definition and the agent proceeds past them. So
        # once *any* item parsed with a label, the labeled gates are the
        # count -- a header that says "(1 blocking)" above a lone Answer is
        # a miscounted header, not a hidden gate, and latching it would
        # block a pane whose agent is not actually waiting on anything.
        #
        # The header still wins when nothing labeled parsed at all: there
        # under-reporting a gate is the real failure mode, and a declared
        # count is the only evidence left.
        # Both conditions matter. `authored_labels` excludes gates this parser
        # promoted itself: a block with no labels at all and fewer parsed lines
        # than it declared has lost a gate somewhere, and the header is the only
        # evidence left. `discarded_items` excludes a parse that dropped an
        # unlabeled or malformed line beside real labels -- a mistyped `Gate`
        # next to an `Answer` must not silently retire the human decision.
        if self.authored_labels and self.discarded_items == 0:
            return len(self.gates)
        return max(len(self.gates), self.declared_blocking or 0)

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
            if not head:
                return f"{self.blocking} blocking"
            extra = f" (+{self.blocking - 1})" if self.blocking > 1 else ""
            return head[:80] + extra
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


def _fence_ranges(text: str) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    fence_start: int | None = None
    fence_char: str | None = None
    fence_length = 0
    offset = 0

    for line in text.splitlines(keepends=True):
        line_end = offset + len(line)
        content = line.rstrip("\r\n")
        if fence_start is None:
            opener = _FENCE_OPEN_RE.fullmatch(content)
            if opener:
                marker = opener.group("marker")
                fence_start = offset
                fence_char = marker[0]
                fence_length = len(marker)
        else:
            closer = _FENCE_CLOSE_RE.fullmatch(content)
            if closer:
                marker = closer.group("marker")
                if marker[0] == fence_char and len(marker) >= fence_length:
                    ranges.append((fence_start, line_end))
                    fence_start = None
                    fence_char = None
                    fence_length = 0
        offset = line_end

    if fence_start is not None:
        # An unfinished example is treated as fenced through end-of-message;
        # an ambiguous tail must not fabricate live status markers.
        ranges.append((fence_start, len(text)))
    return ranges


def _visible_matches(
    pattern: re.Pattern[str],
    text: str,
    start: int,
    end: int,
    fences: list[tuple[int, int]],
):
    for match in pattern.finditer(text, start, end):
        if not any(
            fence_start <= match.start() < fence_end
            for fence_start, fence_end in fences
        ):
            yield match


def _section_end(
    text: str, start: int, end: int, fences: list[tuple[int, int]]
) -> int:
    candidates = [
        match.start()
        for pattern in (_DECISIONS_RE, _NOTHING_RE, _WHAT_RE)
        for match in _visible_matches(pattern, text, start, end, fences)
    ]
    return min(candidates, default=end)


def _decisions_end(
    text: str, start: int, end: int, fences: list[tuple[int, int]]
) -> int:
    # The decisions list may sit before the critical-action-points block (the
    # authoring rules put the closing block last), so the item scan must stop
    # at whichever section opens next or the CAP items parse as decisions.
    candidates = [
        match.start()
        for pattern in (
            _DECISIONS_RE,
            _HEADER_RE,
            _NOTHING_RE,
            _WHAT_RE,
            _AGENTS_RE,
            _DONE_RE,
        )
        for match in _visible_matches(pattern, text, start, end, fences)
    ]
    return min(candidates, default=end)


def _closing_blocks(
    text: str, fences: list[tuple[int, int]]
) -> list[_ClosingBlock]:
    """Split top-level CAP sections into blocks and bound their decisions.

    The first CAP belongs to the initial block so valid pre-CAP decisions are
    retained. A top-level Nothing marker bounds decisions in that block; the
    last such marker is the closing boundary, which keeps repeated markers in
    one structural block. A Nothing before the first CAP closes the preamble,
    so that stale decisions cannot flow into the later CAP block.
    """
    headers = list(_visible_matches(_HEADER_RE, text, 0, len(text), fences))
    nothings = list(_visible_matches(_NOTHING_RE, text, 0, len(text), fences))
    if not headers:
        if not nothings:
            return []
        return [_ClosingBlock(0, len(text), None, nothings[-1].start())]

    first_start = 0
    if nothings and nothings[0].start() < headers[0].start():
        first_start = headers[0].start()

    blocks: list[_ClosingBlock] = []
    for position, header in enumerate(headers):
        start = first_start if position == 0 else header.start()
        end = (
            headers[position + 1].start()
            if position + 1 < len(headers)
            else len(text)
        )
        block_nothings = [
            marker
            for marker in nothings
            if start <= marker.start() < end
        ]
        decisions_end = block_nothings[-1].start() if block_nothings else end
        blocks.append(_ClosingBlock(start, end, header, decisions_end))
    return blocks


def _parse_what_to_test(
    text: str,
    start: int,
    end: int,
    items: list[Item],
    fences: list[tuple[int, int]],
) -> list[Item]:
    headings = list(_visible_matches(_WHAT_RE, text, start, end, fences))
    parsed: list[Item] = []
    for position, heading in enumerate(headings):
        end_candidates = [
            match.start()
            for pattern in (_DECISIONS_RE, _NOTHING_RE, _AGENTS_RE, _DONE_RE)
            for match in _visible_matches(
                pattern, text, heading.end(), end, fences
            )
        ]
        if position + 1 < len(headings):
            end_candidates.append(headings[position + 1].start())
        section_end = min(end_candidates, default=end)
        body = text[heading.end() : section_end].strip()
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


def _parse_items(
    text: str, start: int, end: int, fences: list[tuple[int, int]]
) -> list[Item]:
    section_end = _section_end(text, start, end, fences)
    matches = list(_visible_matches(_ITEM_RE, text, start, section_end, fences))
    parsed: list[Item] = []
    for position, match in enumerate(matches):
        end_candidates = [section_end]
        if position + 1 < len(matches):
            end_candidates.append(matches[position + 1].start())
        for pattern in (_AGENTS_RE, _DONE_RE):
            end_candidates.extend(
                candidate.start()
                for candidate in _visible_matches(
                    pattern, text, match.end(), section_end, fences
                )
            )
        item_end = min(end_candidates)
        body = text[match.start("body") : item_end]
        label = match.group("label") or match.group("plain_label") or ""
        parsed.append(
            Item(
                int(match.group("idx")),
                label.capitalize(),
                _clean_body(body),
            )
        )
    return parsed


def _parse_decisions(
    text: str,
    headings: list[re.Match[str]],
    end: int,
    fences: list[tuple[int, int]],
) -> list[Decision]:
    parsed: list[Decision] = []
    for decisions_heading in headings:
        decisions_end = _decisions_end(
            text, decisions_heading.end(), end, fences
        )
        matches = list(
            _visible_matches(
                _DECISION_ITEM_RE,
                text,
                decisions_heading.end(),
                decisions_end,
                fences,
            )
        )
        for position, match in enumerate(matches):
            end_candidates = [decisions_end]
            if position + 1 < len(matches):
                end_candidates.append(matches[position + 1].start())
            for pattern in (_AGENTS_RE, _DONE_RE):
                end_candidates.extend(
                    candidate.start()
                    for candidate in _visible_matches(
                        pattern, text, match.end(), decisions_end, fences
                    )
                )
            body = _clean_body(
                text[match.start("body") : min(end_candidates)]
            )
            if not body:
                continue
            recommendation, decided_at = _decision_fields(body)
            parsed.append(
                Decision(int(match.group("idx")), body, recommendation, decided_at)
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

    if text.startswith("\ufeff"):
        text = text[1:]
    fences = _fence_ranges(text)
    blocks = _closing_blocks(text, fences)
    selected = blocks[-1] if blocks else None
    start = None
    if selected:
        block.present = True
        if selected.header:
            declared = selected.header.group("n")
            block.declared_blocking = int(declared) if declared is not None else None
            start = selected.header.end()
        else:
            block.declared_blocking = 0

        if start is not None:
            block.items.extend(_parse_items(text, start, selected.end, fences))
            # Unlabeled numbered items exist only in the lenient plain form.
            # Promote them to gates, oldest first, until the declared blocking
            # count is met; the rest are dropped exactly as the strict parser
            # always dropped them, so labeled authoring is unchanged.
            # Only for a block that carries no labels at all. Mixing an
            # unlabeled line in beside real labels means the author did label
            # their gates, so the unlabeled line is prose, not a silent gate.
            labeled_gates = len(block.gates)
            declared = block.declared_blocking or 0
            block.authored_labels = any(item.label for item in block.items)
            if not block.authored_labels:
                for item in block.items:
                    if item.label == "" and labeled_gates < declared:
                        item.label = "Gate"
                        labeled_gates += 1
            kept = [item for item in block.items if item.label]
            block.discarded_items = len(block.items) - len(kept)
            block.items = kept
            block.items.extend(
                _parse_what_to_test(
                    text, start, selected.end, block.items, fences
                )
            )

        decision_headings = list(
            _visible_matches(
                _DECISIONS_RE,
                text,
                selected.start,
                selected.decisions_end,
                fences,
            )
        )
        if selected.header:
            post_cap_headings = [
                heading
                for heading in decision_headings
                if heading.start() >= selected.header.end()
            ]
            if post_cap_headings:
                # Pre-CAP decisions are a supported authoring order when no
                # later section exists. Once a selected CAP has a post-CAP
                # section, that section is operative and supersedes all
                # earlier pre-CAP sections. Keep every heading on the
                # operative side so repeated sections in one block survive.
                decision_headings = post_cap_headings

        block.decisions.extend(
            _parse_decisions(
                text, decision_headings, selected.decisions_end, fences
            )
        )

    agents = None
    for candidate in _visible_matches(_AGENTS_RE, text, 0, len(text), fences):
        agents = candidate
    if agents:
        block.declared_agents = int(agents.group("n"))
        block.agents = [
            part.strip() for part in agents.group("rest").split(";") if part.strip()
        ]
        block.present = True
    elif next(_visible_matches(_DONE_RE, text, 0, len(text), fences), None):
        block.done_here = True
        block.declared_agents = 0
        block.present = True

    if block.present:
        contract = None
        for candidate in _visible_matches(
            _CONTRACT_RE, text, 0, len(text), fences
        ):
            contract = candidate
        if contract:
            block.contract = contract.group("text").strip()
            block.contract_met = contract.group("state").lower() == "met"

    return block
