# Auto-naming and tab/sidebar title convergence

Status: investigation and design for `research/auto-naming`.

## Findings

### Naming path today

There are two title paths in the repository; they must not be treated as one
source.

The newer hook path derives a work title in the server-facing runtime:

- Claude installs `UserPromptSubmit -> herdr-agent-state.sh title` in
  `src/integration/claude_settings.rs:97-104`; Codex installs the equivalent
  hook in `src/integration/targets.rs:177-195`.
- The shell emitters read hook stdin and invoke `herdr agent turn-title
  --provider ...` in `src/integration/assets/claude/herdr-agent-state.sh:10-31`
  and `src/integration/assets/codex/herdr-agent-state.sh:10-31`.
- `src/cli/agent.rs:40-135` validates Herdr/pane/provider identity, guards the
  session, and sends `pane.report_agent_session` followed by
  `pane.report_metadata`.
- `src/work_title.rs:52-105` accepts only `UserPromptSubmit`, rejects Claude
  subagents, requires a session id, and derives work context from the prompt.
  `calculate_work_title` at `src/work_title.rs:107-127` sanitizes the prompt,
  selects objective words, takes at most seven words, and caps the generated
  title at `WORK_TITLE_MAX_CHARS` (48) without cutting a word.
- The wire shape is `PaneReportMetadataParams.title` and `.work_context` at
  `src/api/schema/panes.rs:492-524`. The server validates the source/session
  guards at `src/app/api/panes.rs:1374-1426`, rejects stale sequence numbers at
  `src/app/api/panes.rs:1496-1505`, resolves the session title at
  `src/app/api/panes.rs:1539-1551`, replaces hook work context at
  `src/app/api/panes.rs:1554-1567`, and emits metadata at
  `src/app/api/panes.rs:1601-1615`.
- `AppState` applies that event to terminal metadata at
  `src/app/actions.rs:3056-3086`. Effective work context uses manual >
  hook-turn > git > restored-fallback precedence at
  `src/work_context.rs:374-418`.

The older closing-block mirror path derives the raw title seen in the supplied
status sample:

- `src/integration/assets/closing-block/herdr-codex-notify.py:50-78` preserves
  an existing mirror title, then walks the current turn's input messages and
  returns the first non-preamble line at `:71-77`.
- The visible defect is the fixed `line[:80]` at `:77`: it cuts paths and words
  at an arbitrary character boundary. The sample ending in `/or` is this
  failure mode.
- `src/integration/assets/closing-block/herdr_status.py:168-220` places that
  value in the JSON mirror's `title` field. The socket metadata at `:243-267`
  carries it only as the `session_title` token at `:250`, not as canonical
  `pane.report_metadata.title`. The mirror is written under
  `~/.local/state/herdr/agent-status` by `:103-136`; it is an external status
  artifact, not the TUI source of truth.

The current runtime display path is therefore:

`hook input -> PaneWorkContext.work_title -> terminal effective work context
-> Tab::work_context_display_projection -> TabDisplayProjection::full_label`.

The relevant code is `src/workspace/tab.rs:244-277`,
`src/workspace/tab.rs:57-79`, and `src/workspace.rs:510-523`. The workspace
resolver gives stored `custom_name` precedence. The tab bar reads this
projection at `src/ui/tabs.rs:90-109`; sidebar entries read the same projection
at `src/ui/sidebar.rs:490-503`. Non-TUI tab information also uses it at
`src/app/creation.rs:331-342`.

`TerminalState::effective_title` is a separate pane/border metadata surface
(`src/terminal/metadata.rs:297-309`, newest-title precedence at `:422-427`).
It is not the work-context title consumed by the tab projection.

### Divergence and precedent

The historical cause was multiple consumers resolving labels through different
chains. `feat/tab-title-sync` (`fba16bbc`) records that the tab bar, sidebar,
navigator, mobile view, and API previously did not share a resolver; it routed
them through `TabDisplayProjection`. The current branch has that semantic
convergence and regression coverage at `src/ui/sidebar.rs:4581-4673`.

Two divergences remain possible if the contract is not explicit:

- The closing-block mirror's raw first-line `title` can disagree with the
  server work title consumed by the TUI.
- The tab bar and sidebar have different widths and decorations. The tab bar
  uses component-aware fitting in `src/ui/tabs.rs:38-87`; the sidebar truncates
  its full label at `src/ui/sidebar.rs:217-224` and may append a provider
  suffix. Their visible fragments can differ even when their semantic name is
  identical.

The fix is one semantic source, not two competing title fields:
`Workspace::tab_display_projection` is the canonical tab/row projection. A
human `custom_name` is the higher-precedence override; hooks never write it.
The older precedence fixes support this boundary: cwd-only terminal-title
filtering is in `5aae6909` (`src/workspace/tab.rs:129-217`), and the stale
rename-modal fix is in `d2f57235`.

## Proposed design

### Source and precedence

The server-owned neutral fact is the pane's effective `work_title` in
`PaneWorkContextState`, with hook/session provenance held by the terminal. The
tab-level human override remains `Tab.custom_name`. Resolve the canonical name
in this order:

1. human `Tab.custom_name`;
2. explicit manual pane label/work-title input;
3. an informative agent title, after rejecting cwd/agent identity noise;
4. the deterministic hook-derived work title;
5. ticket/agent-only fallback, then the structural tab number for no useful
   agent context.

Auto naming never writes `custom_name`. Every consumer receives the same
`TabDisplayProjection`; fitting and status/provider decoration stay local to
the surface.

### Timing, anti-flap, and cost

Naming is two-stage. At spawn there is no reliable purpose, so keep a structural
tab number or existing human name. On the first root-agent `UserPromptSubmit`,
derive a deterministic candidate and publish it with the guarded session id.
A later update is allowed only from a higher-confidence explicit title or a
strictly stronger deterministic signal such as a ticket/PR, and at most once
per session. Ordinary later prompts do not rename the row.

The session id, source, monotonic sequence, and stored initial subject form the
anti-flap boundary. A no-op/low-confidence first candidate may be replaced once
by the first stronger candidate; repeated prompt hooks are idempotent. This is
the intended follow-up to the current `resolve_work_title_for_session` path at
`src/terminal/state.rs:490-520`, which currently accepts a new non-empty
candidate on every turn.

The hook invokes one local Herdr CLI process per supported root-agent prompt.
The calculation is deterministic Rust string processing: model calls zero,
model tokens zero, no network summarizer. A future model summarizer would need
explicit opt-in and a session budget, never a per-turn default.

The hook can observe provider, session id, root/subagent identity where the
provider supplies it, and the submitted prompt. It cannot know the eventual
work outcome at spawn; later evidence is accepted only within the anti-flap
rule.

### Matching invariant and truncation

For every tab `T`:

`sidebar_entry(T).primary_tab_label == Workspace::tab_display_name_from(T)`

and the tab bar fits that same projection rather than resolving another source.
Width fitting may produce different visible truncation because surfaces reserve
different columns; it must never change semantic name or precedence. Tests cover
auto title, human custom title, split tabs, and no-agent panes, comparing both
surfaces to the same unbounded `TabDisplayProjection::full_label`.

The raw mirror's `[:80]` rule is not part of the naming contract. The server
stores a sanitized whole-word candidate with a 48-character semantic limit.
UI fitting uses display width and `truncate_end` with an ellipsis only when a
surface cannot fit a complete candidate; it never cuts a path or word in the
middle. Paths are reduced to useful basename/context or omitted, never shown as
an arbitrary prefix.

### Lifecycle

- Rename: a human rename writes `Tab.custom_name`; both surfaces change on the
  next projection. Auto hooks cannot overwrite it. Clearing the human name
  re-exposes the live projection.
- Handoff/session replacement: preserve human/structural names; clear the
  hook-tier context and start a new auto-name latch for the new session.
- Restart: restore human names and tiered context. Live session reports may
  supersede restored fallback, never a human name.
- No agent: do not use cwd or prompt fragments as a row title. Show a human
  name, explicit manual context, or structural tab number.

## Delivery split

1. Invariant PR: keep `Workspace::tab_display_projection` as the only semantic
   source, make both consumers call the canonical name accessor, and add the
   focused consumer-level invariant test. No sidebar-origin refactor.
2. Auto-naming PR: replace the raw closing-block title as a naming input, make
   the hook latch/stronger-signal rule explicit, add fixtures for path prompts,
   secrets, subagents, and session replacement, and document mirror migration.
   This is larger and should not be half-shipped with the invariant patch.


## Delivered: the agent's own session name

The design above assumed the derived work title was the strongest automatic
signal. It is not. Claude records the name it gives a session as an `ai-title`
entry in the session transcript and appends a fresh entry on every rename, so
the current name is the last such entry for the reported session id.

That fact is now carried end to end:

- `PaneWorkContext.session_name` is the wire field. It is additive and
  optional, so the protocol version is unchanged.
- `herdr agent session-name --provider claude` reads the transcript the hook
  payload points at, takes the last `ai-title`, and reports it under
  `herdr:session-name` with the same agent/session guards the work title uses.
- The Claude hook runs that command on `UserPromptSubmit` **and** `Stop`. A
  rename lands between turns, so binding it to turn start alone is what made
  the name go stale.
- The server treats a session-name report as a patch of the hook tier, not a
  replacement, so a rename cannot erase the ticket and link evidence the last
  turn established.
- `Tab::work_context_display_projection` ranks the session name above the
  terminal title and the derived work title, and below a human pane label or
  `Tab.custom_name`. The terminal title is whatever the agent last painted —
  frequently the checkout directory before a session has been named — so it
  must never outrank an explicit name.

Codex has no equivalent channel today; `request_from_session_name` rejects it
rather than inventing one.
