use std::path::PathBuf;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{App, AppState, DockSurface, Mode},
    layout::PaneId,
    pane::{AgentDetection, PaneLaunchEnv},
    terminal::{TerminalId, TerminalRuntime, TerminalRuntimeRegistry},
};

pub(crate) fn focused_agent_pane_id(app: &AppState) -> Option<PaneId> {
    let ws_idx = app.active?;
    let workspace = app.workspaces.get(ws_idx)?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id = workspace.terminal_id(pane_id)?;
    let terminal = app.terminals.get(terminal_id)?;
    if !terminal.is_agent_terminal() {
        return None;
    }
    Some(pane_id)
}

fn focused_agent_pane(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<(PaneId, PathBuf)> {
    let pane_id = focused_agent_pane_id(app)?;
    let ws_idx = app.active?;
    let workspace = app.workspaces.get(ws_idx)?;
    let cwd = workspace
        .active_tab()?
        .cwd_for_pane(pane_id, &app.terminals, terminal_runtimes)?;
    Some((pane_id, cwd))
}

fn focused_editor_terminal_id(app: &AppState) -> Option<TerminalId> {
    if app.mode != Mode::Terminal
        || !app.dock_editor_focused
        || app.dock_collapsed
        || app.dock_tab != Some(DockSurface::Editor)
    {
        return None;
    }
    editor_terminal_id_for_focused_agent(app)
}

fn editor_terminal_id_for_focused_agent(app: &AppState) -> Option<TerminalId> {
    let agent_pane_id = focused_agent_pane_id(app)?;
    app.dock_editor_sessions
        .get(&agent_pane_id)
        .map(|session| session.terminal_id.clone())
}

pub(super) fn render_editor_body(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    let area = app.view.dock_body_rect;
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some((agent_pane_id, _)) = focused_agent_pane(app, terminal_runtimes) else {
        render_editor_message(app, frame, area, "focus an agent first");
        return;
    };
    let Some(session) = app.dock_editor_sessions.get(&agent_pane_id) else {
        let message = app
            .dock_editor_errors
            .get(&agent_pane_id)
            .map(|error| format!("editor unavailable: {error}"))
            .unwrap_or_else(|| "opening editor…".to_string());
        render_editor_message(app, frame, area, &message);
        return;
    };
    let Some(runtime) = terminal_runtimes.get(&session.terminal_id) else {
        render_editor_message(app, frame, area, "editor session unavailable");
        return;
    };

    runtime.render(
        frame,
        area,
        app.mode == Mode::Terminal && app.dock_editor_focused,
    );
}

fn render_editor_message(app: &AppState, frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            message,
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::DIM),
        ))),
        area,
    );
}

impl App {
    /// Sessions are keyed by the agent pane they follow, so a closed agent leaves
    /// its editor behind with nothing left to reach it: no focus can select the
    /// key again, and the PTY would outlive the session it belonged to.
    pub(crate) fn reap_orphaned_dock_editors(&mut self) {
        let orphaned: Vec<PaneId> = self
            .state
            .dock_editor_sessions
            .keys()
            .copied()
            .filter(|pane_id| self.find_pane(*pane_id).is_none())
            .collect();
        for agent_pane_id in orphaned {
            if let Some(session) = self.state.dock_editor_sessions.remove(&agent_pane_id) {
                if let Some(runtime) = self.terminal_runtimes.remove(&session.terminal_id) {
                    runtime.shutdown();
                }
            }
            self.state.dock_editor_errors.remove(&agent_pane_id);
            self.state
                .dock_editor_requested_paths
                .remove(&agent_pane_id);
        }
    }

    /// Opens the focused repository's scratchpad in the dock's `$EDITOR`. The
    /// existing session is torn down first: it was started on a directory and
    /// cannot be redirected at a file after the fact.
    pub(crate) fn open_scratchpad_in_editor(&mut self) {
        let Some(root) = crate::scratchpad::focused_repo_root(&self.state) else {
            self.show_work_link_notice("no repository for this pane");
            return;
        };
        let Some(agent_pane_id) = focused_agent_pane_id(&self.state) else {
            self.show_work_link_notice("focus an agent first");
            return;
        };
        let path = crate::scratchpad::scratchpad_path(&root);
        if let Err(error) = crate::scratchpad::ensure_scratchpad_file(&path) {
            tracing::warn!(path = %path.display(), %error, "could not create scratchpad");
            self.show_work_link_notice("could not create the scratchpad");
            return;
        }
        if let Some(session) = self.state.dock_editor_sessions.remove(&agent_pane_id) {
            if let Some(runtime) = self.terminal_runtimes.remove(&session.terminal_id) {
                runtime.shutdown();
            }
        }
        self.state.dock_editor_errors.remove(&agent_pane_id);
        self.state
            .dock_editor_requested_paths
            .insert(agent_pane_id, path);
        self.state.dock_collapsed = false;
        self.state.dock_tab = Some(DockSurface::Editor);
        self.state.dock_editor_focused = true;
        self.ensure_dock_editor();
    }

    pub(crate) fn ensure_dock_editor(&mut self) {
        self.reap_orphaned_dock_editors();
        if self.state.dock_collapsed || self.state.dock_tab != Some(DockSurface::Editor) {
            return;
        }
        let Some((agent_pane_id, cwd)) = focused_agent_pane(&self.state, &self.terminal_runtimes)
        else {
            return;
        };
        // Keying by agent pane keeps the PTY alive across dock presentation changes; a new
        // focused agent gets its own session and therefore cannot corrupt another buffer.
        if self.state.dock_editor_sessions.contains_key(&agent_pane_id)
            || self.state.dock_editor_errors.contains_key(&agent_pane_id)
        {
            return;
        }

        let body = self.state.view.dock_body_rect;
        let (rows, cols) = if body.width > 0 && body.height > 0 {
            (body.height, body.width)
        } else {
            let (rows, cols) = self.state.estimate_pane_size();
            (rows.max(1), cols.max(1))
        };
        let editor_pane_id = PaneId::alloc();
        let terminal_id = TerminalId::alloc();
        let launch_env = PaneLaunchEnv::default();
        let mut last_error = None;

        let requested_path = self
            .state
            .dock_editor_requested_paths
            .get(&agent_pane_id)
            .cloned();
        for argv in editor_argv_candidates(requested_path.as_deref()) {
            match TerminalRuntime::spawn_argv_command(
                editor_pane_id,
                rows,
                cols,
                cwd.clone(),
                &argv,
                &launch_env,
                AgentDetection::Disabled,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                self.state.host_terminal_appearance,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ) {
                Ok(runtime) => {
                    self.terminal_runtimes.insert(terminal_id.clone(), runtime);
                    self.state.dock_editor_sessions.insert(
                        agent_pane_id,
                        crate::app::state::DockEditorSession {
                            pane_id: editor_pane_id,
                            terminal_id,
                        },
                    );
                    self.state.dock_editor_errors.remove(&agent_pane_id);
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                    return;
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }

        self.state.dock_editor_errors.insert(
            agent_pane_id,
            last_error.unwrap_or_else(|| "no editor command found".to_string()),
        );
    }

    pub(crate) fn resize_dock_editor(&self) {
        if self.state.dock_collapsed || self.state.dock_tab != Some(DockSurface::Editor) {
            return;
        }
        let Some(terminal_id) = editor_terminal_id_for_focused_agent(&self.state) else {
            return;
        };
        let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
            return;
        };
        let area = self.state.view.dock_body_rect;
        if area.width > 0 && area.height > 0 {
            runtime.resize(
                area.height,
                area.width,
                self.state.host_cell_size.width_px,
                self.state.host_cell_size.height_px,
            );
        }
    }

    pub(crate) fn dock_editor_terminal_id(&self) -> Option<TerminalId> {
        focused_editor_terminal_id(&self.state)
    }

    pub(crate) fn dock_editor_runtime(&self) -> Option<&TerminalRuntime> {
        let terminal_id = self.dock_editor_terminal_id()?;
        self.terminal_runtimes.get(&terminal_id)
    }

    pub(crate) fn handle_dock_editor_exit(&mut self, pane_id: PaneId) -> bool {
        let Some((agent_pane_id, session)) = self
            .state
            .dock_editor_sessions
            .iter()
            .find(|(_, session)| session.pane_id == pane_id)
            .map(|(agent_pane_id, session)| (*agent_pane_id, session.clone()))
        else {
            return false;
        };
        self.state.dock_editor_sessions.remove(&agent_pane_id);
        self.state
            .dock_editor_errors
            .insert(agent_pane_id, "editor exited".to_string());
        self.state
            .dock_editor_requested_paths
            .remove(&agent_pane_id);
        if let Some(runtime) = self.terminal_runtimes.remove(&session.terminal_id) {
            runtime.shutdown();
        }
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
        true
    }
}

/// `path` is the file the editor should open. Without one the editor starts in the
/// pane's directory, which is the dock editor's original behaviour.
pub(crate) fn editor_argv_candidates(path: Option<&std::path::Path>) -> Vec<Vec<String>> {
    let mut candidates = Vec::new();
    if let Ok(editor) = std::env::var("EDITOR") {
        if let Some(argv) = parse_editor_command(&editor) {
            candidates.push(argv);
        }
    }
    candidates.push(vec!["nvim".to_string()]);
    #[cfg(windows)]
    candidates.push(vec!["notepad.exe".to_string()]);
    #[cfg(not(windows))]
    candidates.push(vec!["vi".to_string()]);
    if let Some(path) = path {
        let argument = path.to_string_lossy().into_owned();
        for candidate in candidates.iter_mut() {
            candidate.push(argument.clone());
        }
    }
    candidates
}

fn parse_editor_command(command: &str) -> Option<Vec<String>> {
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                argv.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        argv.push(word);
    }
    (!argv.is_empty()).then_some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::state::DockEditorSession, detect::Agent};

    #[tokio::test(flavor = "current_thread")]
    async fn closing_the_agent_shuts_down_the_editor_it_was_keyed_to() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let closed_agent_pane_id = PaneId::alloc();
        let editor_terminal_id = TerminalId::alloc();
        app.state.dock_editor_sessions.insert(
            closed_agent_pane_id,
            DockEditorSession {
                pane_id: PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );
        app.terminal_runtimes.insert(
            editor_terminal_id.clone(),
            TerminalRuntime::test_with_screen_bytes(10, 2, b"EDITOR"),
        );

        app.reap_orphaned_dock_editors();

        assert!(app.state.dock_editor_sessions.is_empty());
        assert!(app.terminal_runtimes.get(&editor_terminal_id).is_none());
    }

    #[test]
    fn a_requested_file_is_appended_to_every_editor_candidate() {
        let path = std::path::Path::new("/repo/.herdr/scratchpad.md");
        let with_path = editor_argv_candidates(Some(path));
        let without_path = editor_argv_candidates(None);

        assert_eq!(with_path.len(), without_path.len());
        assert!(
            with_path
                .iter()
                .all(|argv| argv.last().map(String::as_str) == Some(path.to_str().unwrap())),
            "candidates: {with_path:?}"
        );
        assert!(
            without_path
                .iter()
                .all(|argv| argv.last().map(String::as_str) != Some(path.to_str().unwrap())),
            "candidates: {without_path:?}"
        );
    }

    #[test]
    fn editor_command_parser_preserves_quoted_arguments() {
        assert_eq!(
            parse_editor_command("nvim --cmd \"set title\""),
            Some(vec![
                "nvim".to_string(),
                "--cmd".to_string(),
                "set title".to_string()
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn editor_tab_renders_the_focused_agent_editor_terminal() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("editor")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let agent_pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
        let agent_terminal_id = app.workspaces[0]
            .terminal_id(agent_pane_id)
            .expect("agent terminal")
            .clone();
        app.terminals
            .get_mut(&agent_terminal_id)
            .expect("agent terminal state")
            .detected_agent = Some(Agent::Codex);
        app.dock_collapsed = false;
        app.dock_editor_focused = true;
        app.view.dock_body_rect = Rect::new(0, 0, 30, 4);
        let editor_terminal_id = TerminalId::alloc();
        app.dock_editor_sessions.insert(
            agent_pane_id,
            DockEditorSession {
                pane_id: PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            editor_terminal_id,
            TerminalRuntime::test_with_screen_bytes(30, 4, b"EDITOR"),
        );

        let backend = ratatui::backend::TestBackend::new(30, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_editor_body(&app, &terminal_runtimes, frame))
            .expect("render editor body");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("EDITOR"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn switching_away_and_back_does_not_respawn_the_same_editor_process() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("editor")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let agent_pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let agent_terminal_id = app.state.workspaces[0]
            .terminal_id(agent_pane_id)
            .expect("agent terminal")
            .clone();
        app.state
            .terminals
            .get_mut(&agent_terminal_id)
            .expect("agent terminal state")
            .detected_agent = Some(Agent::Codex);
        let editor_terminal_id = TerminalId::alloc();
        app.terminal_runtimes.insert(
            editor_terminal_id.clone(),
            TerminalRuntime::test_with_screen_bytes(30, 4, b"EDITOR"),
        );
        app.state.dock_collapsed = false;
        app.state.dock_editor_focused = true;
        app.state.dock_editor_sessions.insert(
            agent_pane_id,
            DockEditorSession {
                pane_id: PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );
        let runtime_count = app.terminal_runtimes.len();

        app.ensure_dock_editor();
        app.state.dock_tab = Some(DockSurface::Shortcuts);
        app.ensure_dock_editor();
        app.state.dock_collapsed = true;
        app.ensure_dock_editor();
        app.state.dock_collapsed = false;
        app.state.dock_tab = Some(DockSurface::Editor);
        app.ensure_dock_editor();

        assert_eq!(
            app.state
                .dock_editor_sessions
                .get(&agent_pane_id)
                .map(|session| &session.terminal_id),
            Some(&editor_terminal_id)
        );
        assert_eq!(app.terminal_runtimes.len(), runtime_count);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resizing_the_dock_resizes_the_existing_editor_pty() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("editor")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let agent_pane_id = app.state.workspaces[0]
            .focused_pane_id()
            .expect("focused pane");
        let agent_terminal_id = app.state.workspaces[0]
            .terminal_id(agent_pane_id)
            .expect("agent terminal")
            .clone();
        app.state
            .terminals
            .get_mut(&agent_terminal_id)
            .expect("agent terminal state")
            .detected_agent = Some(Agent::Codex);
        let editor_terminal_id = TerminalId::alloc();
        let runtime = TerminalRuntime::test_with_screen_bytes(30, 4, b"EDITOR");
        app.terminal_runtimes
            .insert(editor_terminal_id.clone(), runtime);
        app.state.dock_collapsed = false;
        // Home is the default tab now; the editor only resizes on its own tab.
        app.state.dock_tab = Some(DockSurface::Editor);
        app.state.view.dock_body_rect = Rect::new(0, 0, 18, 5);
        app.state.dock_editor_sessions.insert(
            agent_pane_id,
            DockEditorSession {
                pane_id: PaneId::alloc(),
                terminal_id: editor_terminal_id.clone(),
            },
        );

        app.resize_dock_editor();

        assert_eq!(
            app.terminal_runtimes
                .get(&editor_terminal_id)
                .expect("editor runtime")
                .current_size(),
            (5, 18)
        );
    }

    #[test]
    fn editor_does_not_spawn_without_a_focused_agent() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.dock_collapsed = false;
        app.state.dock_editor_focused = true;

        app.ensure_dock_editor();

        assert!(app.state.dock_editor_sessions.is_empty());
        assert_eq!(app.terminal_runtimes.len(), 0);
    }

    #[test]
    fn editor_body_says_to_focus_an_agent_without_a_focused_agent() {
        let app = AppState::test_new();
        let area = Rect::new(0, 0, 40, 4);
        let mut app = app;
        app.view.dock_body_rect = area;
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_editor_body(&app, &TerminalRuntimeRegistry::new(), frame))
            .expect("render editor body");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("focus an agent first"),
            "rendered text: {text:?}"
        );
    }
}
