use std::time::{Duration, Instant};

use bytes::Bytes;
use tracing::warn;

use super::{state::GitAction, App};
use crate::layout::PaneId;

const RESULT_PREFIX: &str = "__t3_exit=";
const COMMIT_MESSAGE_INSTRUCTION: &str = "Write a lowercase conventional commit message for this diff. Return only the subject and optional body. Keep the subject concise and use an imperative verb.";
const SUCCESS_CLOSE_DELAY: Duration = Duration::from_millis(1500);
const COMMAND_SEND_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct GitActionPrograms {
    git: std::path::PathBuf,
    gh: std::path::PathBuf,
    claude: std::path::PathBuf,
    codex: std::path::PathBuf,
}

impl Default for GitActionPrograms {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
            claude: "claude".into(),
            codex: "codex".into(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum GitActionPanePhase {
    Running,
    Succeeded { close_at: Instant },
}

#[derive(Debug, Clone)]
enum BottomActionCompletion {
    Git(GitAction),
    Pr,
    User,
    AddProjectClone {
        target: std::path::PathBuf,
    },
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

pub(crate) fn wrapped_command(
    action: GitAction,
    commit_message_model: &str,
    commit_stage_all: bool,
) -> String {
    wrapped_command_with_programs(
        action,
        commit_message_model,
        commit_stage_all,
        &GitActionPrograms::default(),
    )
}

fn wrapped_command_with_programs(
    action: GitAction,
    commit_message_model: &str,
    commit_stage_all: bool,
    programs: &GitActionPrograms,
) -> String {
    if action == GitAction::Commit {
        if let Some(generator) = commit_generator_argv(commit_message_model, programs) {
            return wrapped_generated_commit(programs, &generator, commit_stage_all);
        }
    }
    if action == GitAction::CreatePr {
        return wrapped_create_pr(programs);
    }

    let argv = match action {
        GitAction::Pull => vec![
            programs.git.to_string_lossy().into_owned(),
            "pull".into(),
            "--rebase".into(),
        ],
        GitAction::Commit => vec![programs.git.to_string_lossy().into_owned(), "commit".into()],
        GitAction::Push => vec![programs.git.to_string_lossy().into_owned(), "push".into()],
        GitAction::CreatePr => unreachable!("Create PR uses its summary wrapper"),
    };
    wrapped_script_with_result(&shell_argv(&argv))
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

fn commit_generator_argv(model: &str, programs: &GitActionPrograms) -> Option<Vec<String>> {
    let model = sanitized_model(model)?;
    if model.starts_with("claude") {
        Some(vec![
            programs.claude.to_string_lossy().into_owned(),
            "-p".into(),
            "--model".into(),
            model.into(),
            "--tools".into(),
            String::new(),
            "--permission-prompts".into(),
            "none".into(),
            "--no-session-persistence".into(),
            "--output-format".into(),
            "text".into(),
            COMMIT_MESSAGE_INSTRUCTION.into(),
        ])
    } else {
        Some(vec![
            programs.codex.to_string_lossy().into_owned(),
            "exec".into(),
            "--sandbox".into(),
            "read-only".into(),
            "--model".into(),
            model.into(),
            COMMIT_MESSAGE_INSTRUCTION.into(),
        ])
    }
}

fn wrapped_generated_commit(
    programs: &GitActionPrograms,
    generator: &[String],
    commit_stage_all: bool,
) -> String {
    let git = shell_single_quote(&programs.git.to_string_lossy());
    let stage = if commit_stage_all {
        format!("{git} add -A; stage_status=$?")
    } else {
        "stage_status=0".to_string()
    };
    let generator = shell_argv(generator);
    let script = format!(
        r#"tmp_dir=$(mktemp -d "${{TMPDIR:-/tmp}}/herdr-commit.XXXXXX" 2>/dev/null)
if [ -z "$tmp_dir" ]; then
  printf '\nCommit message generator unavailable; opening editor.\n'
  {git} commit
else
  diff_file="$tmp_dir/diff"
  message_file="$tmp_dir/message"
  trap 'rm -f "$diff_file" "$message_file"; rmdir "$tmp_dir" 2>/dev/null || true' EXIT HUP INT TERM
  {stage}
  if [ "$stage_status" -ne 0 ]; then
    false
  else
    {git} diff --cached > "$diff_file"
    diff_status=$?
    if [ "$diff_status" -eq 0 ] && [ ! -s "$diff_file" ]; then
      {git} diff > "$diff_file"
      diff_status=$?
    fi
    if [ "$diff_status" -eq 0 ]; then
      {generator} < "$diff_file" > "$message_file"
      generator_status=$?
    else
      generator_status=$diff_status
    fi
    if [ "$generator_status" -eq 0 ] && grep -q '[^[:space:]]' "$message_file"; then
      printf '\nGenerated commit message:\n'
      cat "$message_file"
      printf '\n'
      {git} commit -F "$message_file"
    else
      printf '\nCommit message generator unavailable; opening editor.\n'
      {git} commit
    fi
  fi
fi"#
    );
    wrapped_script_with_result(&script)
}

fn wrapped_create_pr(programs: &GitActionPrograms) -> String {
    let gh = shell_argv(&[
        programs.gh.to_string_lossy().into_owned(),
        "pr".into(),
        "create".into(),
        "--fill".into(),
    ]);
    let script = format!(
        r#"output=$({gh} 2>&1)
status=$?
printf '%s\n' "$output"
if [ "$status" -eq 0 ]; then
  url=$(printf '%s\n' "$output" | grep -Eo 'https://github\.com/[^[:space:]]+/[^[:space:]]+/pull/[0-9]+' | tail -n 1)
  if [ -n "$url" ]; then
    printf '\nPull request created: %s\n' "$url"
  fi
fi
printf '\n{RESULT_PREFIX}%s\n' "$status""#
    );
    format!("sh -c {}", shell_single_quote(&script))
}

fn wrapped_script_with_result(script: &str) -> String {
    let script = format!(
        r#"{script}
status=$?
printf '\n{RESULT_PREFIX}%s\n' "$status""#
    );
    format!("sh -c {}", shell_single_quote(&script))
}

fn shell_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| shell_single_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn wrapped_user_command(command: &str) -> String {
    format!(
        r#"sh -c {}; status=$?; printf "\n__t3_exit=%s\n" "$status""#,
        shell_single_quote(command)
    )
}

pub(crate) fn wrapped_argv(argv: &[String]) -> String {
    let command = argv
        .iter()
        .map(|argument| shell_single_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"sh -c {}; status=$?; printf "\n__t3_exit=%s\n" "$status""#,
        shell_single_quote(&command)
    )
}

pub(crate) fn wrapped_clone_command(
    git_program: &std::path::Path,
    url: &str,
    target: &std::path::Path,
) -> String {
    let command = format!(
        "{} clone -- {} {}",
        shell_single_quote(&git_program.to_string_lossy()),
        shell_single_quote(url),
        shell_single_quote(&target.to_string_lossy())
    );
    format!(
        r#"sh -c {}; status=$?; printf "\n__t3_exit=%s\n" "$status""#,
        shell_single_quote(&command)
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

fn pr_url_from_screen(screen: &str) -> Option<String> {
    crate::work_context::extract_pr_urls(screen)
        .into_iter()
        .last()
}

fn apply_pr_url(terminal: &mut crate::terminal::TerminalState, url: &str) -> Result<bool, String> {
    terminal.set_inferred_pr_url(url.to_string())
}

impl App {
    pub(crate) fn apply_pr_command_request(&mut self) -> bool {
        let Some(request) = self.state.request_pr_command.take() else {
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
        let argv = pr_command_argv(&self.work_index_gh_program(), &request);
        let spawned =
            self.spawn_bottom_action_pane(wrapped_argv(&argv), BottomActionCompletion::Pr, cwd);
        if !spawned {
            return false;
        }
        self.state.clear_work_view();
        self.next_work_index_refresh = Instant::now();
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
            wrapped_command(
                action,
                &self.state.commit_message_model,
                self.state.commit_stage_all,
            ),
            BottomActionCompletion::Git(action),
            None,
        )
    }

    pub(crate) fn apply_add_project_clone_request(&mut self) -> bool {
        let request = self
            .state
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
            .and_then(|project| project.clone_request.take());
        let Some(request) = request else {
            return false;
        };
        let command = wrapped_clone_command(
            &self.git_program_for_refresh(),
            &request.url,
            &request.target,
        );
        if self.spawn_bottom_action_pane(
            command,
            BottomActionCompletion::AddProjectClone {
                target: request.target,
            },
            None,
        ) {
            return true;
        }
        if let Some(project) = self
            .state
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.clone_pending = false;
            project.error = Some("A bottom pane is required to clone this project.".into());
        }
        true
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
                right_click: Default::default(),
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
                    if let BottomActionCompletion::AddProjectClone { target } =
                        state.completion.clone()
                    {
                        if exit_code == 0 {
                            self.state.finish_add_project_clone(target, true);
                            self.git_action_panes.insert(
                                pane_id,
                                GitActionPaneState {
                                    phase: GitActionPanePhase::Succeeded {
                                        close_at: now + SUCCESS_CLOSE_DELAY,
                                    },
                                    ..state
                                },
                            );
                        } else {
                            self.git_action_panes.remove(&pane_id);
                            self.state.finish_add_project_clone(target, false);
                        }
                        changed = true;
                        continue;
                    }
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
                        changed |= self
                            .apply_created_pr_url(state.source_pane_id, &screen)
                            .is_some();
                    }
                    if matches!(&state.completion, BottomActionCompletion::Git(_)) {
                        self.mark_git_status_refresh_due(now);
                    }
                    if matches!(&state.completion, BottomActionCompletion::Pr) {
                        self.next_work_index_refresh = now;
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

    fn apply_created_pr_url(&mut self, source_pane_id: PaneId, screen: &str) -> Option<String> {
        let url = pr_url_from_screen(screen)?;
        let (ws_idx, pane) = self.find_pane(source_pane_id)?;
        let terminal_id = pane.attached_terminal_id.clone();
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .and_then(|terminal| apply_pr_url(terminal, &url).ok())
            .unwrap_or(false);
        if changed {
            self.schedule_session_save();
            self.emit_pane_updated(ws_idx, source_pane_id);
        }
        let previous_toast = self.state.toast.clone();
        self.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pull request created".into(),
            context: url.clone(),
            position: None,
            target: None,
        });
        self.sync_toast_deadline(previous_toast);
        if self
            .event_tx
            .try_send(crate::events::AppEvent::ClipboardWrite {
                content: url.as_bytes().to_vec(),
            })
            .is_err()
        {
            tracing::warn!(%url, "failed to queue pull request URL clipboard event");
        }
        Some(url)
    }
}

pub(crate) fn pr_command_argv(
    gh_program: &std::path::Path,
    request: &crate::app::state::PrCommandRequest,
) -> Vec<String> {
    let mut argv = vec![gh_program.to_string_lossy().into_owned(), "pr".into()];
    match request.action {
        crate::app::state::PrCommandAction::Merge(method) => {
            argv.extend([
                "merge".into(),
                request.number.to_string(),
                method.flag().into(),
            ]);
        }
        crate::app::state::PrCommandAction::OpenOnGithub => {
            argv.extend(["view".into(), request.number.to_string(), "--web".into()]);
        }
    }
    argv
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
        assert!(!wrapped_command(GitAction::Commit, "", false).contains(" -F "));
        assert!(wrapped_command(GitAction::Commit, "claude-opus-5", false)
            .contains(COMMIT_MESSAGE_INSTRUCTION));
        // Pull is untouched, and an unusable model name keeps the editor path.
        assert!(!wrapped_command(GitAction::Pull, "claude-opus-5", false).contains("claude"));
        assert!(!wrapped_command(GitAction::Commit, "a; rm -rf /", false).contains("codex"));
    }

    #[test]
    fn commit_generator_argv_uses_each_agents_noninteractive_read_only_mode() {
        let programs = GitActionPrograms::default();
        assert_eq!(
            commit_generator_argv("claude-opus-5", &programs).expect("Claude generator"),
            [
                "claude",
                "-p",
                "--model",
                "claude-opus-5",
                "--tools",
                "",
                "--permission-prompts",
                "none",
                "--no-session-persistence",
                "--output-format",
                "text",
                COMMIT_MESSAGE_INSTRUCTION,
            ]
        );
        assert_eq!(
            commit_generator_argv("gpt-5.6-codex", &programs).expect("Codex generator"),
            [
                "codex",
                "exec",
                "--sandbox",
                "read-only",
                "--model",
                "gpt-5.6-codex",
                COMMIT_MESSAGE_INSTRUCTION,
            ]
        );
        assert!(commit_generator_argv("bad model;", &programs).is_none());
    }

    #[cfg(unix)]
    fn executable_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("write fake program");
        let mut permissions = std::fs::metadata(path)
            .expect("fake program metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake program executable");
    }

    #[cfg(unix)]
    fn generator_fixture(label: &str) -> (std::path::PathBuf, GitActionPrograms) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let git = root.join("fake git");
        let claude = root.join("fake claude");
        let codex = root.join("fake codex");
        let gh = root.join("fake gh");
        executable_script(
            &git,
            r#"printf '%s\n' "$*" >> "$HERDR_TEST_GIT_LOG"
case "$1" in
  add) exit "${HERDR_TEST_ADD_STATUS:-0}" ;;
  diff)
    if [ "$2" = "--cached" ]; then
      printf '%s' "$HERDR_TEST_CACHED_DIFF"
    else
      printf '%s' "$HERDR_TEST_UNSTAGED_DIFF"
    fi
    ;;
  commit)
    if [ "$2" = "-F" ]; then
      cp "$3" "$HERDR_TEST_COMMIT_MESSAGE"
    fi
    ;;
esac
"#,
        );
        let generator = r#"printf '%s\n' "$@" > "$HERDR_TEST_GENERATOR_ARGS"
cat > "$HERDR_TEST_GENERATOR_STDIN"
printf '%s' "$HERDR_TEST_GENERATOR_OUTPUT"
exit "$HERDR_TEST_GENERATOR_STATUS"
"#;
        executable_script(&claude, generator);
        executable_script(&codex, generator);
        executable_script(
            &gh,
            "printf '%s\\n' \"$@\" > \"$HERDR_TEST_GH_ARGS\"\nprintf '%s\\n' \"$HERDR_TEST_GH_OUTPUT\"\nexit \"$HERDR_TEST_GH_STATUS\"\n",
        );
        (
            root,
            GitActionPrograms {
                git,
                gh,
                claude,
                codex,
            },
        )
    }

    #[cfg(unix)]
    fn run_fixture_command(
        command: &str,
        root: &std::path::Path,
        cached_diff: &str,
        unstaged_diff: &str,
        generator_output: &str,
        generator_status: i32,
    ) -> std::process::Output {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("HERDR_TEST_GIT_LOG", root.join("git.log"))
            .env("HERDR_TEST_CACHED_DIFF", cached_diff)
            .env("HERDR_TEST_UNSTAGED_DIFF", unstaged_diff)
            .env("HERDR_TEST_COMMIT_MESSAGE", root.join("commit-message"))
            .env("HERDR_TEST_GENERATOR_ARGS", root.join("generator-args"))
            .env("HERDR_TEST_GENERATOR_STDIN", root.join("generator-stdin"))
            .env("HERDR_TEST_GENERATOR_OUTPUT", generator_output)
            .env("HERDR_TEST_GENERATOR_STATUS", generator_status.to_string())
            .env("HERDR_TEST_GH_ARGS", root.join("gh-args"))
            .env("HERDR_TEST_GH_OUTPUT", "")
            .env("HERDR_TEST_GH_STATUS", "0")
            .output()
            .expect("run wrapped action")
    }

    #[cfg(unix)]
    #[test]
    fn generated_commit_uses_cached_diff_without_staging_and_commits_the_message() {
        let (root, programs) = generator_fixture("generated-commit");
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n+new line\n";
        let command =
            wrapped_command_with_programs(GitAction::Commit, "claude-opus-5", false, &programs);
        let output = run_fixture_command(
            &command,
            &root,
            diff,
            "unused",
            "fix: keep generated commits bounded\n\nexplain the change\n",
            0,
        );
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
        let git_log = std::fs::read_to_string(root.join("git.log")).expect("git log");

        assert!(stdout.contains("Generated commit message:"), "{stdout:?}");
        assert!(stdout.contains("__t3_exit=0"), "{stdout:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("generator-stdin")).expect("generator stdin"),
            diff
        );
        let args = std::fs::read_to_string(root.join("generator-args")).expect("generator args");
        assert!(args.starts_with("-p\n--model\nclaude-opus-5\n"), "{args:?}");
        assert!(args.contains(COMMIT_MESSAGE_INSTRUCTION));
        assert!(git_log.contains("diff --cached"), "{git_log:?}");
        assert!(!git_log.lines().any(|line| line == "add -A"), "{git_log:?}");
        assert!(git_log.lines().any(|line| line.starts_with("commit -F ")));
        assert_eq!(
            std::fs::read_to_string(root.join("commit-message")).expect("commit message"),
            "fix: keep generated commits bounded\n\nexplain the change\n"
        );
        std::fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn commit_stages_only_when_enabled_and_falls_back_to_editor() {
        for (label, output, status) in [
            ("generator-failure", "ignored", 7),
            ("generator-empty", "  \n", 0),
        ] {
            let (root, programs) = generator_fixture(label);
            let command =
                wrapped_command_with_programs(GitAction::Commit, "gpt-5.6-codex", true, &programs);
            let result = run_fixture_command(
                &command,
                &root,
                "",
                "diff --git a/a b/a\n+unstaged\n",
                output,
                status,
            );
            let stdout = String::from_utf8(result.stdout).expect("UTF-8 output");
            let git_log = std::fs::read_to_string(root.join("git.log")).expect("git log");
            let generator_stdin =
                std::fs::read_to_string(root.join("generator-stdin")).expect("generator stdin");
            let args =
                std::fs::read_to_string(root.join("generator-args")).expect("generator args");

            assert!(stdout.contains("opening editor"), "{stdout:?}");
            assert!(git_log.lines().any(|line| line == "add -A"), "{git_log:?}");
            assert!(git_log.contains("diff --cached\ndiff"), "{git_log:?}");
            assert!(git_log.lines().any(|line| line == "commit"), "{git_log:?}");
            assert!(generator_stdin.contains("+unstaged"));
            assert!(args.starts_with("exec\n--sandbox\nread-only\n"), "{args:?}");
            std::fs::remove_dir_all(root).expect("remove fixture");
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_pr_wrapper_prints_an_explicit_url_summary() {
        let (root, programs) = generator_fixture("create-pr-summary");
        let command = wrapped_command_with_programs(GitAction::CreatePr, "", false, &programs);
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("HERDR_TEST_GH_ARGS", root.join("gh-args"))
            .env(
                "HERDR_TEST_GH_OUTPUT",
                "Creating pull request\nhttps://github.com/acme/widgets/pull/42",
            )
            .env("HERDR_TEST_GH_STATUS", "0")
            .output()
            .expect("run Create PR wrapper");
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");

        assert!(stdout.contains("Pull request created: https://github.com/acme/widgets/pull/42"));
        assert!(stdout.contains("__t3_exit=0"));
        assert_eq!(
            std::fs::read_to_string(root.join("gh-args")).expect("gh args"),
            "pr\ncreate\n--fill\n"
        );
        std::fs::remove_dir_all(root).expect("remove fixture");
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

    #[cfg(unix)]
    #[test]
    fn clone_command_runs_the_injected_git_program() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("herdr-clone-command-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let git = root.join("git fixture");
        std::fs::write(&git, "#!/bin/sh\nprintf 'arg=%s\\n' \"$@\"\n").expect("fake git");
        let mut permissions = std::fs::metadata(&git).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("executable");

        let command = wrapped_clone_command(
            &git,
            "https://github.com/acme/project.git",
            &root.join("project with spaces"),
        );
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("run wrapped clone");
        let stdout = String::from_utf8(output.stdout).expect("utf-8 output");

        assert!(output.status.success());
        assert!(stdout.contains("arg=clone\narg=--\n"));
        assert!(stdout.contains("arg=https://github.com/acme/project.git\n"));
        assert!(stdout.contains("arg="));
        assert!(stdout.contains("project with spaces"));
        assert!(stdout.contains("__t3_exit=0"));
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
    fn merge_commands_use_the_injected_gh_and_selected_method() {
        for method in crate::config::MergeMethodConfig::ALL {
            let request = crate::app::state::PrCommandRequest {
                repo: "owner/repo".into(),
                number: 42,
                action: crate::app::state::PrCommandAction::Merge(method),
            };
            assert_eq!(
                pr_command_argv(std::path::Path::new("/test/gh"), &request),
                ["/test/gh", "pr", "merge", "42", method.flag()]
            );
        }
    }

    #[test]
    fn open_on_github_uses_the_injected_gh() {
        let request = crate::app::state::PrCommandRequest {
            repo: "owner/repo".into(),
            number: 42,
            action: crate::app::state::PrCommandAction::OpenOnGithub,
        };
        assert_eq!(
            pr_command_argv(std::path::Path::new("/test/gh"), &request),
            ["/test/gh", "pr", "view", "42", "--web"]
        );
    }

    #[test]
    fn inferred_pr_url_from_screen_does_not_replace_an_explicit_assignment() {
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

        let url = pr_url_from_screen(fixture).expect("valid PR URL");
        assert!(apply_pr_url(&mut terminal, &url).expect("valid PR URL"));
        assert_eq!(
            terminal.effective_work_context().pr_urls,
            vec!["https://github.com/acme/repo/pull/12"]
        );
        assert_eq!(
            terminal
                .work_context
                .snapshot_tiers()
                .git_observation
                .pr_urls,
            vec!["https://github.com/acme/repo/pull/42"]
        );
    }

    #[test]
    fn created_pr_url_updates_context_notifies_and_requests_copy() {
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("pr")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let url = "https://github.com/acme/widgets/pull/42";

        assert_eq!(
            app.apply_created_pr_url(pane_id, &format!("Pull request created: {url}")),
            Some(url.to_string())
        );
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("pane terminal");
        assert_eq!(
            app.state.terminals[terminal_id]
                .effective_work_context()
                .pr_urls,
            [url]
        );
        let toast = app.state.toast.as_ref().expect("PR notification");
        assert_eq!(toast.title, "pull request created");
        assert_eq!(toast.context, url);
        assert!(app.toast_deadline.is_some());
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                assert_eq!(content, url.as_bytes())
            }
            event => panic!("unexpected event: {event:?}"),
        }
    }
}
