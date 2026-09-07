use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::home::{AddProjectCloneRequest, AddProjectState, AddProjectTab, NO_SUCH_DIRECTORY};
use super::{App, AppState};
use crate::events::AppEvent;

const GITHUB_LIST_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) fn repo_name_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let tail = trimmed.rsplit(['/', ':']).next()?.trim_end_matches(".git");
    (!tail.is_empty() && tail != "." && tail != "..").then(|| tail.to_string())
}

fn github_owner_and_filter(query: &str) -> (&str, &str) {
    query
        .split_once('/')
        .map_or(("", query), |(owner, filter)| (owner.trim(), filter))
}

fn output_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("gh failed with status {}", output.status)
    }
}

pub(crate) fn list_github_repos(gh_program: &Path, owner: &str) -> Result<Vec<String>, String> {
    let mut command = crate::noninteractive_process::command(gh_program);
    command.arg("repo").arg("list");
    if !owner.is_empty() {
        command.arg(owner);
    }
    command
        .arg("--limit")
        .arg("100")
        .arg("--json")
        .arg("nameWithOwner");
    let output = crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + GITHUB_LIST_TIMEOUT,
        1024 * 1024,
    )
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "GitHub CLI not found. Install gh and sign in.".to_string(),
        _ => format!("GitHub repositories could not be loaded: {error}"),
    })?;
    if !output.status.success() {
        return Err(output_error(&output));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh returned invalid repository data: {error}"))?;
    let mut repos = value
        .as_array()
        .ok_or_else(|| "gh returned invalid repository data".to_string())?
        .iter()
        .filter_map(|item| {
            item.get("nameWithOwner")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    repos.sort_by_key(|repo| repo.to_ascii_lowercase());
    repos.dedup();
    Ok(repos)
}

impl AddProjectState {
    pub(crate) fn github_matches(&self) -> Vec<(usize, &str)> {
        self.github_filter.matches(&self.github_repos)
    }

    fn schedule_github_refresh(&mut self, owner: &str) {
        self.github_owner.clear();
        self.github_owner.push_str(owner);
        self.github_repos.clear();
        self.github_filter.selected = 0;
        self.github_loading = true;
        self.github_refresh_request = Some(owner.to_string());
        self.error = None;
    }

    pub(crate) fn select_tab(&mut self, tab: AddProjectTab) {
        self.tab = tab;
        self.error = None;
        if tab == AddProjectTab::GitHub && self.github_repos.is_empty() && !self.github_loading {
            let owner = self.github_owner.clone();
            self.schedule_github_refresh(&owner);
        }
    }

    fn move_tab(&mut self, delta: i32) {
        let current = AddProjectTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or_default();
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            current.saturating_add(delta as usize)
        }
        .min(AddProjectTab::ALL.len().saturating_sub(1));
        self.select_tab(AddProjectTab::ALL[next]);
    }

    fn update_github_filter(&mut self) {
        let (owner, filter) = github_owner_and_filter(&self.github_query);
        let owner = owner.to_string();
        let filter = filter.to_string();
        self.github_filter.set_query(&filter);
        if owner != self.github_owner {
            self.schedule_github_refresh(&owner);
        }
    }

    pub(crate) fn push_text(&mut self, text: &str) {
        if self.clone_pending {
            return;
        }
        match self.tab {
            AddProjectTab::LocalFolder => {
                for character in text.chars().filter(|character| !character.is_control()) {
                    self.browse.push_filter(character);
                }
            }
            AddProjectTab::GitUrl => self
                .git_url
                .extend(text.chars().filter(|character| !character.is_control())),
            AddProjectTab::GitHub => {
                self.github_query
                    .extend(text.chars().filter(|character| !character.is_control()));
                self.update_github_filter();
            }
        }
        self.error = None;
    }

    fn pop(&mut self) {
        if self.clone_pending {
            return;
        }
        match self.tab {
            AddProjectTab::LocalFolder => self.browse.backspace(),
            AddProjectTab::GitUrl => {
                self.git_url.pop();
            }
            AddProjectTab::GitHub => {
                self.github_query.pop();
                self.update_github_filter();
            }
        }
        self.error = None;
    }
}

impl AppState {
    pub(crate) fn add_project_active(&self) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| home.add_project.is_some())
    }

    pub(crate) fn close_add_project(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.add_project = None;
        }
    }

    pub(crate) fn add_project_select_tab(&mut self, tab: AddProjectTab) {
        if let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.select_tab(tab);
        }
    }

    pub(crate) fn add_project_select_row(&mut self, index: usize) {
        let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        else {
            return;
        };
        match project.tab {
            AddProjectTab::LocalFolder => project.browse.open_entry(index),
            AddProjectTab::GitHub => project.github_filter.selected = index,
            AddProjectTab::GitUrl => {}
        }
    }

    pub(crate) fn add_project_jump_to_breadcrumb(&mut self, directory: PathBuf) {
        if let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.browse.jump_to(directory);
        }
    }

    pub(crate) fn add_project_toggle_hidden(&mut self) {
        if let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.browse.toggle_hidden();
        }
    }

    pub(crate) fn add_project_scroll(&mut self, delta: i32) {
        let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        else {
            return;
        };
        match project.tab {
            AddProjectTab::LocalFolder => project.browse.move_selection(delta),
            AddProjectTab::GitHub => {
                let count = project.github_matches().len();
                project.github_filter.move_selection(delta, count);
            }
            AddProjectTab::GitUrl => {}
        }
    }

    pub(crate) fn add_project_clone_root(&self) -> PathBuf {
        let configured = self.add_project_start_dir.trim();
        if !configured.is_empty() {
            return crate::worktree::expand_tilde_path(configured);
        }
        self.worktree_directory
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.worktree_directory.clone())
    }

    fn queue_add_project_clone(&mut self, url: String) {
        let Some(name) = repo_name_from_url(&url) else {
            if let Some(project) = self
                .home
                .as_mut()
                .and_then(|home| home.add_project.as_mut())
            {
                project.error = Some("Enter a Git repository URL.".into());
            }
            return;
        };
        let target = self.add_project_clone_root().join(name);
        if target.exists() {
            if let Some(project) = self
                .home
                .as_mut()
                .and_then(|home| home.add_project.as_mut())
            {
                project.error = Some(format!("{} already exists", target.display()));
            }
            return;
        }
        if let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.clone_request = Some(AddProjectCloneRequest { url, target });
            project.clone_pending = true;
            project.error = None;
        }
    }

    pub(crate) fn accept_add_project(&mut self) {
        let Some(tab) = self
            .home
            .as_ref()
            .and_then(|home| home.add_project.as_ref())
            .map(|project| project.tab)
        else {
            return;
        };
        match tab {
            AddProjectTab::LocalFolder => {
                let path = self
                    .home
                    .as_ref()
                    .and_then(|home| home.add_project.as_ref())
                    .map(|project| project.browse.path());
                let Some(path) = path else {
                    return;
                };
                if !path.is_dir() {
                    if let Some(project) = self
                        .home
                        .as_mut()
                        .and_then(|home| home.add_project.as_mut())
                    {
                        project.error = Some(NO_SUCH_DIRECTORY.into());
                    }
                    return;
                }
                self.home_set_directory(crate::worktree::canonical_or_original(&path));
                self.close_add_project();
            }
            AddProjectTab::GitUrl => {
                let url = self
                    .home
                    .as_ref()
                    .and_then(|home| home.add_project.as_ref())
                    .map(|project| project.git_url.trim().to_string())
                    .unwrap_or_default();
                self.queue_add_project_clone(url);
            }
            AddProjectTab::GitHub => {
                let repo = self
                    .home
                    .as_ref()
                    .and_then(|home| home.add_project.as_ref())
                    .and_then(|project| {
                        project
                            .github_matches()
                            .get(project.github_filter.selected)
                            .map(|(_, repo)| (*repo).to_string())
                    });
                if let Some(repo) = repo {
                    self.queue_add_project_clone(format!("https://github.com/{repo}.git"));
                }
            }
        }
    }

    pub(crate) fn finish_add_project_clone(&mut self, target: PathBuf, succeeded: bool) {
        if succeeded {
            self.home_set_directory(target);
            self.close_add_project();
            return;
        }
        if let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        {
            project.clone_pending = false;
            project.error = Some("Clone failed. Check the bottom pane, then retry.".into());
        }
    }

    pub(crate) fn handle_add_project_key(&mut self, key: KeyEvent) {
        let Some(project) = self
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.close_add_project(),
            KeyCode::Left if key.modifiers.is_empty() => project.move_tab(-1),
            KeyCode::Right if key.modifiers.is_empty() => project.move_tab(1),
            KeyCode::Up if key.modifiers.is_empty() => match project.tab {
                AddProjectTab::GitHub => {
                    let count = project.github_matches().len();
                    project.github_filter.move_selection(-1, count);
                }
                AddProjectTab::LocalFolder => project.browse.move_selection(-1),
                AddProjectTab::GitUrl => {}
            },
            KeyCode::Down if key.modifiers.is_empty() => match project.tab {
                AddProjectTab::GitHub => {
                    let count = project.github_matches().len();
                    project.github_filter.move_selection(1, count);
                }
                AddProjectTab::LocalFolder => project.browse.move_selection(1),
                AddProjectTab::GitUrl => {}
            },
            KeyCode::Backspace if key.modifiers.is_empty() => project.pop(),
            KeyCode::Char('.') if key.modifiers.is_empty() => match project.tab {
                AddProjectTab::LocalFolder => project.browse.toggle_hidden(),
                _ => project.push_text("."),
            },
            KeyCode::Char(' ')
                if key.modifiers.is_empty()
                    && !project.clone_pending
                    && project.tab == AddProjectTab::LocalFolder =>
            {
                self.accept_add_project();
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                project.push_text(&character.to_string());
            }
            KeyCode::Enter
                if key.modifiers.is_empty()
                    && !project.clone_pending
                    && project.tab == AddProjectTab::LocalFolder =>
            {
                project.browse.open_selected();
            }
            KeyCode::Enter if key.modifiers.is_empty() && !project.clone_pending => {
                self.accept_add_project();
            }
            _ => {}
        }
    }
}

impl App {
    pub(crate) fn start_home_github_refresh_if_requested(&mut self) {
        let request = self
            .state
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
            .and_then(|project| project.github_refresh_request.take());
        let Some(owner) = request else {
            return;
        };
        let gh_program = self.work_index_gh_program();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = list_github_repos(&gh_program, &owner);
            let _ = event_tx.blocking_send(AppEvent::HomeGithubReposRefreshed { owner, result });
        });
    }

    pub(crate) fn handle_home_github_repos_refreshed(
        &mut self,
        owner: String,
        result: Result<Vec<String>, String>,
    ) -> bool {
        let Some(project) = self
            .state
            .home
            .as_mut()
            .and_then(|home| home.add_project.as_mut())
            .filter(|project| project.github_owner == owner)
        else {
            return false;
        };
        project.github_loading = false;
        match result {
            Ok(repos) => {
                project.github_repos = repos;
                project.github_filter.selected = 0;
                project.error = None;
            }
            Err(error) => {
                project.github_repos.clear();
                project.error = Some(error);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::home::FolderBrowser;

    fn folder_fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-folder-browser-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpha/.git")).expect("alpha repository");
        std::fs::create_dir_all(root.join(".secret")).expect("hidden directory");
        std::fs::write(
            root.join("alpha/.git/HEAD"),
            "ref: refs/heads/feature/f24\n",
        )
        .expect("git head");
        std::fs::write(root.join("notes.txt"), "fixture").expect("file row");
        root
    }

    #[cfg(unix)]
    fn fake_program(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("herdr-add-project-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fake program directory");
        let program = root.join(name);
        std::fs::write(&program, format!("#!/bin/sh\n{body}\n")).expect("fake program");
        let mut permissions = std::fs::metadata(&program).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("executable");
        program
    }

    #[cfg(unix)]
    #[test]
    fn github_repo_list_uses_injected_gh_and_owner() {
        let gh = fake_program(
            "gh",
            "test \"$1 $2 $3 $4 $5 $6 $7\" = \"repo list acme --limit 100 --json nameWithOwner\" || exit 9\nprintf '[{\"nameWithOwner\":\"acme/zeta\"},{\"nameWithOwner\":\"acme/alpha\"}]'",
        );

        let repos = list_github_repos(&gh, "acme").expect("fixture gh succeeds");

        assert_eq!(repos, ["acme/alpha", "acme/zeta"]);
    }

    #[test]
    fn github_repo_list_reports_missing_injected_gh() {
        let error = list_github_repos(Path::new("/missing/herdr-gh"), "")
            .expect_err("missing gh is inline failure data");

        assert!(error.contains("GitHub CLI not found"));
    }

    #[test]
    fn owner_prefix_switches_source_and_keeps_filter_after_slash() {
        let mut project = AddProjectState::starting_at(Path::new("/tmp"));
        project.select_tab(AddProjectTab::GitHub);
        project.github_repos = vec!["self/one".into()];
        project.github_loading = false;
        project.push_text("acme/pro");

        assert_eq!(project.github_owner, "acme");
        assert_eq!(project.github_filter.query, "pro");
        assert_eq!(project.github_refresh_request.as_deref(), Some("acme"));
    }

    #[test]
    fn clone_destination_uses_config_then_worktree_parent() {
        let mut state = AppState::test_new();
        state.add_project_start_dir = "~/Repos".into();
        assert_eq!(
            state.add_project_clone_root(),
            crate::worktree::expand_tilde_path("~/Repos")
        );

        state.add_project_start_dir.clear();
        state.worktree_directory = PathBuf::from("/var/tmp/herdr-worktrees");
        assert_eq!(state.add_project_clone_root(), PathBuf::from("/var/tmp"));
    }

    #[test]
    fn repository_name_accepts_https_and_ssh_clone_urls() {
        assert_eq!(
            repo_name_from_url("https://github.com/acme/project.git"),
            Some("project".into())
        );
        assert_eq!(
            repo_name_from_url("git@github.com:acme/project.git"),
            Some("project".into())
        );
    }

    #[test]
    fn folder_filter_updates_immediately_and_keeps_dim_file_candidates() {
        let root = folder_fixture("filter");
        let mut browser = FolderBrowser::starting_at(&root);

        browser.push_filter('n');

        assert_eq!(browser.filter, "n");
        assert_eq!(browser.entries.len(), 1);
        assert_eq!(browser.entries[0].name, "notes.txt");
        assert!(!browser.entries[0].is_dir);
    }

    #[test]
    fn folder_hidden_toggle_and_git_branch_are_projected_into_rows() {
        let root = folder_fixture("hidden-git");
        let mut browser = FolderBrowser::starting_at(&root);

        assert!(!browser.entries.iter().any(|entry| entry.name == ".secret"));
        assert_eq!(
            browser
                .entries
                .iter()
                .find(|entry| entry.name == "alpha")
                .and_then(|entry| entry.branch.as_deref()),
            Some("feature/f24")
        );

        browser.toggle_hidden();

        assert!(browser.entries.iter().any(|entry| entry.name == ".secret"));
    }

    #[test]
    fn folder_navigation_opens_rows_and_backspace_moves_to_parent() {
        let root = folder_fixture("navigation");
        let mut browser = FolderBrowser::starting_at(&root);
        let alpha = browser
            .entries
            .iter()
            .position(|entry| entry.name == "alpha")
            .expect("alpha row");

        browser.open_entry(alpha);
        assert_eq!(browser.directory, root.join("alpha"));

        browser.backspace();
        assert_eq!(browser.directory, root);
    }

    #[test]
    fn space_selects_the_current_folder() {
        let root = folder_fixture("select");
        let mut state = AppState::test_new();
        let mut home = super::super::home::HomeState::default();
        home.add_project = Some(AddProjectState::starting_at(&root));
        state.home = Some(home);

        state.handle_add_project_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        let home = state.home.as_ref().expect("home remains open");
        assert_eq!(
            home.directory,
            crate::worktree::canonical_or_original(&root)
        );
        assert!(home.add_project.is_none());
    }

    #[test]
    fn closing_modal_preserves_composer_prompt() {
        let mut state = AppState::test_new();
        let mut home = super::super::home::HomeState::default();
        home.prompt = "keep this exact prompt".into();
        home.add_project = Some(AddProjectState::starting_at(Path::new("/tmp")));
        state.home = Some(home);

        state.close_add_project();

        assert_eq!(
            state.home.as_ref().map(|home| home.prompt.as_str()),
            Some("keep this exact prompt")
        );
    }

    #[test]
    fn escape_closes_modal_without_closing_home() {
        let mut state = AppState::test_new();
        let mut home = super::super::home::HomeState::default();
        home.add_project = Some(AddProjectState::starting_at(Path::new("/tmp")));
        state.home = Some(home);

        state.handle_add_project_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(state.home.is_some());
        assert!(!state.add_project_active());
    }

    #[test]
    fn failed_clone_keeps_url_and_composer_prompt_for_retry() {
        let mut state = AppState::test_new();
        let mut home = super::super::home::HomeState::default();
        home.prompt = "keep prompt".into();
        let mut project = AddProjectState::starting_at(Path::new("/tmp"));
        project.tab = AddProjectTab::GitUrl;
        project.git_url = "https://example.invalid/acme/project.git".into();
        project.clone_pending = true;
        home.add_project = Some(project);
        state.home = Some(home);

        state.finish_add_project_clone(PathBuf::from("/tmp/project"), false);

        let home = state.home.as_ref().expect("composer stays open");
        let project = home.add_project.as_ref().expect("modal stays open");
        assert_eq!(home.prompt, "keep prompt");
        assert_eq!(project.git_url, "https://example.invalid/acme/project.git");
        assert!(!project.clone_pending);
        assert!(project
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Clone failed")));
    }

    #[test]
    fn successful_clone_selects_directory_without_changing_prompt() {
        let mut state = AppState::test_new();
        let target = std::env::temp_dir();
        let mut home = super::super::home::HomeState::default();
        home.prompt = "keep prompt".into();
        home.add_project = Some(AddProjectState::starting_at(Path::new("/tmp")));
        state.home = Some(home);

        state.finish_add_project_clone(target.clone(), true);

        let home = state.home.as_ref().expect("composer stays open");
        assert_eq!(home.directory, target);
        assert_eq!(home.prompt, "keep prompt");
        assert!(home.add_project.is_none());
    }
}
