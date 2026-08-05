use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::App;
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::work_context::{extract_pr_urls, extract_preview_urls, extract_ticket_ids};

pub(crate) const WORK_CONTEXT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const WORK_CONTEXT_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitWorkContextRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GitWorkContextCacheKey {
    pub(crate) repo_root: PathBuf,
    pub(crate) branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitWorkContextInput {
    pub(crate) cwd: PathBuf,
    pub(crate) repo_root: Option<PathBuf>,
    pub(crate) branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitWorkContextObservation {
    pub(crate) pane_id: PaneId,
    pub(crate) input: GitWorkContextInput,
    pub(crate) context: crate::work_context::PaneWorkContext,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GitWorkContextTarget {
    pane_id: PaneId,
    cwd: PathBuf,
}

#[derive(Debug)]
struct GitWorkContextRefreshOutput {
    observations: Vec<GitWorkContextObservation>,
    cache_updates: Vec<(GitWorkContextCacheKey, crate::work_context::PaneWorkContext)>,
}

impl App {
    fn git_work_context_program(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(program) = self.git_program_override.as_ref() {
            return program.clone();
        }

        PathBuf::from("git")
    }

    fn gh_program() -> PathBuf {
        PathBuf::from("gh")
    }

    pub(crate) fn git_work_context_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.git_work_context_refresh_in_flight.as_ref() {
            return Some(refresh.deadline);
        }
        (!self.state.workspaces.is_empty()).then_some(self.next_git_work_context_refresh)
    }

    pub(crate) fn start_git_work_context_refresh_if_due(&mut self, now: Instant) {
        if self
            .git_work_context_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.git_work_context_refresh_in_flight = None;
        }

        if self.git_work_context_refresh_in_flight.is_some()
            || now < self.next_git_work_context_refresh
        {
            return;
        }

        self.next_git_work_context_refresh = now + WORK_CONTEXT_REFRESH_INTERVAL;
        let targets = self.git_work_context_targets();
        if targets.is_empty() {
            return;
        }

        self.last_git_work_context_refresh_generation = self
            .last_git_work_context_refresh_generation
            .wrapping_add(1);
        let generation = self.last_git_work_context_refresh_generation;
        let deadline = now + WORK_CONTEXT_REFRESH_TIMEOUT;
        self.git_work_context_refresh_in_flight = Some(GitWorkContextRefreshInFlight {
            generation,
            deadline,
        });

        let event_tx = self.event_tx.clone();
        let cache = self.git_work_context_cache.clone();
        let git_program = self.git_work_context_program();
        let gh_program = Self::gh_program();
        let _ = std::thread::Builder::new()
            .name("herdr-work-context-git".into())
            .spawn(move || {
                let output =
                    refresh_git_work_contexts(targets, cache, deadline, &git_program, &gh_program);
                let _ = event_tx.blocking_send(AppEvent::GitWorkContextRefreshed {
                    generation,
                    observations: output.observations,
                    cache_updates: output.cache_updates,
                });
            });
    }

    pub(crate) fn request_git_work_context_refresh(&mut self, now: Instant) {
        if self.git_work_context_refresh_in_flight.is_none() {
            self.next_git_work_context_refresh = now;
        }
    }

    pub(crate) fn handle_git_work_context_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<GitWorkContextObservation>,
        cache_updates: Vec<(GitWorkContextCacheKey, crate::work_context::PaneWorkContext)>,
    ) -> bool {
        let Some(refresh) = self.git_work_context_refresh_in_flight.as_ref() else {
            return false;
        };
        if refresh.generation != generation {
            return false;
        }

        let now = Instant::now();
        if now >= refresh.deadline {
            self.git_work_context_refresh_in_flight = None;
            self.next_git_work_context_refresh = now + WORK_CONTEXT_REFRESH_INTERVAL;
            return false;
        }
        self.git_work_context_refresh_in_flight = None;
        for (key, context) in cache_updates {
            self.git_work_context_cache.insert(key, context);
        }

        let mut changed = false;
        for observation in observations {
            let Some((ws_idx, terminal_id)) =
                self.state
                    .workspaces
                    .iter()
                    .enumerate()
                    .find_map(|(ws_idx, workspace)| {
                        workspace
                            .tabs
                            .iter()
                            .find_map(|tab| tab.terminal_id(observation.pane_id))
                            .cloned()
                            .map(|terminal_id| (ws_idx, terminal_id))
                    })
            else {
                continue;
            };

            let current_cwd = self
                .terminal_runtimes
                .get(&terminal_id)
                .and_then(|runtime| runtime.cwd())
                .or_else(|| {
                    self.state
                        .terminals
                        .get(&terminal_id)
                        .map(|terminal| terminal.cwd.clone())
                });
            if current_cwd.as_ref() != Some(&observation.input.cwd) {
                continue;
            }

            if self.git_work_context_inputs.get(&observation.pane_id) == Some(&observation.input) {
                continue;
            }

            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            let Ok(observation_changed) = terminal.replace_git_work_context(observation.context)
            else {
                continue;
            };
            self.git_work_context_inputs
                .insert(observation.pane_id, observation.input);
            if !observation_changed {
                continue;
            }

            changed = true;
            self.schedule_session_save();
            self.emit_pane_updated(ws_idx, observation.pane_id);
        }

        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed
    }

    fn git_work_context_targets(&self) -> Vec<GitWorkContextTarget> {
        self.state
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace.tabs.iter().flat_map(|tab| {
                    tab.layout.pane_ids().into_iter().filter_map(|pane_id| {
                        tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                            .map(|cwd| GitWorkContextTarget { pane_id, cwd })
                    })
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn test_begin_git_work_context_refresh(&mut self, generation: u64) {
        let deadline = Instant::now() + WORK_CONTEXT_REFRESH_TIMEOUT;
        self.last_git_work_context_refresh_generation = generation;
        self.git_work_context_refresh_in_flight = Some(GitWorkContextRefreshInFlight {
            generation,
            deadline,
        });
    }
}

fn refresh_git_work_contexts(
    targets: Vec<GitWorkContextTarget>,
    cache: HashMap<GitWorkContextCacheKey, crate::work_context::PaneWorkContext>,
    deadline: Instant,
    git_program: &Path,
    gh_program: &Path,
) -> GitWorkContextRefreshOutput {
    let mut cache = cache;
    let mut cache_updates = Vec::new();
    let mut observations = Vec::new();
    let mut discovered = HashMap::<PathBuf, Option<GitWorkContextInput>>::new();

    for target in targets {
        let input = if let Some(input) = discovered.get(&target.cwd) {
            input.clone()
        } else {
            let input = discover_git_input(&target.cwd, deadline, git_program);
            discovered.insert(target.cwd.clone(), input.clone());
            input
        };
        let Some(input) = input else {
            continue;
        };

        let context = match (&input.repo_root, &input.branch) {
            (Some(repo_root), Some(branch)) => {
                let key = GitWorkContextCacheKey {
                    repo_root: repo_root.clone(),
                    branch: branch.clone(),
                };
                if let Some(context) = cache.get(&key) {
                    context.clone()
                } else {
                    let context =
                        git_work_context_for_branch(branch, repo_root, deadline, gh_program);
                    cache_updates.push((key.clone(), context.clone()));
                    cache.insert(key, context.clone());
                    context
                }
            }
            _ => crate::work_context::PaneWorkContext::default(),
        };

        observations.push(GitWorkContextObservation {
            pane_id: target.pane_id,
            input,
            context,
        });
    }

    GitWorkContextRefreshOutput {
        observations,
        cache_updates,
    }
}

fn discover_git_input(
    cwd: &Path,
    deadline: Instant,
    git_program: &Path,
) -> Option<GitWorkContextInput> {
    let mut root_command = crate::noninteractive_process::command(git_program);
    root_command
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"]);
    let root_output = crate::noninteractive_process::output_with_deadline(root_command, deadline);
    let root_output = match root_output {
        Ok(output) if output.status.success() => output,
        Ok(_) => {
            return Some(GitWorkContextInput {
                cwd: cwd.to_path_buf(),
                repo_root: None,
                branch: None,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return None,
        Err(_) => {
            return Some(GitWorkContextInput {
                cwd: cwd.to_path_buf(),
                repo_root: None,
                branch: None,
            });
        }
    };
    let repo_root = String::from_utf8(root_output.stdout)
        .ok()
        .map(|root| PathBuf::from(root.trim()))
        .filter(|root| !root.as_os_str().is_empty())
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root));
    let Some(repo_root) = repo_root else {
        return Some(GitWorkContextInput {
            cwd: cwd.to_path_buf(),
            repo_root: None,
            branch: None,
        });
    };

    let mut branch_command = crate::noninteractive_process::command(git_program);
    branch_command
        .arg("-C")
        .arg(cwd)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let branch_output =
        crate::noninteractive_process::output_with_deadline(branch_command, deadline);
    let branch = match branch_output {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|branch| branch.trim().to_string())
            .filter(|branch| !branch.is_empty()),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return None,
        Err(_) => None,
    };

    Some(GitWorkContextInput {
        cwd: cwd.to_path_buf(),
        repo_root: Some(repo_root),
        branch,
    })
}

fn git_work_context_for_branch(
    branch: &str,
    repo_root: &Path,
    deadline: Instant,
    gh_program: &Path,
) -> crate::work_context::PaneWorkContext {
    let mut context = crate::work_context::PaneWorkContext {
        ticket_ids: extract_ticket_ids(branch),
        branch: Some(branch.to_string()),
        ..crate::work_context::PaneWorkContext::default()
    };

    if Instant::now() >= deadline {
        return context;
    }

    let mut command = crate::noninteractive_process::command(gh_program);
    command
        .current_dir(repo_root)
        .args(["pr", "view", "--json", "url,statusCheckRollup"]);
    let Ok(output) = crate::noninteractive_process::output_with_deadline(command, deadline) else {
        return context;
    };
    if !output.status.success() {
        return context;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        return context;
    };

    if let Some(url) = value.get("url").and_then(Value::as_str) {
        context.pr_urls = extract_pr_urls(url);
    }
    let mut preview_urls = Vec::new();
    collect_preview_urls(&value, &mut preview_urls);
    context.preview_urls =
        crate::work_context::normalize_preview_urls(preview_urls).unwrap_or_default();
    context
}

fn collect_preview_urls(value: &Value, urls: &mut Vec<String>) {
    match value {
        Value::String(text) => urls.extend(extract_preview_urls(text)),
        Value::Array(values) => {
            for value in values {
                collect_preview_urls(value, urls);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_preview_urls(value, urls);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fake executable");
        let mut permissions = std::fs::metadata(path)
            .expect("read fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake executable");
    }

    #[cfg(unix)]
    fn fixture_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-work-context-{name}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create fixture directory");
        path
    }

    #[cfg(unix)]
    fn fake_git(path: &Path, repo: &Path, branch: &str) {
        write_executable(
            path,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n  *'rev-parse --show-toplevel'*) printf '%s\\n' '{}' ;;\n  *'symbolic-ref --quiet --short HEAD'*) printf '%s\\n' '{}' ;;\n  *) exit 1 ;;\nesac\n",
                repo.display(), branch
            ),
        );
    }

    #[cfg(unix)]
    fn refresh_one(
        git: &Path,
        gh: &Path,
        cwd: &Path,
        deadline: Instant,
    ) -> GitWorkContextRefreshOutput {
        refresh_git_work_contexts(
            vec![GitWorkContextTarget {
                pane_id: PaneId::from_raw(1),
                cwd: cwd.to_path_buf(),
            }],
            HashMap::new(),
            deadline,
            git,
            gh,
        )
    }

    #[cfg(unix)]
    #[test]
    fn branch_ticket_ids_reuse_shared_extractor() {
        let dir = fixture_dir("ticket");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-123-thing");
        write_executable(&gh, "#!/bin/sh\nexit 1\n");

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(1));
        assert_eq!(output.observations[0].context.ticket_ids, vec!["MAT-123"]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn branch_without_ticket_is_valid_and_empty() {
        let dir = fixture_dir("no-ticket");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feature/no-ticket");
        write_executable(&gh, "#!/bin/sh\nexit 1\n");

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(1));
        assert!(output.observations[0].context.ticket_ids.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_failure_and_timeout_keep_branch_tickets_without_errors() {
        let dir = fixture_dir("gh-failure");
        let git = dir.join("git");
        let gh_failure = dir.join("gh-failure");
        let gh_timeout = dir.join("gh-timeout");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/SCA-44-test");
        write_executable(&gh_failure, "#!/bin/sh\nexit 7\n");
        write_executable(&gh_timeout, "#!/bin/sh\nsleep 2\n");

        let gh_missing = dir.join("missing-gh");
        for gh in [&gh_failure, &gh_timeout, &gh_missing] {
            // The deadline covers two git subprocesses before gh is invoked; keep enough
            // budget for process startup while still exercising gh's bounded timeout path.
            let output = refresh_one(&git, gh, &repo, Instant::now() + Duration::from_millis(500));
            assert_eq!(output.observations[0].context.ticket_ids, vec!["SCA-44"]);
            assert!(output.observations[0].context.pr_urls.is_empty());
            assert!(output.observations[0].context.preview_urls.is_empty());
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_preview_urls_are_capped_and_pr_url_is_extracted() {
        let dir = fixture_dir("preview-cap");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-1-preview");
        let checks = (0..crate::work_context::MAX_PREVIEW_URLS + 3)
            .map(|index| format!("{{\"targetUrl\":\"https://preview-{index}.vercel.app\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"url\":\"https://github.com/o/r/pull/7\",\"statusCheckRollup\":[{}]}}'\n",
                checks
            ),
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(1));
        let context = &output.observations[0].context;
        assert_eq!(context.pr_urls, vec!["https://github.com/o/r/pull/7"]);
        assert_eq!(
            context.preview_urls.len(),
            crate::work_context::MAX_PREVIEW_URLS
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unchanged_git_observation_does_not_schedule_render_or_session_save() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("git-context");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.no_session = false;
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let cwd = app.state.terminals[&terminal_id].cwd.clone();
        app.test_begin_git_work_context_refresh(1);

        let changed =
            app.handle_internal_event_with_render_impact(AppEvent::GitWorkContextRefreshed {
                generation: 1,
                observations: vec![GitWorkContextObservation {
                    pane_id,
                    input: GitWorkContextInput {
                        cwd,
                        repo_root: None,
                        branch: None,
                    },
                    context: crate::work_context::PaneWorkContext::default(),
                }],
                cache_updates: Vec::new(),
            });

        assert!(!changed);
        assert!(!app.render_dirty.is_pending());
        assert!(app.session_save_deadline.is_none());
    }
}
