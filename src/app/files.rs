use std::io::Read;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyModifiers};

use super::{App, FilesRefreshInFlight};
use crate::app::{AppState, DockSurface, Mode};
use crate::events::AppEvent;
use crate::files::{FileTreeRow, FileTreeRowKind};
use crate::input::TerminalKey;

const FILE_DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
const FILE_PREVIEW_LIMIT_BYTES: u64 = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileClickAction {
    DirectoryToggled,
    Preview(PathBuf),
    Open(PathBuf),
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

    pub(crate) fn click_dock_file_row_at(
        &mut self,
        col: u16,
        row: u16,
        now: std::time::Instant,
    ) -> Option<FileClickAction> {
        let hit = self
            .view
            .dock_file_row_hit_areas
            .iter()
            .find(|hit| {
                col >= hit.rect.x
                    && col < hit.rect.x.saturating_add(hit.rect.width)
                    && row >= hit.rect.y
                    && row < hit.rect.y.saturating_add(hit.rect.height)
            })
            .cloned()?;
        self.dock_files_selection = Some(hit.path.clone());
        self.dock_files_focused = true;
        if hit.kind == FileTreeRowKind::Directory {
            self.dock_files_last_click = None;
            if !self.dock_files_collapsed.remove(&hit.path) {
                self.dock_files_collapsed.insert(hit.path);
            }
            return Some(FileClickAction::DirectoryToggled);
        }
        let double_click = self
            .dock_files_last_click
            .as_ref()
            .is_some_and(|(path, clicked_at)| {
                path == &hit.path
                    && now
                        .checked_duration_since(*clicked_at)
                        .is_some_and(|age| age <= FILE_DOUBLE_CLICK_WINDOW)
            });
        self.dock_files_last_click = (!double_click).then(|| (hit.path.clone(), now));
        Some(if double_click {
            FileClickAction::Open(hit.path)
        } else {
            FileClickAction::Preview(hit.path)
        })
    }

    pub(crate) fn cycle_dock_files_sort(&mut self) {
        self.dock_files_sort = self.dock_files_sort.next();
        self.dock_scroll = 0;
        self.reconcile_dock_files_selection();
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

        self.spawn_dock_files_refresh(cwd);
    }

    pub(crate) fn force_dock_files_refresh(&mut self) {
        let Some(cwd) = self.focused_files_cwd() else {
            return;
        };
        if let Some(root) = self.state.dock_files_root.take() {
            self.state.dock_file_cache.remove(&root);
        }
        self.state.dock_files_cwd = None;
        self.state.dock_files_roots_by_cwd.remove(&cwd);
        self.spawn_dock_files_refresh(cwd);
    }

    fn spawn_dock_files_refresh(&mut self, cwd: PathBuf) {
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
            KeyCode::Esc if self.state.dock_editor_preview.is_some() => {
                self.state.dock_editor_preview = None;
            }
            KeyCode::Esc if !self.state.dock_files_filter.is_empty() => {
                self.state.dock_files_filter.clear();
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Esc if self.state.dock_files_search_active => {
                self.state.dock_files_search_active = false;
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
                Some(row) => self.open_dock_file_in_editor(row.path),
                None => {}
            },
            KeyCode::Backspace if event.modifiers.is_empty() => {
                self.state.dock_files_filter.pop();
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            KeyCode::Char('/') if event.modifiers.is_empty() => {
                self.state.dock_files_search_active = true;
            }
            KeyCode::Char('r')
                if event.modifiers.is_empty() && !self.state.dock_files_search_active =>
            {
                self.force_dock_files_refresh();
            }
            KeyCode::Char('s')
                if event.modifiers.is_empty() && !self.state.dock_files_search_active =>
            {
                self.state.cycle_dock_files_sort();
            }
            KeyCode::Char(character)
                if event.modifiers.is_empty() || event.modifiers == KeyModifiers::SHIFT =>
            {
                self.state.dock_files_search_active = true;
                self.state.dock_files_filter.push(character);
                self.state.dock_scroll = 0;
                self.state.reconcile_dock_files_selection();
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn preview_dock_file(&mut self, relative: PathBuf) {
        let Some(root) = self.state.dock_files_root.clone() else {
            return;
        };
        let path = root.join(relative);
        self.state.dock_editor_preview = Some(read_file_preview(path));
    }

    pub(crate) fn refresh_dock_editor_preview(&mut self) {
        let Some(path) = self
            .state
            .dock_editor_preview
            .as_ref()
            .map(|preview| preview.path.clone())
        else {
            return;
        };
        self.state.dock_editor_preview = Some(read_file_preview(path));
    }

    pub(crate) fn open_file_in_dock_editor(&mut self, path: PathBuf) {
        self.state.dock_editor_preview = None;
        self.state.open_dock_surface(DockSurface::Editor);
        self.state.dock_editor_focused = true;
        self.state.dock_files_focused = false;
        self.state.dock_agents_focused = false;
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
        let escaped = crate::app::repo_editor::vim_fnameescape(&path.to_string_lossy());
        let _ = runtime.try_send_bytes(Bytes::from(format!("\x1b:e {escaped}\r")));
    }

    pub(crate) fn open_dock_editor_preview_in_editor(&mut self) {
        let Some(path) = self
            .state
            .dock_editor_preview
            .take()
            .map(|preview| preview.path)
        else {
            return;
        };
        self.open_repo_editor_file(path);
    }

    pub(crate) fn open_dock_file_in_editor(&mut self, relative: PathBuf) {
        let Some(root) = self.state.dock_files_root.clone() else {
            return;
        };
        self.state.dock_editor_preview = None;
        self.open_repo_editor_file(root.join(relative));
    }
}

fn read_file_preview(path: PathBuf) -> crate::app::state::DockEditorPreview {
    let mut content = Vec::new();
    let result = std::fs::File::open(&path).and_then(|file| {
        file.take(FILE_PREVIEW_LIMIT_BYTES + 1)
            .read_to_end(&mut content)
    });
    let mut notice = result.err().map(|error| format!("render failure: {error}"));
    if notice.is_none() && content.contains(&0) {
        content.clear();
        notice = Some("binary file cannot be previewed".to_string());
    }
    if content.len() > FILE_PREVIEW_LIMIT_BYTES as usize {
        content.truncate(FILE_PREVIEW_LIMIT_BYTES as usize);
        notice = Some("preview truncated at 512 KiB".to_string());
    }
    crate::app::state::DockEditorPreview {
        path,
        content: String::from_utf8_lossy(&content).into_owned(),
        notice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    kind: FileTreeRowKind::File,
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
    fn files_keys_cycle_sort_and_keep_search_input_unambiguous() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(DockSurface::Files);
        app.state.dock_files_focused = true;
        let mut workspace = crate::workspace::Workspace::test_new("files-keys");
        workspace.identity_cwd = std::env::current_dir().expect("current directory");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();

        assert!(app
            .handle_dock_files_key(&TerminalKey::new(KeyCode::Char('s'), KeyModifiers::empty(),)));
        assert_eq!(app.state.dock_files_sort, crate::files::FileSort::Type);
        assert!(app
            .handle_dock_files_key(&TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty(),)));
        assert!(app
            .handle_dock_files_key(&TerminalKey::new(KeyCode::Char('s'), KeyModifiers::empty(),)));
        assert_eq!(app.state.dock_files_filter, "s");
        assert_eq!(app.state.dock_files_sort, crate::files::FileSort::Type);
        for _ in 0..2 {
            assert!(
                app.handle_dock_files_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty(),))
            );
        }
        app.state.dock_files_focused = true;
        assert!(app
            .handle_dock_files_key(&TerminalKey::new(KeyCode::Char('r'), KeyModifiers::empty(),)));
        assert!(app.files_refresh_in_flight.is_some());
    }

    #[test]
    fn clicking_a_directory_toggles_it() {
        let mut state = AppState::test_new();
        state.view.dock_file_row_hit_areas = vec![crate::app::state::DockFileRowHitArea {
            path: PathBuf::from("src"),
            kind: FileTreeRowKind::Directory,
            rect: ratatui::layout::Rect::new(20, 4, 12, 1),
        }];

        let now = std::time::Instant::now();
        assert_eq!(
            state.click_dock_file_row_at(24, 4, now),
            Some(FileClickAction::DirectoryToggled)
        );
        assert!(state.dock_files_collapsed.contains(Path::new("src")));
        assert_eq!(
            state.click_dock_file_row_at(24, 4, now + std::time::Duration::from_millis(50)),
            Some(FileClickAction::DirectoryToggled)
        );
        assert!(!state.dock_files_collapsed.contains(Path::new("src")));
    }

    #[test]
    fn file_click_dispatch_distinguishes_single_and_double_clicks() {
        let mut state = AppState::test_new();
        state.view.dock_file_row_hit_areas = vec![crate::app::state::DockFileRowHitArea {
            path: PathBuf::from("src/lib.rs"),
            kind: FileTreeRowKind::File,
            rect: ratatui::layout::Rect::new(20, 4, 12, 1),
        }];
        let now = std::time::Instant::now();

        assert_eq!(
            state.click_dock_file_row_at(24, 4, now),
            Some(FileClickAction::Preview(PathBuf::from("src/lib.rs")))
        );
        assert_eq!(
            state.click_dock_file_row_at(24, 4, now + std::time::Duration::from_millis(200)),
            Some(FileClickAction::Open(PathBuf::from("src/lib.rs")))
        );
        assert_eq!(
            state.click_dock_file_row_at(24, 4, now + std::time::Duration::from_secs(1)),
            Some(FileClickAction::Preview(PathBuf::from("src/lib.rs")))
        );
    }

    #[test]
    fn preview_replaces_the_previous_file_without_starting_a_runtime() {
        let root = std::env::temp_dir().join(format!(
            "herdr-file-preview-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("preview root");
        std::fs::write(root.join("first.rs"), "fn first() {}\n").expect("first file");
        std::fs::write(root.join("second.py"), "def second():\n    pass\n").expect("second file");
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.dock_files_root = Some(root.clone());
        let runtime_count = app.terminal_runtimes.len();

        app.preview_dock_file(PathBuf::from("first.rs"));
        app.preview_dock_file(PathBuf::from("second.py"));

        let preview = app
            .state
            .dock_editor_preview
            .as_ref()
            .expect("second preview");
        assert_eq!(preview.path, root.join("second.py"));
        assert!(preview.content.contains("def second"));
        assert_eq!(app.terminal_runtimes.len(), runtime_count);
        std::fs::remove_dir_all(root).expect("remove preview root");
    }

    #[test]
    fn escape_closes_a_preview_before_leaving_the_files_surface() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.mode = Mode::Terminal;
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(DockSurface::Files);
        app.state.dock_files_focused = true;
        app.state.dock_editor_preview = Some(crate::app::state::DockEditorPreview {
            path: PathBuf::from("/repo/src/lib.rs"),
            content: "fn main() {}".to_string(),
            notice: None,
        });

        assert!(app.handle_dock_files_key(&TerminalKey::new(KeyCode::Esc, KeyModifiers::empty(),)));

        assert!(app.state.dock_editor_preview.is_none());
        assert!(app.state.dock_files_focused);
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
