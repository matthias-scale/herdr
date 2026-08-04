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

The only sidebar projection is Spaces. A repository and its linked worktrees
render as one Space row, with disclosure immediately before the title and the
direct window count immediately after it. Each tab/window appears exactly once
directly under that Space, whether it is agentless, inactive, completed, or
owns multiple panes; linked worktrees never add intermediate rows. Each window
row is one line with agent lifecycle status before its sole title; agentless
rows omit lifecycle status, and no row has an agent/model child or subtitle.
Single-provider Claude, Codex, and Pi rows append `· cc`, `· cx`, or `· pi`
after the title. Mixed-provider, unsupported, agentless, and exited-agent rows
omit that suffix. A nonzero Codex-reported background-terminal count appears as
`N >_` after the suffix, sums across panes in that tab, and never changes
lifecycle state or ordering. The count clears when Codex exits, even if the
screen content does not change. Working status and its live age use the
blue activity accent; unread completed work shows done, and opening that tab
removes the status instead of replacing it with idle. Warning and
machine-status colors are unchanged.
The selected space and tab use darker,
bolder title text. Each agent-backed tab ends with a right-aligned elapsed age
for the latest user/agent communication. When space is limited, age,
background count, and lifecycle text disappear in that order before the title
is truncated, while the provider suffix remains reserved. A multi-pane tab
rolls up its strongest lifecycle state and latest
communication timestamp, clicking the row preserves that tab's focused pane,
and agent navigation scrolls the owning tab row into view. Complete Space groups
have one compact blank row between them by default, while direct tab/window rows
remain adjacent. Desktop, compact, and mobile presentations retain the same
ownership topology.

Evidence: `ac1_ac2_ac3_ac4_cumulative_space_first_single_line_fixture`,
`state_dots_and_labels_use_semantic_workspace_colors`,
`working_summary_uses_blue_activity_accent`,
`ac4_tab_rollup_does_not_let_done_mask_working`,
`ac4_clicking_tab_row_preserves_that_tabs_focused_pane`,
`tab_rows_show_working_then_done_lifecycle_text`,
`seen_idle_tab_omits_status_while_retaining_title_and_clock`,
`multi_pane_tab_age_uses_latest_thread_communication`,
`headless_activity_clock_streams_a_new_frame_at_the_age_boundary`,
`active_title_color_darkens_rgb_themes_and_preserves_terminal_fallbacks`,
`default_space_workspace_style_tracks_active_state`,
`defaults_show_only_thread_titles_and_space_names`,
`desktop_worktree_group_renders_one_space_row`,
`active_linked_window_darkens_its_root_space_title`,
`linked_worktrees_render_as_one_space_with_direct_window_rows`,
`desktop_worktree_group_has_no_intermediate_connector_rows`,
`final_space_row_ignores_legacy_custom_token_rows`,
`tab_background_jobs_sum_across_panes_without_adding_rows`,
`tab_background_job_badge_renders_immediately_after_title`,
`background_jobs_change_does_not_change_lifecycle_or_seen_state`,
`background_scan_runs_when_the_agent_process_exits_without_new_content`,
`codex_background_job_count_uses_live_footer_only`,
`background_job_count_is_unknown_for_unsupported_agents`,
`tab_provider_suffixes_distinguish_codex_and_claude_after_title`,
`pi_uses_pi_suffix_while_unsupported_and_agentless_tabs_omit_it`,
`mixed_provider_tab_omits_provider_suffix`,
`unseen_agentless_tab_omits_lifecycle_status`,
`completed_agent_process_exit_retains_done_without_provider_suffix`,
`space_row_gap_separates_flattened_groups`,
`space_row_gap_separates_groups_but_never_tabs_inside_them`,
`ac1_ac2_ac3_mobile_tabs_are_status_first_single_line_rows`,
`seen_idle_mobile_tab_omits_status_segment`,
`ac4_mobile_sidebar_tab_click_preserves_tabs_focused_pane`, and
`review_findings_agent_navigation_reveals_against_final_picker_projection`.

## same-session-title-replacement

A later `UserPromptSubmit` from the same agent session selects its latest
meaningful final paragraph for one fresh persisted title. A terse/meta
continuation retains that session's initial briefing subject instead of
manufacturing a generic label. It does not append a subtitle or duplicate
title.

Evidence: `ac1_ac7_title_hooks_forward_the_full_fixture_to_the_owning_binary`,
`turn_start_fixture_reaches_guarded_herdr_title`,
`terse_later_user_prompt_submit_retains_the_session_initial_title`, and
`work_title_initial_briefings_are_session_scoped_and_later_objectives_replace_them`.

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
