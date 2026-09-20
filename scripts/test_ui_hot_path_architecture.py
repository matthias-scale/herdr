from __future__ import annotations

import re
import unittest
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
HOT_PATH_SOURCES = (
    PROJECT_ROOT / "src" / "ui.rs",
    *sorted((PROJECT_ROOT / "src" / "ui").rglob("*.rs")),
    PROJECT_ROOT / "src" / "server" / "render_stream.rs",
)
APP_SERVER_SOURCES = (
    *sorted((PROJECT_ROOT / "src" / "app").rglob("*.rs")),
    *sorted((PROJECT_ROOT / "src" / "server").rglob("*.rs")),
)
RUST_SOURCES = tuple(sorted((PROJECT_ROOT / "src").rglob("*.rs")))
TEST_MODULE = re.compile(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+\w+\s*\{")
INPUT_STATE_CALL = re.compile(r"(?:\.|::)input_state\b")
KEYBOARD_STATE_ANSI_CALL = re.compile(
    r"(?:\.|::)(?:keyboard_state_ansi|kitty_keyboard_state_ansi)\b"
)
AGGREGATE_STATE_CALLS = (
    (INPUT_STATE_CALL, "aggregate terminal input state; add a narrow accessor"),
    (KEYBOARD_STATE_ANSI_CALL, "formatted keyboard state"),
)
FORBIDDEN_CALLS = (
    *AGGREGATE_STATE_CALLS,
    (
        re.compile(r"(?:\.|::)screen_text_snapshot\b"),
        "formatted terminal screen snapshot",
    ),
    (
        re.compile(r"\bforeground_job\s*\("),
        "process-tree inspection",
    ),
)


def blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def mask_comments_and_literals(source: str) -> str:
    chars = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth > 0:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "r":
            quote = index + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                suffix = '"' + "#" * (quote - index - 1)
                end = source.find(suffix, quote + 1)
                end = len(source) if end == -1 else end + len(suffix)
                blank_non_newlines(chars, index, end)
                index = end
                continue

        if source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            blank_non_newlines(chars, index, min(end, len(source)))
            index = end
            continue

        if source[index] == "'":
            end = index + 2
            if index + 1 < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                end += 1
                blank_non_newlines(chars, index, end)
                index = end
                continue

        index += 1

    return "".join(chars)


def production_code(source: str) -> str:
    code = mask_comments_and_literals(source)
    chars = list(code)
    search_from = 0

    while test_module := TEST_MODULE.search(code, search_from):
        depth = 0
        end = test_module.end() - 1
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        blank_non_newlines(chars, test_module.start(), end)
        code = "".join(chars)
        search_from = end

    return code


def function_body(source: str, signature: str) -> str:
    code = production_code(source)
    start = code.index(signature)
    opening = code.index("{", start)
    depth = 0
    for index in range(opening, len(code)):
        if code[index] == "{":
            depth += 1
        elif code[index] == "}":
            depth -= 1
            if depth == 0:
                return code[opening + 1 : index]
    raise AssertionError(f"unclosed function: {signature}")


def find_violations(paths, rules) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for pattern, description in rules:
            for match in pattern.finditer(code):
                line = code.count("\n", 0, match.start()) + 1
                relative_path = path.relative_to(PROJECT_ROOT)
                violations.append(f"{relative_path}:{line}: {description}")
    return violations


class UiHotPathArchitectureTests(unittest.TestCase):
    def test_render_hot_paths_avoid_known_expensive_runtime_queries(self) -> None:
        violations = find_violations(HOT_PATH_SOURCES, FORBIDDEN_CALLS)

        self.assertEqual(
            violations,
            [],
            "Render/layout code must not perform pane-scaled expensive reads:\n"
            + "\n".join(violations),
        )

    def test_app_and_server_avoid_aggregate_terminal_state(self) -> None:
        self.assertTrue(APP_SERVER_SOURCES, "No app/server Rust sources were discovered")
        violations = find_violations(APP_SERVER_SOURCES, AGGREGATE_STATE_CALLS)

        self.assertEqual(
            violations,
            [],
            "App/server code must use narrow terminal-state accessors:\n"
            + "\n".join(violations),
        )

    def test_pane_projection_scans_closing_items_once_without_allocating(self) -> None:
        pane_projection = function_body(
            (PROJECT_ROOT / "src" / "pane" / "state.rs").read_text(encoding="utf-8"),
            "fn agent_projection",
        )
        item_classification = function_body(
            (PROJECT_ROOT / "src" / "api" / "schema" / "panes.rs").read_text(
                encoding="utf-8"
            ),
            "fn requires_human_input",
        )

        self.assertNotIn("has_pending_human_input", pane_projection)
        self.assertEqual(pane_projection.count(".closing_items"), 1)
        self.assertNotIn(".sidebar_projection(", pane_projection)
        self.assertIn(
            ".sidebar_projection_with_pending_human_input(", pane_projection
        )
        preclassified_projection = function_body(
            (PROJECT_ROOT / "src" / "terminal" / "state.rs").read_text(
                encoding="utf-8"
            ),
            "fn sidebar_projection_with_pending_human_input",
        )
        self.assertNotIn("has_pending_human_input()", preclassified_projection)
        self.assertNotIn(".closing_items", preclassified_projection)
        pane_details = function_body(
            (PROJECT_ROOT / "src" / "workspace" / "aggregate.rs").read_text(
                encoding="utf-8"
            ),
            "fn pane_details",
        )
        self.assertNotIn("metadata_tokens_for_api", pane_details)
        self.assertIn("terminal.has_closing_report()", pane_details)
        self.assertNotIn("to_ascii_lowercase", item_classification)

    def test_terminal_closing_report_is_the_only_runtime_fact_owner(self) -> None:
        terminal_state = production_code(
            (PROJECT_ROOT / "src" / "terminal" / "state.rs").read_text(
                encoding="utf-8"
            )
        )
        terminal_fields_start = terminal_state.index("pub struct TerminalState {")
        terminal_fields = terminal_state[
            terminal_fields_start : terminal_state.index(
                "impl TerminalState", terminal_fields_start
            )
        ]
        self.assertIn("closing_report: Option<ClosingReport>", terminal_fields)
        field_declarations = re.findall(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z0-9_]+)\s*:\s*([^,\n]+),",
            terminal_fields,
            re.M,
        )
        closing_fields = [
            name
            for name, field_type in field_declarations
            if "closing" in name.lower() or "closing" in field_type.lower()
        ]
        self.assertEqual(
            closing_fields,
            ["closing_report"],
            "ClosingReport must own every terminal closing fact",
        )
        synthetic_fields = re.findall(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z0-9_]+)\s*:\s*([^,\n]+),",
            "legacy_guard: Option<LegacyClosingReportGuard>,",
            re.M,
        )
        self.assertTrue(
            any(
                "closing" in name.lower() or "closing" in field_type.lower()
                for name, field_type in synthetic_fields
            ),
            "ownership scan must catch closing state hidden behind another prefix",
        )
        for legacy_field in (
            "closing_gates:",
            "closing_items:",
            "closing_decisions:",
            "closing_report_subagents:",
            "closing_idle:",
            "closing_contract:",
            "closing_contract_met:",
            "closing_contract_met_at:",
        ):
            self.assertNotIn(legacy_field, terminal_fields)

        sidebar = function_body(
            (PROJECT_ROOT / "src" / "ui" / "sidebar.rs").read_text(encoding="utf-8"),
            "fn collect_agent_panel_entries_with_runtimes",
        )
        self.assertIn("detail.has_closing_report", sidebar)
        self.assertIn("detail.active_subagents", sidebar)
        self.assertNotIn('get("closing_agents")', sidebar)
        self.assertNotIn('starts_with("closing_")', sidebar)

        violations = []
        direct_read = re.compile(
            r"\bterminal\.closing_(?:gates|items|decisions|idle|contract"
            r"|contract_met|contract_met_at|report_subagents)\b(?!\s*\()"
        )
        legacy_token_read = re.compile(r"metadata_tokens\s*\.\s*get\s*\(\s*\"closing_")
        for path in RUST_SOURCES:
            code = production_code(path.read_text(encoding="utf-8"))
            for pattern in (direct_read, legacy_token_read):
                for match in pattern.finditer(code):
                    violations.append(
                        f"{path.relative_to(PROJECT_ROOT)}:"
                        f"{code.count(chr(10), 0, match.start()) + 1}"
                    )
        self.assertEqual(
            violations,
            [],
            "Closing facts must be read through TerminalState's ClosingReport accessors:\n"
            + "\n".join(violations),
        )

    def test_scanner_ignores_non_production_references(self) -> None:
        source = '''
// runtime.input_state()
const EXAMPLE: &str = "runtime.input_state()";
#[cfg(test)]
mod tests {
    fn aggregate_state_test() { runtime.input_state(); }
}
fn production_after_tests() {}
'''
        code = production_code(source)
        self.assertNotRegex(code, FORBIDDEN_CALLS[0][0])
        self.assertIn("fn production_after_tests()", code)
        self.assertEqual(code.count("\n"), source.count("\n"))

    def test_scanner_checks_production_after_test_modules(self) -> None:
        source = '''
#[cfg(test)]
mod tests {
    const BRACES: &str = "}}";
}
fn render() { TerminalRuntime::input_state; }
'''
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[0][0])

    def test_scanner_catches_each_aggregate_state_call(self) -> None:
        cases = (
            ("fn render() { runtime.input_state(); }", INPUT_STATE_CALL),
            ("fn render() { runtime.keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
            ("fn render() { runtime.kitty_keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
        )
        for source, pattern in cases:
            with self.subTest(source=source):
                self.assertRegex(production_code(source), pattern)

    def test_scanner_catches_imported_process_query(self) -> None:
        source = "fn render() { foreground_job(pid); }"
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[3][0])


if __name__ == "__main__":
    unittest.main()
