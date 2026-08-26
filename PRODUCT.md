# Product

Fork-local strategic context for `matthias-scale/herdr`. Upstream `ogulcancelik/herdr`
serves a general audience of developers running coding agents; this fork serves one
operator running a fleet, and the design decisions below follow from that narrower job.
Where the two disagree, this file describes the fork.

## Register

product

## Users

One operator running many coding agents in parallel — Claude Code, Codex and the rest —
across several repositories and worktrees at once. Agents run unattended for long
stretches; the operator is not watching them work and does not want to.

The operator's context when opening herdr is almost always the same: coming back after
being away, needing to find out what happened and what now needs a human. They are not
browsing. They arrived because something might be waiting.

## Product Purpose

**Herdr's job is to route attention, not to display information.**

The primary question it answers is *what needs me?* Everything else is downstream of
that. Once the operator knows where they are needed, herdr is the surface for three
kinds of work:

- **Unblocking** — answering an agent that stopped and needs a decision.
- **Planning** — thinking a feature through with an agent before it writes anything.
- **QA and deep problem-solving** — working a hard problem alongside an agent, in its
  own terminal, with its own context.

Success is the operator spending their attention on the agent that most needs it, and
spending none of it discovering which one that is.

Two surfaces serve this, and they are deliberately different:

1. **Overview** — the sidebar. Who is blocked, who is running, what exists. Scannable,
   dense, clickable. This is the map.

2. **Inbox** — a distraction-free mode that shows one thing: either "nothing is
   blocked", or the *oldest* blocker with its conversation inline, plus a count of how
   many remain. Answering it advances to the next blocked session. The count drains to
   zero and the mode says so. Spawning is part of this surface too: the prompt field is
   already open, the agent and workspace are already selected by default, so starting
   new work costs a paste rather than a setup ritual.

The overview is for orientation. The inbox is for clearing. Neither should grow into
the other.

## Brand Personality

**Dense, calm, factual.**

- **Dense** — information per row is high. This is a tool for someone who reads fast and
  already knows the domain. Whitespace is not a virtue here; wasted rows are.
- **Calm** — nothing manufactures urgency. No badges demanding to be cleared, no
  celebration when work finishes, no color screaming for attention that a glance would
  have caught anyway. A blocked agent is reported, not alarmed about.
- **Factual** — state is stated. `blocked 40m` beats `⚠️ Attention needed!`. When herdr
  does not know something it says `unknown` rather than guessing a friendlier answer.

The in-app voice is already consistent and should stay that way: lowercase, terse,
one to four words per label, no hedging, no exclamation points, periods only in full
sentences. The single deliberate exception is destructive confirmation, which switches
to sentence case and states consequences plainly ("Dirty or untracked files will be
permanently deleted."). Formality is reserved for the places where being misread costs
the operator something irreversible.

## Anti-references

- **IDE chrome.** VS Code-shaped nesting: ribbons, breadcrumbs, minimaps, panels inside
  panels. Herdr owns terminals; it is not the editor and must not grow into one. When a
  surface needs real editing, it hands off to `$EDITOR` rather than reimplementing it.
- **Chat-app notification anxiety.** Slack and Discord patterns — unread badges, red dots
  that exist to be cleared, celebratory toasts. These train dismissal, not reading, and
  they make a calm fleet feel like an emergency.
- **Observability dashboard.** Grafana-shaped panels, sparklines, gauges, KPI tiles.
  Metrics shown because they are available rather than because they change the next
  action. If a number does not redirect attention, it does not earn its row.

## Design Principles

1. **Route attention, don't display state.** Every surface should be judged by whether it
   changes what the operator does next. A number that informs but never redirects is
   decoration.

2. **Report, never dramatize.** State is reported at its true confidence. Say `unknown`
   when detection is uncertain. Never inflate a signal to make it noticeable, and never
   soften one to make the interface feel calmer than the fleet actually is.

3. **Evidence over inference.** Agent state comes from evidence — hooks, structured
   turn-end reports, anchored screen rules — not from pattern-matching incidental text.
   A confident wrong state is worse than an honest unknown, because the operator plans
   around it.

4. **Hand off rather than reimplement.** Editing goes to `$EDITOR`, agents stay the CLIs
   they already are, links open in the real browser. Herdr owns terminals and attention;
   everything else it delegates to the tool that already does it better.

5. **The next action should already be open.** Setup is friction between intent and work.
   Defaults that are right most of the time beat prompts that are right every time.

## Accessibility & Inclusion

**Never color alone.** Every state distinction must survive being read in a single
color. This is already half-built: `ui.status_indicators = "symbols"` gives blocked,
working, done, idle and unknown distinct static glyphs, while the default `dots` mode
renders working, blocked and idle-active as the same `●` differentiated only by hue.
Symbols carry the meaning; color reinforces it.

This is the stated bar and the only one. There is no WCAG programme here, no contrast
audit across the eighteen built-in themes, and claiming otherwise would be aspirational
rather than true. Treat this section as the floor, revisited if it ever proves too low.
