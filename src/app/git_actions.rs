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

#[derive(Debug, Clone, Copy)]
pub(crate) struct GitActionPaneState {
    action: GitAction,
    source_pane_id: PaneId,
    phase: GitActionPanePhase,
}

pub(crate) fn wrapped_command(action: GitAction) -> String {
    let command = action.argv().join(" ");
    format!(
        r#"sh -c '{}; status=$?; printf "\n__t3_exit=%s\n" "$status"'"#,
        command
    )
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
    pub(crate) fn apply_git_action_request(&mut self) -> bool {
        let Some(action) = self.state.request_git_action.take() else {
            return false;
        };
        self.spawn_git_action_pane(action)
    }

    fn spawn_git_action_pane(&mut self, action: GitAction) -> bool {
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
        let cwd = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::active_tab)
            .and_then(|tab| {
                tab.cwd_for_pane(
                    source_pane_id,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
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
            warn!(
                ?action,
                "git action pane split did not create a focused pane"
            );
            return false;
        };
        let Some(runtime) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            ws_idx,
            action_pane_id,
        ) else {
            warn!(
                pane = action_pane_id.raw(),
                ?action,
                "git action pane has no runtime"
            );
            return false;
        };

        let command = format!("{}\r", wrapped_command(action));
        runtime.send_bytes_after(Bytes::from(command), COMMAND_SEND_DELAY);
        self.git_action_panes.insert(
            action_pane_id,
            GitActionPaneState {
                action,
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
            let Some(state) = self.git_action_panes.get(&pane_id).copied() else {
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
                    if exit_code != 0 {
                        self.git_action_panes.remove(&pane_id);
                        changed = true;
                        continue;
                    }

                    if state.action == GitAction::CreatePr {
                        changed |= self.apply_created_pr_url(state.source_pane_id, &screen);
                    }
                    self.mark_git_status_refresh_due(now);
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
        assert!(wrapped_command(GitAction::Commit).contains("git commit; status=$?"));
        assert!(!wrapped_command(GitAction::Commit).contains(" -m "));
    }

    #[test]
    fn exit_code_parser_requires_a_result_line() {
        assert_eq!(exit_code_from_screen("$ command\n__t3_exit=0\n$ "), Some(0));
        assert_eq!(exit_code_from_screen("failure\n__t3_exit=7\n$ "), Some(7));
        assert_eq!(exit_code_from_screen("echo __t3_exit=$?\n"), None);
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
