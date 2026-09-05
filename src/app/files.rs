use std::path::{Path, PathBuf};

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, FilesRefreshInFlight};
use crate::app::{AppState, DockSurface, Mode};
use crate::events::AppEvent;
use crate::files::{FileTreeRow, FileTreeRowKind};
use crate::input::TerminalKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileOpenTarget {
    DockEditor,
    RightSplit,
}

pub(crate) fn file_open_target(editor_open: bool) -> FileOpenTarget {
    if editor_open {
        FileOpenTarget::DockEditor
    } else {
        FileOpenTarget::RightSplit
    }
}

impl AppState {
    pub(crate) fn dock_files_rows(&self) -> Vec<FileTreeRow> {
        crate::ui::dock::files::visible_rows(self)
    }

    pub(crate) fn reconcile_dock_files_selection(&mut self) {
        let rows = self.dock_files_rows();
        if !rows
            .iter()
            .any(|row| Some(&row.path) == self.dock_files_selection.as_ref())
        {
            self.dock_files_selection = rows.first().map(|row| row.path.clone());
        }
        self.keep_dock_files_selection_visible(rows.len());
    }

    pub(crate) fn move_dock_files_selection(&mut self, delta: isize) {
        let rows = self.dock_files_rows();
        if rows.is_empty() {
            self.dock_files_selection = None;
            return;
        }
        let current = self
            .dock_files_selection
            .as_ref()
            .and_then(|path| rows.iter().position(|row| &row.path == path))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(rows.len() - 1);
        self.dock_files_selection = Some(rows[next].path.clone());
        self.keep_dock_files_selection_visible(rows.len());
    }

    fn keep_dock_files_selection_visible(&mut self, row_count: usize) {
        let Some(index) = self.dock_files_selection.as_ref().and_then(|path| {
            self.dock_files_rows()
                .iter()
                .position(|row| &row.path == path)
        }) else {
            self.dock_scroll = 0;
            return;
        };
        let header = crate::ui::dock::files::header_rows(self);
        let height = usize::from(
            self.view
                .dock_body_rect
                .height
                .saturating_sub(header)
                .max(1),
        );
        let max_scroll = row_count.saturating_sub(height);
        let current = usize::from(self.dock_scroll).min(max_scroll);
        let scroll = if index < current {
            index
        } else if index >= current + height {
            index + 1 - height
        } else {
            current
        };
        self.dock_scroll = u16::try_from(scroll.min(max_scroll)).unwrap_or(u16::MAX);
    }

    pub(crate) fn selected_dock_file_row(&self) -> Option<FileTreeRow> {
        let selected = self.dock_files_selection.as_ref()?;
        self.dock_files_rows()
            .into_iter()
            .find(|row| &row.path == selected)
    }

    pub(crate) fn toggle_selected_dock_directory(&mut self) -> bool {
        let Some(row) = self.selected_dock_file_row() else {
            return false;
        };
        if row.kind != FileTreeRowKind::Directory {
            return false;
        }
        if !self.dock_files_collapsed.remove(&row.path) {
            self.dock_files_collapsed.insert(row.path);
        }
        self.reconcile_dock_files_selection();
        true
    }

    pub(crate) fn set_selected_dock_directory_expanded(&mut self, expanded: bool) -> bool {
        let Some(row) = self.selected_dock_file_row() else {
            return false;
        };
        if row.kind != FileTreeRowKind::Directory {
            return false;
        }
        if expanded {
            self.dock_files_collapsed.remove(&row.path)
        } else {
            self.dock_files_collapsed.insert(row.path)
        }
    }

    pub(crate) fn click_dock_file_row(&mut self, col: u16, row: u16) -> bool {
        let Some(hit) = self
            .view
            .dock_file_row_hit_areas
            .iter()
            .find(|hit| {
                col >= hit.rect.x
                    && col < hit.rect.x.saturating_add(hit.rect.width)
                    && row >= hit.rect.y
                    && row < hit.rect.y.saturating_add(hit.rect.height)
            })
            .cloned()
        else {
            return false;
        };
        self.dock_files_selection = Some(hit.path.clone());
        self.dock_files_focused = true;
        if hit.kind == FileTreeRowKind::Directory && !self.dock_files_collapsed.remove(&hit.path) {
            self.dock_files_collapsed.insert(hit.path);
        }
        true
    }
}

impl App {
    pub(crate) fn start_dock_files_refresh_if_needed(&mut self) {
        if self.state.dock_collapsed || self.state.dock_tab != Some(crate::app::DockSurface::Files)
        {
            return;
        }
        self.start_dock_files_refresh();
    }

    pub(crate) fn start_dock_files_refresh(&mut self) {
        let Some(cwd) = self.focused_files_cwd() else {
            return;
        };
        if let Some(root) = self.state.dock_files_roots_by_cwd.get(&cwd).cloned() {
            self.state.dock_files_root = Some(root.clone());
            self.state.dock_files_cwd = Some(cwd.clone());
            if self.state.dock_file_cache.contains_key(&root) {
                self.state.reconcile_dock_files_selection();
                return;
            }
        }
        if self
            .files_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| refresh.cwd == cwd)
        {
            return;
        }
        if self.state.dock_files_cwd.as_ref() == Some(&cwd)
            && self
                .state
                .dock_files_root
                .as_ref()
                .is_some_and(|root| self.state.dock_file_cache.contains_key(root))
        {
            return;
        }

        self.last_files_refresh_generation = self.last_files_refresh_generation.wrapping_add(1);
        let generation = self.last_files_refresh_generation;
        self.files_refresh_in_flight = Some(FilesRefreshInFlight {
            generation,
            cwd: cwd.clone(),
        });
        let event_tx = self.event_tx.clone();
        let spawn = std::thread::Builder::new()
            .name("herdr-dock-files".to_string())
            .spawn(move || {
                let snapshot = crate::files::build_file_tree(&cwd, Path::new("git"));
                let _ = event_tx.blocking_send(AppEvent::DockFilesRefreshed {
                    generation,
                    snapshot,
                });
            });
        if let Err(error) = spawn {
            tracing::warn!(%error, "could not start dock files refresh");
            self.files_refresh_in_flight = None;
        }
    }

    fn focused_files_cwd(&self) -> Option<PathBuf> {
        self.state
            .active
            .and_then(|index| self.state.workspaces.get(index))
            .and_then(|workspace| {
                workspace.focused_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            })
    }

    pub(crate) fn handle_dock_files_refreshed(
        &mut self,
        generation: u64,
        snapshot: crate::files::FileTreeSnapshot,
    ) -> bool {
        let Some(refresh) = self.files_refresh_in_flight.as_ref() else {
            return false;
        };
        if refresh.generation != generation {
            return false;
        }
        let cwd = refresh.cwd.clone();
        self.files_refresh_in_flight = None;
        let root = snapshot.root.clone();
        let changed = self.state.dock_file_cache.get(&root) != Some(&snapshot)
            || self.state.dock_files_root.as_ref() != Some(&root);
        self.state.dock_file_cache.insert(root.clone(), snapshot);
        self.state.dock_files_root = Some(root);
        self.state.dock_files_cwd = Some(cwd);
        if let (Some(cwd), Some(root)) = (
            self.state.dock_files_cwd.clone(),
            self.state.dock_files_root.clone(),
        ) {
            self.state.dock_files_roots_by_cwd.insert(cwd, root);
        }
        self.state.reconcile_dock_files_selection();
        changed
    }

    pub(crate) fn handle_dock_files_key(&mut self, key: &TerminalKey) -> bool {
        if self.state.mode != Mode::Terminal
            || self.state.dock_collapsed
            || self.state.dock_tab != Some(DockSurface::Files)
            || !self.state.dock_files_focused
        {
            return false;
        }
        let event = key.as_key_event();
        match event.code {
            KeyCode::Esc if !self.state.dock_files_filter.is_empty() => {
                self.state.dock_files_filter.clear();
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Esc => self.state.dock_files_focused = false,
            KeyCode::Up => self.state.move_dock_files_selection(-1),
            KeyCode::Down => self.state.move_dock_files_selection(1),
            KeyCode::Left => {
                self.state.set_selected_dock_directory_expanded(false);
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Right => {
                self.state.set_selected_dock_directory_expanded(true);
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Enter => match self.state.selected_dock_file_row() {
                Some(row) if row.kind == FileTreeRowKind::Directory => {
                    self.state.toggle_selected_dock_directory();
                }
                Some(row) => self.open_dock_file(row.path),
                None => {}
            },
            KeyCode::Backspace if event.modifiers.is_empty() => {
                self.state.dock_files_filter.pop();
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Char(character)
                if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT =>
            {
                if character != '/' || !self.state.dock_files_filter.is_empty() {
                    self.state.dock_files_filter.push(character);
                }
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            _ => return false,
        }
        true
    }

    fn open_dock_file(&mut self, relative: PathBuf) {
        let Some(root) = self.state.dock_files_root.clone() else {
            return;
        };
        let path = root.join(relative);
        match file_open_target(self.state.dock_open_surfaces.contains(&DockSurface::Editor)) {
            FileOpenTarget::DockEditor => self.open_file_in_dock_editor(path),
            FileOpenTarget::RightSplit => self.open_file_in_right_split(path),
        }
    }

    fn open_file_in_dock_editor(&mut self, path: PathBuf) {
        self.state.open_dock_surface(DockSurface::Editor);
        self.state.dock_editor_focused = true;
        self.state.dock_files_focused = false;
        self.ensure_dock_editor();
        let Some(agent_pane_id) = crate::ui::dock::editor::focused_agent_pane_id(&self.state)
        else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .dock_editor_sessions
            .get(&agent_pane_id)
            .map(|session| session.terminal_id.clone())
        else {
            return;
        };
        let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
            return;
        };
        let escaped = vim_fnameescape(&path.to_string_lossy());
        let command = format!("\x1b:e {escaped}\r");
        let _ = runtime.try_send_bytes(Bytes::from(command));
    }

    fn open_file_in_right_split(&mut self, path: PathBuf) {
        let before = self.state.current_pane_focus_target();
        self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Right);
        let after = self.state.current_pane_focus_target();
        if after.is_none() || after == before {
            return;
        }
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(runtime) = self
            .state
            .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
        else {
            return;
        };
        let Some(argv) = crate::ui::dock::editor::editor_argv_candidates(Some(&path))
            .into_iter()
            .next()
        else {
            return;
        };
        let Some(shell_name) = crate::app::agents::available_shell_name(runtime) else {
            return;
        };
        let Some(command) = crate::platform::interactive_shell_command(&argv, &shell_name) else {
            return;
        };
        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &command);
        let _ = runtime.try_send_bytes(Bytes::from(bytes));
        self.state.dock_files_focused = false;
    }
}

fn vim_fnameescape(path: &str) -> String {
    path.chars()
        .flat_map(|character| {
            if matches!(character, ' ' | '\\' | '|' | '%' | '#') {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_action_targets_editor_when_open_and_right_split_when_not() {
        assert_eq!(file_open_target(true), FileOpenTarget::DockEditor);
        assert_eq!(file_open_target(false), FileOpenTarget::RightSplit);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn existing_editor_receives_an_escaped_edit_command() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("editor")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let pane_terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("pane terminal")
            .clone();
        app.state
            .terminals
            .get_mut(&pane_terminal_id)
            .expect("pane state")
            .detected_agent = Some(crate::detect::Agent::Codex);
        let editor_terminal_id = crate::terminal::TerminalId::alloc();
        let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(editor_terminal_id.clone(), runtime);
        app.state.dock_editor_sessions.insert(
            pane_id,
            crate::app::state::DockEditorSession {
                pane_id: crate::layout::PaneId::alloc(),
                terminal_id: editor_terminal_id,
            },
        );
        app.state.dock_collapsed = false;

        app.open_file_in_dock_editor(PathBuf::from("/repo/two words.rs"));

        assert_eq!(
            input.try_recv().expect("editor input"),
            Bytes::from_static(b"\x1b:e /repo/two\\ words.rs\r")
        );
        assert_eq!(app.state.dock_tab, Some(DockSurface::Editor));
    }

    #[test]
    fn directory_keys_collapse_and_expand() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.mode = Mode::Terminal;
        let root = PathBuf::from("/repo");
        app.state.dock_files_root = Some(root.clone());
        app.state.dock_file_cache.insert(
            root.clone(),
            crate::files::FileTreeSnapshot {
                root,
                files: vec![crate::files::FileRecord {
                    path: PathBuf::from("src/lib.rs"),
                    status: None,
                }],
                fingerprint: 1,
                source: crate::files::FileTreeSource::Git,
                error: None,
            },
        );
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(DockSurface::Files);
        app.state.dock_files_focused = true;
        app.state.dock_files_selection = Some(PathBuf::from("src"));

        assert!(app.handle_dock_files_key(&TerminalKey::new(KeyCode::Left, KeyModifiers::empty(),)));
        assert!(app.state.dock_files_collapsed.contains(Path::new("src")));
        assert!(
            app.handle_dock_files_key(&TerminalKey::new(KeyCode::Right, KeyModifiers::empty(),))
        );
        assert!(!app.state.dock_files_collapsed.contains(Path::new("src")));
        assert!(
            app.handle_dock_files_key(&TerminalKey::new(KeyCode::Enter, KeyModifiers::empty(),))
        );
        assert!(app.state.dock_files_collapsed.contains(Path::new("src")));
    }

    #[test]
    fn clicking_a_directory_toggles_it() {
        let mut state = AppState::test_new();
        state.view.dock_file_row_hit_areas = vec![crate::app::state::DockFileRowHitArea {
            path: PathBuf::from("src"),
            kind: FileTreeRowKind::Directory,
            rect: ratatui::layout::Rect::new(20, 4, 12, 1),
        }];

        assert!(state.click_dock_file_row(24, 4));
        assert!(state.dock_files_collapsed.contains(Path::new("src")));
        assert!(state.click_dock_file_row(24, 4));
        assert!(!state.dock_files_collapsed.contains(Path::new("src")));
    }

    #[test]
    fn visible_files_start_a_background_refresh() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("files");
        workspace.identity_cwd = std::env::current_dir().expect("current directory");
        app.state.workspaces.push(workspace);
        app.state.active = Some(0);
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(DockSurface::Files);

        app.start_dock_files_refresh_if_needed();

        assert!(app.files_refresh_in_flight.is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let event = loop {
            if let Ok(event) = app.event_rx.try_recv() {
                break event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "files refresh timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(matches!(event, AppEvent::DockFilesRefreshed { .. }));
    }
}
