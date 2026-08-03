# Sidebar lifecycle QA journeys

Herdr is a terminal application. These journeys provide executable behavioral
evidence, not a browser preview or visual screenshot claim. Every command must
run against the full Git `HEAD` it names and writes a durable, head-bound local
assertion artifact under `QA_ARTIFACT_DIR` (or an explicit `--artifact-dir`).

Use the declaration at `.qa/preview-adapter.json` to create a terminal contract
card. Its `preview_url` is intentionally empty: Herdr has no web preview.

## sidebar-all-tab-retention

The sidebar projection retains every tab in expanded and compact presentation:
inactive, completed, singleton, multi-pane, and agentless tabs remain present.
Compact mode changes presentation only; it does not choose only the latest
active tab.

Evidence: `initial_sidebar_projection_keeps_every_workspace_tab_in_expanded_and_collapsed_views`.

## space-first-single-line-sidebar

The only sidebar projection is Spaces. Each tab/window appears exactly once
under its owning space, whether it is agentless, inactive, completed, or owns
multiple panes. Each row is one line with status before its sole title; it has
no agent/model child row or subtitle. The selected space and tab use darker,
bolder title text. Each agent-backed tab ends with a right-aligned elapsed age
for the latest user/agent communication, truncating the title first when space
is limited. A multi-pane tab rolls up its strongest lifecycle state and latest
communication timestamp, clicking the row preserves that tab's focused pane,
and agent navigation scrolls the owning tab row into view. Desktop, compact,
and mobile presentations retain the same ownership topology.

Evidence: `ac1_ac2_ac3_ac4_cumulative_space_first_single_line_fixture`,
`ac4_tab_rollup_does_not_let_done_mask_working`,
`ac4_clicking_tab_row_preserves_that_tabs_focused_pane`,
`tab_rows_show_working_then_done_lifecycle_text`,
`multi_pane_tab_age_uses_latest_thread_communication`,
`default_space_workspace_style_tracks_active_state`,
`ac1_ac2_ac3_mobile_tabs_are_status_first_single_line_rows`,
`ac4_mobile_sidebar_tab_click_preserves_tabs_focused_pane`, and
`review_findings_agent_navigation_reveals_against_final_picker_projection`.

## same-session-title-replacement

A later `UserPromptSubmit` from the same agent session selects its latest
meaningful final paragraph for one fresh persisted title. A terse/meta
continuation retains that session's initial briefing subject instead of
manufacturing a generic label. It does not append a subtitle or duplicate
title.

Evidence: `terse_later_user_prompt_submit_retains_the_session_initial_title`
and `work_title_initial_briefings_are_session_scoped_and_later_objectives_replace_them`.

## reopen-clears-done-without-reorder

Opening a completed pane clears its done presentation while the canonical
workspace/tab order remains stable.

Evidence: `api_pane_focus_marks_already_focused_done_pane_seen` and
`initial_sidebar_projection_keeps_every_workspace_tab_in_expanded_and_collapsed_views`.

## working-latches-until-genuine-completion

Authoritative lifecycle working state remains latched when fallback idle evidence
appears. A genuine completion transition is the only event that changes it to
done.

Evidence: `fallback_idle_does_not_override_full_lifecycle_hook_working` and
`full_internal_event_queue_eventually_applies_working_to_idle_transition`.

## Running one journey

```sh
scripts/qa-sidebar-lifecycle-journey.sh \
  --repo-dir "$PWD" \
  --head "$(git rev-parse HEAD)" \
  --flow sidebar-all-tab-retention \
  --artifact-dir .local/qa/sidebar-all-tab-retention
```

The result is a `qa-journey-result/v1` JSON document on stdout. Exit `0` means
the named assertion passed, `64` is invalid input, `66` is an exact-head binding
failure, and any other nonzero code is an assertion failure. The runner never
starts a Herdr session, changes a live configuration or hook, or emits prompt
content.
