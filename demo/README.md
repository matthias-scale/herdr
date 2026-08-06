# Turn-end agent status → herdr

Surface what an agent is *actually* doing at the end of a turn as **three
independent channels** instead of one screen-scraped status.

## The question

When a response ends in `**Nothing to act on.**`, can an outside observer tell
whether that means *"idle, genuinely done"* or *"nothing for you, but agents are
still working"* — and separately, whether blockers exist?

**Yes.** The CLAUDE.md closing block already emits two orthogonal fields:

| Signal | Marker | Machine-readable |
|---|---|---|
| Human action needed | `**Critical action points (N blocking)**` vs `**Nothing to act on.**` | `N` = gate count |
| Still working | `N agents running: <name — topic>` vs `Done here.` | counted + named |

`Nothing to act on.` only replaces the *item list*, never the liveness line, so
`Nothing to act on.` + `3 agents running:` is legal and common.

## The gap

herdr has a first-class `Blocked` state (`src/detect/mod.rs`) with top lifecycle
priority, red dot, and a "needs attention" toast. But for Claude and Codex it is
derived purely from **screen scraping**:

- `src/detect/manifests/claude.toml:41-52` — an on-screen permission form
- `src/detect/manifests/codex.toml:7-12` — `Action Required` in the OSC title

That is *harness*-blocked: waiting on a keypress. It knows nothing about
*semantic* blocked — a **Gate**. Worse, `claude.toml:64-79` (`live_prompt_box`,
priority 950) sees the `❯` box the instant a turn ends and calls the pane idle,
even when three agents are still running.

Meanwhile `herdr:claude` is on the **reserved** source list
(`src/agent_resume.rs:81-93`), whose reported state is discarded and downgraded
to session identity only. That is why herdr's own managed Claude hook never
reports lifecycle.

## The contract

One payload, one transport, any coding agent:

```json
{"v": 1, "agent": "claude", "blocking": 2, "agents": 3,
 "gates": ["Merge #30"], "agent_names": ["reviewer A — round 4"]}
```

`blocking` counts items only a human can clear. `agents` counts those still
working. They are independent — `blocking=0, agents=3` is the state herdr could
not previously see.

**Hook is the trigger, JSON is the payload.** A turn-end hook fires
deterministically; a tool call the model has to remember every turn does not.
So the agent never has to maintain anything — its harness's existing turn-end
hook does.

**Transport is the herdr socket**, already injected into every pane as
`$HERDR_SOCKET_PATH` and already proven. A directory watcher would be new Rust
surface for no gain. A JSON mirror is written atomically to
`$XDG_STATE_HOME/herdr/agent-status/<pane_id>.json` so the last known status
survives restarts and is inspectable without the socket.

Any agent reports in one line from whatever turn-end hook it has:

```sh
herdr-status --agent codex --blocking 1 --agents 2
echo '{"blocking":0,"agents":3}' | herdr-status --agent opencode
```

## What's here

| File | Role |
|---|---|
| `herdr_status.py` | The contract: payload, state mapping, socket push, atomic mirror |
| `herdr-status` | Agent-agnostic CLI, no dependencies |
| `closing_block.py` | Parses a CLAUDE.md closing block into the contract's counts |
| `test_closing_block.py` | 6 unit tests over closing blocks lifted from real transcripts |
| `herdr-closing-block.py` | Claude Code `Stop` hook adapter |
| `herdr-codex-notify.py` | Codex `notify` handler adapter |
| `drive.py` | Feeds fixtures through the real hooks, reads herdr back over the socket |
| `run_demo.sh` | Boots an **isolated** herdr server and runs the above |
| `persistence_check.py` | Checks a reported state survives later detection ticks |

Isolation: every `HERDR_*` var is unset and the config/state/socket roots are
redirected before the server starts, so the live daily-driver server is never
touched. Unsetting the vars alone is not enough — herdr also autodetects a
running server.

## Results

**Parser** — 6/6.

**Live socket demo against the stock shipping binary** — 5/5:

| Entry point | Input | herdr status | tokens |
|---|---|---|---|
| Claude `Stop` hook | `Nothing to act on.` + 3 agents | `working` | `blocking=0 agents=3 idle=0` |
| Claude `Stop` hook | `Critical action points (2 blocking)` | `blocked` | `blocking=2 agents=2 idle=0` |
| Claude `Stop` hook | `Nothing to act on.` + `Done here.` | `idle` | `blocking=0 agents=0 idle=1` |
| Codex `notify` | same 2-gate block | `blocked` | mirror matches |
| Bare CLI as `opencode` | `--blocking 0 --agents 4` | `working` | mirror matches |

`state_labels` render as `gate ×2` and `3 agents` in the sidebar.

**This works on stock herdr with no Rust change**, because reports go under
`herdr:<agent>-closing-block` rather than the reserved `herdr:<agent>`.

**Live end-to-end against a real `claude` process** — the check that matters,
and the one that initially failed:

| Reported | herdr status | label |
|---|---|---|
| `blocking=0 agents=3` | `working` | `3 agents` |
| `blocking=2 agents=2` + gate | `blocked` | `gate ×2` |
| same, 3 detection ticks later | `blocked` | `gate ×2` |
| same, after focusing the tab | `blocked` | `gate ×3` |
| `blocking=0 agents=0` | `idle` | — |

**Blocked persists across visiting the tab** for free: `src/ui/status.rs:380`
and `src/ui/sidebar.rs:380` match `(AgentState::Blocked, _)`, ignoring `seen`.
`seen` only separates idle from done. A gate therefore stays red until a later
turn reports `blocking=0` — cleared by answering it, not by glancing at it.
This is the same behaviour a real permission dialog has.

**Fork change, part 1** (`src/detect/mod.rs`): `is_closing_block_source` admits
any `herdr:<agent>-closing-block` whose suffix names the agent it claims to
speak for, so a new agent adopting the contract needs no herdr release.
`hook_authority_is_effective` still independently requires that agent's process
to be present, so the shape match is not a trust hole.

**Fork change, part 2** (`src/terminal/state.rs`) — necessary, and missed on the
first pass. Part 1 alone armed a suppression that was never reachable: the
report was accepted at the RPC edge and dropped inside `TerminalState`, while
the caller still saw `{"type":"ok"}` because `handle_pane_report_agent`
discards the mutation. Two gates rejected it, both keyed on session identity:

1. `route_full_lifecycle_hook_report` requires an `AgentSessionRef`, but
   `agent_resume::session_ref_from_report` mints one only for sources on the
   `is_official_agent_source` allowlist. A closing-block source reports what the
   agent *said*, not where to *resume* it, so its ref is always `None` — the
   report fell through to a pending-replacement slot that nothing drains while
   the agent stays continuously detected.
2. `set_hook_authority_at` rejected it as an owner conflict on any pane where
   the real agent had already announced itself under `herdr:<agent>`.

Both now accept on process presence, the only guarantee this source claims.
`accept_hook_report` still enforces per-source sequence monotonicity.

Tests: `closing_block_source_matches_only_its_own_agent` (`src/detect/mod.rs`)
and `closing_block_authority_outranks_visible_idle_prompt_box`
(`src/terminal/state.rs`), the latter also asserting the authority does **not**
outlive the agent process.

The state.rs test originally passed a session ref the RPC can never produce and
a session announcement under its own source, so it stayed green while the live
path dropped every report. It now reports with `session_ref: None` against a
pane already owned by `herdr:claude`, and was confirmed to **fail without the
fix** before being accepted as a guard.

`cargo fmt` clean, `cargo clippy --all-targets` clean. Full `cargo test`
single-threaded, mine vs baseline: **13 failures on both sides, zero new**. The
13 are pre-existing codex-manifest cache drift (manifests are remote-updatable
and the local cache has diverged from test expectations).

## Two constraints found the hard way

1. **A full-lifecycle source is only honoured while the real agent process is
   confirmed in the pane** (`hook_authority_is_effective`,
   `src/terminal/state.rs`). A synthetic shell pane can never qualify — the
   report is stored and then ignored at read time, silently.
2. **A source must announce its own session before its state reports count.**
   Sequences are tracked per source, and herdr's managed hook announces under
   `herdr:<agent>`, so it does not cover us. `herdr_status.report` sends
   `pane.report_agent_session` at `seq-1` before `pane.report_agent` at `seq`.
   Without it the report is buffered and dropped with no error.

Also fixed: metadata tokens persist across reports, so a key omitted on a later
turn kept its old value — a finished pane kept advertising agents that had
already exited. Every key is now written every time.

## Not done

- **UI.** Blockers and running agents ride the existing single status field plus
  `state_labels`/tokens. Rendering them as separate sidebar columns is not built.
- The `Stop` hook is not yet wired into `~/.claude/settings.json`, so the live
  end-to-end run drove the contract by hand rather than from a real turn end.
  Wiring it means adding a sibling hook — herdr overwrites its own managed
  `herdr-agent-state.sh` on integration update.
- The Codex adapter is verified against a synthesised `agent-turn-complete`
  payload, not a live `codex` turn. Codex already has a `notify` entry on this
  machine, so wiring it means chaining, not replacing.

## Running it

```sh
bash demo/run_demo.sh                              # stock binary

PATH="$HOME/.local/zig-0.15.2:$PATH" cargo build   # herdr pins zig 0.15.2
HERDR_DEMO_BIN=target/debug/herdr DEMO_ROOT=/tmp/herdr-cb-fork bash demo/run_demo.sh
```
