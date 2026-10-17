# MAT-349: project-first sidebar with working hidden per project

Worktree: the ra-launch worktree (it is your cwd)
Branch: feat/mat-349-project-first-sidebar (pushed; equals origin/main at 2f04f2746, which includes the merged MAT-344 PR #615)
Repo: matthias-scale/herdr (fork; base branch `main`)
Zig 0.16.0: on ub2 `export ZIG=$HOME/.local/bin/zig`; on ub1 `export ZIG=$HOME/.local/zig-0.16.0/zig-x86_64-linux-0.16.0/zig`.
Verified commands (green in CI on main 2f04f2746): `just check`, `cargo nextest run sidebar`.
Known load-flaky tests (pass alone on main): `agent_wait_accepts_repeated_until_and_exits_when_one_status_matches`,
`machine_add_reports_strict_host_key_failure`, `shell_conditional_input_guards_a_real_idle_pty_shell`,
`pane_send_text_if_sends_only_to_the_observed_idle_shell`, `direct_primary_rgba_accepts_fullscreen_retina_frame`.

## Approved prototype (Matthias signed off 2026-10-16: "i like this approach build it")

https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-grouping-proto/ (fetch it; tailnet-reachable). The target is
the first section, "your direction: project first, working hidden" (three mocks d). The other sections are
rejected alternatives; ignore them. The mocks are the spec for structure, order, markers and dimming. Reuse the
existing sidebar styling (colors, icons, provider + device token, age column); do not copy HTML colors.

## Goal

MAT-344 (PR #615) built the unified cross-host projection in `src/ui/sidebar/fleet_grouping.rs`:
state → project → agent (`rows()`, `state_index()`, `collapsed_placement()`, `project_section_key()`).
Invert it to project → agent, with status as a per-row marker and working rows hidden per project.

| ID | Observable clause | Verification |
| -- | -- | -- |
| UI1 | Top level is one folder per project across all hosts; no state-level group headers. Rows keep provider icon + one-char device code + age. | projection test in `sidebar::fleet_grouping` |
| UI2 | Inside a project, rows sort blocked → done(/idle) → waiting, then by age within each status. Each row starts with a status marker: blocked red ●, done green ●, waiting `?`, working blue ●. Plain-text fallback keeps a distinct char per status. | projection + render test |
| UI3 | Working rows are hidden by default behind one dim line per project, `● N working ▸`. Activating it (mouse click and keyboard Enter on the selected line) toggles that project's working rows only; rows stay dimmed while expanded. Expansion state is TUI/client presentation state, not server state. | input + render tests |
| UI4 | Project header count reads `<visible> of <total>` when the project has working rows, else `<visible>`. | render test |
| UI5 | A project with only working agents renders as one dim collapsed line (`▸ project ● N working`) after all projects with visible rows; activating it expands like UI3. | render test |
| UI6 | A header toggle switches sort between `status` (UI2 order) and `activity` (rows and projects by age, newest first; working still hidden). Default `status`. The existing host filter (`all hosts ▾`, device-char click) still works in both modes. Persist the sort choice the same way `sidebar_blocker_scope` persists (`src/client/presentation.rs`), with a legacy-default test. | input + persistence tests |
| AC7 | Projects ordered by their most urgent visible status, then by age (status mode). | projection test |
| AC8 | Existing behaviour kept: lifecycle/coverage accounting, pinned windows, sub-agent parents staying working, unknown-active → waiting, host-filter isolation between clients, render-scale profile. Update the existing `fleet_grouping` tests to the new shape rather than deleting their intent. | `cargo nextest run sidebar` |

Also check (report, fix only if caused by this projection): Matthias reported that clicking agent rows in the
unified sidebar did not open the agent. Reproduce with a unit/input test on a local agent row: clicking the label
area must focus that agent's pane as it did before MAT-344. If it already works for local rows, say so and note
whether remote-host rows are expected to be inert.

Repo UI rules (CLAUDE.md): reuse existing patterns, icons with tooltips and plain-text fallback, render is pure
(no state mutation in render), no new server API fields, multiplicative render path: no per-row allocation or I/O
beyond what rows do today.

Overlap: open PR #594 (MAT-333, compact project sidebar for the Current/Repo layout, owned by another session)
touches `src/ui/sidebar.rs`, `src/app/input/sidebar.rs`, `src/app/state.rs`. Do not depend on it; keep edits
outside `fleet_grouping.rs` minimal so it can merge cleanly. Do not change the Current/Repo layout.

## Constraints

- Do not install, restart or hand off any Herdr server on ub1/ub2; do not touch `~/.config/herdr` or `~/.local/bin/herdr`.
- No new dependencies. No `unwrap()` in production code.
- Do not edit CI, justfile or docs/next/CHANGELOG.md.
- Lowercase conventional commits, no AI co-author lines, body `refs MAT-349`.

## Done means

1. `~/.claude/skills/acode/scripts/check-wait -- just check` passes, or only the known local Windows-lint archive
   step fails; paste the tail.
2. Push (no force) and open a PR against `main` titled `feat(MAT-349): project-first sidebar with working hidden`.
   PR body: plain-words summary, AC table with evidence, rendered text snapshots (status sort default, one project
   expanded, activity sort) at default width and 28 columns, the click-check result, the #594 overlap note,
   and `Refs MAT-349`.
3. Run `shared/scripts/github-ci-watch --repo matthias-scale/herdr --pr <N> --expected-head <SHA>` once (fleet
   checkout ~/Repos/scalable-agent-fleet). Rerun a failed job once only for a listed flaky test; otherwise fix and push.
4. Stop at the open PR. Do not merge.
5. End with the PR URL, head SHA, and the exact command proving CI state.
