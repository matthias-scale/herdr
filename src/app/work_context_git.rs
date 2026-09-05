use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::App;
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::work_context::{extract_pr_urls, extract_preview_urls, extract_ticket_ids};

pub(crate) const WORK_CONTEXT_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
// Each pane gets its own probe budget so one slow repository cannot consume the
// whole batch and leave every later pane without links. The batch ceiling still
// bounds a refresh that would otherwise walk many slow repositories back to back,
// and it is also the in-flight lifetime: a shorter scheduler deadline would let a
// successor supersede a worker that is still producing valid observations.
pub(crate) const WORK_CONTEXT_TARGET_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const WORK_CONTEXT_BATCH_TIMEOUT: Duration = Duration::from_secs(45);
// Cache GitHub metadata for repeated refresh requests within a short window.
pub(crate) const WORK_CONTEXT_CACHE_TTL: Duration = Duration::from_secs(30);

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
pub(crate) struct GitWorkContextCacheEntry {
    pub(crate) context: crate::work_context::PaneWorkContext,
    pub(crate) cached_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitWorkContextInput {
    pub(crate) cwd: PathBuf,
    pub(crate) repo_root: Option<PathBuf>,
    pub(crate) branch: Option<String>,
    /// `owner/repo` of the checkout's `origin` remote. This is an observation,
    /// not a declaration: it describes where the pane's cwd points, which is
    /// frequently not the repository the pane is working on. It therefore
    /// enters the lowest work-context tier and any declaration outranks it.
    pub(crate) repo: Option<String>,
    /// True when `origin` exists but is not a plain github.com URL — an SSH
    /// host alias such as `git@github.com-scale:owner/repo.git`. Only then is
    /// it worth spending a `gh` call to resolve the slug; a checkout with no
    /// origin at all has nothing to resolve.
    pub(crate) origin_unparsed: bool,
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
    cache_updates: Vec<(GitWorkContextCacheKey, GitWorkContextCacheEntry)>,
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
            if self.git_work_context_refresh_due_after_in_flight {
                self.next_git_work_context_refresh = now;
                self.git_work_context_refresh_due_after_in_flight = false;
            }
        }

        if self.git_work_context_refresh_in_flight.is_some()
            || now < self.next_git_work_context_refresh
        {
            return;
        }

        self.next_git_work_context_refresh = now + WORK_CONTEXT_REFRESH_INTERVAL;
        self.prune_git_work_context_state();
        let mut targets = self.git_work_context_targets();
        if targets.is_empty() {
            return;
        }
        // The batch budget can run out before the last target is probed, so start
        // from a different pane each cycle. Without this the same tail panes would
        // be the ones dropped every time.
        let rotation = self.git_work_context_rotation % targets.len();
        targets.rotate_left(rotation);
        self.git_work_context_rotation = self.git_work_context_rotation.wrapping_add(1);

        self.last_git_work_context_refresh_generation = self
            .last_git_work_context_refresh_generation
            .wrapping_add(1);
        let generation = self.last_git_work_context_refresh_generation;
        let batch_deadline = now + WORK_CONTEXT_BATCH_TIMEOUT;
        self.git_work_context_refresh_in_flight = Some(GitWorkContextRefreshInFlight {
            generation,
            deadline: batch_deadline,
        });

        let event_tx = self.event_tx.clone();
        let cache = self.git_work_context_cache.clone();
        let git_program = self.git_work_context_program();
        let gh_program = Self::gh_program();
        let cache_now = Instant::now();
        let _ = std::thread::Builder::new()
            .name("herdr-work-context-git".into())
            .spawn(move || {
                let output = refresh_git_work_contexts(
                    targets,
                    cache,
                    cache_now,
                    batch_deadline,
                    batch_deadline,
                    WORK_CONTEXT_TARGET_TIMEOUT,
                    &git_program,
                    &gh_program,
                );
                let _ = event_tx.blocking_send(AppEvent::GitWorkContextRefreshed {
                    generation,
                    observations: output.observations,
                    cache_updates: output.cache_updates,
                });
            });
    }

    pub(crate) fn request_git_work_context_refresh(&mut self, now: Instant) {
        if self.git_work_context_refresh_in_flight.is_some() {
            self.git_work_context_refresh_due_after_in_flight = true;
        } else {
            self.next_git_work_context_refresh = now;
        }
    }

    pub(crate) fn handle_git_work_context_refreshed(
        &mut self,
        generation: u64,
        observations: Vec<GitWorkContextObservation>,
        cache_updates: Vec<(GitWorkContextCacheKey, GitWorkContextCacheEntry)>,
    ) -> bool {
        self.prune_git_work_context_state();
        if generation <= self.last_applied_git_work_context_refresh_generation
            || generation != self.last_git_work_context_refresh_generation
        {
            return false;
        }

        let now = Instant::now();
        if self
            .git_work_context_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| refresh.generation == generation)
        {
            let overran_deadline = self
                .git_work_context_refresh_in_flight
                .as_ref()
                .is_some_and(|refresh| now >= refresh.deadline);
            self.git_work_context_refresh_in_flight = None;
            if self.git_work_context_refresh_due_after_in_flight {
                self.next_git_work_context_refresh = now;
                self.git_work_context_refresh_due_after_in_flight = false;
            } else if overran_deadline {
                self.next_git_work_context_refresh = now + WORK_CONTEXT_REFRESH_INTERVAL;
            }
        }
        self.last_applied_git_work_context_refresh_generation = generation;
        let refreshed_cache_keys: HashSet<_> =
            cache_updates.iter().map(|(key, _)| key.clone()).collect();
        for (key, entry) in cache_updates {
            self.git_work_context_cache.insert(key, entry);
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

            // Record the repository root for this cwd before any early exit:
            // the chooser reads it for the focused pane, so it must be present
            // even when the observation itself is unchanged.
            let previous_root = self.state.git_root_for_cwd.insert(
                observation.input.cwd.clone(),
                observation.input.repo_root.clone(),
            );
            // Only a repository root that moved is a visible change. The first
            // observation for a cwd rides on the observation's own signal, so
            // recording it never forces a redraw on its own.
            let root_changed =
                previous_root.is_some_and(|previous| previous != observation.input.repo_root);

            let cache_refreshed = cache_key(&observation.input)
                .is_some_and(|key| refreshed_cache_keys.contains(&key));
            if !cache_refreshed
                && self.git_work_context_inputs.get(&observation.pane_id)
                    == Some(&observation.input)
            {
                changed |= root_changed;
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
                changed |= root_changed;
                continue;
            }

            changed = true;
            self.schedule_session_save();
            self.emit_pane_updated(ws_idx, observation.pane_id);
            // The observation may have resolved the repository for the first
            // time. It is the weakest tier, so it only routes a pane that has
            // declared nothing better.
            self.route_pane_to_bound_workspace(ws_idx, observation.pane_id);
        }

        self.prune_git_work_context_state();

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

    fn prune_git_work_context_state(&mut self) {
        let active_panes: HashSet<_> = self
            .git_work_context_targets()
            .into_iter()
            .map(|target| target.pane_id)
            .collect();
        self.git_work_context_inputs
            .retain(|pane_id, _| active_panes.contains(pane_id));

        let active_cwds: HashSet<_> = self
            .git_work_context_targets()
            .into_iter()
            .map(|target| target.cwd)
            .collect();
        self.state
            .git_root_for_cwd
            .retain(|cwd, _| active_cwds.contains(cwd));

        let active_cache_keys: HashSet<_> = self
            .git_work_context_inputs
            .values()
            .filter_map(cache_key)
            .collect();
        self.git_work_context_cache
            .retain(|key, _| active_cache_keys.contains(key));
    }

    #[cfg(test)]
    pub(crate) fn test_begin_git_work_context_refresh(&mut self, generation: u64) {
        let deadline = Instant::now() + WORK_CONTEXT_BATCH_TIMEOUT;
        self.last_git_work_context_refresh_generation = generation;
        self.git_work_context_refresh_in_flight = Some(GitWorkContextRefreshInFlight {
            generation,
            deadline,
        });
    }
}

fn refresh_git_work_contexts(
    targets: Vec<GitWorkContextTarget>,
    cache: HashMap<GitWorkContextCacheKey, GitWorkContextCacheEntry>,
    now: Instant,
    git_deadline: Instant,
    gh_deadline: Instant,
    target_timeout: Duration,
    git_program: &Path,
    gh_program: &Path,
) -> GitWorkContextRefreshOutput {
    let mut cache = cache;
    let mut cache_updates = Vec::new();
    let mut observations = Vec::new();
    let mut discovered = HashMap::<PathBuf, Option<GitWorkContextInput>>::new();

    for target in targets {
        // Clamp to the batch ceiling so the per-target budget can extend a probe
        // but never outlive the refresh as a whole.
        let target_git_deadline = (Instant::now() + target_timeout).min(git_deadline);
        let input = if let Some(input) = discovered.get(&target.cwd) {
            input.clone()
        } else {
            let input = discover_git_input(&target.cwd, target_git_deadline, git_program);
            discovered.insert(target.cwd.clone(), input.clone());
            input
        };
        let Some(input) = input else {
            tracing::debug!(cwd = ?target.cwd, "git work context: discovery produced nothing");
            continue;
        };
        // Measured only once git discovery is done. Sharing one instant with the
        // git budget let a slow checkout spend gh's entire window, so gh was
        // skipped outright and the pane reported a branch with no pull request.
        let target_gh_deadline = (Instant::now() + target_timeout).min(gh_deadline);

        let mut input = input;
        if input.repo.is_none() && input.origin_unparsed {
            if let Some(repo_root) = input.repo_root.as_deref() {
                input.repo = gh_repo_slug(repo_root, target_gh_deadline, gh_program);
                // Remember it so sibling panes in the same checkout do not each
                // pay for another gh call.
                discovered.insert(target.cwd.clone(), Some(input.clone()));
            }
        }

        let context = match (&input.repo_root, &input.branch) {
            (Some(repo_root), Some(branch)) => {
                let key = GitWorkContextCacheKey {
                    repo_root: repo_root.clone(),
                    branch: branch.clone(),
                };
                if let Some(entry) = cache.get(&key).filter(|entry| {
                    now.saturating_duration_since(entry.cached_at) < WORK_CONTEXT_CACHE_TTL
                }) {
                    entry.context.clone()
                } else {
                    let context = git_work_context_for_branch(
                        branch,
                        repo_root,
                        input.repo.as_deref(),
                        target_gh_deadline,
                        gh_program,
                    );
                    let entry = GitWorkContextCacheEntry {
                        context: context.clone(),
                        cached_at: now,
                    };
                    cache_updates.push((key.clone(), entry.clone()));
                    cache.insert(key, entry);
                    context
                }
            }
            // A pane with no branch (detached HEAD during a bisect, rebase or
            // `gh pr checkout`) used to overwrite its tier with an empty
            // context, silently dropping a pull request it had already found.
            // Reporting nothing new leaves the previous observation standing.
            _ => {
                tracing::debug!(
                    cwd = ?target.cwd,
                    repo_root = ?input.repo_root,
                    "git work context: no branch, keeping the previous observation"
                );
                let context = crate::work_context::PaneWorkContext {
                    repo: input.repo.clone(),
                    ..crate::work_context::PaneWorkContext::default()
                };
                observations.push(GitWorkContextObservation {
                    pane_id: target.pane_id,
                    input,
                    context,
                });
                continue;
            }
        };
        // Applied outside the branch cache so a detached HEAD, which produces
        // no branch and therefore no cache key, still reports its repository.
        let mut context = context;
        context.repo = input.repo.clone();

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

fn cache_key(input: &GitWorkContextInput) -> Option<GitWorkContextCacheKey> {
    Some(GitWorkContextCacheKey {
        repo_root: input.repo_root.clone()?,
        branch: input.branch.clone()?,
    })
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
                repo: None,
                origin_unparsed: false,
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => return None,
        Err(_) => {
            return Some(GitWorkContextInput {
                cwd: cwd.to_path_buf(),
                repo_root: None,
                branch: None,
                repo: None,
                origin_unparsed: false,
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
            repo: None,
            origin_unparsed: false,
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

    let (repo, origin_unparsed) = discover_origin_repo(cwd, deadline, git_program);

    Some(GitWorkContextInput {
        cwd: cwd.to_path_buf(),
        repo_root: Some(repo_root),
        branch,
        repo,
        origin_unparsed,
    })
}

/// Read `origin` and canonicalize it to `owner/repo`.
///
/// A checkout without an `origin`, with a non-GitHub or unparseable remote,
/// simply yields no observation: an absent repository is always safer than a
/// wrong one, because a wrong one would route the pane into another
/// repository's space.
/// Returns the `owner/repo` slug and whether an unparseable origin was seen.
fn discover_origin_repo(
    cwd: &Path,
    deadline: Instant,
    git_program: &Path,
) -> (Option<String>, bool) {
    let mut command = crate::noninteractive_process::command(git_program);
    command
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "remote.origin.url"]);
    let Ok(output) = crate::noninteractive_process::output_with_deadline(command, deadline) else {
        return (None, false);
    };
    if !output.status.success() {
        return (None, false);
    }
    let Ok(remote) = String::from_utf8(output.stdout) else {
        return (None, false);
    };
    let remote = remote.trim();
    if remote.is_empty() {
        return (None, false);
    }
    if let Ok(slug) = crate::work_context::normalize_repo_slug(remote) {
        return (Some(slug), false);
    }
    tracing::debug!(
        remote,
        "git work context: origin remote is not a plain github.com URL"
    );
    (None, true)
}

/// Resolve the repository slug for a checkout whose `origin` uses an SSH host
/// alias (`git@github.com-scale:owner/repo.git`), which is not a github.com URL
/// and so cannot be parsed directly. `gh` already understands the alias.
fn gh_repo_slug(repo_root: &Path, deadline: Instant, gh_program: &Path) -> Option<String> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut command = crate::noninteractive_process::command(gh_program);
    command.current_dir(repo_root).args([
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).ok()?;
    if !output.status.success() {
        return None;
    }
    let slug = String::from_utf8(output.stdout).ok()?;
    crate::work_context::normalize_repo_slug(slug.trim()).ok()
}

fn git_work_context_for_branch(
    branch: &str,
    repo_root: &Path,
    repo: Option<&str>,
    deadline: Instant,
    gh_program: &Path,
) -> crate::work_context::PaneWorkContext {
    let mut context = crate::work_context::PaneWorkContext {
        ticket_ids: extract_ticket_ids(branch),
        branch: Some(branch.to_string()),
        ..crate::work_context::PaneWorkContext::default()
    };

    if Instant::now() >= deadline {
        tracing::debug!(branch, "git work context: gh budget spent before the query");
        return context;
    }

    let mut command = crate::noninteractive_process::command(gh_program);
    command
        .current_dir(repo_root)
        .args(gh_pr_view_args(branch, repo));
    let Ok(output) = crate::noninteractive_process::output_with_deadline(command, deadline) else {
        tracing::debug!(branch, "git work context: gh did not run to completion");
        return context;
    };
    if !output.status.success() {
        tracing::debug!(
            branch,
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "git work context: gh exited non-zero"
        );
        return context;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        tracing::debug!(branch, "git work context: gh output was not JSON");
        return context;
    };
    let Some(prs) = value.as_array() else {
        return context;
    };
    // `--head` matches by branch name alone, so every fork with a branch of this
    // name comes back. A PR whose head lives in this repository is unambiguously
    // ours; otherwise only a single candidate is safe to attribute to this pane.
    let same_repo = prs
        .iter()
        .find(|pr| pr.get("isCrossRepository").and_then(Value::as_bool) == Some(false));
    let Some(pr) = same_repo.or(match prs.as_slice() {
        [only] => Some(only),
        _ => None,
    }) else {
        return context;
    };
    if let Some(url) = pr.get("url").and_then(Value::as_str) {
        context.pr_urls = extract_pr_urls(url);
    }
    let mut preview_urls = Vec::new();
    collect_preview_urls(pr, &mut preview_urls);
    // Already host-validated by extraction. Running them back through
    // `normalize_preview_urls` would reject every URL carrying a path and throw
    // the whole list away, which is what it used to do.
    preview_urls.truncate(crate::work_context::MAX_PREVIEW_URLS);
    context.preview_urls = preview_urls;
    context
}

fn gh_pr_view_args(branch: &str, repo: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["pr", "list", "--head", branch]
        .into_iter()
        .map(str::to_string)
        .collect();
    if let Some(repo) = repo {
        args.push("--repo".to_string());
        args.push(repo.to_string());
    }
    // `body` and `comments` are where a preview URL actually lives: Vercel and
    // our own `post-preview-urls` workflow post the alias as a comment, while
    // `statusCheckRollup` only ever carries a vercel.com dashboard link.
    args.push("--json".to_string());
    args.push("url,statusCheckRollup,isCrossRepository,body,comments".to_string());
    args.push("--limit".to_string());
    args.push("10".to_string());
    args
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
        refresh_one_at(git, gh, cwd, HashMap::new(), Instant::now(), deadline)
    }

    #[cfg(unix)]
    fn refresh_one_at(
        git: &Path,
        gh: &Path,
        cwd: &Path,
        cache: HashMap<GitWorkContextCacheKey, GitWorkContextCacheEntry>,
        now: Instant,
        deadline: Instant,
    ) -> GitWorkContextRefreshOutput {
        refresh_git_work_contexts(
            vec![GitWorkContextTarget {
                pane_id: PaneId::from_raw(1),
                cwd: cwd.to_path_buf(),
            }],
            cache,
            now,
            deadline,
            deadline,
            Duration::from_secs(5),
            git,
            gh,
        )
    }

    #[cfg(unix)]
    fn refresh_one_with_deadlines(
        git: &Path,
        gh: &Path,
        cwd: &Path,
        cache: HashMap<GitWorkContextCacheKey, GitWorkContextCacheEntry>,
        now: Instant,
        git_deadline: Instant,
        gh_deadline: Instant,
    ) -> GitWorkContextRefreshOutput {
        refresh_git_work_contexts(
            vec![GitWorkContextTarget {
                pane_id: PaneId::from_raw(1),
                cwd: cwd.to_path_buf(),
            }],
            cache,
            now,
            git_deadline,
            gh_deadline,
            Duration::from_secs(5),
            git,
            gh,
        )
    }

    #[test]
    fn work_context_refresh_interval_is_one_minute() {
        assert_eq!(WORK_CONTEXT_REFRESH_INTERVAL, Duration::from_secs(60));
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

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(output.observations[0].context.ticket_ids, vec!["MAT-123"]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_query_uses_flag_head_selector_and_parses_pr_list_array() {
        let dir = fixture_dir("branch-qualified-gh");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-27-branch-qualified");
        write_executable(
            &gh,
            r#"#!/bin/sh
set -- "$@"
[ "$1" = pr ] || exit 2
[ "$2" = list ] || exit 2
shift 2
after_separator=0
expecting=
head=
json=
limit=
positionals=0
for arg
do
    case "$arg" in
        --head|--json|--limit|--repo)
            [ "$after_separator" -eq 0 ] || exit 2
            [ -z "$expecting" ] || exit 2
            expecting="$arg"
            ;;
        --)
            [ -z "$expecting" ] || exit 2
            after_separator=1
            ;;
        --*)
            [ "$after_separator" -eq 0 ] || exit 2
            exit 2
            ;;
        *)
            if [ -n "$expecting" ]; then
                case "$arg" in --*) exit 2 ;; esac
                case "$expecting" in
                    --head) head="$arg" ;;
                    --json) json="$arg" ;;
                    --limit) limit="$arg" ;;
                    --repo) repo_arg="$arg" ;;
                esac
                expecting=
            elif [ "$after_separator" -eq 1 ]; then
                positionals=$((positionals + 1))
                [ "$positionals" -le 1 ] || exit 2
            else
                exit 2
            fi
            ;;
    esac
done
[ -z "$expecting" ] || exit 2
[ "$positionals" -eq 0 ] || exit 2
[ "$head" = feat/MAT-27-branch-qualified ] || exit 2
[ "$json" = url,statusCheckRollup,isCrossRepository,body,comments ] || exit 2
[ "$limit" = 10 ] || exit 2
printf '%s\n' '[{"url":"https://github.com/o/r/pull/27","statusCheckRollup":[]}]'
"#,
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            output.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/27"]
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_empty_pr_list_keeps_branch_context() {
        let dir = fixture_dir("empty-pr-list");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-28-no-pr");
        write_executable(&gh, "#!/bin/sh\nprintf '%s\\n' '[]'\n");

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        let context = &output.observations[0].context;
        assert_eq!(context.ticket_ids, vec!["MAT-28"]);
        assert!(context.pr_urls.is_empty());
        assert!(context.preview_urls.is_empty());

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

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
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
        write_executable(
            &gh_timeout,
            "#!/bin/sh\nsleep 2\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/timeout\"}]'\n",
        );

        let gh_missing = dir.join("missing-gh");
        for gh in [&gh_failure, &gh_timeout, &gh_missing] {
            let output = refresh_one_with_deadlines(
                &git,
                gh,
                &repo,
                HashMap::new(),
                Instant::now(),
                Instant::now() + Duration::from_secs(5),
                Instant::now() + Duration::from_millis(500),
            );
            assert_eq!(output.observations[0].context.ticket_ids, vec!["SCA-44"]);
            assert!(output.observations[0].context.pr_urls.is_empty());
            assert!(output.observations[0].context.preview_urls.is_empty());
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn slow_repository_does_not_starve_later_panes_in_the_same_batch() {
        let dir = fixture_dir("batch-starvation");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let slow_repo = dir.join("slow");
        let fast_repo = dir.join("fast");
        std::fs::create_dir(&slow_repo).expect("create slow repo fixture");
        std::fs::create_dir(&fast_repo).expect("create fast repo fixture");

        write_executable(
            &git,
            // git is invoked as `git -C <cwd> ...`, so the target directory is $2.
            "#!/bin/sh\nroot=$2\ncase \"$*\" in\n  *'rev-parse --show-toplevel'*) printf '%s\\n' \"$root\" ;;\n  *'symbolic-ref --quiet --short HEAD'*) printf '%s\\n' \"feat/MAT-1-$(basename \"$root\")\" ;;\n  *) exit 1 ;;\nesac\n",
        );
        // The slow repository outlives both its own per-target budget and, under a
        // single shared deadline, the whole batch.
        write_executable(
            &gh,
            "#!/bin/sh\ncase \"$(pwd -P)\" in\n  *slow*) sleep 6 ;;\nesac\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/2\"}]'\n",
        );

        let output = refresh_git_work_contexts(
            vec![
                GitWorkContextTarget {
                    pane_id: PaneId::from_raw(1),
                    cwd: slow_repo.clone(),
                },
                GitWorkContextTarget {
                    pane_id: PaneId::from_raw(2),
                    cwd: fast_repo.clone(),
                },
            ],
            HashMap::new(),
            Instant::now(),
            Instant::now() + Duration::from_secs(3),
            Instant::now() + Duration::from_secs(3),
            WORK_CONTEXT_TARGET_TIMEOUT,
            &git,
            &gh,
        );

        let fast = output
            .observations
            .iter()
            .find(|observation| observation.pane_id == PaneId::from_raw(2))
            .expect("observation for the fast pane");
        assert_eq!(
            fast.context.pr_urls,
            vec!["https://github.com/o/r/pull/2".to_string()],
            "the later pane must still get its PR link after a slow repository"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn expired_cache_rechecks_gh_while_fresh_cache_is_reused() {
        let dir = fixture_dir("cache-ttl");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let marker = dir.join("pr-open");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-1-cache");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\nif [ -f '{}' ]; then printf '%s\\n' '[{{\"url\":\"https://github.com/o/r/pull/8\"}}]'; else exit 1; fi\n",
                marker.display()
            ),
        );

        let first_now = Instant::now();
        let first = refresh_one_at(
            &git,
            &gh,
            &repo,
            HashMap::new(),
            first_now,
            Instant::now() + Duration::from_secs(5),
        );
        assert!(first.observations[0].context.pr_urls.is_empty());
        assert_eq!(first.cache_updates.len(), 1);
        let cache: HashMap<GitWorkContextCacheKey, GitWorkContextCacheEntry> =
            first.cache_updates.iter().cloned().collect();

        std::fs::write(&marker, "open").expect("mark PR as opened");
        let within_ttl = refresh_one_at(
            &git,
            &gh,
            &repo,
            cache.clone(),
            first_now + WORK_CONTEXT_CACHE_TTL - Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
        );
        assert!(within_ttl.observations[0].context.pr_urls.is_empty());
        assert!(within_ttl.cache_updates.is_empty());

        let expired = refresh_one_at(
            &git,
            &gh,
            &repo,
            cache,
            first_now + WORK_CONTEXT_CACHE_TTL + Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(
            expired.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/8"]
        );
        assert_eq!(expired.cache_updates.len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_prefers_the_pr_whose_head_lives_in_this_repository() {
        let dir = fixture_dir("same-repo-pr");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "master");
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/stranger/r/pull/1\",\"statusCheckRollup\":[],\"isCrossRepository\":true},{\"url\":\"https://github.com/o/r/pull/2\",\"statusCheckRollup\":[],\"isCrossRepository\":false}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            output.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/2"]
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_reports_no_pr_when_only_unrelated_forks_share_the_branch_name() {
        let dir = fixture_dir("ambiguous-fork-prs");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "master");
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/a/r/pull/1\",\"statusCheckRollup\":[],\"isCrossRepository\":true},{\"url\":\"https://github.com/b/r/pull/2\",\"statusCheckRollup\":[],\"isCrossRepository\":true}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        let context = &output.observations[0].context;
        assert!(context.pr_urls.is_empty());
        assert!(context.preview_urls.is_empty());
        assert_eq!(context.branch.as_deref(), Some("master"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn preview_urls_come_from_the_pull_request_comments_that_actually_carry_them() {
        let dir = fixture_dir("preview-from-comments");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/studio-resize");

        // The shape GitHub and Vercel really produce: statusCheckRollup carries
        // only a vercel.com dashboard link, while the deployment alias is posted
        // as a comment by the preview workflow, with a bypass token in its query.
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/9\",\"isCrossRepository\":false,\"statusCheckRollup\":[{\"__typename\":\"StatusContext\",\"context\":\"Vercel\",\"targetUrl\":\"https://vercel.com/scalableso/scalablev2/5tLT6NxeWv7d\"},{\"__typename\":\"CheckRun\",\"name\":\"Vercel Preview Comments\",\"detailsUrl\":\"https://vercel.com/github\"}],\"body\":\"nothing here\",\"comments\":[{\"body\":\"Preview: https://app-git-studio-resize-team.vercel.app/auth?x-vercel-protection-bypass=abc123\"}]}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        let context = &output.observations[0].context;
        assert_eq!(context.pr_urls, vec!["https://github.com/o/r/pull/9"]);
        assert_eq!(
            context.preview_urls,
            vec![
                "https://app-git-studio-resize-team.vercel.app",
                "https://app-git-studio-resize-team.vercel.app/auth?x-vercel-protection-bypass=abc123",
            ],
            "the bare root and the full URL must both survive, and vercel.com dashboard links must not"
        );

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
                "#!/bin/sh\nprintf '%s\\n' '[{{\"url\":\"https://github.com/o/r/pull/7\",\"statusCheckRollup\":[{}]}}]'\n",
                checks
            ),
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        let context = &output.observations[0].context;
        assert_eq!(context.pr_urls, vec!["https://github.com/o/r/pull/7"]);
        assert_eq!(
            context.preview_urls.len(),
            crate::work_context::MAX_PREVIEW_URLS
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn late_git_work_context_refresh_applies_observation_and_cache() {
        let dir = fixture_dir("late-refresh");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let gh_invocations = dir.join("gh-invocations");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/MAT-1-late");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\ncount=0\nif [ -f '{}' ]; then count=$(cat '{}'); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nprintf '%s\\n' '[{{\"url\":\"https://github.com/o/r/pull/9\"}}]'\n",
                gh_invocations.display(),
                gh_invocations.display(),
                gh_invocations.display(),
            ),
        );

        let first_now = Instant::now();
        let first = refresh_one_at(
            &git,
            &gh,
            &repo,
            HashMap::new(),
            first_now,
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(first.observations.len(), 1);
        assert_eq!(first.cache_updates.len(), 1);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("late-git-context");
        workspace.identity_cwd = repo.clone();
        workspace.cached_identity_cwd = repo.clone();
        workspace.cached_git_status_key = repo.clone();
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");

        let mut observation = first
            .observations
            .into_iter()
            .next()
            .expect("git observation");
        observation.pane_id = pane_id;
        let cache_updates = first.cache_updates;
        app.test_begin_git_work_context_refresh(1);
        app.git_work_context_refresh_in_flight
            .as_mut()
            .expect("git refresh in flight")
            .deadline = Instant::now() - Duration::from_millis(1);

        assert!(app.handle_git_work_context_refreshed(1, vec![observation], cache_updates));
        let tiers = app.state.terminals[&terminal_id]
            .work_context
            .snapshot_tiers();
        assert_eq!(
            tiers.git_observation.pr_urls,
            vec!["https://github.com/o/r/pull/9"]
        );
        assert_eq!(app.git_work_context_cache.len(), 1);
        assert!(app.git_work_context_refresh_in_flight.is_none());
        assert!(app.next_git_work_context_refresh > Instant::now());

        let second = refresh_one_at(
            &git,
            &gh,
            &repo,
            app.git_work_context_cache.clone(),
            first_now + Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
        );
        assert!(second.cache_updates.is_empty());
        assert_eq!(
            std::fs::read_to_string(&gh_invocations).expect("read gh invocation count"),
            "1"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn git_work_context_request_during_refresh_is_replayed_after_completion() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("replay")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.git_program_override = Some(PathBuf::from("herdr-test-missing-git"));
        app.test_begin_git_work_context_refresh(1);
        app.next_git_work_context_refresh = Instant::now() + WORK_CONTEXT_REFRESH_INTERVAL;

        app.request_git_work_context_refresh(Instant::now());
        assert!(app.git_work_context_refresh_due_after_in_flight);

        app.handle_git_work_context_refreshed(1, Vec::new(), Vec::new());

        assert!(!app.git_work_context_refresh_due_after_in_flight);
        assert!(app.next_git_work_context_refresh <= Instant::now());
        app.start_git_work_context_refresh_if_due(Instant::now());
        assert_eq!(
            app.git_work_context_refresh_in_flight
                .as_ref()
                .map(|refresh| refresh.generation),
            Some(2)
        );
    }

    #[test]
    fn queued_request_does_not_supersede_a_worker_inside_its_batch_budget() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("budget")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.git_program_override = Some(PathBuf::from("herdr-test-missing-git"));
        let first_now = Instant::now();
        app.next_git_work_context_refresh = first_now;
        app.start_git_work_context_refresh_if_due(first_now);

        app.request_git_work_context_refresh(first_now);
        assert!(app.git_work_context_refresh_due_after_in_flight);

        // Far past a two-second scheduler deadline, but still inside the budget the
        // worker was actually given. Superseding here would discard the observations
        // that worker is still producing.
        app.start_git_work_context_refresh_if_due(first_now + Duration::from_secs(5));

        assert_eq!(
            app.git_work_context_refresh_in_flight
                .as_ref()
                .map(|refresh| refresh.generation),
            Some(1),
            "a queued request must not supersede a worker inside its batch budget"
        );
        assert_eq!(app.last_git_work_context_refresh_generation, 1);
        assert!(app.git_work_context_refresh_due_after_in_flight);
    }

    #[test]
    fn scheduler_expired_git_refresh_accepts_late_result_and_rejects_older_generation() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("scheduled-late-git-context");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let cwd = app.state.terminals[&terminal_id].cwd.clone();
        app.git_program_override = Some(PathBuf::from("herdr-test-missing-git"));

        let first_now = Instant::now();
        app.next_git_work_context_refresh = first_now;
        app.start_git_work_context_refresh_if_due(first_now);
        let first_refresh = app
            .git_work_context_refresh_in_flight
            .clone()
            .expect("first git refresh in flight");

        app.start_git_work_context_refresh_if_due(
            first_refresh.deadline + Duration::from_millis(1),
        );
        assert!(app.git_work_context_refresh_in_flight.is_none());

        let key = GitWorkContextCacheKey {
            repo_root: PathBuf::from("/scheduled-late/repo"),
            branch: "feat/SCA-1-scheduled-late".into(),
        };
        let first_context = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/1".into()],
            branch: Some(key.branch.clone()),
            ..crate::work_context::PaneWorkContext::default()
        };
        let first_entry = GitWorkContextCacheEntry {
            context: first_context.clone(),
            cached_at: Instant::now(),
        };
        assert!(app.handle_git_work_context_refreshed(
            first_refresh.generation,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: None,
                    origin_unparsed: false,
                    cwd: cwd.clone(),
                    repo_root: Some(key.repo_root.clone()),
                    branch: Some(key.branch.clone()),
                },
                context: first_context,
            }],
            vec![(key.clone(), first_entry)],
        ));

        app.start_git_work_context_refresh_if_due(first_now + WORK_CONTEXT_REFRESH_INTERVAL);
        let second_generation = app
            .git_work_context_refresh_in_flight
            .as_ref()
            .expect("second git refresh in flight")
            .generation;
        assert_eq!(second_generation, first_refresh.generation + 1);

        let second_context = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/2".into()],
            branch: Some(key.branch.clone()),
            ..crate::work_context::PaneWorkContext::default()
        };
        let second_entry = GitWorkContextCacheEntry {
            context: second_context.clone(),
            cached_at: Instant::now(),
        };
        assert!(app.handle_git_work_context_refreshed(
            second_generation,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: None,
                    origin_unparsed: false,
                    cwd: cwd.clone(),
                    repo_root: Some(key.repo_root.clone()),
                    branch: Some(key.branch.clone()),
                },
                context: second_context.clone(),
            }],
            vec![(key.clone(), second_entry)],
        ));

        assert!(!app.handle_git_work_context_refreshed(
            first_refresh.generation,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: None,
                    origin_unparsed: false,
                    cwd,
                    repo_root: Some(key.repo_root.clone()),
                    branch: Some(key.branch.clone()),
                },
                context: crate::work_context::PaneWorkContext {
                    pr_urls: vec!["https://github.com/o/r/pull/old".into()],
                    branch: Some(key.branch.clone()),
                    ..crate::work_context::PaneWorkContext::default()
                },
            }],
            Vec::new(),
        ));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .work_context
                .snapshot_tiers()
                .git_observation
                .pr_urls,
            vec!["https://github.com/o/r/pull/2"]
        );
    }

    #[test]
    fn superseded_git_refresh_drops_result_before_successor() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("superseded-git-context");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let cwd = app.state.terminals[&terminal_id].cwd.clone();
        app.git_program_override = Some(PathBuf::from("herdr-test-missing-git"));

        app.test_begin_git_work_context_refresh(1);
        let first_refresh = app
            .git_work_context_refresh_in_flight
            .clone()
            .expect("first git refresh in flight");

        app.test_begin_git_work_context_refresh(2);
        let successor_generation = app
            .git_work_context_refresh_in_flight
            .as_ref()
            .expect("successor git refresh in flight")
            .generation;
        assert_eq!(successor_generation, first_refresh.generation + 1);

        let key = GitWorkContextCacheKey {
            repo_root: PathBuf::from("/superseded/repo"),
            branch: "feat/superseded".into(),
        };
        let stale_context = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/7".into()],
            branch: Some(key.branch.clone()),
            ..crate::work_context::PaneWorkContext::default()
        };
        assert!(!app.handle_git_work_context_refreshed(
            first_refresh.generation,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: None,
                    origin_unparsed: false,
                    cwd,
                    repo_root: Some(key.repo_root.clone()),
                    branch: Some(key.branch.clone()),
                },
                context: stale_context.clone(),
            }],
            vec![(
                key.clone(),
                GitWorkContextCacheEntry {
                    context: stale_context,
                    cached_at: Instant::now(),
                },
            )],
        ));
        assert!(app.state.terminals[&terminal_id]
            .work_context
            .snapshot_tiers()
            .git_observation
            .pr_urls
            .is_empty());
        assert!(app.git_work_context_cache.is_empty());
        assert_eq!(
            app.git_work_context_refresh_in_flight
                .as_ref()
                .map(|refresh| refresh.generation),
            Some(successor_generation)
        );

        app.handle_git_work_context_refreshed(successor_generation, Vec::new(), Vec::new());
        assert_eq!(
            app.last_applied_git_work_context_refresh_generation,
            successor_generation
        );
    }

    #[test]
    fn git_work_context_generation_mismatch_drops_observation_and_cache() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("stale-git-context");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let cwd = app.state.terminals[&terminal_id].cwd.clone();
        app.test_begin_git_work_context_refresh(2);
        app.last_applied_git_work_context_refresh_generation = 1;

        let key = GitWorkContextCacheKey {
            repo_root: PathBuf::from("/stale/repo"),
            branch: "feat/SCA-2-stale".into(),
        };
        let context = crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/o/r/pull/10".into()],
            branch: Some("feat/SCA-2-stale".into()),
            ..crate::work_context::PaneWorkContext::default()
        };
        app.handle_git_work_context_refreshed(
            1,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: None,
                    origin_unparsed: false,
                    cwd,
                    repo_root: Some(key.repo_root.clone()),
                    branch: Some(key.branch.clone()),
                },
                context: context.clone(),
            }],
            vec![(
                key,
                GitWorkContextCacheEntry {
                    context,
                    cached_at: Instant::now(),
                },
            )],
        );

        assert_eq!(
            app.state.terminals[&terminal_id]
                .work_context
                .snapshot_tiers()
                .git_observation,
            crate::work_context::PaneWorkContext::default()
        );
        assert!(app.git_work_context_cache.is_empty());
        assert_eq!(
            app.git_work_context_refresh_in_flight
                .map(|refresh| refresh.generation),
            Some(2)
        );
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
                        repo: None,
                        origin_unparsed: false,
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

    #[tokio::test]
    async fn not_due_git_work_context_refresh_does_not_query_runtime_cwd() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("git-context-not-due");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id, runtime);

        let now = Instant::now();
        app.next_git_work_context_refresh = now + WORK_CONTEXT_REFRESH_INTERVAL;
        crate::terminal::TerminalRuntime::test_reset_cwd_query_count();

        for _ in 0..64 {
            app.start_git_work_context_refresh_if_due(now);
        }

        assert_eq!(crate::terminal::TerminalRuntime::test_cwd_query_count(), 0);
    }
}
