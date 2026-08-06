# Closing-block signals → herdr

Feasibility demo: surface the CLAUDE.md closing block to herdr as **three
independent channels** instead of one collapsed status.

## The question

When a response ends in `**Nothing to act on.**`, can an outside observer tell
whether that means *"idle, genuinely done"* or *"nothing for you, but agents are
still working"* — and separately, whether blockers exist?

**Yes.** The closing-block spec already emits two orthogonal fields:

| Signal | Marker | Machine-readable |
|---|---|---|
| Human action needed | `**Critical action points (N blocking)**` vs `**Nothing to act on.**` | `N` = gate count |
| Still working | `N agents running: <name — topic>` vs `Done here.` | counted + named |

`Nothing to act on.` only replaces the *item list*, never the liveness line, so
`Nothing to act on.` + `3 agents running:` is legal and common. Nothing needs to
be added to CLAUDE.md — the information is already there and unambiguous.

## The gap

herdr has a first-class `Blocked` state (`src/detect/mod.rs:11-20`) with top
lifecycle priority, red dot, and a "needs attention" toast. But for Claude and
Codex it is derived purely from **screen scraping**:

- `src/detect/manifests/claude.toml:41-52` — an on-screen permission form
- `src/detect/manifests/codex.toml:7-12` — `Action Required` in the OSC title

That is *harness*-blocked: waiting on a keypress. It knows nothing about
*semantic* blocked — a **Gate**. Worse, `claude.toml:64-79` (`live_prompt_box`,
priority 950) sees the `❯` box the instant a turn ends and calls the pane idle,
even when three agents are still running.

Meanwhile `herdr:claude` is on the **reserved** source list
(`src/agent_resume.rs:81-93`), whose reported state is discarded and downgraded
to session identity only. That is why herdr's own managed Claude hook
(`~/.claude/hooks/herdr-agent-state.sh`) never reports lifecycle.

## What's here

| File | Role |
|---|---|
| `closing_block.py` | Parses the closing block into `blocking` / `agents_running` / `done_here` |
| `test_closing_block.py` | 6 unit tests over closing blocks lifted from real transcripts |
| `herdr-closing-block.py` | Claude Code `Stop` hook → `pane.report_agent` + `pane.report_metadata` |
| `drive.py` | Feeds fixtures through the real hook, reads herdr back over the socket |
| `run_demo.sh` | Boots an **isolated** herdr server and runs the above |
| `persistence_check.py` | Checks whether a reported state survives later detection ticks |

Isolation: every `HERDR_*` var is unset and the config/state/socket roots are
redirected before the server starts. The live daily-driver server is never
touched. (Unsetting the vars alone is not enough — herdr also autodetects a
running server.)

## Results

**Python parser** — 6/6 pass.

**Live socket demo, stock shipping binary** (`~/.local/bin/herdr`) — 3/3 pass:

| Closing block | herdr status | tokens |
|---|---|---|
| `Nothing to act on.` + 3 agents running | `working` | `blocking=0 agents=3 idle=0` |
| `Critical action points (2 blocking)` + 2 agents | `blocked` | `blocking=2 agents=2 idle=0` |
| `Nothing to act on.` + `Done here.` | `idle` | `blocking=0 agents=0 idle=1` |

`state_labels` render as `gate ×2` and `3 agents` in the sidebar.

**This works on stock herdr with no Rust change**, because the hook reports under
`herdr:claude-closing-block` — not the reserved `herdr:claude`.

**Fork change** (`src/detect/mod.rs`): add `herdr:claude-closing-block` /
`herdr:codex-closing-block` to `full_lifecycle_hook_authority`. This is what stops
the `❯` prompt box from taking the pane back to idle at turn end. Covered by
`closing_block_authority_outranks_visible_idle_prompt_box` in
`src/terminal/state.rs`, which also asserts the authority does **not** outlive the
agent process.

Full `cargo test`, single-threaded, mine vs baseline: **13 failures on both
sides, zero new**. The 13 are pre-existing codex-manifest cache drift (manifests
are remote-updatable and the local cache has diverged from test expectations).

## Two constraints found the hard way

1. **A full-lifecycle source is only honoured while the real agent process is
   confirmed in the pane** (`hook_authority_is_effective`,
   `src/terminal/state.rs:2008-2013`). A synthetic shell pane can never qualify —
   the report is stored and then ignored at read time, silently. This is why the
   first fork run regressed to `unknown`: the mechanism was right, the test pane
   was wrong.
2. **A source must announce its own session before its state reports count.**
   Sequences are tracked per source, and herdr's managed hook announces under
   `herdr:claude`, so it does not cover us. The hook sends
   `pane.report_agent_session` at `seq-1` before `pane.report_agent` at `seq`.
   Without it the report is buffered and dropped with no error.

Also fixed during the demo: metadata tokens persist across reports, so a key
omitted on a later turn keeps its old value — a finished pane kept advertising
agents that had already exited. The hook now always writes every key.

## Not done

- **Codex.** The allowlist entry exists, but no Codex-side hook is written. The
  seam is `notify = [...]` in `~/.codex/config.toml` (already wired to a
  `turn-ended` handler), which is the equivalent of Claude's `Stop`.
- **UI.** Blockers and running-agents ride the existing single status field plus
  `state_labels`/tokens. Rendering them as genuinely separate columns in the
  sidebar is not implemented.
- Verified against a synthetic pane and Rust unit tests, not against a live
  `claude` process in a herdr pane end-to-end.

## Running it

```sh
# stock binary
bash demo/run_demo.sh

# forked binary
PATH="$HOME/.local/zig-0.15.2:$PATH" cargo build   # herdr pins zig 0.15.2
HERDR_DEMO_BIN=target/debug/herdr DEMO_ROOT=/tmp/herdr-cb-fork bash demo/run_demo.sh
```
