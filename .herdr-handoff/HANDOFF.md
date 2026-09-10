# Herdr fork: sidebar tooltips, notepad caret, pane right-click links

Worktree: /home/ubuntu2/Repos/herdr-worktrees/sidebar-tooltips
Branch: feat/sidebar-hover-tooltips (off main d899b7b3). No PR opened yet.

## Done (committed)
1. 61ce0a13 feat(sidebar): hover tooltips for status glyphs, agent dots,
   truncated work titles. New `ViewState.sidebar_hover_targets`,
   `ControlId::SidebarHover(usize)`, `compute_sidebar_hover_targets`,
   `nested_header_spans`, `WorkGroupStatus::label()`.
2. 7fc2fb64 fix(notepad): caret placed last in ui.rs render (before pomodoro),
   `notepad_caret_position` + `render_notepad_caret`. Proven: disabling the
   call makes `the_notepad_caret_outranks_every_other_cursor_claim` fail.

## In progress
3. Right-click in a pane on ANY link -> "Copy link" menu entry.
   Pieces: `AppState::url_at_pane_cell` (actions.rs:2505) already returns any
   URL; `request_clipboard_write` (state.rs:3328) is the clipboard channel;
   menu kind `ContextMenuKind::Pane` (state.rs:2873) + `items()` (state.rs:3041);
   dispatch in modal.rs ~877 (mouse) and ~1366 (key); menu built in
   mouse.rs ~2014.
   Plan: add `link: Option<String>` to `ContextMenuKind::Pane`, capture it in
   mouse.rs next to `linkable_work_link_at`, add `COPY_LINK_ITEM` to items(),
   handle in both dispatchers via `state.request_clipboard_write`.

## Verification
`cargo test --bin herdr` from the worktree: 2 pre-existing failures only,
both environmental and reproduced on clean main / neutral cwd:
- app::tab_bar_status::tests::reload_aborts_... (fails on clean main too)
- ui::sidebar::tests::sidebar_header_controls_... (cwd-name dependent; passes
  when the binary runs from /tmp)
`cargo clippy --all-targets` clean, `cargo fmt` applied.

## Still owed to the user
- Suggestions for further pane right-click improvements.
