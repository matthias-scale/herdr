# Agent workflow

Keep this execution loop small and shared. `AGENTS.md` links here and
`CLAUDE.md` is a symlink to `AGENTS.md`, so provider instructions stay aligned.

1. Recalculate the active objective from the latest user message. A correction
   or override replaces stale scope immediately.
2. Classify the request as inspect, propose, or change. For an in-scope,
   reversible change, state the proposed action and apply it in the same turn.
   Stop for approval only when the action is destructive, externally visible,
   needs new authority, or would materially expand scope.
3. Resolve the canonical repository, base, worktree, and dirty state before
   creating branches or worktrees.
4. Verify in layers: focused checks first, then one consolidated final pass.
   Run independent checks concurrently when they do not compete for mutable
   state. Use `just check-parallel` on Unix for the repository-wide pass.
5. After pushing a PR, use `just pr-watch <number>` so only check transitions
   are printed. Re-run the final pass only when the diff changes.

