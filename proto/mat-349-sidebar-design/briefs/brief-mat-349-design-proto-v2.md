# MAT-349 design prototype v2: terminal translation of the approved T3 reference sidebar (prototype only, no Rust)
Repo matthias-scale/herdr, branch feat/mat-349-project-first-sidebar (head 4e06ec137). PR #620 already implements a project-first sidebar (src/ui/sidebar.rs, src/ui/sidebar/fleet_grouping.rs).

Visual target (Matthias, 2026-10-16: "it should look closer to this"):
- Reference prototype: https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/ and its interactive page prototype.html there (fetch with curl).
- Matthias's T3 Code screenshot: https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/reference.png
- Rendered reference prototype: https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/desktop.png
Read the typography companion too: https://ub2.tailc1d69d.ts.net/herdr-t3-typography/ (regular-weight titles, muted grey ages right-aligned, muted group headers, bold only for the focused row, one dot glyph whose colour carries state).

Key look: search row with ⌘K hint; "Projects" header with sort/new icons; project rows = chevron + coloured 2-letter badge + bold-ish repo name + muted "N projects"; indented thread rows = optional state dot + regular title + right-aligned muted age; "Completed" small green label; focused row = filled rounded highlight; "Show more" per project; collapsed projects as single lines; generous spacing, no box-drawing clutter.

Task: one self-contained HTML page that shows this design as it would render INSIDE herdr's terminal grid (monospace cells, 24-bit colour, no pixel fonts or rounded pills unless approximated with cell backgrounds), side by side with the reference at the same scale, at sidebar widths 32 and 40 columns, dark theme. Show: expanded/collapsed projects, focused row, blocked row, working/idle/done dots, Show more, search row, remote host rows. Under each, a short "terminal compromises" note (what can't match the reference and how it is approximated), and Nerd Font vs plain-text fallback glyphs.
Data constraints: order projects/rows by stable identity, never activity time (MAT-340 flicker); remote rows are per pane, local per tab; parked remote agents show as idle (known gap, owned elsewhere); ages are display-only.
Deliver: write proto/mat-349/index.html in your worktree (untracked). Do not publish, modify Rust, commit, push, or open PRs.

## Scope clarification (Matthias, 2026-10-16)
Keep every PR #620 feature and behaviour as is (project-first grouping, working-hidden fold, blocked promotion, folding keys, shelves, remote rows). Only the styling adopts the 13 Oct reference. Do not add, remove or reorder features.

## Round 2 corrections (orchestrator review of round 1 output)
Round 1 (kept at proto/mat-349/index.html as starting point if present) missed:
1. Left column must embed the ACTUAL reference images (<img src="https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/desktop.png"> and reference.png), not a monospace redraw.
2. Show today's #620 sidebar beside the proposal using the same data: derive it from src/ui/sidebar.rs + src/ui/sidebar/fleet_grouping.rs render code at this head (row glyphs, section headers, working-hidden fold line, blocked promotion, shelves). Three columns: reference image | #620 today | proposed restyle.
3. The proposal must carry every #620 feature unchanged (project-first grouping, working-hidden fold with count, blocked rows promoted, folding, shelves, remote per-pane rows) and include a focused row (full-row cell background, bold title) and a blocked row.
4. Project badges as coloured 2-letter chips: cell background colour + contrasting fg, like the reference.
5. Glyph fallback: render the plain-text variant by default (Nerd Font glyphs show as tofu without the font); offer a toggle.
Only styling changes vs #620. Write proto/mat-349/index.html (overwrite).
