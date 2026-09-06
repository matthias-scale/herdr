use std::time::{Duration, Instant};

use bytes::Bytes;
use tracing::warn;

use super::{state::GitAction, App};
use crate::layout::PaneId;

const RESULT_PREFIX: &str = "__t3_exit=";
const SUCCESS_CLOSE_DELAY: Duration = Duration::from_millis(1500);
const COMMAND_SEND_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy)]
enum GitActionPanePhase {
    Running,
    Succeeded { close_at: Instant },
}

#[derive(Debug, Clone)]
enum BottomActionCompletion {
    Git(GitAction),
    User,
    WorktreeHooks {
        plan: Box<crate::app::home::HomeDispatchPlan>,
        create: Box<crate::app::state::WorktreeCreateState>,
        result_path: std::path::PathBuf,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct GitActionPaneState {
    completion: BottomActionCompletion,
    source_pane_id: PaneId,
    phase: GitActionPanePhase,
}

pub(crate) fn wrapped_command(action: GitAction, commit_message_model: &str) -> String {
    let command = action.argv().join(" ");
    // The Commit action is the one place a message model is meaningful. It is
    // exported rather than spliced into the command line, so a
    // `prepare-commit-msg` hook can use it and an unset model leaves the
    // command byte for byte what it was.
    let prefix = match (action, sanitized_model(commit_message_model)) {
        (GitAction::Commit, Some(model)) => format!("HERDR_COMMIT_MESSAGE_MODEL={model} "),
        _ => String::new(),
    };
    format!(r#"sh -c '{prefix}{command}; status=$?; printf "\n__t3_exit=%s\n" "$status"'"#)
}

/// A model name only reaches the shell when it is a bare token, so the export
/// can never carry quoting or a command substitution into `sh -c`.
fn sanitized_model(model: &str) -> Option<&str> {
    let model = model.trim();
    if model.is_empty()
        || !model.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return None;
    }
    Some(model)
}

pub(crate) fn wrapped_user_command(command: &str) -> String {
    format!(
        r#"sh -c {}; status=$?; printf "\n__t3_exit=%s\n" "$status""#,
        shell_single_quote(command)
    )
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

pub(crate) fn wrapped_worktree_hooks(actions: &[crate::config::UserAction]) -> String {
    let commands = actions
        .iter()
        .map(|action| {
            format!(
                "sh -c {}; code=$?; if [ \"$code\" -ne 0 ]; then status=$code; fi",
                shell_single_quote(&action.command)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(r#"status=0; {commands}; printf "\n__t3_exit=%s\n" "$status""#)
}

pub(crate) fn exit_code_from_screen(screen: &str) -> Option<i32> {
    screen.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix(RESULT_PREFIX)
            .and_then(|value| value.parse().ok())
    })
}

fn apply_pr_url_from_screen(
    terminal: &mut crate::terminal::TerminalState,
    screen: &str,
) -> Result<bool, String> {
    let Some(url) = crate::work_context::extract_pr_urls(screen)
        .into_iter()
        .last()
    else {
        return Ok(false);
    };
    let mut pr_urls = terminal.effective_work_context().pr_urls.clone();
    if pr_urls.contains(&url) {
        return Ok(false);
    }
    pr_urls.push(url);
    terminal.apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
        pr_urls: Some(pr_urls),
        ..Default::default()
    })
}

impl App {
    pub(crate) fn apply_pr_land_request(&mut self) -> bool {
        let Some(request) = self.state.request_pr_land.take() else {
            return false;
        };
        let cwd = self.state.workspaces.iter().find_map(|workspace| {
            let matches = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.values())
                .any(|pane| {
                    self.state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .and_then(|terminal| terminal.effective_work_context().repo.as_deref())
                        .is_some_and(|repo| {
                            crate::work_context::repo_slugs_match(repo, &request.repo)
                        })
                });
            matches.then(|| workspace.identity_cwd.clone())
        });
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let before = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id);
        self.runtime_pane_split(
            "tui.pr-land.split",
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Down,
                ratio: None,
                cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
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
            return false;
        };
        let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
            return false;
        };
        if !request
            .head_sha
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        {
            tracing::warn!("refusing PR land request with a non-hex head SHA");
            return false;
        }
        let command = pr_land_argv(&request).join(" ");
        runtime.send_bytes_after(Bytes::from(format!("{command}\r")), COMMAND_SEND_DELAY);
        self.state.clear_work_view();
        true
    }

    pub(crate) fn apply_git_action_request(&mut self) -> bool {
        let Some(action) = self.state.request_git_action.take() else {
            return false;
        };
        self.spawn_git_action_pane(action)
    }

    fn spawn_git_action_pane(&mut self, action: GitAction) -> bool {
        self.spawn_bottom_action_pane(
            wrapped_command(action, &self.state.commit_message_model),
            BottomActionCompletion::Git(action),
            None,
        )
    }

    pub(crate) fn apply_user_action_request(&mut self) -> bool {
        let Some(index) = self.state.request_user_action.take() else {
            return false;
        };
        let repo = self.state.focused_repo_slug();
        let Some(action) = self
            .state
            .keybinds
            .user_actions
            .get(index)
            .filter(|action| action.applies_to_repo(repo.as_deref()))
            .cloned()
        else {
            return false;
        };
        if !action.open_in_bottom_pane {
            if let Some(ws_idx) = self.state.active {
                if let Some(runtime) = self
                    .state
                    .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
                {
                    if crate::app::agents::available_shell_name(runtime).is_some() {
                        let bytes = crate::app::api_helpers::encode_api_submission(
                            runtime,
                            &action.command,
                        );
                        return runtime.try_send_bytes(Bytes::from(bytes)).is_ok();
                    }
                }
            }
        }
        self.spawn_bottom_action_pane(
            wrapped_user_command(&action.command),
            BottomActionCompletion::User,
            None,
        )
    }

    pub(crate) fn spawn_worktree_hook_pane(
        &mut self,
        actions: &[crate::config::UserAction],
        plan: crate::app::home::HomeDispatchPlan,
        create: crate::app::state::WorktreeCreateState,
        result_path: std::path::PathBuf,
    ) -> bool {
        self.spawn_bottom_action_pane(
            wrapped_worktree_hooks(actions),
            BottomActionCompletion::WorktreeHooks {
                plan: Box::new(plan),
                create: Box::new(create),
                result_path: result_path.clone(),
            },
            Some(result_path),
        )
    }

    fn spawn_bottom_action_pane(
        &mut self,
        command: String,
        completion: BottomActionCompletion,
        cwd_override: Option<std::path::PathBuf>,
    ) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(source_pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
        else {
            return false;
        };
        let cwd = cwd_override.or_else(|| {
            self.state
                .workspaces
                .get(ws_idx)
                .and_then(crate::workspace::Workspace::active_tab)
                .and_then(|tab| {
                    tab.cwd_for_pane(
                        source_pane_id,
                        &self.state.terminals,
                        &self.terminal_runtimes,
                    )
                })
        });

        self.runtime_pane_split(
            "tui.git-action.split",
            crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Down,
                ratio: None,
                cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
                focus: true,
                env: Default::default(),
                work_context: None,
            },
        );

        let Some(action_pane_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
            .filter(|pane_id| *pane_id != source_pane_id)
        else {
            warn!("bottom action pane split did not create a focused pane");
            return false;
        };
        let Some(runtime) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            ws_idx,
            action_pane_id,
        ) else {
            warn!(
                pane = action_pane_id.raw(),
                "bottom action pane has no runtime"
            );
            return false;
        };

        runtime.send_bytes_after(Bytes::from(format!("{command}\r")), COMMAND_SEND_DELAY);
        self.git_action_panes.insert(
            action_pane_id,
            GitActionPaneState {
                completion,
                source_pane_id,
                phase: GitActionPanePhase::Running,
            },
        );
        true
    }

    pub(crate) fn git_action_deadline(&self) -> Option<Instant> {
        self.git_action_panes
            .values()
            .filter_map(|state| match state.phase {
                GitActionPanePhase::Running => None,
                GitActionPanePhase::Succeeded { close_at } => Some(close_at),
            })
            .min()
    }

    pub(crate) fn process_git_action_panes(&mut self, now: Instant) -> bool {
        let pane_ids = self.git_action_panes.keys().copied().collect::<Vec<_>>();
        let mut changed = false;

        for pane_id in pane_ids {
            let Some(state) = self.git_action_panes.get(&pane_id).cloned() else {
                continue;
            };
            match state.phase {
                GitActionPanePhase::Succeeded { close_at } if now >= close_at => {
                    self.git_action_panes.remove(&pane_id);
                    if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                        if let Some(public_id) = self.public_pane_id(ws_idx, pane_id) {
                            self.runtime_pane_close("tui.git-action.close", public_id);
                            changed = true;
                        }
                    }
                }
                GitActionPanePhase::Succeeded { .. } => {}
                GitActionPanePhase::Running => {
                    let Some((_, pane)) = self.find_pane(pane_id) else {
                        self.git_action_panes.remove(&pane_id);
                        continue;
                    };
                    let terminal_id = pane.attached_terminal_id.clone();
                    let Some(screen) = self
                        .terminal_runtimes
                        .get(&terminal_id)
                        .map(|runtime| runtime.recent_unwrapped_text_snapshot(32).text)
                    else {
                        continue;
                    };
                    let Some(exit_code) = exit_code_from_screen(&screen) else {
                        continue;
                    };
                    if let BottomActionCompletion::WorktreeHooks {
                        plan,
                        create,
                        result_path,
                    } = state.completion.clone()
                    {
                        self.git_action_panes.remove(&pane_id);
                        self.finish_home_worktree_hooks(
                            *plan,
                            *create,
                            result_path,
                            exit_code != 0,
                        );
                        if exit_code != 0 {
                            changed = true;
                            continue;
                        }
                        self.git_action_panes.insert(
                            pane_id,
                            GitActionPaneState {
                                phase: GitActionPanePhase::Succeeded {
                                    close_at: now + SUCCESS_CLOSE_DELAY,
                                },
                                ..state
                            },
                        );
                        changed = true;
                        continue;
                    }
                    if exit_code != 0 {
                        self.git_action_panes.remove(&pane_id);
                        changed = true;
                        continue;
                    }

                    if matches!(
                        &state.completion,
                        BottomActionCompletion::Git(GitAction::CreatePr)
                    ) {
                        changed |= self.apply_created_pr_url(state.source_pane_id, &screen);
                    }
                    if matches!(&state.completion, BottomActionCompletion::Git(_)) {
                        self.mark_git_status_refresh_due(now);
                    }
                    if let Some(action_state) = self.git_action_panes.get_mut(&pane_id) {
                        action_state.phase = GitActionPanePhase::Succeeded {
                            close_at: now + SUCCESS_CLOSE_DELAY,
                        };
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    fn apply_created_pr_url(&mut self, source_pane_id: PaneId, screen: &str) -> bool {
        let Some((ws_idx, pane)) = self.find_pane(source_pane_id) else {
            return false;
        };
        let terminal_id = pane.attached_terminal_id.clone();
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .and_then(|terminal| apply_pr_url_from_screen(terminal, screen).ok())
            .unwrap_or(false);
        if changed {
            self.schedule_session_save();
            self.emit_pane_updated(ws_idx, source_pane_id);
        }
        changed
    }
}

pub(crate) fn pr_land_argv(request: &crate::app::state::PrLandConfirmation) -> Vec<String> {
    vec![
        "gh".into(),
        "pr".into(),
        "merge".into(),
        request.number.to_string(),
        "--squash".into(),
        "--match-head-commit".into(),
        request.head_sha.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_action_item_maps_to_expected_argv() {
        assert_eq!(GitAction::Pull.argv(), &["git", "pull", "--rebase"]);
        assert_eq!(GitAction::Commit.argv(), &["git", "commit"]);
        assert_eq!(GitAction::Push.argv(), &["git", "push"]);
        assert_eq!(
            GitAction::CreatePr.argv(),
            &["gh", "pr", "create", "--fill"]
        );
        assert!(wrapped_command(GitAction::Commit, "").contains("git commit; status=$?"));
        assert!(!wrapped_command(GitAction::Commit, "").contains(" -m "));
        assert!(wrapped_command(GitAction::Commit, "claude-opus-5")
            .contains("HERDR_COMMIT_MESSAGE_MODEL=claude-opus-5 git commit"));
        // Pull is untouched, and an unusable model name is ignored.
        assert!(!wrapped_command(GitAction::Pull, "claude-opus-5").contains("HERDR_COMMIT"));
        assert!(!wrapped_command(GitAction::Commit, "a; rm -rf /").contains("HERDR_COMMIT"));
    }

    #[test]
    fn exit_code_parser_requires_a_result_line() {
        assert_eq!(exit_code_from_screen("$ command\n__t3_exit=0\n$ "), Some(0));
        assert_eq!(exit_code_from_screen("failure\n__t3_exit=7\n$ "), Some(7));
        assert_eq!(exit_code_from_screen("echo __t3_exit=$?\n"), None);
    }

    #[test]
    fn worktree_hooks_keep_config_order_in_one_shell_command() {
        let actions = [
            crate::config::UserAction {
                name: "first".into(),
                command: "touch first".into(),
                bindings: Default::default(),
                run_on_worktree_create: true,
                open_in_bottom_pane: true,
                repo: None,
            },
            crate::config::UserAction {
                name: "second".into(),
                command: "touch second".into(),
                bindings: Default::default(),
                run_on_worktree_create: true,
                open_in_bottom_pane: true,
                repo: None,
            },
        ];

        let command = wrapped_worktree_hooks(&actions);
        assert!(
            command.find("touch first").expect("first hook")
                < command.find("touch second").expect("second hook")
        );
        assert_eq!(command.matches(RESULT_PREFIX).count(), 1);
        assert!(command.contains("status=$code"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn focused_shell_user_action_writes_command_and_enter() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        workspace.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.keybinds.user_actions = vec![crate::config::UserAction {
            name: "test".into(),
            command: "just test".into(),
            bindings: Default::default(),
            run_on_worktree_create: false,
            open_in_bottom_pane: false,
            repo: None,
        }];
        app.state.request_user_action = Some(0);

        assert!(app.apply_user_action_request());
        assert_eq!(
            rx.try_recv().expect("command bytes").as_ref(),
            b"just test\r"
        );
    }

    #[test]
    fn land_command_is_bound_to_confirmed_head() {
        let request = crate::app::state::PrLandConfirmation {
            repo: "owner/repo".into(),
            number: 42,
            head_sha: "abc123".into(),
            approval_signal: "approved review".into(),
        };
        assert_eq!(
            pr_land_argv(&request),
            [
                "gh",
                "pr",
                "merge",
                "42",
                "--squash",
                "--match-head-commit",
                "abc123"
            ]
        );
    }

    #[test]
    fn pr_url_screen_result_patches_work_context() {
        let mut terminal = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/repo"),
        );
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/acme/repo/pull/12".into()]),
                ..Default::default()
            })
            .expect("seed work context");
        let fixture = "Creating pull request for topic into main\nhttps://github.com/acme/repo/pull/42\n\n__t3_exit=0\n$ ";

        assert!(apply_pr_url_from_screen(&mut terminal, fixture).expect("valid PR URL"));
        assert_eq!(
            terminal.effective_work_context().pr_urls,
            vec![
                "https://github.com/acme/repo/pull/12",
                "https://github.com/acme/repo/pull/42"
            ]
        );
    }
}
