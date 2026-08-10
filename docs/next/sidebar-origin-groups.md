# Sidebar origin groups

## Decision

Do not add a second origin-grouping layer on top of PR #46. At its pinned head
(`df4f6a3f317e3162935f1fd65ae8a89f0f45197b`), #46 already makes the default
sidebar an ownership tree: panes roll up to tabs, tabs sit under their Space
(workspace), and explicitly related Herdr-managed worktrees share one repository
parent. That is the supported origin grouping this slice was intended to add.

## Origin

For a pane, the origin Herdr can identify today is its owning Space, keyed by the
server-owned `workspace_id`. The server enriches that Space with Git/worktree
metadata derived from its identity CWD. This gives the TUI a stable identity and
a useful label without treating presentation text as identity.

The other candidate signals are not sound identity keys:

- `cwd` is useful evidence for deriving Space and Git metadata, but panes in one
  Space can have different working directories and paths can change.
- `work_context.branch` is mutable, optional, and not unique across repositories.
- The SSH target used by `herdr --remote` belongs to the local launcher. It is not
  present on the remote server's `AgentInfo`, and one attached TUI renders one
  remote server/session, so it cannot truthfully distinguish several hosts.

If a future aggregate session combines multiple servers, the server protocol
will need a neutral host/server identity first. Any pane lacking that identity
must then appear in an explicit `Ungrouped` bucket. Inferring a host from a path,
branch, or display label would silently assign the pane to the wrong origin.

## Relationship to PR #46

PR #46 supplies both the grouping scaffolding (`SectionHeader`) and the actual
ownership projection (`Workspace` and `Tab` rows). It also groups only explicit
Herdr-managed worktree memberships; its tests deliberately keep unrelated or
ordinary Git workspaces separate. Blocked and pinned sections are shortcut
worklists layered above the complete Spaces tree, not competing ownership
groups.

Adding origin headers now would either duplicate every Space row or create a
nested status/pin → origin → pane hierarchy. Neither adds a runtime fact, and the
latter is explicitly outside this slice's default scope. No API or TUI change is
therefore justified.

## Acceptance criteria

| ID | Result | Operative clause | Evidence |
| --- | --- | --- | --- |
| AC1 | ✓ | Every sidebar pane is visibly owned by its server-known Space. | #46 makes `sidebar_shows_spaces_tree()` unconditional and projects `Workspace` → `Tab` rows. |
| AC2 | ✓ | Explicit Herdr-managed worktrees from one repository share one visible parent without merging unrelated work. | `workspace_list_entries_group_multiple_workspaces_in_same_git_space`, `workspace_list_entries_group_non_contiguous_explicit_members`, and the ordinary-Git negative tests. |
| AC3 | ✓ | Group presentation remains TUI-only while shared identity remains server-owned. | `workspace_id` is already present on `PaneInfo` and `AgentInfo`; group rows, labels, collapse state, geometry, and rendering remain in the client/TUI. |
| AC4 | ✓ | Existing custom `[ui.sidebar.agents] rows` continue to control agent row contents and the default remains unchanged. | No config or row-resolution code changes; #46's row resolver remains the only agent-row layout path. |
| AC5 | ✗ | Several remote hosts can be grouped by a truthful server-known host origin. | `--remote` retains its SSH target only in the local launcher; `AgentInfo` has no host identity and a TUI attaches to one server/session. This requires a separate aggregate-runtime design. |
| AC6 | ✓ | Unknown future origins never fall into another origin's group. | This slice adds no inferred origin. A future aggregate design is required to emit an explicit `Ungrouped` bucket when neutral server identity is absent. |
