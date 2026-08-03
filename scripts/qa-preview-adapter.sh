#!/usr/bin/env bash
# Emit a terminal-only QA card for the checked-out Herdr revision.
set -euo pipefail

repo_dir="${QA_REPO_DIR:-$PWD}"
pr=""
head=""
mode=""
format=""

usage() {
    echo "usage: qa-preview-adapter.sh [--repo-dir <path>] --pr <number> --head <full-sha> --mode <qa|card> --format json" >&2
}

require_value() {
    [[ $# -ge 2 && -n "$2" ]] || { usage; exit 64; }
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo-dir) require_value "$@"; repo_dir="$2"; shift 2 ;;
        --pr) require_value "$@"; pr="$2"; shift 2 ;;
        --head) require_value "$@"; head="$2"; shift 2 ;;
        --mode) require_value "$@"; mode="$2"; shift 2 ;;
        --format) require_value "$@"; format="$2"; shift 2 ;;
        *) usage; exit 64 ;;
    esac
done

[[ -d "$repo_dir" ]] || { echo "qa-preview-adapter: --repo-dir must name a directory" >&2; exit 64; }
[[ "$pr" =~ ^[1-9][0-9]*$ ]] || { echo "qa-preview-adapter: --pr must be a positive number" >&2; exit 64; }
[[ "$head" =~ ^[0-9a-fA-F]{40}$ ]] || { echo "qa-preview-adapter: full --head required" >&2; exit 64; }
[[ "$mode" == "qa" || "$mode" == "card" ]] || { echo "qa-preview-adapter: --mode must be qa or card" >&2; exit 64; }
[[ "$format" == "json" ]] || { echo "qa-preview-adapter: --format must be json" >&2; exit 64; }

head="$(printf '%s' "$head" | tr '[:upper:]' '[:lower:]')"
repo_root="$(git -C "$repo_dir" rev-parse --show-toplevel 2>/dev/null)" \
    || { echo "qa-preview-adapter: --repo-dir is not a Git checkout" >&2; exit 66; }
repo_root="$(cd "$repo_root" && pwd -P)"
actual_head="$(git -C "$repo_root" rev-parse HEAD)"
[[ "$actual_head" == "$head" ]] \
    || { echo "qa-preview-adapter: checkout is not the requested exact head" >&2; exit 66; }

declaration="$repo_root/.qa/preview-adapter.json"
journeys="$repo_root/docs/qa/user-journeys.md"
journey_runner="$repo_root/scripts/qa-sidebar-lifecycle-journey.sh"
[[ -f "$declaration" && -f "$journeys" && -x "$journey_runner" ]] \
    || { echo "qa-preview-adapter: declaration, journeys, or flow runner is unavailable" >&2; exit 66; }

python3 - "$declaration" "$head" <<'PY'
import json
import sys

declaration = json.load(open(sys.argv[1], encoding="utf-8"))
if declaration != {
    "schema": "preview-adapter/v1",
    "command": ["./scripts/qa-preview-adapter.sh"],
    "journeys_document": "docs/qa/user-journeys.md",
}:
    raise SystemExit("qa-preview-adapter: invalid repository declaration")

head = sys.argv[2]
flows = [
    (
        "sidebar-all-tab-retention",
        "Sidebar retains every tab in expanded and compact presentation",
        "Exercise the sidebar projection with inactive, completed, singleton, multi-pane, and agentless tabs.",
        "Both presentations retain the same canonical tab identities; compact mode changes presentation only.",
        ["src/ui/sidebar.rs", "src/app/state.rs"],
    ),
    (
        "space-first-single-line-sidebar",
        "Spaces show one status-first line per tab or window",
        "Render agent, agentless, completed, inactive, and multi-pane tabs, then select, click, and navigate between their rows.",
        "Every tab has one stable row under its space; status precedes its sole title, selected titles darken, latest communication age is right-aligned, model and pane subtitles are absent, and tab focus is preserved.",
        ["src/ui/sidebar.rs", "src/activity_age.rs", "src/app/actions.rs", "src/app/input"],
    ),
    (
        "same-session-title-replacement",
        "Later prompts replace the one title for the same session",
        "Submit a later UserPromptSubmit for an existing agent session.",
        "The persisted tab title is replaced once, with no subtitle or duplicate title.",
        ["src/app/api/panes.rs", "src/app/state.rs"],
    ),
    (
        "reopen-clears-done-without-reorder",
        "Opening completed work clears done without moving its tab",
        "Focus a completed pane while preserving canonical workspace and tab identity order.",
        "Done clears on focus; its row identity and position remain unchanged.",
        ["src/app/api/panes.rs", "src/ui/sidebar.rs"],
    ),
    (
        "working-latches-until-genuine-completion",
        "Working remains latched until a genuine completion transition",
        "Apply fallback idle evidence after an authoritative lifecycle working event, then complete the turn.",
        "Fallback idle does not flicker working; genuine completion changes working to done.",
        ["src/terminal/state.rs", "src/app/mod.rs"],
    ),
]
card = {
    "schema": "preview-card/v1",
    "head_sha": head,
    # Herdr is a terminal application. An empty URL is deliberate: no web preview exists.
    "preview_url": "",
    "card_markdown": (
        "## Terminal contract ready\n\n"
        "This change has no web preview. Run the named repository-owned "
        "nonvisual journeys; each emits a head-bound local assertion artifact."
    ),
    "required_flows": [
        {
            "id": flow_id,
            "title": title,
            "actor": "Herdr operator",
            "action": action,
            "expected": expected,
            "dependency_surfaces": surfaces,
            "visual_required": False,
            "automation": {
                "schema": "qa-automation/v1",
                "command": ["./scripts/qa-sidebar-lifecycle-journey.sh", "--flow", flow_id],
            },
        }
        for flow_id, title, action, expected, surfaces in flows
    ],
    "artifacts": [],
}
print(json.dumps(card, sort_keys=True, separators=(",", ":")))
PY
