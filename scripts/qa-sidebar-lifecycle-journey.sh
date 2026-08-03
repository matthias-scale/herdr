#!/usr/bin/env bash
# Run a single head-bound, nonvisual sidebar/lifecycle contract journey.
set -euo pipefail

repo_dir="${QA_REPO_DIR:-$PWD}"
head="${QA_EXACT_HEAD:-}"
flow="${QA_FLOW_ID:-}"
artifact_dir="${QA_ARTIFACT_DIR:-}"

usage() {
    echo "usage: qa-sidebar-lifecycle-journey.sh --repo-dir <path> --head <full-sha> --flow <id> --artifact-dir <path>" >&2
}

require_value() {
    [[ $# -ge 2 && -n "$2" ]] || { usage; exit 64; }
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo-dir) require_value "$@"; repo_dir="$2"; shift 2 ;;
        --head) require_value "$@"; head="$2"; shift 2 ;;
        --flow) require_value "$@"; flow="$2"; shift 2 ;;
        --artifact-dir) require_value "$@"; artifact_dir="$2"; shift 2 ;;
        *) usage; exit 64 ;;
    esac
done

[[ "$head" =~ ^[0-9a-fA-F]{40}$ ]] || { echo "qa-sidebar-lifecycle-journey: full --head required" >&2; exit 64; }
[[ -n "$artifact_dir" ]] || { echo "qa-sidebar-lifecycle-journey: --artifact-dir required" >&2; exit 64; }
head="$(printf '%s' "$head" | tr '[:upper:]' '[:lower:]')"
repo_root="$(git -C "$repo_dir" rev-parse --show-toplevel 2>/dev/null)" \
    || { echo "qa-sidebar-lifecycle-journey: --repo-dir is not a Git checkout" >&2; exit 66; }
repo_root="$(cd "$repo_root" && pwd -P)"
actual_head="$(git -C "$repo_root" rev-parse HEAD)"
[[ "$actual_head" == "$head" ]] \
    || { echo "qa-sidebar-lifecycle-journey: checkout is not the requested exact head" >&2; exit 66; }

case "$flow" in
    sidebar-all-tab-retention)
        filters=("initial_sidebar_projection_keeps_every_workspace_tab_in_expanded_and_collapsed_views")
        ;;
    space-first-single-line-sidebar)
        filters=(
            "ac1_ac2_ac3_ac4_cumulative_space_first_single_line_fixture"
            "ac4_tab_rollup_does_not_let_done_mask_working"
            "ac4_clicking_tab_row_preserves_that_tabs_focused_pane"
            "ac1_ac2_ac3_mobile_tabs_are_status_first_single_line_rows"
            "ac4_mobile_sidebar_tab_click_preserves_tabs_focused_pane"
            "review_findings_agent_navigation_reveals_against_final_picker_projection"
        )
        ;;
    same-session-title-replacement)
        filters=(
            "terse_later_user_prompt_submit_retains_the_session_initial_title"
            "work_title_initial_briefings_are_session_scoped_and_later_objectives_replace_them"
        )
        ;;
    reopen-clears-done-without-reorder)
        filters=(
            "api_pane_focus_marks_already_focused_done_pane_seen"
            "initial_sidebar_projection_keeps_every_workspace_tab_in_expanded_and_collapsed_views"
        )
        ;;
    working-latches-until-genuine-completion)
        filters=(
            "fallback_idle_does_not_override_full_lifecycle_hook_working"
            "full_internal_event_queue_eventually_applies_working_to_idle_transition"
        )
        ;;
    *) echo "qa-sidebar-lifecycle-journey: unknown flow $flow" >&2; exit 64 ;;
esac

artifact_dir="$(mkdir -p "$artifact_dir" && cd "$artifact_dir" && pwd -P)"
log_path="$artifact_dir/${flow}.log"
zig_bin=""
if command -v mise >/dev/null 2>&1; then
    zig_root="$(mise where zig@0.15.2 2>/dev/null || true)"
    if [[ -x "$zig_root/zig" ]]; then
        zig_bin="$zig_root/zig"
    fi
fi
for filter in "${filters[@]}"; do
    if ! (
        cd "$repo_root"
        if [[ -n "$zig_bin" ]]; then
            ZIG="$zig_bin" CARGO_TARGET_DIR="$artifact_dir/cargo-target" just test-one "$filter"
        else
            CARGO_TARGET_DIR="$artifact_dir/cargo-target" just test-one "$filter"
        fi
    ) >>"$log_path" 2>&1; then
        echo "qa-sidebar-lifecycle-journey: assertion failed for $flow" >&2
        exit 1
    fi
done

artifact_path="$artifact_dir/${flow}.json"
python3 - "$artifact_path" "$head" "$flow" "${filters[@]}" <<'PY'
import json
import sys

path, head, flow, *filters = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump(
        {
            "schema": "herdr-terminal-contract/v1",
            "head_sha": head,
            "flow_id": flow,
            "assertions": filters,
            "visual_evidence": "not-required",
        },
        output,
        sort_keys=True,
        separators=(",", ":"),
    )
    output.write("\n")
print(json.dumps({
    "schema": "qa-journey-result/v1",
    "status": "PASS",
    "head_sha": head,
    "artifacts": [{"id": f"{flow}-assertions", "type": "assertion", "path": path}],
}, sort_keys=True, separators=(",", ":")))
PY
