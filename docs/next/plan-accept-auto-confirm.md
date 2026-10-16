# Plan accept prompts and auto-confirm

Herdr detects Claude's plan-accept dialog as Blocked through two signals:

- The transcript contains a pending `ExitPlanMode` tool use.
- The screen matches the `plan_approval_prompt` rule.

Herdr never types into the pane to accept a plan. Auto-confirm belongs in a
fleet Claude Code `PreToolUse` hook matching `ExitPlanMode`. The hook returns
`permissionDecision: "allow"` only when the session transcript contains
Matthias's explicit approval for that same plan. It refuses plans that touch
production, money, or destructive operations, and otherwise falls through to
the dialog.

Herdr reports structured evidence: the blocked state, plus the matched rule id
`plan_approval_prompt` or the pending tool name in explain output.
