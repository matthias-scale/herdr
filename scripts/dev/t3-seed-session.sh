#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: HERDR_BIN=/path/to/herdr %s <session-name> [--reset]\n' "${0##*/}" >&2
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
    usage
    exit 2
fi

session_name=$1
reset=false
if [[ $# -eq 2 ]]; then
    if [[ $2 != "--reset" ]]; then
        usage
        exit 2
    fi
    reset=true
fi

if [[ -z $session_name || $session_name == "default" ]]; then
    printf 'refusing to seed the default Herdr session\n' >&2
    exit 2
fi

if [[ -z ${HERDR_BIN:-} || ! -x ${HERDR_BIN:-} ]]; then
    printf 'HERDR_BIN must name an executable Herdr binary\n' >&2
    exit 2
fi
HERDR_BIN=$(realpath -- "$HERDR_BIN")

if ! command -v jq >/dev/null 2>&1; then
    printf 'jq is required\n' >&2
    exit 2
fi

live_socket=$(realpath -m -- "$HOME/.config/herdr/herdr.sock")
for socket_var in HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH; do
    socket_path=${!socket_var:-}
    if [[ ${socket_path:0:1} == "~" && ${socket_path:1:1} == "/" ]]; then
        socket_path=$HOME/${socket_path#\~/}
    fi
    if [[ -n $socket_path && $(realpath -m -- "$socket_path") == "$live_socket" ]]; then
        printf 'refusing to run while %s points at the live server socket\n' "$socket_var" >&2
        exit 2
    fi
done

sample_cwd=$HOME/Repos/herdr-worktrees/t3-integration
if [[ ! -d $sample_cwd ]]; then
    printf 'sample checkout does not exist: %s\n' "$sample_cwd" >&2
    exit 2
fi

run_herdr() {
    env \
        -u HERDR_SOCKET_PATH \
        -u HERDR_CLIENT_SOCKET_PATH \
        "$HERDR_BIN" --session "$session_name" "$@"
}

print_panes() {
    local panes_json=$1
    local pane_name
    for pane_name in sample-linear sample-pr sample-missive sample-shell sample-settled; do
        jq -r --arg name "$pane_name" \
            '.result.panes[] | select(.label == $name) | "\(.label): \(.pane_id)"' \
            <<<"$panes_json"
    done
}

settle_and_assert() {
    local pane_id=$1
    local pane_json
    local attempt
    for attempt in {1..20}; do
        run_herdr pane settle "$pane_id" >/dev/null
        pane_json=$(run_herdr pane get "$pane_id")
        if jq -e '.result.pane.settled_at != null' <<<"$pane_json" >/dev/null; then
            return 0
        fi
        sleep 0.1
    done
    printf 'sample-settled pane %s has null settled_at after 20 pane settle attempts\n' \
        "$pane_id" >&2
    return 1
}

workspaces_json=$(run_herdr workspace list)
mapfile -t sample_workspace_ids < <(
    jq -r '.result.workspaces[] | select(.label == "t3-sample") | .workspace_id' \
        <<<"$workspaces_json"
)

if [[ $reset == true ]]; then
    for workspace_id in "${sample_workspace_ids[@]}"; do
        run_herdr workspace close "$workspace_id" >/dev/null
    done
elif [[ ${#sample_workspace_ids[@]} -gt 0 ]]; then
    if [[ ${#sample_workspace_ids[@]} -ne 1 ]]; then
        printf 'multiple t3-sample workspaces exist; rerun with --reset\n' >&2
        exit 1
    fi
    panes_json=$(run_herdr pane list --workspace "${sample_workspace_ids[0]}")
    actual_names=$(jq -r '.result.panes[] | .label // ""' <<<"$panes_json" | sort)
    expected_names=$(printf '%s\n' \
        sample-linear sample-pr sample-missive sample-shell sample-settled | sort)
    if [[ $actual_names != "$expected_names" ]]; then
        printf 't3-sample exists with different panes; rerun with --reset\n' >&2
        exit 1
    fi
    sample_settled=$(jq -er '.result.panes[] | select(.label == "sample-settled") | .pane_id' \
        <<<"$panes_json")
    settle_and_assert "$sample_settled"
    print_panes "$panes_json"
    exit 0
fi

workspace_json=$(run_herdr workspace create \
    --cwd "$sample_cwd" \
    --label t3-sample \
    --no-focus)
workspace_id=$(jq -er '.result.workspace.workspace_id' <<<"$workspace_json")
sample_linear=$(jq -er '.result.root_pane.pane_id' <<<"$workspace_json")
run_herdr pane rename "$sample_linear" sample-linear >/dev/null
run_herdr pane work-context set "$sample_linear" \
    --ticket SCA-3165 \
    --title 'image-edit-simple v3 reference addendum' >/dev/null

create_tab() {
    local pane_name=$1
    local cwd=$2
    local tab_json
    local pane_id
    tab_json=$(run_herdr tab create \
        --workspace "$workspace_id" \
        --cwd "$cwd" \
        --label "$pane_name" \
        --no-focus)
    pane_id=$(jq -er '.result.root_pane.pane_id' <<<"$tab_json")
    run_herdr pane rename "$pane_id" "$pane_name" >/dev/null
    printf '%s\n' "$pane_id"
}

sample_pr=$(create_tab sample-pr "$sample_cwd")
run_herdr pane work-context set "$sample_pr" \
    --pr https://github.com/matthias-scale/herdr/pull/159 \
    --ticket SCA-3165 >/dev/null

sample_missive=$(create_tab sample-missive "$sample_cwd")
run_herdr pane work-context set "$sample_missive" \
    --missive-url https://mail.missiveapp.com/#inbox/conversations/sample-conversation-1 \
    --title 'Sample support conversation' >/dev/null

sample_shell=$(create_tab sample-shell /tmp)
sample_settled=$(create_tab sample-settled "$sample_cwd")
settle_and_assert "$sample_settled"

printf 'sample-linear: %s\n' "$sample_linear"
printf 'sample-pr: %s\n' "$sample_pr"
printf 'sample-missive: %s\n' "$sample_missive"
printf 'sample-shell: %s\n' "$sample_shell"
printf 'sample-settled: %s\n' "$sample_settled"
