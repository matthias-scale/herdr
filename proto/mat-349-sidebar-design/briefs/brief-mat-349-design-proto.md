# MAT-349 design prototype: T3-style herdr sidebar (prototype only, no Rust)
Repo matthias-scale/herdr, PR #620 branch feat/mat-349-project-first-sidebar (head 4e06ec137). Read src/ui/sidebar.rs and src/ui/sidebar/fleet_grouping.rs to learn the current project-first sidebar data (projects, workspaces/tabs, agent states idle/working/blocked/done, hosts ub1/ub2/mbpro, folding).
Prior approved direction mock: https://ubuntu-direct.tailc1d69d.ts.net/herdr-sidebar-grouping-proto/ ("project first, working hidden").
Task: produce ONE self-contained HTML prototype (inline CSS/JS, monospace terminal grid look, Nerd Font glyphs with plain-text fallback toggle) of an improved visual sidebar in the style of T3 Code's thread sidebar: project headers with collapse chevron + count, compact single-line thread rows (status dot, name, right-aligned relative age/host), active-row highlight bar, blocked rows promoted with accent, hover tooltips on every icon, a quiet "working" collapse per project. Show 3 variants (A conservative, B T3-faithful, C dense) side by side at 26/32/40 column widths, with realistic data (6 projects, ~25 agents, mixed states, 3 hosts). Each variant notes which existing herdr UI patterns it reuses and any new state it needs (TUI-only vs server).
Deliver: write the file to proto/mat-349/index.html inside your worktree (leave it as an untracked file; the orchestrator publishes it via /tsd). Do not publish, modify Rust, commit, push, or open PRs.

## Constraints from MAT-340 RCA (scalable-66, /tmp/mat-herdr/mat-340-rca-result.md)
- No ordering by activity time (projects or rows): sort on stable keys/identities carried across refreshes. ubuntu-4c fixes the base cause on main.
- Remote parked agents currently show as Done/idle (no parked flag in AgentInfo); ubuntu-4c owns that fix, so don't design around it.
- Remote rows are per pane, local rows per tab (sidebar.rs:3262).
- MAT-351 (fix/mat-340-cross-host-flicker-sync) changes remote activity timestamps + row tiebreaks; #620 merges main after it lands — sort logic must expect that.
