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

        // Adoption runs once the whole batch is applied, and over every
        // workspace rather than only the ones this batch touched. Adopting
        // inside the loop read a half-applied batch, so a workspace holding two
        // checkouts bound itself to whichever pane reported first; and a
        // restored session, whose panes already carry their repository, never
        // reached an adoption call at all because no observation changed.
        for ws_idx in 0..self.state.workspaces.len() {
            self.adopt_repo_binding_for_workspace(ws_idx);
        }

        self.prune_git_work_context_state();

        if changed {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
        changed | self.finish_sidebar_refresh_if_idle()
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
    // Keyed by repository root, not by cwd: sibling panes sitting in different
    // subdirectories of one checkout share a slug, and paying `gh repo view`
    // again for each of them used to spend the budget the pull-request query
    // needed.
    let mut repo_slugs = HashMap::<PathBuf, Option<String>>::new();

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
                input.repo = match repo_slugs.get(repo_root) {
                    Some(slug) => slug.clone(),
                    None => {
                        let slug = gh_repo_slug(repo_root, target_gh_deadline, gh_program);
                        repo_slugs.insert(repo_root.to_path_buf(), slug.clone());
                        slug
                    }
                };
                // Remember it so sibling panes in the same checkout do not each
                // pay for another gh call.
                discovered.insert(target.cwd.clone(), Some(input.clone()));
            }
        }
        // Re-measured after the slug lookup. Sharing one window between the two
        // gh calls let `gh repo view` spend it all, so the pull-request query
        // was skipped outright and the pane reported a branch with no link.
        let target_gh_deadline = (Instant::now() + target_timeout).min(gh_deadline);

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
                    let probe = git_work_context_for_branch(
                        branch,
                        repo_root,
                        input.repo.as_deref(),
                        target_gh_deadline,
                        gh_program,
                    );
                    if probe.answered {
                        let entry = GitWorkContextCacheEntry {
                            context: probe.context.clone(),
                            cached_at: now,
                        };
                        cache_updates.push((key.clone(), entry.clone()));
                        cache.insert(key, entry);
                        probe.context
                    } else if let Some(entry) = cache.get(&key) {
                        // The probe never reached GitHub. Serving the expired
                        // answer keeps the pane's pull request on screen instead
                        // of replacing it with an empty context, and leaving the
                        // entry stale means the next refresh retries at once.
                        entry.context.clone()
                    } else {
                        probe.context
                    }
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

/// The outcome of one `gh` probe for a branch.
///
/// A probe that never reached GitHub is not evidence that the branch has no
/// pull request, so the two cases stay distinguishable: an answered probe may
/// be cached and published, an unanswered one must not overwrite what a
/// previous refresh already found.
struct BranchProbe {
    context: crate::work_context::PaneWorkContext,
    answered: bool,
}

impl BranchProbe {
    fn unanswered(context: crate::work_context::PaneWorkContext) -> Self {
        Self {
            context,
            answered: false,
        }
    }

    fn answered(context: crate::work_context::PaneWorkContext) -> Self {
        Self {
            context,
            answered: true,
        }
    }
}

fn git_work_context_for_branch(
    branch: &str,
    repo_root: &Path,
    repo: Option<&str>,
    deadline: Instant,
    gh_program: &Path,
) -> BranchProbe {
    let mut context = crate::work_context::PaneWorkContext {
        ticket_ids: extract_ticket_ids(branch),
        branch: Some(branch.to_string()),
        ..crate::work_context::PaneWorkContext::default()
    };

    if Instant::now() >= deadline {
        tracing::debug!(branch, "git work context: gh budget spent before the query");
        return BranchProbe::unanswered(context);
    }

    let mut command = crate::noninteractive_process::command(gh_program);
    command
        .current_dir(repo_root)
        .args(gh_pr_view_args(branch, repo));
    let Ok(output) = crate::noninteractive_process::output_with_deadline(command, deadline) else {
        tracing::debug!(branch, "git work context: gh did not run to completion");
        return BranchProbe::unanswered(context);
    };
    if !output.status.success() {
        tracing::debug!(
            branch,
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "git work context: gh exited non-zero"
        );
        return BranchProbe::unanswered(context);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) else {
        tracing::debug!(branch, "git work context: gh output was not JSON");
        return BranchProbe::unanswered(context);
    };
    let Some(prs) = value.as_array() else {
        return BranchProbe::unanswered(context);
    };
    let Some(pr) = choose_branch_pr(prs) else {
        return BranchProbe::answered(context);
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
    BranchProbe::answered(context)
}

/// Pick the one pull request a branch's panes should link to.
///
/// `--head` matches by branch name alone, so every fork with a branch of this
/// name comes back, and `--state all` additionally brings back every pull
/// request a reused branch name ever opened in this repository. Those are two
/// different kinds of plurality: several pull requests from one head are all
/// ours and the best of them is the answer, while heads in different forks are
/// namesakes and none of them can be attributed to this pane.
fn choose_branch_pr(prs: &[Value]) -> Option<&Value> {
    let same_repo: Vec<&Value> = prs
        .iter()
        .filter(|pr| pr.get("isCrossRepository").and_then(Value::as_bool) == Some(false))
        .collect();
    // A head in this repository is unambiguously ours, so it wins outright.
    let candidates = if same_repo.is_empty() {
        // Otherwise every candidate must come from the same fork. Fork work is
        // the normal shape here, so one owner with several attempts on a reused
        // branch name still resolves; two owners genuinely do not.
        let owners: HashSet<Option<&str>> = prs
            .iter()
            .map(|pr| pr.get("headRepositoryOwner").and_then(owner_login))
            .collect();
        if owners.len() > 1 {
            return None;
        }
        prs.iter().collect()
    } else {
        same_repo
    };
    // Rank rather than take the first: a branch whose only pull request has
    // merged still belongs to it, which is why the open-only query left panes
    // showing a diff and no link at all.
    candidates.into_iter().max_by_key(|pr| {
        let state = match pr.get("state").and_then(Value::as_str) {
            Some("OPEN") => 2,
            Some("MERGED") => 1,
            // Closed-unmerged is the weakest signal but still better than none.
            _ => 0,
        };
        // Highest number is the most recent attempt on a reused branch name.
        let number = pr.get("number").and_then(Value::as_i64).unwrap_or(0);
        (state, number)
    })
}

/// `headRepositoryOwner` is an object in gh's JSON, and its `login` is the fork.
fn owner_login(owner: &Value) -> Option<&str> {
    owner.get("login").and_then(Value::as_str)
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
    // Without this `gh` filters to open pull requests, so a branch whose pull
    // request had already merged or closed reported no link at all.
    args.push("--state".to_string());
    args.push("all".to_string());
    args.push("--json".to_string());
    args.push(
        "url,state,number,headRepositoryOwner,statusCheckRollup,isCrossRepository,body,comments"
            .to_string(),
    );
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
state=
positionals=0
for arg
do
    case "$arg" in
        --head|--json|--limit|--repo|--state)
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
                    --state) state="$arg" ;;
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
[ "$state" = all ] || exit 2
[ "$json" = url,state,number,headRepositoryOwner,statusCheckRollup,isCrossRepository,body,comments ] || exit 2
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
                "#!/bin/sh\nif [ -f '{}' ]; then printf '%s\\n' '[{{\"url\":\"https://github.com/o/r/pull/8\"}}]'; else printf '%s\\n' '[]'; fi\n",
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
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/a/r/pull/1\",\"statusCheckRollup\":[],\"isCrossRepository\":true,\"headRepositoryOwner\":{\"login\":\"a\"}},{\"url\":\"https://github.com/b/r/pull/2\",\"statusCheckRollup\":[],\"isCrossRepository\":true,\"headRepositoryOwner\":{\"login\":\"b\"}}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        let context = &output.observations[0].context;
        assert!(context.pr_urls.is_empty());
        assert!(context.preview_urls.is_empty());
        assert_eq!(context.branch.as_deref(), Some("master"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gh_query_covers_every_pull_request_state() {
        let args = gh_pr_view_args("feat/x", Some("o/r"));
        let state = args
            .iter()
            .position(|arg| arg == "--state")
            .and_then(|idx| args.get(idx + 1));
        assert_eq!(
            state.map(String::as_str),
            Some("all"),
            "gh defaults to open, so a merged pull request reported no link at all"
        );
    }

    #[cfg(unix)]
    #[test]
    fn gh_reports_the_merged_pull_request_for_a_branch_that_is_still_checked_out() {
        let dir = fixture_dir("merged-pr");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/landed");
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/11\",\"state\":\"MERGED\",\"number\":11,\"isCrossRepository\":false}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            output.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/11"],
            "a finished branch still on disk has a pull request, not just a diff"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn gh_prefers_the_open_pull_request_when_one_fork_reused_a_branch_name() {
        let dir = fixture_dir("reused-branch-name");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/reused");
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/3\",\"state\":\"CLOSED\",\"number\":3,\"isCrossRepository\":true,\"headRepositoryOwner\":{\"login\":\"fork\"}},{\"url\":\"https://github.com/o/r/pull/7\",\"state\":\"OPEN\",\"number\":7,\"isCrossRepository\":true,\"headRepositoryOwner\":{\"login\":\"fork\"}}]'\n",
        );

        let output = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            output.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/7"],
            "one fork with several attempts is not the ambiguous case"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_gh_probe_keeps_the_pull_request_the_last_one_found() {
        let dir = fixture_dir("gh-failure-keeps-pr");
        let git = dir.join("git");
        let gh = dir.join("gh");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("create repo fixture");
        fake_git(&git, &repo, "feat/flaky");
        write_executable(
            &gh,
            "#!/bin/sh\nprintf '%s\\n' '[{\"url\":\"https://github.com/o/r/pull/5\",\"state\":\"OPEN\",\"number\":5,\"isCrossRepository\":false}]'\n",
        );
        let first = refresh_one(&git, &gh, &repo, Instant::now() + Duration::from_secs(5));
        assert_eq!(
            first.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/5"]
        );

        // gh is now unauthenticated, offline, or simply too slow. That is not
        // evidence the branch lost its pull request.
        write_executable(&gh, "#!/bin/sh\nexit 1\n");
        let cache: HashMap<_, _> = first.cache_updates.iter().cloned().collect();
        let now = Instant::now();
        let second = refresh_git_work_contexts(
            vec![GitWorkContextTarget {
                pane_id: PaneId::from_raw(1),
                cwd: repo.clone(),
            }],
            cache,
            // Past the cache TTL, so the probe really runs again.
            now + WORK_CONTEXT_CACHE_TTL,
            now + Duration::from_secs(5),
            now + Duration::from_secs(5),
            Duration::from_secs(5),
            &git,
            &gh,
        );
        assert_eq!(
            second.observations[0].context.pr_urls,
            vec!["https://github.com/o/r/pull/5"],
            "a probe that never reached GitHub must not erase what the last one found"
        );

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
    fn periodic_git_refresh_preserves_hook_assigned_work_items() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("hook-before-git-refresh");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("test pane terminal");
        let cwd = app.state.terminals[&terminal_id].cwd.clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal")
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-1".into()],
                pr_urls: vec!["https://github.com/hook/repo/pull/1".into()],
                ..Default::default()
            })
            .expect("hook assignment");

        for generation in 1..=2 {
            let branch = format!("feat/SCA-{}-git", generation + 1);
            app.test_begin_git_work_context_refresh(generation);
            assert!(app.handle_git_work_context_refreshed(
                generation,
                vec![GitWorkContextObservation {
                    pane_id,
                    input: GitWorkContextInput {
                        repo: Some("git/repo".into()),
                        origin_unparsed: false,
                        cwd: cwd.clone(),
                        repo_root: Some(PathBuf::from("/git/repo")),
                        branch: Some(branch.clone()),
                    },
                    context: crate::work_context::PaneWorkContext {
                        ticket_ids: vec![format!("SCA-{}", generation + 1)],
                        pr_urls: vec![format!(
                            "https://github.com/git/repo/pull/{}",
                            generation + 1
                        )],
                        branch: Some(branch),
                        repo: Some("git/repo".into()),
                        ..Default::default()
                    },
                }],
                Vec::new(),
            ));
            let effective = &app.state.terminals[&terminal_id].work_context;
            assert_eq!(effective.effective().ticket_ids, ["SCA-1"]);
            assert_eq!(
                effective.effective().pr_urls,
                ["https://github.com/hook/repo/pull/1"]
            );
            let git = effective.snapshot_tiers().git_observation;
            assert_eq!(git.ticket_ids, [format!("SCA-{}", generation + 1)]);
            assert_eq!(
                git.pr_urls,
                [format!(
                    "https://github.com/git/repo/pull/{}",
                    generation + 1
                )]
            );
        }
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

    /// One observation carrying `repo` for `pane_id`.
    fn repo_observation(
        pane_id: crate::layout::PaneId,
        cwd: PathBuf,
        repo: &str,
        branch: &str,
    ) -> GitWorkContextObservation {
        GitWorkContextObservation {
            pane_id,
            input: GitWorkContextInput {
                repo: Some(repo.into()),
                origin_unparsed: false,
                cwd,
                repo_root: Some(PathBuf::from(format!("/adopt/{repo}"))),
                branch: Some(branch.into()),
            },
            context: crate::work_context::PaneWorkContext {
                repo: Some(repo.into()),
                branch: Some(branch.into()),
                ..crate::work_context::PaneWorkContext::default()
            },
        }
    }

    #[test]
    fn a_batch_resolving_two_repositories_leaves_the_workspace_unbound() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("two-repos");
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane_ids: Vec<crate::layout::PaneId> = app.state.workspaces[0].tabs[0]
            .panes
            .keys()
            .copied()
            .collect();
        assert_eq!(pane_ids.len(), 2, "fixture must hold two panes");
        let cwds: Vec<PathBuf> = pane_ids
            .iter()
            .map(|pane_id| {
                let terminal_id = app.state.workspaces[0].tabs[0]
                    .terminal_id(*pane_id)
                    .cloned()
                    .expect("test pane terminal");
                app.state.terminals[&terminal_id].cwd.clone()
            })
            .collect();
        app.git_program_override = Some(PathBuf::from("herdr-test-missing-git"));

        let now = Instant::now();
        app.next_git_work_context_refresh = now;
        app.start_git_work_context_refresh_if_due(now);
        let generation = app
            .git_work_context_refresh_in_flight
            .as_ref()
            .expect("git refresh in flight")
            .generation;

        // Applied in order: adopting inside the loop bound the workspace to the
        // first pane's repository before the second was ever seen.
        app.handle_git_work_context_refreshed(
            generation,
            vec![
                repo_observation(pane_ids[0], cwds[0].clone(), "owner/one", "feat/one"),
                repo_observation(pane_ids[1], cwds[1].clone(), "owner/two", "feat/two"),
            ],
            Vec::new(),
        );

        assert_eq!(
            app.state.workspaces[0].repo_binding, None,
            "a workspace holding two checkouts must stay unbound"
        );
    }

    #[test]
    fn a_resolved_repository_binds_the_workspace_through_the_refresh_path() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("adopt-through-refresh");
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
        assert_eq!(app.state.workspaces[0].repo_binding, None);

        let now = Instant::now();
        app.next_git_work_context_refresh = now;
        app.start_git_work_context_refresh_if_due(now);
        let generation = app
            .git_work_context_refresh_in_flight
            .as_ref()
            .expect("git refresh in flight")
            .generation;

        let repo_root = PathBuf::from("/adopt-through-refresh/repo");
        let branch = "feat/adopt".to_string();
        let context = crate::work_context::PaneWorkContext {
            repo: Some("owner/adopted".into()),
            branch: Some(branch.clone()),
            ..crate::work_context::PaneWorkContext::default()
        };
        assert!(app.handle_git_work_context_refreshed(
            generation,
            vec![GitWorkContextObservation {
                pane_id,
                input: GitWorkContextInput {
                    repo: Some("owner/adopted".into()),
                    origin_unparsed: false,
                    cwd,
                    repo_root: Some(repo_root),
                    branch: Some(branch),
                },
                context,
            }],
            Vec::new(),
        ));

        assert_eq!(
            app.state.workspaces[0].repo_binding.as_deref(),
            Some("owner/adopted"),
            "the workspace must adopt the repository its pane resolved"
        );
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
