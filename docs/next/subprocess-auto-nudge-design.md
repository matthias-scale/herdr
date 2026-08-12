# Follow-up design: active auto-nudge for subprocess-held panes

Status: specification only. Nothing in this document is implemented. The shipped
behaviour is passive: a pane holding a live agent sub-process tree reads working
(`src/app/foreground_process.rs`), and if the agent never reports again the
status watchdog marks the pane `supervisor_stale`
(`src/terminal/state.rs::agent_status_watchdog_deadline`).

Passive detection ends there. The pane carries a peach `!`, it is excluded from
working counts, and a human decides what to do. This document specifies the
optional next step: herdr itself asking the agent to continue.

## Trigger

Opt-in, config-gated. Default off. A pane is only eligible while the operator
has enabled the feature for it; no global implicit enablement, and disabling the
config disables the behaviour immediately for panes already in the state.

The trigger is a **transition**, not a level: the pane's agent sub-process tree
goes from non-empty to empty while the pane is subprocess-held or already
`supervisor_stale`, and the agent has produced no fresh report since the report
that ended its turn. A pane that merely sits stale is not a trigger; a pane
whose tree exits and whose agent then reports is not a trigger either, because
the fresh report clears the state first.

At most one nudge per stale episode. A new report, a process exit, or a session
change ends the episode.

## The message is a fixed literal

The nudge submits exactly:

```text
continue (by herdr)
```

Never free text. Never model-generated. Never templated with pane, task, or
error context. The string is a constant in the source and is the only string
this path may ever send.

The reason is auditability. A misfire — wrong pane, wrong moment, wrong
session — has to be identifiable after the fact by anyone reading the session
transcript, without correlating logs. A fixed, herdr-attributed literal is
self-labelling: any occurrence of it in an agent's history was written by herdr
and by nothing else. Free or generated text would be indistinguishable from what
the human typed, which is precisely the confusion an automated writer must not
introduce.

## Auditability does not cover non-destruction

These are two separate properties and the fixed string buys only the first.

A submit into a pane whose composer already holds human-typed text destroys that
draft: the send concatenates with or replaces it, and the newline commits the
result. The transcript afterwards is perfectly auditable and the work is still
gone. Attribution is a forensic property; it does not prevent the loss it lets
you diagnose.

Therefore the follow-up requires a **draft-safety precondition checked at send
time, not at decision time**. The window between deciding to nudge and writing
to the pane is exactly when a human types. A precondition evaluated when the
timer fires and trusted when the write happens is a race, and losing that race
costs a human their draft.

The rule: immediately before submitting, re-read the pane's composer. If it is
non-empty, **refuse to submit** and abandon the nudge for this episode. Do not
queue it, do not clear the composer, do not append to it, do not retry on the
next tick. The pane stays stale and the human resolves it.

This refusal is load-bearing and must be proven by a fixture, not asserted by
review: the test drives a pane to the trigger transition with a non-empty
composer at send time and asserts that no bytes were written to the pane. A test
that only checks the decision-time state does not test this property. The
fixture must also cover the race directly — composer empty at decision, non-empty
at send — and assert the same refusal.

## Out of scope

- Any nudge to a pane with no agent session.
- Sending a newline to a pane herdr did not verify as the intended target
  (workspace, pane, host, cwd, TTY, foreground process).
- Retrying, escalating, or varying the message.
