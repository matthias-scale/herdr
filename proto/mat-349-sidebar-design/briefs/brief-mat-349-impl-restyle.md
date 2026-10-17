# MAT-349 implement: restyle the PR #620 sidebar to the approved round-7 prototype

Repo matthias-scale/herdr, branch feat/mat-349-project-first-sidebar (PR #620, head 4e06ec137). Commit on this branch and push it (never main, no force-push, no merge, no new PR).
Approved prototype (Matthias, 2026-10-16 "then you can build it"): https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-349-restyle/ — fetch it with curl and read the notes list and the JS `side()` function; it is the spec. Style reference: https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/reference.png

## Scope: styling only
Keep every #620 behaviour: project-first grouping, working-hidden fold per group, blocked promotion, folding keys, shelves, remote per-pane rows, views (Repos / Spaces / others), stable-identity ordering (MAT-340: never sort by activity time; MAT-351 will change remote timestamps later). Do not add or remove features beyond the list below. Reuse existing elements in code: status glyphs (`●` working, `○` blocked/idle, `◉` done-unseen, grey `?` unknown) from sidebar.rs ~line 320, section icons (sidebar.rs ~3925), repo icons `builtin_repo_icon` in src/ui/icons.rs, `ALL_PROJECTS_LABEL`, `show_host_identity`, `NO_REPO_YET_SECTION_TITLE`.

## Changes
1. Section order unchanged: Pinned → Blocked → repo/space groups (+ "No repo yet" group for agents with no detected repo) → Snoozed → Settled. No divider rules, no uppercase labels, no blank lines between sections (compact).
2. Section headers: collapse chevron at column 0 directly before the icon, then the title in muted bold, count right-aligned. Pinned, Blocked, Snoozed, Settled all collapsible. Blocked gets a new icon: nf-md-alert_octagon U+F0029, plain-text fallback `!` (it has none today). Every icon-only element has a tooltip and plain fallback.
3. Pinned and Blocked rows are ONE line each: state glyph, the row's repo icon (or chip), title, provider+machine token, age right-aligned. No project sub-lines inside these sections.
4. Group headers: chevron col 0, repo icon from `builtin_repo_icon` when known, else a 2-cell chip with first+last letter of the repo name (tinted cell background, coloured fg, stable colour from the name), name bold, muted `N of M`.
5. Rows: indent 2 cells, existing state glyph, title in regular dim text, token (provider icon + machine char, each hideable), muted age right-aligned. Focused row: full-width cell background + bold bright title. Remove "Show more": all non-working rows show.
6. Working lines (`● N working ›`) and groups whose agents are all working (single collapsed line `name ● N working`) render at ~50% opacity (dim/blended colour, terminal has no alpha).
7. Header row 2: view selector (`Repos ▾`/`Spaces ▾`), then a settings cog (nf-md-cog U+F0493, plain `*`) , then `All projects ▾` right-aligned. The cog opens a popup reusing the existing modal/popup pattern with: provider icon on/off, machine name on/off, and the machine picker (move the existing `all hosts ▾` scope picker from the Blocked header into this popup). Persist like today's equivalents (TUI-only state; do not add server/API fields).
8. Same styling applies in the Spaces view and other views that use these rows.

## Rules
- Follow CLAUDE.md: render is pure, no `unwrap()`, no new deps, multiplicative-path rules (no new per-row allocation/IO in render loops; if render work widens, run `just bench-render-scale` and report 1 vs 15 panes).
- Update/add unit tests with `AppState::test_new()` for: one-line Pinned/Blocked rows with repo icon, chip fallback letters, no Show more, 50%-dim working lines, cog popup contents, Blocked icon + fallback, No repo yet group.
- Build needs Zig 0.16.0: on ub1 `ZIG=~/.local/zig-0.16.0/zig-x86_64-linux-0.16.0/zig`. Run `just check-parallel` and report the result honestly.
- Commit style: lowercase conventional, e.g. `feat(sidebar): restyle project sidebar to t3 reference`, body `refs MAT-349`. No AI co-author lines.
- Capture `herdr` sidebar output before/after (e.g. `cargo run` in a throwaway session, `herdr pane read`) and save as text files in `.local/qa-349/` (ignored) — report their paths.
