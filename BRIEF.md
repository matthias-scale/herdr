# Task: herdr fork sidebar — compact grammar

Repo: /tmp/wt-sidebar-grammar (git worktree, branch feat/sidebar-compact-grammar, base origin/fork/pr-base @4357b707)
Build: ZIG=/opt/homebrew/opt/zig@0.15/bin/zig cargo build --release --locked
Test:  ZIG=/opt/homebrew/opt/zig@0.15/bin/zig cargo test
Lint:  just lint

This is a FORK. Replace the old rendering outright; no config flag, no back-compat path.
Design is fully settled by the human — implement exactly what is below, do not redesign.

## 1. Single dot per row (kills every double-dot site)

Today two dots can render on one row:
- src/ui/sidebar.rs:105 `tab_blocker_dot_visible` — red blocker dot beside the state dot
- render_prio_panel_row (~sidebar.rs:2270) — peach prio dot beside the state dot

Collapse to ONE dot column. Shape = running or not. Colour = whose move it is.

| glyph | colour        | means                                        |
|-------|---------------|----------------------------------------------|
| `●`   | blue          | working                                      |
| `●`   | red           | working AND gate latched (running, owes you) |
| `○`   | red           | blocked / stopped, waiting on human          |
| `◆`   | teal          | done, unread (Idle + !seen)                  |
| `○`   | grey/green    | idle, seen                                   |
| `·`   | dim overlay0  | plain shell, no agent                        |

Reuse the existing palette from ui/status.rs `state_label_color` — Blocked=red,
Working=blue, Idle+!seen=teal, Idle+seen=green, Unknown=overlay0. Do not invent colours.
When gate count > 1, render the dot with the count appended: `●2`. Count == 1 renders bare `●`.
DELETE the words: "working", "done", "idle", "blocked", "stale", "gate", "gates".
DELETE the reserved blocker-dot gutter width and the prio dot.

## 2. Provider mark column

`tab_agent_suffix` (sidebar.rs:38) already maps Codex->cx, Claude->cc, Pi->pi.
ADD: `Agent::Kimi => "ki"` (the enum variant already exists at detect/mod.rs:60).

Provider mark is coloured by provider: Claude=peach, Codex=green, Pi=mauve, Kimi=yellow.
Composition, in this exact order, right-aligned in an 8-col field:

    cc          agent, no live shell, no sub-agents
    cc+2        2 live sub-agents
    cc >_       agent holding at least one live shell
    cc+2 >_     both

`>_` ALONE (no provider) is the row marker for a plain terminal with no agent.
It replaces the literal word "Terminal" everywhere it is rendered as a row label.

DELETE the foreground process name token (" · claude.exe", " · node") entirely.
DELETE the agent version token (e.g. "2.1.250") from every row.
DELETE the word "reported" and the suffix " ago" from the age column; render "3m", "22h", "—".

## 3. Live-shell detection (new)

`background_job_count` (detect/mod.rs:148) is Codex-only screen-scrape. DELETE it and its
`N >_` badge, and remove `background_jobs` from TabRowLayout.

Replace with a provider-agnostic signal built on the EXISTING primitive
`crate::platform::session_processes(child_pid)` (implemented in platform/macos.rs,
platform/linux.rs, platform/fallback.rs). A pane "holds a shell" when the pty session
contains at least one live pid that is neither the pane's own shell pid nor the agent
foreground pid. Surface it as a new bool on AgentPanelEntry (`holds_shell`).
Poll it on the same tick that already calls `foreground_job`; do not add a new poll loop.

## 4. Stale resolves away (peach `!` deleted from the sidebar)

`supervisor_stale` (terminal/state.rs:1368) currently renders as peach `!` via
`state_icon_with_stale` (ui/status.rs:503). Stale is doubt about a state, not a state.
Resolve it with the same `session_processes` primitive:

| pty session contains                   | resolve to |
|----------------------------------------|------------|
| no agent process                       | done  (Idle, !seen) |
| agent process + live descendants       | working    |
| agent process, nothing under it        | idle  (Idle, seen)  |

Keep `stale` as an internal flag; it must no longer produce a glyph or a colour.
IMPORTANT: when a row's state came from this fallback rather than a live hook,
SUPPRESS the `+N` sub-agent badge — a stale transcript's count is a claim about the past.
Render `cc`, never `cc+2`, on fallback-resolved rows.

## 5. Blocked filter toggle replaces the Blocked and PRIO sections

DELETE the pinned "Blocked (N)" section and the "PRIO" section from the sidebar entirely.
Rename the top-bar `inbox N` label to `blocked N`. That label is now a TOGGLE:
clicking it hides every non-blocking row (keep rows whose dot is red); clicking again restores.
Space headers keep their `N/M` counts in both states so the filter never hides scale.
The top-bar `note`/`dock` pair currently toggles together — keep only `dock`.

Also bind a key. Prefer `prefix+f` (filter); if BindingRegistry reports a conflict,
fall back to `prefix+m`. Add it to the keybinds config with a description and to any
keybind reference doc the repo generates (scripts/config_reference_check.py must pass).

## 6. Space auto-naming

Space header label resolution order:
1. manual label, if the user set one — always wins, never overridden
2. the title of the FIRST NON-TERMINAL (agent) session in that space's row list
3. the existing git `auto_label` (workspace.rs:70) as fallback when the space holds only terminals

Derived names (case 2) render dim so they are distinguishable from manual ones.
When two spaces still resolve to the same string, suffix a superscript window index (¹²³).

## 7. Row grid

    dot 3 │ title flex │ provider 8 right │ age 4 right

Apply to ALL renderers that draw agent rows: tab rows (`tab_row_layout` /
render around sidebar.rs:2940-3020), the prio panel row renderer, and `render_agent_card`
(sidebar.rs:3060-3170). They currently each re-implement padding; keep them consistent.

## Constraints

- Rust edition/toolchain per rust-toolchain.toml. `cargo build` needs ZIG as above or it
  fails in the build.zig step.
- src/ui/sidebar.rs is ~8700 lines and heavily tested. Update the existing tests to the new
  grammar rather than deleting them. Add tests for: single-dot invariant (no row emits two
  dot glyphs), `ki` mapping, `+N` suppression on fallback-resolved rows, `>_` in both
  positions, filter toggle hides only non-red rows, and space-name resolution order.
- Verify glyph widths: `●○◆·>_` are all width 1 in the shipped font; `◐ ◑ ◍ ⬤ ②` are NOT
  present in JetBrainsMono Nerd Font — never use them.
- Conventional commits, one concern per commit. Do NOT push, do NOT open a PR, do NOT merge.
  Stop when `cargo test` and `just lint` are green and report the artifact path.

## Standing lane rules

1. Wait discipline: never enter a "wait for agents" loop unless you spawned one. One wait
   mechanism per task; never duplicate a wait already running.
2. One agent = one worktree. This worktree is yours; do not touch ~/Repos/herdr itself.
3. Checkpoint floor: at <=15% context remaining, write a handoff file before any other action.
4. No history surgery: never cherry-pick --skip, rebase-drop, or discard a commit.
   Conflict -> stop, record it, escalate.
