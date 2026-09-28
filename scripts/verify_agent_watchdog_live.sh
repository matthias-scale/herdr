#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  verify_agent_watchdog_live.sh agents
  verify_agent_watchdog_live.sh workers

Required environment for both modes:
  HERDR_BIN                  Prebuilt Herdr binary connected to the test session
  WATCHDOG_EVIDENCE_DIR      Empty directory for command output and logs

Agents mode:
  GEMINI_BIN                 Gemini CLI or timing wrapper
  WATCHDOG_BLOCKED_PANE      Real agent pane waiting on a yes/no question
  WATCHDOG_QUIET_PANE         Shell pane running exactly `sleep 300`
  GEMINI_API_KEY              Supplied through the environment; never printed

Workers mode:
  WATCHDOG_RUNS_DIR           Isolated directory containing only the test run
  WATCHDOG_WORKER_ID          Test run directory name
  WATCHDOG_PARENT_SESSION     Parent agent session ID recorded by the run
  WATCHDOG_NORMALIZE_WORKER_METADATA  Set to 1 to exercise notify logic past
                                      known launcher metadata incompatibilities
  WATCHDOG_EXPECT_PARENT_GONE Set to 1 to assert the missing-parent result
EOF
}

die() {
  printf 'watchdog live check: %s\n' "$*" >&2
  exit 2
}

mode=${1:-}
[[ -n "$mode" ]] || { usage >&2; exit 2; }
shift
[[ $# -eq 0 ]] || { usage >&2; exit 2; }

herdr_bin=${HERDR_BIN:-herdr}
evidence_dir=${WATCHDOG_EVIDENCE_DIR:?set WATCHDOG_EVIDENCE_DIR}
mkdir -p "$evidence_dir"

run_logged() {
  local name=$1
  shift
  local output="$evidence_dir/$name.stdout.json"
  local rc
  {
    printf 'command:'
    printf ' %q' "$@"
    printf '\nstarted_at='
    date -u +%Y-%m-%dT%H:%M:%SZ
  } | tee "$evidence_dir/$name.command.txt"
  set +e
  "$@" 2>&1 | tee "$output"
  rc=$?
  set -e
  {
    printf 'ended_at='
    date -u +%Y-%m-%dT%H:%M:%SZ
    printf 'exit_code=%s\n' "$rc"
  } | tee -a "$evidence_dir/$name.command.txt"
  return "$rc"
}

case "$mode" in
  agents)
    gemini_bin=${GEMINI_BIN:-gemini}
    blocked_pane=${WATCHDOG_BLOCKED_PANE:?set WATCHDOG_BLOCKED_PANE}
    quiet_pane=${WATCHDOG_QUIET_PANE:?set WATCHDOG_QUIET_PANE}
    [[ -n ${GEMINI_API_KEY:-} ]] || die 'GEMINI_API_KEY is not set'

    "$herdr_bin" pane process-info --pane "$blocked_pane" \
      | tee "$evidence_dir/blocked-pane.process.json" \
      | jq -e '.result.process_info.foreground_processes | any(.name == "claude" or .name == "codex")' >/dev/null \
      || die 'blocked pane is not running a Codex or Claude CLI'
    "$herdr_bin" pane read "$blocked_pane" --source detection --format text \
      | tee "$evidence_dir/blocked-pane.txt" \
      | rg -qi '\b(yes|no|y/n)\b' \
      || die 'blocked pane does not show a yes/no prompt'
    "$herdr_bin" pane process-info --pane "$quiet_pane" \
      | tee "$evidence_dir/quiet-pane.process.json" \
      | jq -e '.result.process_info.foreground_processes | any(.argv == ["sleep", "300"])' >/dev/null \
      || die 'quiet pane is not running exactly sleep 300'

    set +e
    run_logged agents "$herdr_bin" watchdog --once \
      --stall-secs "${WATCHDOG_STALL_SECS:-600}" \
      --lines "${WATCHDOG_LINES:-40}" \
      --gemini-bin "$gemini_bin" \
      --state-file "$evidence_dir/agent-state.json" \
      --status-log "$evidence_dir/agent-status.jsonl" --json
    rc=$?
    set -e
    (( rc == 0 )) || die "watchdog exited with status $rc"
    jq -e --arg pane "$blocked_pane" \
      '.decisions | any(.[]; .pane_id == $pane and .status == "corrected" and .old_state == "working")' \
      "$evidence_dir/agents.stdout.json" >/dev/null \
      || die 'watchdog did not correct the live blocked agent from working'
    jq -e --arg pane "$quiet_pane" \
      'all(.decisions[]?; .pane_id != $pane)' \
      "$evidence_dir/agents.stdout.json" >/dev/null \
      || die 'watchdog included the quiet sleep pane'
    ;;

  workers)
    runs_dir=${WATCHDOG_RUNS_DIR:?set WATCHDOG_RUNS_DIR to an isolated run directory}
    worker_id=${WATCHDOG_WORKER_ID:?set WATCHDOG_WORKER_ID}
    parent_session=${WATCHDOG_PARENT_SESSION:?set WATCHDOG_PARENT_SESSION}
    expect_parent_gone=${WATCHDOG_EXPECT_PARENT_GONE:-0}
    [[ -f "$runs_dir/$worker_id/state.json" ]] || die 'test worker state.json is missing'
    jq -e --arg parent "$parent_session" \
      '(.parent_session == $parent) or (.parent == $parent) or (.parent.session == $parent)' \
      "$runs_dir/$worker_id/state.json" >/dev/null \
      || die 'test run is not linked to the expected parent session'
    isolated_runs="$evidence_dir/worker-runs"
    isolated_run="$isolated_runs/$worker_id"
    mkdir -p "$isolated_runs"
    if [[ ${WATCHDOG_NORMALIZE_WORKER_METADATA:-0} == 1 ]]; then
      [[ ! -e "$isolated_run" ]] || die 'isolated test-run copy already exists; use a fresh evidence directory'
      mkdir -p "$isolated_run/turns"
      shopt -s dotglob nullglob
      for entry in "$runs_dir/$worker_id"/*; do
        if [[ ${entry##*/} != turns ]]; then
          cp -a "$entry" "$isolated_run/"
        fi
      done
      for entry in "$runs_dir/$worker_id/turns"/*; do
        [[ -d "$entry" ]] && cp -a "$entry" "$isolated_run/turns/"
      done
      shopt -u dotglob nullglob
      state_mtime=$(stat -c %Y "$isolated_run/state.json")
      jq --arg parent "$parent_session" '. + {parent_session:$parent}' \
        "$isolated_run/state.json" > "$isolated_run/state.json.tmp"
      mv "$isolated_run/state.json.tmp" "$isolated_run/state.json"
      touch -d "@$state_mtime" "$isolated_run/state.json"
    else
      [[ ! -e "$isolated_run" ]] || die 'isolated test-run copy already exists; use a fresh evidence directory'
      cp -a "$runs_dir/$worker_id" "$isolated_run"
    fi

    set +e
    run_logged workers "$herdr_bin" watchdog workers --once \
      --stall-minutes "${WATCHDOG_STALL_MINUTES:-1}" \
      --runs-dir "$isolated_runs" \
      --claude-projects-dir "${WATCHDOG_CLAUDE_PROJECTS_DIR:-$evidence_dir/empty-claude-projects}" \
      --state-file "$evidence_dir/worker-state.json" \
      --log-file "$evidence_dir/worker-notifications.jsonl" --json
    rc=$?
    set -e
    if [[ "$expect_parent_gone" == 1 ]]; then
      jq -e --arg id "$worker_id" \
        '.decisions | any(.[]; .worker_id == $id and .action == "notify_failed" and (.error | contains("no live Herdr pane")))' \
        "$evidence_dir/workers.stdout.json" >/dev/null \
        || die 'checker did not report the missing parent pane'
    else
      (( rc == 0 )) || die "watchdog exited with status $rc"
      jq -e --arg id "$worker_id" \
        '.decisions | any(.[]; .worker_id == $id and .action == "notified")' \
        "$evidence_dir/workers.stdout.json" >/dev/null \
        || die 'checker did not notify the live parent'
    fi
    ;;

  *)
    usage >&2
    exit 2
    ;;
esac

printf 'watchdog live check: %s passed\n' "$mode"
