use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use super::{
    App, GitRefreshInFlight, GIT_REFRESH_TIMEOUT, GIT_REMOTE_STATUS_REFRESH_INTERVAL,
    GIT_REPO_DISCOVERY_REFRESH_INTERVAL,
};
use crate::events::AppEvent;
use crate::workspace::{GitStatusCacheEntry, GitStatusRefreshDemand, WorkspaceGitStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshItem {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
    cache_key_hint: Option<PathBuf>,
    demand: GitStatusRefreshDemand,
    updates_workspace_identity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshTarget {
    workspace_id: String,
    resolved_identity_cwd: PathBuf,
    demand: GitStatusRefreshDemand,
    updates_workspace_identity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshJob {
    cache_key: PathBuf,
    cached: Option<GitStatusCacheEntry>,
    targets: Vec<WorkspaceGitRefreshTarget>,
    demand: GitStatusRefreshDemand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitRefreshOutput {
    results: Vec<WorkspaceGitStatus>,
    cache_updates: Vec<(PathBuf, GitStatusCacheEntry)>,
    file_fingerprints: Vec<(PathBuf, u64)>,
}

impl App {
    fn git_program_for_refresh(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(program) = self.git_program_override.as_ref() {
            return program.clone();
        }

        PathBuf::from("git")
    }

    #[cfg(test)]
    pub(crate) fn set_test_git_program(&mut self, program: PathBuf) {
        self.git_program_override = Some(program);
    }

    #[cfg(test)]
    pub(crate) fn test_begin_git_refresh(&mut self, generation: u64) {
        let started_at = Instant::now();
        self.last_git_refresh_generation = generation;
        self.git_refresh_in_flight = Some(GitRefreshInFlight {
            generation,
            started_at,
            deadline: started_at + GIT_REFRESH_TIMEOUT,
        });
    }

    pub(crate) fn start_git_status_refresh_if_due(&mut self, now: Instant) {
        if self
            .git_refresh_in_flight
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.invalidate_expired_git_refresh(now);
        }

        let Some(deadline) = self.git_refresh_deadline() else {
            return;
        };

        if now < deadline {
            return;
        }

        let git_menu_visible = self.state.view.git_menu_button_hit_area.width > 0;
        if (self.state.status_bar_enabled || git_menu_visible)
            && self.sync_status_focused_runtime_cwd()
        {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }

        let refresh_repo_discovery = self.git_identity_refresh_requested
            || now.saturating_duration_since(self.last_git_repo_discovery_refresh)
                >= GIT_REPO_DISCOVERY_REFRESH_INTERVAL;
        let workspaces = self.workspace_git_refresh_items(refresh_repo_discovery);

        if workspaces.is_empty() {
            self.last_git_remote_status_refresh = now;
            self.git_identity_refresh_requested = false;
            return;
        }

        self.last_git_refresh_generation = self.last_git_refresh_generation.wrapping_add(1);
        let generation = self.last_git_refresh_generation;
        let deadline = now + GIT_REFRESH_TIMEOUT;
        self.git_refresh_in_flight = Some(GitRefreshInFlight {
            generation,
            started_at: now,
            deadline,
        });
        let event_tx = self.event_tx.clone();
        let cache = self.git_status_cache.clone();
        let git_program = self.git_program_for_refresh();
        let file_roots = self
            .state
            .dock_files_root
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        self.git_identity_refresh_requested = false;
        if refresh_repo_discovery {
            self.last_git_repo_discovery_refresh = now;
        }
        std::thread::spawn(move || {
            let mut output =
                refresh_workspace_git_statuses(workspaces, &cache, deadline, &git_program);
            output.file_fingerprints = file_roots
                .into_iter()
                .filter_map(|root| {
                    crate::files::git_file_fingerprint(&root, &git_program)
                        .map(|fingerprint| (root, fingerprint))
                })
                .collect();
            let _ = event_tx.blocking_send(AppEvent::GitStatusRefreshed {
                generation,
                results: output.results,
                cache_updates: output.cache_updates,
                file_fingerprints: output.file_fingerprints,
            });
        });
    }

    pub(crate) fn request_git_identity_refresh(&mut self, now: Instant) {
        self.git_identity_refresh_requested = true;
        self.mark_git_status_refresh_due(now);
    }

    pub(crate) fn mark_git_status_refresh_due(&mut self, now: Instant) {
        self.git_status_cache
            .retain(|_, entry| entry.fingerprint.is_some());
        if self.git_refresh_in_flight.is_some() {
            self.git_refresh_due_after_in_flight = true;
            return;
        }
        self.last_git_remote_status_refresh = now
            .checked_sub(GIT_REMOTE_STATUS_REFRESH_INTERVAL)
            .unwrap_or(now);
        self.git_refresh_due_after_in_flight = false;
    }

    pub(crate) fn git_refresh_deadline(&self) -> Option<Instant> {
        if let Some(refresh) = self.git_refresh_in_flight {
            return Some(refresh.deadline);
        }

        (!self.state.workspaces.is_empty()
            && (self.git_identity_refresh_requested || !self.git_refresh_demand().is_empty()))
        .then_some(self.last_git_remote_status_refresh + GIT_REMOTE_STATUS_REFRESH_INTERVAL)
    }

    fn invalidate_expired_git_refresh(&mut self, now: Instant) {
        let Some(refresh) = self.git_refresh_in_flight.take() else {
            return;
        };
        tracing::warn!(
            generation = refresh.generation,
            elapsed_ms = now
                .saturating_duration_since(refresh.started_at)
                .as_millis(),
            "git status refresh exceeded its deadline; scheduling retry"
        );
        self.git_refresh_due_after_in_flight = false;
        self.mark_git_status_refresh_due(now);
    }

    fn git_refresh_demand(&self) -> GitStatusRefreshDemand {
        let mut demand = self.sidebar_git_refresh_demand();
        demand.branch |= self.state.status_bar_enabled;
        demand.ahead_behind |= self.state.view.git_menu_button_hit_area.width > 0;
        demand.branch |= !self.state.dock_collapsed
            && self.state.dock_tab == Some(crate::app::DockSurface::Files);
        demand.branch |= self.dock_files_refresh_demand;
        demand
    }

    fn sidebar_git_refresh_demand(&self) -> GitStatusRefreshDemand {
        let mut demand = GitStatusRefreshDemand::default();
        for token in self.state.sidebar_spaces.rows.iter().flatten() {
            match token.parts().0 {
                crate::config::SpaceSidebarToken::Branch => demand.branch = true,
                crate::config::SpaceSidebarToken::GitStatus => demand.ahead_behind = true,
                _ => {}
            }
        }
        demand
    }

    fn workspace_git_refresh_items(
        &self,
        refresh_repo_discovery: bool,
    ) -> Vec<WorkspaceGitRefreshItem> {
        let mut workspace_demand = self.sidebar_git_refresh_demand();
        workspace_demand.branch |= self.git_identity_refresh_requested;
        let git_menu_visible = self.state.view.git_menu_button_hit_area.width > 0;
        workspace_demand.ahead_behind |= git_menu_visible;
        workspace_demand.branch |= !self.state.dock_collapsed
            && self.state.dock_tab == Some(crate::app::DockSurface::Files);
        workspace_demand.branch |= self.dock_files_refresh_demand;
        let mut items = if workspace_demand.is_empty() {
            Vec::new()
        } else {
            self.state
                .workspaces
                .iter()
                .filter_map(|ws| {
                    let cwd = ws.resolved_identity_cwd_from(
                        &self.state.terminals,
                        &self.terminal_runtimes,
                    )?;
                    let cache_key_hint = (!refresh_repo_discovery && ws.cached_identity_cwd == cwd)
                        .then(|| ws.cached_git_status_key.clone());
                    Some(WorkspaceGitRefreshItem {
                        workspace_id: ws.id.clone(),
                        resolved_identity_cwd: cwd,
                        cache_key_hint,
                        demand: workspace_demand,
                        updates_workspace_identity: true,
                    })
                })
                .collect::<Vec<_>>()
        };

        if self.state.status_bar_enabled || git_menu_visible {
            // The status bar projects whichever pane becomes focused next, so every
            // live pane cwd across all workspaces and tabs needs an observation.
            // Job-level deduplication by checkout/cache key happens downstream in
            // `deduplicate_git_refresh_items`.
            for ws in &self.state.workspaces {
                for tab in &ws.tabs {
                    for pane_id in tab.layout.pane_ids() {
                        let Some(cwd) = tab.cwd_for_pane(
                            pane_id,
                            &self.state.terminals,
                            &self.terminal_runtimes,
                        ) else {
                            continue;
                        };
                        push_focused_git_refresh_item(
                            &mut items,
                            ws,
                            cwd,
                            refresh_repo_discovery,
                            self.state.status_bar_enabled,
                            git_menu_visible,
                        );
                    }
                }
            }

            // Keep the focused status-bar projection target even when its cached
            // projection cwd is not resolvable through a live pane right now.
            if let Some(ws) = self
                .state
                .active
                .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            {
                if let Some(cwd) = self.state.status_focused_cwd.clone() {
                    push_focused_git_refresh_item(
                        &mut items,
                        ws,
                        cwd,
                        refresh_repo_discovery,
                        self.state.status_bar_enabled,
                        git_menu_visible,
                    );
                }
            }
        }

        items
    }
}

fn push_focused_git_refresh_item(
    items: &mut Vec<WorkspaceGitRefreshItem>,
    ws: &crate::workspace::Workspace,
    cwd: PathBuf,
    refresh_repo_discovery: bool,
    branch: bool,
    ahead_behind: bool,
) {
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.workspace_id == ws.id && item.resolved_identity_cwd == cwd)
    {
        existing.demand.branch |= branch;
        existing.demand.ahead_behind |= ahead_behind;
        return;
    }
    let cache_key_hint = (!refresh_repo_discovery && ws.cached_identity_cwd == cwd)
        .then(|| ws.cached_git_status_key.clone());
    items.push(WorkspaceGitRefreshItem {
        workspace_id: ws.id.clone(),
        resolved_identity_cwd: cwd,
        cache_key_hint,
        demand: GitStatusRefreshDemand {
            branch,
            ahead_behind,
        },
        updates_workspace_identity: false,
    });
}

fn deduplicate_git_refresh_items(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
) -> Vec<WorkspaceGitRefreshJob> {
    let mut indexes = HashMap::<PathBuf, usize>::new();
    let mut jobs = Vec::<WorkspaceGitRefreshJob>::new();

    for item in items {
        let cache_key = item.cache_key_hint.unwrap_or_else(|| {
            crate::workspace::git_status_cache_key(&item.resolved_identity_cwd)
                .unwrap_or_else(|| item.resolved_identity_cwd.clone())
        });
        let target = WorkspaceGitRefreshTarget {
            workspace_id: item.workspace_id,
            resolved_identity_cwd: item.resolved_identity_cwd,
            demand: item.demand,
            updates_workspace_identity: item.updates_workspace_identity,
        };
        if let Some(&index) = indexes.get(&cache_key) {
            jobs[index].targets.push(target);
            jobs[index].demand.branch |= item.demand.branch;
            jobs[index].demand.ahead_behind |= item.demand.ahead_behind;
            continue;
        }

        let cached = cache.get(&cache_key).cloned();
        indexes.insert(cache_key.clone(), jobs.len());
        jobs.push(WorkspaceGitRefreshJob {
            cache_key,
            cached,
            targets: vec![target],
            demand: item.demand,
        });
    }

    jobs
}

fn refresh_workspace_git_statuses(
    items: Vec<WorkspaceGitRefreshItem>,
    cache: &HashMap<PathBuf, GitStatusCacheEntry>,
    deadline: Instant,
    git_program: &std::path::Path,
) -> WorkspaceGitRefreshOutput {
    let mut results = Vec::new();
    let mut cache_updates = Vec::new();

    for job in deduplicate_git_refresh_items(items, cache) {
        let (snapshot, cache_entry) =
            crate::workspace::git_status_snapshot_for_cwd_with_demand_and_program(
                &job.cache_key,
                job.cached.as_ref(),
                job.demand,
                deadline,
                git_program,
            );
        if let Some(cache_entry) = cache_entry {
            cache_updates.push((job.cache_key.clone(), cache_entry));
        }
        results.extend(job.targets.into_iter().map(move |target| {
            snapshot.clone().into_workspace_status(
                target.workspace_id,
                target.resolved_identity_cwd,
                job.cache_key.clone(),
                target.demand,
                target.updates_workspace_identity,
            )
        }));
    }

    WorkspaceGitRefreshOutput {
        results,
        cache_updates,
        file_fingerprints: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(not(unix))]
    use std::path::Path;
    #[cfg(unix)]
    use std::time::Duration;

    use super::*;
    use crate::workspace::Workspace;

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).expect("write fake git");
        let mut permissions = std::fs::metadata(path)
            .expect("fake git metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake git executable");
    }

    #[cfg(unix)]
    fn pid_is_alive(pid: &str) -> bool {
        let Ok(pid) = pid.parse::<libc::pid_t>() else {
            return false;
        };
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(unix)]
    fn assert_pid_dead(pid: &str, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !pid_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} (pid {pid}) still exists after the deadline");
    }

    #[cfg(unix)]
    fn wait_for_file(path: &std::path::Path, what: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.trim().is_empty() {
                    return contents;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "{what} was not written before the deadline: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    fn wait_for_invocation_count(path: &std::path::Path, minimum: u32) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if contents.trim().parse::<u32>().unwrap_or(0) >= minimum {
                    return contents;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "fake git did not reach invocation {minimum} before the deadline: {}",
            path.display()
        );
    }

    fn config_with_sidebar_branch() -> crate::config::Config {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Branch]];
        config
    }

    #[test]
    fn git_refresh_deduplicates_workspaces_with_same_cache_key() {
        let repo =
            std::env::temp_dir().join(format!("herdr-git-refresh-dedupe-{}", std::process::id()));
        let nested = repo.join("nested");
        let other = repo.join("other");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::create_dir_all(&other).expect("create other dir");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("init")
            .output()
            .expect("run git init");

        let output = refresh_workspace_git_statuses(
            vec![
                WorkspaceGitRefreshItem {
                    workspace_id: "one".into(),
                    resolved_identity_cwd: nested.clone(),
                    cache_key_hint: None,
                    demand: GitStatusRefreshDemand::ALL,
                    updates_workspace_identity: true,
                },
                WorkspaceGitRefreshItem {
                    workspace_id: "two".into(),
                    resolved_identity_cwd: other.clone(),
                    cache_key_hint: None,
                    demand: GitStatusRefreshDemand::ALL,
                    updates_workspace_identity: true,
                },
            ],
            &HashMap::new(),
            Instant::now() + GIT_REFRESH_TIMEOUT,
            Path::new("git"),
        );

        assert_eq!(output.cache_updates.len(), 1);
        assert_eq!(
            output.cache_updates[0].0,
            std::fs::canonicalize(&repo).expect("canonical repo path")
        );
        assert_eq!(output.results.len(), 2);
        assert_eq!(output.results[0].workspace_id, "one");
        assert_eq!(output.results[0].resolved_identity_cwd, nested);
        assert_eq!(output.results[1].workspace_id, "two");
        assert_eq!(output.results[1].resolved_identity_cwd, other);

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn git_refresh_item_collection_does_not_discover_uncached_cwd() {
        let mut app = test_app(&config_with_sidebar_branch());
        let cwd = std::env::temp_dir().join(format!("herdr-uncached-cwd-{}", std::process::id()));
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resolved_identity_cwd, cwd);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn git_refresh_item_collection_reuses_matching_cached_key() {
        let mut app = test_app(&config_with_sidebar_branch());
        let cwd = PathBuf::from("/repo/deep/nested");
        let cache_key = PathBuf::from("/repo");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = cache_key.clone();
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, Some(cache_key));
    }

    #[test]
    fn periodic_repo_discovery_ignores_cached_key_hints() {
        let mut app = test_app(&config_with_sidebar_branch());
        let cwd = PathBuf::from("/repo/deep/nested");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = cwd.clone();
        ws.cached_identity_cwd = cwd;
        ws.cached_git_status_key = PathBuf::from("/repo");
        ws.tabs.clear();
        app.state.workspaces.push(ws);

        let items = app.workspace_git_refresh_items(true);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cache_key_hint, None);
    }

    #[test]
    fn focused_nested_pane_refreshes_its_branch_without_rebinding_workspace_branch() {
        let outer = std::env::temp_dir().join(format!(
            "herdr-focused-status-branch-{}",
            std::process::id()
        ));
        let nested = outer.join("nested");
        let _ = std::fs::remove_dir_all(&outer);
        std::fs::create_dir_all(&nested).expect("create nested fixture");
        for (cwd, branch) in [(&outer, "outer-branch"), (&nested, "nested-branch")] {
            let output = std::process::Command::new("git")
                .args(["init", "-b", branch])
                .arg(cwd)
                .output()
                .expect("initialize fixture repository");
            assert!(output.status.success(), "{output:?}");
        }

        let mut app = test_app(&config_with_sidebar_branch());
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = outer.clone();
        ws.cached_identity_cwd = outer.clone();
        let root = ws.tabs[0].root_pane;
        let root_terminal = ws.terminal_id(root).expect("root terminal").clone();
        let focused = ws.test_split(ratatui::layout::Direction::Horizontal);
        let focused_terminal = ws.terminal_id(focused).expect("focused terminal").clone();
        app.state.terminals.insert(
            root_terminal.clone(),
            crate::terminal::TerminalState::new(root_terminal, outer.clone()),
        );
        app.state.terminals.insert(
            focused_terminal.clone(),
            crate::terminal::TerminalState::new(focused_terminal, nested.clone()),
        );
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.sync_status_focused_cwd(&app.terminal_runtimes);

        let items = app.workspace_git_refresh_items(true);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.resolved_identity_cwd == outer));
        assert!(items
            .iter()
            .any(|item| item.resolved_identity_cwd == nested));
        let output = refresh_workspace_git_statuses(
            items,
            &HashMap::new(),
            Instant::now() + GIT_REFRESH_TIMEOUT,
            Path::new("git"),
        );

        assert!(app
            .state
            .apply_workspace_git_statuses(&app.terminal_runtimes, output.results));
        assert_eq!(
            app.state.workspaces[0].cached_git_branch.as_deref(),
            Some("outer-branch")
        );
        assert_eq!(app.state.status_git_cwd.as_ref(), Some(&nested));
        assert_eq!(
            app.state.status_git_branch.as_deref(),
            Some("nested-branch")
        );

        std::fs::remove_dir_all(outer).expect("remove fixture");
    }

    #[test]
    fn cwd_identity_refresh_runs_once_without_sidebar_git_tokens() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        app.state.active = Some(0);
        let now = Instant::now();

        app.request_git_identity_refresh(now);

        assert!(app.git_refresh_deadline().is_some());
        app.start_git_status_refresh_if_due(now);
        assert!(app.git_refresh_in_flight.is_some());
        assert!(!app.git_identity_refresh_requested);
    }

    #[test]
    fn due_git_refresh_starts_for_status_branch_without_sidebar_consumer() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        app.state.active = Some(0);
        let now = Instant::now();
        app.last_git_remote_status_refresh = now - GIT_REMOTE_STATUS_REFRESH_INTERVAL;

        app.start_git_status_refresh_if_due(now);

        assert!(app.git_refresh_in_flight.is_some());
    }

    // ac: every live pane cwd — across workspaces, tabs, and unfocused panes — is a
    // branch refresh target, deduplicated per (workspace, cwd), while the focused
    // status projection cwd stays included.
    #[test]
    fn status_branch_refresh_targets_every_live_pane_cwd() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);

        let inactive_cwd = PathBuf::from("/repo-inactive");
        let inactive = Workspace::test_new("inactive");
        let inactive_root = inactive.tabs[0].root_pane;
        let inactive_terminal = inactive
            .terminal_id(inactive_root)
            .expect("terminal")
            .clone();
        app.state.terminals.insert(
            inactive_terminal.clone(),
            crate::terminal::TerminalState::new(inactive_terminal, inactive_cwd.clone()),
        );
        let inactive_id = inactive.id.clone();

        let active_cwd = PathBuf::from("/repo-active");
        let unfocused_cwd = PathBuf::from("/repo-unfocused");
        let mut active = Workspace::test_new("active");
        let active_root = active.tabs[0].root_pane;
        let root_terminal = active.terminal_id(active_root).expect("terminal").clone();
        let unfocused = active.test_split(ratatui::layout::Direction::Horizontal);
        let unfocused_terminal = active.terminal_id(unfocused).expect("terminal").clone();
        let other_tab = active.test_add_tab(Some("other-tab"));
        let other_tab_pane = active.tabs[other_tab].root_pane;
        let other_tab_terminal = active
            .terminal_id(other_tab_pane)
            .expect("terminal")
            .clone();
        // Focus back on the root pane so the split pane is live but unfocused.
        active.tabs[0].layout.focus_pane(active_root);
        app.state.terminals.insert(
            root_terminal.clone(),
            crate::terminal::TerminalState::new(root_terminal, active_cwd.clone()),
        );
        app.state.terminals.insert(
            unfocused_terminal.clone(),
            crate::terminal::TerminalState::new(unfocused_terminal, unfocused_cwd.clone()),
        );
        let other_tab_cwd = PathBuf::from("/repo-other-tab");
        app.state.terminals.insert(
            other_tab_terminal.clone(),
            crate::terminal::TerminalState::new(other_tab_terminal, other_tab_cwd.clone()),
        );
        let active_id = active.id.clone();

        app.state.workspaces = vec![inactive, active];
        app.state.active = Some(1);
        app.state.status_focused_cwd = Some(active_cwd.clone());

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 4, "one observation per distinct pane cwd");
        for (workspace_id, cwd) in [
            (&inactive_id, &inactive_cwd),
            (&active_id, &active_cwd),
            (&active_id, &unfocused_cwd),
            (&active_id, &other_tab_cwd),
        ] {
            let item = items
                .iter()
                .find(|item| {
                    &item.workspace_id == workspace_id && &item.resolved_identity_cwd == cwd
                })
                .unwrap_or_else(|| panic!("missing refresh item for {workspace_id} {cwd:?}"));
            assert_eq!(
                item.demand,
                GitStatusRefreshDemand {
                    branch: true,
                    ahead_behind: false,
                }
            );
            assert!(!item.updates_workspace_identity);
        }
    }

    #[test]
    fn disabled_status_bar_without_sidebar_consumer_stops_git_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));

        assert!(app.git_refresh_demand().is_empty());
        assert!(app.git_refresh_deadline().is_none());
    }

    #[test]
    fn visible_git_menu_button_demands_ahead_behind_from_existing_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));
        app.state.view.git_menu_button_hit_area = ratatui::layout::Rect::new(1, 1, 10, 1);

        assert_eq!(
            app.git_refresh_demand(),
            GitStatusRefreshDemand {
                branch: false,
                ahead_behind: true,
            }
        );
        assert!(app.git_refresh_deadline().is_some());
        assert!(app
            .workspace_git_refresh_items(false)
            .iter()
            .all(|item| item.demand.ahead_behind));
    }

    #[test]
    fn disabled_status_bar_keeps_sidebar_git_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Branch]];
        let mut app = test_app(&config);
        app.state.workspaces.push(Workspace::test_new("test"));

        assert_eq!(
            app.git_refresh_demand(),
            GitStatusRefreshDemand {
                branch: true,
                ahead_behind: false,
            }
        );
        assert!(app.git_refresh_deadline().is_some());
    }

    #[test]
    fn disabled_status_bar_sidebar_branch_skips_focused_cwd_refresh_item() {
        let mut config = crate::config::Config::default();
        config.ui.status_bar.enabled = false;
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Branch]];
        let mut app = test_app(&config);
        let outer = PathBuf::from("/repo");
        let nested = outer.join("nested");
        let mut ws = Workspace::test_new("test");
        ws.identity_cwd = outer.clone();
        ws.cached_identity_cwd = outer.clone();
        let root = ws.tabs[0].root_pane;
        let root_terminal = ws.terminal_id(root).expect("root terminal").clone();
        let focused = ws.test_split(ratatui::layout::Direction::Horizontal);
        let focused_terminal = ws.terminal_id(focused).expect("focused terminal").clone();
        app.state.terminals.insert(
            root_terminal.clone(),
            crate::terminal::TerminalState::new(root_terminal, outer.clone()),
        );
        app.state.terminals.insert(
            focused_terminal.clone(),
            crate::terminal::TerminalState::new(focused_terminal, nested),
        );
        app.state.workspaces.push(ws);
        app.state.active = Some(0);

        let items = app.workspace_git_refresh_items(false);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resolved_identity_cwd, outer);
    }

    #[test]
    fn git_refresh_demand_matches_sidebar_rows() {
        let cases = [
            (
                crate::config::SpaceSidebarToken::Workspace,
                GitStatusRefreshDemand {
                    branch: true,
                    ahead_behind: false,
                },
            ),
            (
                crate::config::SpaceSidebarToken::Branch,
                GitStatusRefreshDemand {
                    branch: true,
                    ahead_behind: false,
                },
            ),
            (
                crate::config::SpaceSidebarToken::GitStatus,
                GitStatusRefreshDemand {
                    branch: true,
                    ahead_behind: true,
                },
            ),
        ];

        for (token, expected) in cases {
            let mut config = crate::config::Config::default();
            config.ui.sidebar.spaces.rows = vec![vec![token.clone()]];
            let mut app = test_app(&config);
            app.state.workspaces.push(Workspace::test_new("test"));

            assert_eq!(app.git_refresh_demand(), expected, "token: {token:?}");
            assert!(app.git_refresh_deadline().is_some(), "token: {token:?}");
        }
    }

    #[test]
    fn unnamed_linked_worktree_keeps_status_branch_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        let mut child = Workspace::test_new("test");
        child.custom_name = None;
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo-worktree".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces.push(child);

        assert!(app.git_refresh_deadline().is_some());
    }

    #[test]
    fn custom_named_linked_worktree_keeps_status_branch_refresh() {
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        let mut app = test_app(&config);
        let mut child = Workspace::test_new("custom");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo".into(),
            label: "repo".into(),
            repo_root: "/repo".into(),
            checkout_path: "/repo-worktree".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces.push(child);

        assert!(app.git_refresh_deadline().is_some());
    }

    #[test]
    fn headless_deadline_can_suppress_git_refresh_timer() {
        let mut app = test_app(&crate::config::Config::default());
        app.state.workspaces.push(Workspace::test_new("test"));
        let now = Instant::now();
        app.last_git_remote_status_refresh = now - GIT_REMOTE_STATUS_REFRESH_INTERVAL;

        assert_eq!(
            app.next_headless_loop_deadline_with_client_refresh(now, false, false),
            None
        );
        // The work-context refresh timer starts when App is constructed, so it can be
        // arbitrarily older than this test's `now` under parallel test load. Both
        // deadlines are already due; their exact distance is not part of the contract.
        let deadline = app
            .next_headless_loop_deadline_with_client_refresh(now, false, true)
            .expect("due git refresh should wake the headless loop");
        assert!(
            deadline <= now,
            "deadline {deadline:?} should already be due at {now:?}"
        );
    }

    #[test]
    fn explicit_git_refresh_invalidates_cached_non_git_results() {
        let mut app = test_app(&crate::config::Config::default());
        let cwd = std::env::temp_dir().join(format!("herdr-git-miss-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let (_, entry) = crate::workspace::git_status_snapshot_for_cwd_with_demand(
            &cwd,
            None,
            GitStatusRefreshDemand::ALL,
            Instant::now() + GIT_REFRESH_TIMEOUT,
        );
        app.git_status_cache
            .insert(cwd.clone(), entry.expect("non-Git cache entry"));

        app.mark_git_status_refresh_due(Instant::now());

        assert!(app.git_status_cache.is_empty());
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn git_refresh_due_request_survives_in_flight_refresh() {
        let mut app = test_app(&crate::config::Config::default());
        let now = Instant::now();
        app.test_begin_git_refresh(1);

        app.mark_git_status_refresh_due(now);
        assert!(app.git_refresh_due_after_in_flight);

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 1,
            results: Vec::new(),
            cache_updates: Vec::new(),
            file_fingerprints: Vec::new(),
        });

        assert!(app.git_refresh_in_flight.is_none());
        assert!(!app.git_refresh_due_after_in_flight);
        assert_eq!(app.git_refresh_deadline(), None);

        app.state.workspaces.push(Workspace::test_new("test"));
        let deadline = app
            .git_refresh_deadline()
            .expect("refresh should be due once a workspace exists");
        assert!(deadline <= Instant::now());
    }

    #[test]
    fn git_refresh_invalidates_changed_file_snapshot_for_its_root() {
        let mut app = test_app(&crate::config::Config::default());
        let root = std::env::current_dir().expect("current directory");
        app.state.dock_file_cache.insert(
            root.clone(),
            crate::files::FileTreeSnapshot {
                root: root.clone(),
                files: Vec::new(),
                fingerprint: 1,
            },
        );
        app.test_begin_git_refresh(7);

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            generation: 7,
            results: Vec::new(),
            cache_updates: Vec::new(),
            file_fingerprints: vec![(root.clone(), 2)],
        });

        assert!(!app.state.dock_file_cache.contains_key(&root));
    }

    // ac1: an expired refresh is invalidated, retried, and keeps the last good branch visible.
    #[test]
    fn expired_git_refresh_retries_without_clearing_cached_branch() {
        let mut app = test_app(&crate::config::Config::default());
        let mut workspace = Workspace::test_new("test");
        workspace.cached_git_branch = Some("last-good".into());
        app.state.workspaces.push(workspace);
        app.state.active = Some(0);
        app.test_begin_git_refresh(41);
        let now = Instant::now();
        app.git_refresh_in_flight.as_mut().unwrap().deadline =
            now - std::time::Duration::from_millis(1);

        app.start_git_status_refresh_if_due(now);

        assert_eq!(
            app.git_refresh_in_flight.map(|refresh| refresh.generation),
            Some(42)
        );
        assert_eq!(
            app.state.workspaces[0].cached_git_branch.as_deref(),
            Some("last-good")
        );
    }

    // ac3: a hung refresh-path git descendant is killed at the app deadline; the retry
    // generation proceeds, and a late completion from the invalidated generation cannot
    // replace the last good focused branch.
    #[cfg(unix)]
    #[test]
    fn hung_git_refresh_retries_and_rejects_late_prior_generation() {
        let fixture_dir = std::env::temp_dir().join(format!(
            "herdr-git-refresh-consumer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        let repo = fixture_dir.join("repo");
        let fake_bin = fixture_dir.join("bin");
        std::fs::create_dir_all(repo.join(".git/refs/heads")).expect("create fake repo");
        std::fs::create_dir_all(repo.join(".git/refs/remotes/origin"))
            .expect("create fake upstream refs");
        std::fs::create_dir_all(&fake_bin).expect("create fake git bin");
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/hung\n").expect("write fake HEAD");
        std::fs::write(
            repo.join(".git/refs/heads/hung"),
            "1111111111111111111111111111111111111111\n",
        )
        .expect("write fake branch ref");
        std::fs::write(
            repo.join(".git/refs/remotes/origin/hung"),
            "2222222222222222222222222222222222222222\n",
        )
        .expect("write fake upstream ref");
        std::fs::write(
            repo.join(".git/config"),
            "[branch \"hung\"]\n\tremote = origin\n\tmerge = refs/heads/hung\n",
        )
        .expect("write fake git config");

        let state_file = fake_bin.join("invocations");
        let shell_pid_file = fake_bin.join("shell-pid");
        let descendant_pid_file = fake_bin.join("descendant-pid");
        write_executable(
            &fake_bin.join("git"),
            &format!(
                "#!/bin/sh\ncount=0\nif test -f '{}'; then count=$(cat '{}'); fi\ncount=$((count + 1))\nprintf '%s' \"$count\" > '{}'\nif test \"$count\" -eq 1; then\n  printf '%s' \"$$\" > '{}'\n  sleep 30 &\n  printf '%s' \"$!\" > '{}'\n  wait\nelse\n  printf '0\\t0\\n'\nfi\n",
                state_file.display(),
                state_file.display(),
                state_file.display(),
                shell_pid_file.display(),
                descendant_pid_file.display(),
            ),
        );
        let mut config = crate::config::Config::default();
        config.ui.sidebar.spaces.rows = vec![vec![crate::config::SpaceSidebarToken::GitStatus]];
        let mut app = test_app(&config);
        app.set_test_git_program(fake_bin.join("git"));
        let mut workspace = Workspace::test_new("consumer");
        workspace.identity_cwd = repo.clone();
        workspace.cached_identity_cwd = repo.clone();
        workspace.cached_git_status_key = repo.clone();
        workspace.cached_git_branch = Some("last-good".into());
        let root_pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(root_pane)
            .expect("workspace root terminal")
            .clone();
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id, repo.clone()),
        );
        app.state.workspaces.push(workspace);
        app.state.active = Some(0);
        app.state.status_bar_enabled = true;
        app.state.sync_status_focused_cwd(&app.terminal_runtimes);
        app.state.status_git_cwd = Some(repo.clone());
        app.state.status_git_branch = Some("last-good".into());

        let started_at = Instant::now();
        app.last_git_remote_status_refresh = started_at
            .checked_sub(GIT_REMOTE_STATUS_REFRESH_INTERVAL)
            .expect("refresh interval fits");
        app.start_git_status_refresh_if_due(started_at);
        let first_generation = app
            .git_refresh_in_flight
            .expect("first refresh started")
            .generation;
        let _ = wait_for_file(&shell_pid_file, "first fake git shell pid");
        let _ = wait_for_file(&descendant_pid_file, "first fake git descendant pid");

        let first_deadline = app
            .git_refresh_in_flight
            .expect("first refresh remains in flight")
            .deadline;
        while Instant::now() < first_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        app.start_git_status_refresh_if_due(Instant::now());
        let second_generation = app
            .git_refresh_in_flight
            .expect("retry refresh started")
            .generation;
        assert!(second_generation > first_generation);
        assert_eq!(
            app.state.status_git_branch.as_deref(),
            Some("last-good"),
            "deadline expiry must preserve the focused branch"
        );

        let shell_pid = std::fs::read_to_string(&shell_pid_file).expect("read shell pid");
        let descendant_pid =
            std::fs::read_to_string(&descendant_pid_file).expect("read descendant pid");
        assert_pid_dead(shell_pid.trim(), "hung fake git shell");
        assert_pid_dead(descendant_pid.trim(), "hung fake git descendant");
        let _invocation_count = wait_for_invocation_count(&state_file, 2);

        let event_deadline = Instant::now() + Duration::from_secs(3);
        let mut refresh_events = Vec::new();
        while refresh_events.len() < 2 && Instant::now() < event_deadline {
            match app.event_rx.try_recv() {
                Ok(event) if matches!(&event, AppEvent::GitStatusRefreshed { .. }) => {
                    refresh_events.push(event);
                }
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("refresh event channel closed: {error}"),
            }
        }
        assert_eq!(refresh_events.len(), 2, "both generations must complete");
        refresh_events.sort_by_key(|event| match event {
            AppEvent::GitStatusRefreshed { generation, .. } => *generation,
            _ => u64::MAX,
        });
        let old_event = refresh_events.remove(0);
        let new_event = refresh_events.remove(0);
        assert!(matches!(
            &old_event,
            AppEvent::GitStatusRefreshed { generation, .. } if *generation == first_generation
        ));
        assert!(matches!(
            &new_event,
            AppEvent::GitStatusRefreshed { generation, .. } if *generation == second_generation
        ));

        app.handle_internal_event_with_render_impact(old_event);
        assert_eq!(
            app.state.workspaces[0].cached_git_branch.as_deref(),
            Some("last-good")
        );
        assert_eq!(app.state.status_git_branch.as_deref(), Some("last-good"));

        app.handle_internal_event_with_render_impact(new_event);
        assert!(app.git_refresh_in_flight.is_none());

        std::fs::remove_dir_all(fixture_dir).expect("remove consumer fixture");
    }

    fn test_app(config: &crate::config::Config) -> super::super::App {
        super::super::App::new(
            config,
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }
}
