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

Load the following guardrails only when the named surface is in scope.

## Sidebar and activity changes

- Preserve the fork's row contract unless the brief explicitly changes it:
  one row per tab/window with pane activity aggregated, state before one title,
  no subtitle or idle label, and selection conveyed by darker/bolder foreground.
  Show a provider suffix after the title only when the row has one unambiguous
  supported provider; do not invent one for mixed, agentless, or exited rows.
- Before the repository-wide pass, run focused tests at the supported 18-column
  minimum and a normal width for both desktop and mobile renderers. Preserve,
  in order: state indicator, a readable title fragment, and any present
  provider suffix; age and background-process details yield first.
- Cover the full observed lifecycle, not only static rows: new user input,
  blue working, process exit, green unread done, reopening clears done,
  background-shell changes, and minute-boundary age refresh. A mobile age label
  must also register its next refresh instant.
- Stabilize the branch with these focused checks before requesting an
  exact-head review receipt. Any later fix invalidates the receipt; review the
  final pushed SHA, not an intermediate head.

## Cross-build and release preflight

- Before an expensive build, confirm free disk space, the installed target,
  and the compiler selected by the repository toolchain. For `cargo zigbuild`,
  export `RUSTC="$(rustup which rustc)"` and an absolute `ZIG` path whose
  `version` is exactly `0.15.2`; do not trust an earlier compiler on `PATH`.
- When building after merge, first assert that the reviewed feature head and
  merge commit have the same tree. Architecture-specific binaries will have
  different hashes; bind them to the shared source tree instead of comparing
  binary hashes across architectures.
- Build and copy one architecture artifact at a time when disk is constrained.
  Remove only task-generated build output after its artifact and verification
  evidence are safely outside that directory.

## Live handoff verification

- Prove that replacement occurred: capture the old server PID and executable,
  require the old process to exit, and verify the new server identity before
  judging preservation.
- Snapshot before and after, then compare stable invariants: workspace/tab/pane
  shape, pane IDs, shell PID, foreground PGID and process identity, command,
  cwd, focus IDs, and server protocol/compatibility/restart state.
- Treat regenerated terminal IDs, observation sequence/timestamps, and agent
  detection snapshots as volatile metadata. A broad snapshot difference is a
  prompt to print and narrow the diff, not evidence that a session was lost.
