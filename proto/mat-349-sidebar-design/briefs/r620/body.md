Agents from all hosts now share one folder per project in the sections layout. Each folder shows blocked, done / idle, then waiting rows, and folds its dimmed working rows independently. A header toggle switches to newest activity ordering and remembers the choice on the client.

Project headers keep the visible/total count, and projects containing only working agents stay at the bottom as a single dim collapsed line. Provider/device tokens, ages, host filtering, pinning, lifecycle coverage, and sub-agent working state remain in the existing projection/rendering pipeline. No server API fields or dependencies were added.

| AC | Evidence |
| --- | --- |
| UI1 | `three_hosts_share_one_project_and_sort_by_activity`: one case-insensitive project across three hosts, no status/device group headers, existing provider/device/age rendering. |
| UI2 | `four_states_order_and_working_default_collapse`, `markers_and_expanded_working_rows_are_distinct_and_dim`, and narrow row rendering at 18, 26, 28, 40 columns. Nerd Font status dots use existing red/green/blue palette; plain fallback is `!`, `+`, `?`, `~`. |
| UI3 | `fleet_sidebar_working_toggle_is_project_local_and_sort_preserves_host_filter`: mouse toggle and Enter on the selected working line; other project stays folded. Expanded working rows retain DIM. |
| UI4 | `project_headers_render_visible_total_and_working_only_summary`: `3 of 7` with working rows, `3` without. |
| UI5 | Header-render test plus `projects_sort_by_visible_urgency_then_age_and_working_only_stays_last`: one dim collapsed working-only project, ordered after visible projects. |
| UI6 | Input test covers sort toggle, persistence request and retained host filter. `fleet_sidebar_sort_round_trips_and_legacy_defaults_to_status` covers file persistence/default. Presentation swap tests cover client isolation. Device-character host filtering still works. |
| AC7 | Project urgency/activity ordering test and mixed-status projection test. |
| AC8 | Updated existing lifecycle coverage, pinned window, unknown-active, settled, sub-agent parent and host-filter isolation tests; 762/762 focused sidebar tests; visible-row release profile passed (four profiles; measurements below). |

Click check: the sections projection emits normal local windows as `Tab` rows, which were missing a focus route in the sidebar mouse handler. A sections-only hit route now focuses that row's pane. `fleet_sidebar_local_label_click_focuses_agent_and_working_fold_accepts_enter` clicks the label of an agent in a different workspace and verifies workspace, tab and pane focus. No failing-before input-test run was captured for the local-label route. Remote rows are not inert: the existing mouse-release handler returns `OpenFleetHost` with the agent reference; it remains unchanged.

Overlap: [#594](https://github.com/matthias-scale/herdr/pull/594) owns the Current/Repo layout. This change is independent and keeps that layout's behavior; shared sidebar/state/input edits are limited to the sections projection, client sort, folds, and focus route. The branch includes current main's MAT-342 warm-channel change.

Validation and snapshots follow below. These are TestBackend renders of the candidate code, not captures from an installed server. No Herdr server, daily-driver session or machine configuration was installed or restarted.


Rendered text snapshots (plain font fallback; default width is 26 columns). Blue working markers and expanded row dimming are asserted in renderer tests; text alone cannot show colors or DIM.

status, 26 columns:

```text
  all hosts ▾     status ⇅
  ▾ herdr 3 of 6
   ! billing review c2  4m
   + QA ready       c2  2m
   ? choose layout  c2  1m
     ~ 3 working ▸
  ▸ mable ~ 1 working
```

expanded, 26 columns:

```text
  all hosts ▾     status ⇅
  ▾ herdr 3 of 6
   ! billing review c2  4m
   + QA ready       c2  2m
   ? choose layout  c2  1m
     ~ 3 working ▾
   ~ ub1 long acti… x1
   ~ local long ac… c2  3m
   ~ mbpro long ac… xm  6m
  ▸ mable ~ 1 working
```

activity, 26 columns:

```text
  all hosts ▾   activity ⇅
  ▾ herdr 3 of 6
   ? choose layout  c2  1m
   + QA ready       c2  2m
   ! billing review c2  4m
     ~ 3 working ▸
  ▸ mable ~ 1 working
```

status, 28 columns:

```text
  all hosts ▾       status ⇅
  ▾ herdr 3 of 6
   ! billing review   c2  4m
   + QA ready         c2  2m
   ? choose layout    c2  1m
     ~ 3 working ▸
  ▸ mable ~ 1 working
```

expanded, 28 columns:

```text
  all hosts ▾       status ⇅
  ▾ herdr 3 of 6
   ! billing review   c2  4m
   + QA ready         c2  2m
   ? choose layout    c2  1m
     ~ 3 working ▾
   ~ ub1 long activi… x1
   ~ local long acti… c2  3m
   ~ mbpro long acti… xm  6m
  ▸ mable ~ 1 working
```

activity, 28 columns:

```text
  all hosts ▾     activity ⇅
  ▾ herdr 3 of 6
   ? choose layout    c2  1m
   + QA ready         c2  2m
   ! billing review   c2  4m
     ~ 3 working ▸
  ▸ mable ~ 1 working
```

Validation used `CARGO_TARGET_DIR="$PWD/.local/mat349-target"`, `TMPDIR=/tmp`, Zig 0.16.0, and four nextest workers. The isolated target avoids shared test binaries being replaced by another lane.

`~/.claude/skills/acode/scripts/check-wait --ceiling 1800 --tail 40 -- just check` passed formatting, all-target native Clippy, **7,972/7,972 native tests** (9 ignored), maintenance tests, architecture checks, and integration-asset/script checks. It stopped at the brief's known local Windows cross-lint archive limitation; no check or CI configuration was changed. `just docs-contract-test` passed 7 tests separately.

Native test summary:

```text
Summary [303.894s] 7972 tests run: 7972 passed (1 flaky, 1 leaky), 9 skipped
```

Exact check tail:

```text

  --- stderr

  thread 'main' (79197) panicked at build.rs:126:5:
  libghostty-vt library ghostty-vt-static.lib is missing from /home/ubuntu2/.cache/ra-libghostty-vt/48541a0e2fc77168d10902c6b2e7ff428a0f96ba-x86_64-linux-gnu-ReleaseFast/lib
  note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
warning: build failed, waiting for other jobs to finish...
error: recipe `windows-lint` failed on line 62 with exit code 101
exit=101
```

`cargo nextest run sidebar` passed **762/762** tests in the isolated target. `just bench-render-scale` passed all four release profiles in an isolated target.


The render fixture enables sections layout before expanding the working folder and asserts the expected working rows before sampling. Fixed geometry is 120×40, sidebar width 28; each sample invalidates the projection before drawing. These are the corrected visible-row measurements:

| Cardinality | Median 1 → 15 panes | Delta | Ratio | p95 1 → 15 panes |
| --- | --- | --- | --- | --- |
| Background workspaces (1 → 15 agent rows) | 282 → 847 µs | +565 µs | 3.00× | 318 → 1,020 µs |
| Active workspace (1 → 15 populated panes, one agent row) | 278 → 536 µs | +258 µs | 1.93× | 382 → 625 µs |

The latest native run retried `live_handoff_keeps_agent_started_pane_after_agent_exits` automatically under the existing nextest policy and passed on attempt 2; no manual CI job retry was used.

CI: all six checks passed on `3ef6a7e6e180f07e05fe6b02e0bea0d91da2596f` (Linux, Windows, macOS, Windows ConPTY packaging, conventional commits, validation). The pinned-head watcher returned `SETTLED`, exit 0, with no failed or pending checks.

Refs MAT-349

