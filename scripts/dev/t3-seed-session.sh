#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: HERDR_BIN=/path/to/herdr %s <session-name> [--reset] [--agents]\n' \
        "${0##*/}" >&2
}

if [[ $# -lt 1 ]]; then
    usage
    exit 2
fi

session_name=$1
shift
reset=false
agents=false
for option in "$@"; do
    case $option in
        --reset) reset=true ;;
        --agents) agents=true ;;
        *)
            usage
            exit 2
            ;;
    esac
done

if [[ -z $session_name || $session_name == "default" ]]; then
    printf 'refusing to seed the default Herdr session\n' >&2
    exit 2
fi

if [[ -z ${HERDR_BIN:-} || ! -x ${HERDR_BIN:-} ]]; then
    printf 'HERDR_BIN must name an executable Herdr binary\n' >&2
    exit 2
fi
HERDR_BIN=$(realpath -- "$HERDR_BIN")
HERDR_SEED_CLAUDE=${HERDR_SEED_CLAUDE:-claude}
HERDR_SEED_CODEX=${HERDR_SEED_CODEX:-codex}
if [[ -z $HERDR_SEED_CLAUDE || -z $HERDR_SEED_CODEX ]]; then
    printf 'HERDR_SEED_CLAUDE and HERDR_SEED_CODEX must not be empty\n' >&2
    exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
    printf 'jq is required\n' >&2
    exit 2
fi

# realpath -m is GNU-only; macOS CI lacks it, so normalise with python.
abs_path() { python3 -c "import os,sys;print(os.path.abspath(os.path.expanduser(sys.argv[1])))" "$1"; }
live_socket=$(abs_path "$HOME/.config/herdr/herdr.sock")
for socket_var in HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH; do
    socket_path=${!socket_var:-}
    if [[ ${socket_path:0:1} == "~" && ${socket_path:1:1} == "/" ]]; then
        socket_path=$HOME/${socket_path#\~/}
    fi
    if [[ -n $socket_path && $(abs_path "$socket_path") == "$live_socket" ]]; then
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
            '.result.panes[] | select(.label == $name) | "\(.label): \(.pane_id) · \(.agent // "-") · \(.agent_status // "unknown")"' \
            <<<"$panes_json"
    done
}

report_work_title() {
    local pane_id=$1
    local title=$2
    run_herdr pane report-metadata "$pane_id" \
        --source t3-seed \
        --title "$title" >/dev/null
}

wait_for_agent() {
    local pane_id=$1
    local expected_agent=$2
    local pane_json
    local detected_agent
    local attempt
    for attempt in {1..30}; do
        pane_json=$(run_herdr pane list --workspace "$workspace_id")
        detected_agent=$(jq -r --arg pane "$pane_id" \
            '.result.panes[] | select(.pane_id == $pane) | .agent // empty' \
            <<<"$pane_json")
        if [[ -n $detected_agent ]]; then
            if [[ $detected_agent != "$expected_agent" ]]; then
                printf 'pane %s detected %s, expected %s\n' \
                    "$pane_id" "$detected_agent" "$expected_agent" >&2
                return 1
            fi
            return 0
        fi
        sleep 1
    done
    printf 'pane %s did not detect %s within 30 seconds\n' \
        "$pane_id" "$expected_agent" >&2
    return 1
}

wait_for_agent_startup() {
    local pane_id=$1
    local pane_json
    local status
    local attempt
    for attempt in {1..30}; do
        pane_json=$(run_herdr pane list --workspace "$workspace_id")
        status=$(jq -r --arg pane "$pane_id" \
            '.result.panes[] | select(.pane_id == $pane) | .agent_status // "unknown"' \
            <<<"$pane_json")
        if [[ $status != "working" ]]; then
            return 0
        fi
        sleep 1
    done
    printf 'pane %s was still starting after 30 seconds\n' "$pane_id" >&2
    return 1
}

wait_for_agent_quiet() {
    # Settling during agent startup is undone by the startup screen changes
    # (f11: non-chrome snapshot changes unsettle). Wait until the pane is not
    # working and its revision has been stable for three polls, 5 s apart.
    local pane_id=$1
    local pane_json status revision last_revision="" stable=0 attempt
    for attempt in {1..18}; do
        pane_json=$(run_herdr pane get "$pane_id")
        status=$(jq -r '.result.pane.agent_status // "unknown"' <<<"$pane_json")
        revision=$(jq -r '.result.pane.revision // 0' <<<"$pane_json")
        if [[ $status != "working" && $revision == "$last_revision" ]]; then
            stable=$((stable + 1))
            if [[ $stable -ge 2 ]]; then
                return 0
            fi
        else
            stable=0
        fi
        last_revision=$revision
        sleep 5
    done
    printf 'pane %s did not go quiet within 90 seconds\n' "$pane_id" >&2
    return 1
}

start_sample_agents() {
    local pane_id
    local command
    local expected_agent
    local current_agent
    local index
    local -a agent_specs=(
        "$sample_linear|$HERDR_SEED_CLAUDE|claude"
        "$sample_pr|$HERDR_SEED_CODEX|codex"
        "$sample_missive|$HERDR_SEED_CLAUDE|claude"
        "$sample_settled|$HERDR_SEED_CLAUDE|claude"
    )

    for index in "${!agent_specs[@]}"; do
        IFS='|' read -r pane_id command expected_agent <<<"${agent_specs[$index]}"
        current_agent=$(run_herdr pane list --workspace "$workspace_id" | \
            jq -r --arg pane "$pane_id" \
                '.result.panes[] | select(.pane_id == $pane) | .agent // empty')
        if [[ $current_agent == "$expected_agent" ]]; then
            continue
        fi
        if [[ -n $current_agent ]]; then
            printf 'pane %s already has %s, expected %s\n' \
                "$pane_id" "$current_agent" "$expected_agent" >&2
            return 1
        fi
        run_herdr pane run "$pane_id" "$command" >/dev/null
        wait_for_agent "$pane_id" "$expected_agent"
        if (( index + 1 < ${#agent_specs[@]} )); then
            sleep 5
        fi
    done
    wait_for_agent_startup "$sample_settled"
    wait_for_agent_quiet "$sample_settled"
    settle_and_assert "$sample_settled"
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
# mapfile needs bash 4; macOS ships bash 3.2, so read the ids line by line.
sample_workspace_ids=()
while IFS= read -r workspace_id; do
    [[ -n $workspace_id ]] && sample_workspace_ids+=("$workspace_id")
done < <(
    jq -r '.result.workspaces[] | select(.label == "t3-sample") | .workspace_id' \
        <<<"$workspaces_json"
)

if [[ $reset == true ]]; then
    for workspace_id in ${sample_workspace_ids[@]+"${sample_workspace_ids[@]}"}; do
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
    workspace_id=${sample_workspace_ids[0]}
    sample_settled=$(jq -er '.result.panes[] | select(.label == "sample-settled") | .pane_id' \
        <<<"$panes_json")
    sample_linear=$(jq -er '.result.panes[] | select(.label == "sample-linear") | .pane_id' \
        <<<"$panes_json")
    sample_pr=$(jq -er '.result.panes[] | select(.label == "sample-pr") | .pane_id' \
        <<<"$panes_json")
    sample_missive=$(jq -er '.result.panes[] | select(.label == "sample-missive") | .pane_id' \
        <<<"$panes_json")
    settle_and_assert "$sample_settled"
    if [[ $agents == true ]]; then
        start_sample_agents
    fi
    panes_json=$(run_herdr pane list --workspace "${sample_workspace_ids[0]}")
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
report_work_title "$sample_linear" 'image-edit-simple v3 reference addendum'

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
    --ticket SCA-3165 \
    --title 't3: home screen and dock' >/dev/null
report_work_title "$sample_pr" 't3: home screen and dock'

sample_missive=$(create_tab sample-missive "$sample_cwd")
run_herdr pane work-context set "$sample_missive" \
    --missive-url https://mail.missiveapp.com/#inbox/conversations/sample-conversation-1 \
    --title 'Sample support conversation' >/dev/null
report_work_title "$sample_missive" 'Sample support conversation'

sample_shell=$(create_tab sample-shell /tmp)
sample_settled=$(create_tab sample-settled "$sample_cwd")
settle_and_assert "$sample_settled"

if [[ $agents == true ]]; then
    start_sample_agents
fi
panes_json=$(run_herdr pane list --workspace "$workspace_id")
print_panes "$panes_json"
