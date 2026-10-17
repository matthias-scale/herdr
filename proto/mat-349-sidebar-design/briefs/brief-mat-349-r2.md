# MAT-349 fix round 1: snoozed/settled rows and headless sort persistence

Worktree: the ra-launch worktree (it is your cwd)
Branch: feat/mat-349-project-first-sidebar (PR #620, head 3ef6a7e6e180f07e05fe6b02e0bea0d91da2596f). Push new commits on top; no force-push, no rebase.
Repo: matthias-scale/herdr (fork; base branch `main`)
Zig 0.16.0: on ub2 `export ZIG=$HOME/.local/bin/zig`; on ub1 `export ZIG=$HOME/.local/zig-0.16.0/zig-x86_64-linux-0.16.0/zig`.
Verified commands (green in CI at 3ef6a7e): `just check`, `cargo nextest run sidebar`.
Known load-flaky tests (pass alone on main): `agent_wait_accepts_repeated_until_and_exits_when_one_status_matches`,
`machine_add_reports_strict_host_key_failure`, `shell_conditional_input_guards_a_real_idle_pty_shell`,
`pane_send_text_if_sends_only_to_the_observed_idle_shell`, `direct_primary_rgba_accepts_fullscreen_retina_frame`.

The original brief's AC table (UI1-UI6, AC7, AC8) and constraints still apply. The PR body has them.

## Fix 1 (P1, AC8 lifecycle accounting): snoozed, settled and past-done-threshold agents became visible "done" rows

Confirmed in code: `sidebar_rows_inner` (src/ui/sidebar.rs ~4640) chains `visible_entries`, `snoozed_entries`,
`settled_entries` and `remote_entries` into `fleet_grouping::rows`. `state_index` (fleet_grouping.rs:118) maps every
non-Active lifecycle to 1 (done), and `rows()` hides only state 3. So Snooze/Settle no longer hide anything in the
sections layout, stale agents from every host fill each project, a snoozed blocked agent shows a green marker, and they
inflate `<visible> of <total>`. Before this PR (MAT-344) they sat in collapsed shelves.

Required behaviour: non-Active lifecycle entries (snoozed, settled, past the done-hide threshold) are not visible project
rows, do not count in the project header's visible or total count, and do not affect project urgency/activity ordering.
They stay reachable the way they were before this PR. Preferred: keep them out of the fleet projection and keep
rendering them on the existing collapsed Snoozed/Settled shelves, with `collapsed_placement` placing them there so
coverage accounting has no guard rows. Do not invent new visible UI (a new per-project fold) unless the existing
shelves cannot work in the sections layout; if so, explain why in the PR body.

Tests: a fleet_grouping test with one snoozed, one settled and one past-threshold entry beside Active ones, asserting
they are not project rows, are excluded from counts, and are placed (collapsed_placement is Some). Restore the settled
/unknown-active test's original intent (its Tab/RemoteAgent row count went 2 → 3 in this PR; it should not).

## Fix 2 (UI6): sort choice must persist on the headless server/attach path

Today the fleet sort is loaded only in `App::new` (src/app/mod.rs ~1017) and saved only in the App loop
(~2506-2513). The headless per-client attach path (src/server/headless.rs ~5309-5318) loads group mode, collapsed
groups, work filter and group sorts into `client.sidebar_presentation` but not the fleet sort, and its persistence
drain (~5157-5185) never takes `fleet_sort_persistence_request`. Matthias runs Herdr through the server, so the sort
resets on reattach. Load and drain it on the headless path the same way the neighbouring fields are, and add a test on
that path (not only the file round-trip).

## Out of scope

Do not touch other review notes (dead `"herdr"` branch in collapsed_sidebar_placement, "projects" literal in
mobile.rs, numeric status indices, the duplicate mouse focus route). Do not change the Current/Repo layout.

## Constraints

- Do not install, restart or hand off any Herdr server on ub1/ub2; do not touch `~/.config/herdr` or `~/.local/bin/herdr`.
- No new dependencies. No `unwrap()` in production code. Do not edit CI, justfile or docs/next/CHANGELOG.md.
- Lowercase conventional commits, no AI co-author lines, body `refs MAT-349`.

## Done means

1. `~/.claude/skills/acode/scripts/check-wait -- just check` passes, or only the known local Windows-lint archive step
   fails; paste the tail.
2. Push (no force). Update the PR #620 body: add a "Fix round 1" section naming both fixes and their tests, and refresh
   the rendered text snapshots if output changed. Keep the rest of the body.
3. Run `shared/scripts/github-ci-watch --repo matthias-scale/herdr --pr 620 --expected-head <SHA>` once (fleet checkout
   ~/Repos/scalable-agent-fleet). Rerun a failed job once only for a listed flaky test; otherwise fix and push.
4. Stop at the updated PR. Do not merge.
5. End with the PR URL, new head SHA, and the exact command proving CI state.
