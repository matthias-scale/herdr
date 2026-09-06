//! Open the focused pane's repository in a right-side Vim-family editor.

use std::path::{Path, PathBuf};

use bytes::Bytes;

use super::{App, AppState};
use crate::layout::{NavDirection, PaneId};

const EDITOR_SEND_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

pub(crate) fn resolve_repo_editor_argv() -> Option<Vec<String>> {
    let editor = std::env::var("EDITOR").ok();
    resolve_repo_editor_argv_with(editor.as_deref(), |command| {
        crate::integration::command_available(command)
    })
}

fn resolve_repo_editor_argv_with(
    editor: Option<&str>,
    command_available: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let configured = editor
        .and_then(crate::ui::dock::editor::parse_editor_command)
        .filter(|argv| editor_argv_is_vim_family(argv))
        .filter(|argv| command_available(&argv[0]));
    configured.or_else(|| command_available("nvim").then(|| vec!["nvim".to_string()]))
}

fn editor_name_is_vim_family(command: &str) -> bool {
    let name = Path::new(command)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "vi" | "vim" | "view" | "gvim" | "vimdiff" | "nvim" | "nvim-qt"
    )
}

fn editor_argv_is_vim_family(argv: &[String]) -> bool {
    argv.first()
        .is_some_and(|command| editor_name_is_vim_family(command))
}

fn workspace_checkout_root(workspace: &crate::workspace::Workspace) -> PathBuf {
    workspace
        .worktree_space()
        .map(|space| space.checkout_path.clone())
        .or_else(|| workspace.git_space().map(|space| space.repo_root.clone()))
        .unwrap_or_else(|| workspace.identity_cwd.clone())
}

fn cached_root(app: &AppState, candidate: PathBuf) -> PathBuf {
    app.git_root_for_cwd
        .get(&candidate)
        .cloned()
        .flatten()
        .unwrap_or(candidate)
}

impl AppState {
    pub(crate) fn repo_editor_available(&self) -> bool {
        self.repo_editor_argv.is_some()
    }

    /// Resolve the checkout named by work context before trusting the focused
    /// pane's cwd. A pane can run in one checkout while working on another.
    pub(crate) fn focused_repo_editor_root(&self) -> Option<PathBuf> {
        let workspace = self.active.and_then(|index| self.workspaces.get(index))?;
        let pane_id = workspace.focused_pane_id()?;
        let terminal = workspace
            .terminal_id(pane_id)
            .and_then(|terminal_id| self.terminals.get(terminal_id))?;

        if let Some(repo) = terminal.effective_work_context().repo.as_deref() {
            if let Some(root) =
                self.workspaces
                    .iter()
                    .find(|candidate| {
                        candidate.repo_binding.as_deref().is_some_and(|binding| {
                            crate::work_context::repo_slugs_match(binding, repo)
                        })
                    })
                    .map(workspace_checkout_root)
            {
                return Some(cached_root(self, root));
            }

            if let Some(root) = self
                .workspaces
                .iter()
                .flat_map(|candidate| candidate.tabs.iter())
                .flat_map(|tab| tab.panes.values())
                .filter_map(|pane| self.terminals.get(&pane.attached_terminal_id))
                .filter(|candidate| candidate.id != terminal.id)
                .find(|candidate| {
                    candidate
                        .effective_work_context()
                        .repo
                        .as_deref()
                        .is_some_and(|candidate_repo| {
                            crate::work_context::repo_slugs_match(candidate_repo, repo)
                        })
                })
                .map(|candidate| cached_root(self, candidate.cwd.clone()))
            {
                return Some(root);
            }
        }

        let cwd = self
            .status_focused_cwd
            .clone()
            .unwrap_or_else(|| terminal.cwd.clone());
        self.git_root_for_cwd
            .get(&cwd)
            .cloned()
            .flatten()
            .or_else(|| {
                workspace
                    .git_space()
                    .filter(|space| cwd.starts_with(&space.repo_root))
                    .map(|space| space.repo_root.clone())
            })
    }

    pub(crate) fn right_repo_editor_sibling(&self, root: &Path) -> Option<PaneId> {
        let workspace = self.active.and_then(|index| self.workspaces.get(index))?;
        let sibling = workspace
            .active_tab()?
            .layout
            .adjacent_sibling_pane(NavDirection::Right)?;
        let terminal = workspace
            .terminal_id(sibling)
            .and_then(|terminal_id| self.terminals.get(terminal_id))?;
        (terminal.cwd == root
            && terminal
                .foreground_process_name
                .as_deref()
                .is_some_and(editor_name_is_vim_family))
        .then_some(sibling)
    }
}

impl App {
    pub(crate) fn apply_open_repo_editor_request(&mut self) -> bool {
        if !std::mem::take(&mut self.state.request_open_repo_editor) {
            return false;
        }
        self.open_repo_editor();
        true
    }

    pub(crate) fn open_repo_editor(&mut self) {
        let Some(argv) = self.state.repo_editor_argv.clone() else {
            self.show_work_link_notice("no Vim-family editor found on PATH");
            return;
        };
        let Some(root) = self.state.focused_repo_editor_root() else {
            self.show_work_link_notice("no repository for this pane");
            return;
        };
        let Some(ws_idx) = self.state.active else {
            return;
        };
        if let Some(sibling) = self.state.right_repo_editor_sibling(&root) {
            self.focus_pane_internal_via_api(ws_idx, sibling);
            return;
        }

        let before = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id);
        self.runtime_pane_split(
            "tui.editor.open_repo.split",
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: Some(root.to_string_lossy().into_owned()),
                focus: true,
                env: Default::default(),
                work_context: None,
            },
        );
        let Some(pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .filter(|pane_id| Some(*pane_id) != before)
        else {
            return;
        };
        let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
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
        runtime.send_bytes_after(Bytes::from(bytes), EDITOR_SEND_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{terminal::TerminalState, workspace::Workspace};

    #[test]
    fn editor_resolution_prefers_only_a_vim_family_editor() {
        assert_eq!(
            resolve_repo_editor_argv_with(Some("vim -f"), |command| command == "vim"),
            Some(vec!["vim".into(), "-f".into()])
        );
        assert_eq!(
            resolve_repo_editor_argv_with(Some("code --wait"), |command| command == "nvim"),
            Some(vec!["nvim".into()])
        );
        assert_eq!(
            resolve_repo_editor_argv_with(Some("vim"), |command| command == "nvim"),
            Some(vec!["nvim".into()])
        );
        assert_eq!(resolve_repo_editor_argv_with(Some("code"), |_| false), None);
    }

    #[test]
    fn repo_editor_root_prefers_the_work_context_checkout_then_falls_back_to_git_root() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("focused")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let focused_cwd = PathBuf::from("/shell/elsewhere");
        let focused_terminal = app.workspaces[0]
            .terminal_id(app.workspaces[0].tabs[0].root_pane)
            .cloned()
            .expect("focused terminal");
        app.terminals.get_mut(&focused_terminal).expect("state").cwd = focused_cwd.clone();
        app.terminals
            .get_mut(&focused_terminal)
            .expect("state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: Some("owner/repo".into()),
                ..Default::default()
            })
            .expect("valid context");

        let mut bound = Workspace::test_new("bound");
        bound.repo_binding = Some("OWNER/REPO".into());
        bound.identity_cwd = PathBuf::from("/checkouts/repo/nested");
        app.workspaces.push(bound);
        app.git_root_for_cwd.insert(
            PathBuf::from("/checkouts/repo/nested"),
            Some(PathBuf::from("/checkouts/repo")),
        );
        app.git_root_for_cwd.insert(
            focused_cwd.clone(),
            Some(PathBuf::from("/shell/wrong-repo")),
        );
        assert_eq!(
            app.focused_repo_editor_root(),
            Some(PathBuf::from("/checkouts/repo"))
        );

        app.terminals
            .get_mut(&focused_terminal)
            .expect("state")
            .work_context = Default::default();
        assert_eq!(
            app.focused_repo_editor_root(),
            Some(PathBuf::from("/shell/wrong-repo"))
        );
    }

    #[test]
    fn right_sibling_is_reused_only_for_an_editor_at_the_same_root() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("focused")];
        app.active = Some(0);
        let sibling = app.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.ensure_test_terminals();
        let sibling_terminal = app.workspaces[0]
            .terminal_id(sibling)
            .cloned()
            .expect("sibling terminal");
        let terminal = app.terminals.get_mut(&sibling_terminal).expect("state");
        terminal.cwd = PathBuf::from("/repo");
        terminal.set_foreground_process(Some("nvim".into()), true, std::time::Instant::now());
        let root_pane = app.workspaces[0].tabs[0].root_pane;
        app.workspaces[0].tabs[0].layout.focus_pane(root_pane);

        assert_eq!(
            app.right_repo_editor_sibling(Path::new("/repo")),
            Some(sibling)
        );
        assert_eq!(app.right_repo_editor_sibling(Path::new("/other")), None);

        app.terminals.insert(
            sibling_terminal.clone(),
            TerminalState::new(sibling_terminal, PathBuf::from("/repo")),
        );
        assert_eq!(app.right_repo_editor_sibling(Path::new("/repo")), None);
    }

    #[test]
    fn opening_repo_editor_focuses_a_matching_right_sibling_without_splitting() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("focused")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let sibling = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        let root_pane = app.state.workspaces[0].tabs[0].root_pane;
        let root_terminal = app.state.workspaces[0]
            .terminal_id(root_pane)
            .cloned()
            .expect("root terminal");
        let sibling_terminal = app.state.workspaces[0]
            .terminal_id(sibling)
            .cloned()
            .expect("sibling terminal");
        app.state
            .terminals
            .get_mut(&root_terminal)
            .expect("state")
            .cwd = "/repo".into();
        app.state
            .git_root_for_cwd
            .insert("/repo".into(), Some("/repo".into()));
        let terminal = app
            .state
            .terminals
            .get_mut(&sibling_terminal)
            .expect("state");
        terminal.cwd = "/repo".into();
        terminal.set_foreground_process(Some("vim".into()), true, std::time::Instant::now());
        app.state.workspaces[0].tabs[0].layout.focus_pane(root_pane);
        app.state.repo_editor_argv = Some(vec!["nvim".into()]);

        app.open_repo_editor();

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(sibling));
    }
}
