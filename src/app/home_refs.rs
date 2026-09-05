use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::App;
use crate::events::AppEvent;

const HOME_REF_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);
const HOME_CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeRefTag {
    Current,
    Worktree,
    Remote,
}

impl HomeRefTag {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Worktree => "worktree",
            Self::Remote => "remote",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeRef {
    pub(crate) name: String,
    pub(crate) oid: String,
    pub(crate) tag: Option<HomeRefTag>,
}

impl HomeRef {
    pub(crate) fn display_label(&self) -> String {
        self.tag
            .map(|tag| format!("{}  {}", self.name, tag.label()))
            .unwrap_or_else(|| self.name.clone())
    }

    pub(crate) fn is_current(&self) -> bool {
        self.tag == Some(HomeRefTag::Current)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedRef {
    name: String,
    oid: String,
    remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedWorktree {
    path: PathBuf,
    branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HomeRefCacheEntry {
    refs: Vec<CachedRef>,
    worktrees: Vec<CachedWorktree>,
}

impl HomeRefCacheEntry {
    pub(crate) fn rows_for_directory(&self, directory: &Path) -> Vec<HomeRef> {
        let current_branch = self
            .worktrees
            .iter()
            .filter(|worktree| directory.starts_with(&worktree.path))
            .max_by_key(|worktree| worktree.path.components().count())
            .and_then(|worktree| worktree.branch.as_deref());
        let worktree_branches = self
            .worktrees
            .iter()
            .filter_map(|worktree| worktree.branch.as_deref())
            .collect::<HashSet<_>>();

        let row = |item: &CachedRef| HomeRef {
            name: item.name.clone(),
            oid: item.oid.clone(),
            tag: if !item.remote && current_branch == Some(item.name.as_str()) {
                Some(HomeRefTag::Current)
            } else if !item.remote && worktree_branches.contains(item.name.as_str()) {
                Some(HomeRefTag::Worktree)
            } else if item.remote {
                Some(HomeRefTag::Remote)
            } else {
                None
            },
        };

        self.refs
            .iter()
            .filter(|item| !item.remote && current_branch == Some(item.name.as_str()))
            .chain(
                self.refs
                    .iter()
                    .filter(|item| !item.remote && current_branch != Some(item.name.as_str())),
            )
            .chain(self.refs.iter().filter(|item| item.remote))
            .map(row)
            .collect()
    }
}

pub(crate) fn parse_ref_cache(
    for_each_ref: &str,
    worktree_list: &str,
    remotes: &str,
) -> HomeRefCacheEntry {
    let remote_names = remotes
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let remote_names = remote_names.map(str::to_string).collect::<Vec<_>>();
    let remote_prefixes = remote_names
        .iter()
        .map(|name| format!("{name}/"))
        .collect::<Vec<_>>();
    let refs = for_each_ref
        .lines()
        .filter_map(|line| {
            let (name, oid) = line.split_once('\t')?;
            let name = name.trim();
            let oid = oid.trim();
            (!name.is_empty() && !oid.is_empty()).then(|| CachedRef {
                remote: remote_names.iter().any(|remote| name == remote)
                    || remote_prefixes
                        .iter()
                        .any(|prefix| name.starts_with(prefix)),
                name: name.to_string(),
                oid: oid.to_string(),
            })
        })
        .collect();

    let mut worktrees = Vec::new();
    let mut path = None;
    let mut branch = None;
    for line in worktree_list.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = path.take() {
                worktrees.push(CachedWorktree {
                    path,
                    branch: branch.take(),
                });
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
            branch = Some(value.to_string());
        }
    }

    HomeRefCacheEntry { refs, worktrees }
}

fn output_error(action: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("{action} failed with status {}", output.status)
    }
}

fn run_git_output(
    git_program: &Path,
    repo_root: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<String, String> {
    let mut command = crate::noninteractive_process::command(git_program);
    command.arg("-C").arg(repo_root).args(args);
    let output =
        crate::noninteractive_process::output_with_deadline_limited(command, deadline, 1024 * 1024)
            .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(output_error("git", &output));
    }
    String::from_utf8(output.stdout).map_err(|_| "git returned non-UTF-8 output".to_string())
}

fn refresh_home_refs(repo_root: &Path, git_program: &Path) -> Result<HomeRefCacheEntry, String> {
    let deadline = Instant::now() + HOME_REF_REFRESH_TIMEOUT;
    let refs = run_git_output(
        git_program,
        repo_root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(objectname:short)",
            "refs/heads",
            "refs/remotes",
        ],
        deadline,
    )?;
    let worktrees = run_git_output(
        git_program,
        repo_root,
        &["worktree", "list", "--porcelain"],
        deadline,
    )?;
    let remotes = run_git_output(git_program, repo_root, &["remote"], deadline)?;
    Ok(parse_ref_cache(&refs, &worktrees, &remotes))
}

trait HomeGitRunner {
    fn run(&mut self, directory: &Path, args: &[String]) -> Result<Vec<u8>, String>;
}

struct ProcessHomeGitRunner {
    git_program: PathBuf,
    deadline: Instant,
}

impl HomeGitRunner for ProcessHomeGitRunner {
    fn run(&mut self, directory: &Path, args: &[String]) -> Result<Vec<u8>, String> {
        let mut command = crate::noninteractive_process::command(&self.git_program);
        command.arg("-C").arg(directory).args(args);
        let output = crate::noninteractive_process::output_with_deadline_limited(
            command,
            self.deadline,
            1024 * 1024,
        )
        .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(output_error("git", &output));
        }
        Ok(output.stdout)
    }
}

fn prepare_current_checkout(
    runner: &mut impl HomeGitRunner,
    directory: &Path,
    git_ref: &HomeRef,
) -> Result<(), String> {
    let status = runner.run(directory, &["status".into(), "--porcelain".into()])?;
    if !status.is_empty() {
        return Err("working tree is dirty, commit or stash first".into());
    }

    let checkout_args = if git_ref.tag == Some(HomeRefTag::Remote) {
        let short = git_ref
            .name
            .split_once('/')
            .map(|(_, short)| short)
            .unwrap_or(git_ref.name.as_str());
        vec![
            "checkout".into(),
            "-b".into(),
            short.into(),
            "--track".into(),
            git_ref.name.clone(),
        ]
    } else {
        vec!["checkout".into(), git_ref.name.clone()]
    };
    runner.run(directory, &checkout_args)?;
    Ok(())
}

impl App {
    pub(crate) fn start_home_ref_refresh_if_requested(&mut self) {
        let Some(repo_root) = self.state.request_home_ref_refresh.take() else {
            return;
        };
        if !self.home_ref_refreshes_in_flight.insert(repo_root.clone()) {
            return;
        }

        let event_tx = self.event_tx.clone();
        let git_program = self.git_program_for_refresh();
        std::thread::spawn(move || {
            let result = refresh_home_refs(&repo_root, &git_program);
            let _ = event_tx.blocking_send(AppEvent::HomeRefsRefreshed { repo_root, result });
        });
    }

    pub(crate) fn handle_home_refs_refreshed(
        &mut self,
        repo_root: PathBuf,
        result: Result<HomeRefCacheEntry, String>,
    ) -> bool {
        self.home_ref_refreshes_in_flight.remove(&repo_root);
        match result {
            Ok(entry) => {
                let changed = self.state.home_ref_cache.get(&repo_root) != Some(&entry);
                let selected_before = self
                    .state
                    .home
                    .as_ref()
                    .and_then(|home| home.selected_ref.clone());
                self.state.home_ref_cache.insert(repo_root.clone(), entry);
                self.state.sync_home_ref_selection(&repo_root);
                let selection_changed = self
                    .state
                    .home
                    .as_ref()
                    .and_then(|home| home.selected_ref.clone())
                    != selected_before;
                let mut cleared_error = false;
                if let Some(home) = self.state.home.as_mut().filter(|home| {
                    home.ref_repo_root.as_ref() == Some(&repo_root)
                        && home
                            .dispatch_error
                            .as_deref()
                            .is_some_and(|error| error.starts_with("failed to load refs:"))
                }) {
                    home.dispatch_error = None;
                    cleared_error = true;
                }
                changed || selection_changed || cleared_error
            }
            Err(error) => {
                tracing::warn!(repo_root = %repo_root.display(), error = %error, "home ref refresh failed");
                if self
                    .state
                    .home
                    .as_ref()
                    .is_some_and(|home| home.ref_repo_root.as_ref() == Some(&repo_root))
                {
                    if let Some(home) = self.state.home.as_mut() {
                        home.dispatch_error = Some(format!("failed to load refs: {error}"));
                    }
                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn start_home_checkout(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
    ) -> Result<(), String> {
        if self
            .state
            .home
            .as_ref()
            .is_some_and(|home| home.pending_dispatch.is_some())
        {
            return Err("checkout is already running".into());
        }
        let Some(git_ref) = plan.git_ref.clone().filter(|git_ref| !git_ref.is_current()) else {
            return self
                .dispatch_home_composer(plan)
                .map_err(|error| error.to_string());
        };

        if let Some(home) = self.state.home.as_mut() {
            home.pending_dispatch = Some(plan.clone());
            home.dispatch_error = None;
            home.picker = None;
        }
        let event_tx = self.event_tx.clone();
        let git_program = self.git_program_for_refresh();
        std::thread::spawn(move || {
            let mut runner = ProcessHomeGitRunner {
                git_program,
                deadline: Instant::now() + HOME_CHECKOUT_TIMEOUT,
            };
            let result = prepare_current_checkout(&mut runner, &plan.directory, &git_ref);
            let _ = event_tx.blocking_send(AppEvent::HomeCheckoutFinished {
                plan: Box::new(plan),
                result,
            });
        });
        Ok(())
    }

    pub(crate) fn handle_home_checkout_finished(
        &mut self,
        plan: crate::app::home::HomeDispatchPlan,
        result: Result<(), String>,
    ) -> bool {
        let pending_matches = self
            .state
            .home
            .as_ref()
            .and_then(|home| home.pending_dispatch.as_ref())
            == Some(&plan);
        if !pending_matches {
            return false;
        }
        if let Some(home) = self.state.home.as_mut() {
            home.pending_dispatch = None;
        }
        match result {
            Ok(()) => match self.dispatch_home_composer(plan) {
                Ok(()) => {
                    self.state.clear_home();
                    self.state.mode = super::Mode::Terminal;
                }
                Err(error) => {
                    if let Some(home) = self.state.home.as_mut() {
                        home.dispatch_error = Some(error.to_string());
                    }
                }
            },
            Err(error) => {
                if let Some(home) = self.state.home.as_mut() {
                    home.dispatch_error = Some(error);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_outputs_assign_tags_and_sort_current_local_then_remote() {
        let refs = concat!(
            "feature/recent\t2222222\n",
            "origin/feature/remote\t3333333\n",
            "main\t1111111\n",
            "linked\t4444444\n",
        );
        let worktrees = concat!(
            "worktree /repo/main\n",
            "HEAD 1111111111111111111111111111111111111111\n",
            "branch refs/heads/main\n\n",
            "worktree /repo/linked\n",
            "HEAD 4444444444444444444444444444444444444444\n",
            "branch refs/heads/linked\n\n",
        );

        let cache = parse_ref_cache(refs, worktrees, "origin\n");
        let rows = cache.rows_for_directory(Path::new("/repo/main/subdirectory"));

        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.tag))
                .collect::<Vec<_>>(),
            vec![
                ("main", Some(HomeRefTag::Current)),
                ("feature/recent", None),
                ("linked", Some(HomeRefTag::Worktree)),
                ("origin/feature/remote", Some(HomeRefTag::Remote)),
            ]
        );
    }

    #[test]
    fn ref_filter_fuzzy_matches_names_through_dropdown_widget() {
        let refs = concat!(
            "main\t1111111\n",
            "feature/ref-picker\t2222222\n",
            "origin/release\t3333333\n",
        );
        let cache = parse_ref_cache(refs, "", "origin\n");
        let names = cache
            .rows_for_directory(Path::new("/repo"))
            .into_iter()
            .map(|git_ref| git_ref.name)
            .collect::<Vec<_>>();

        assert_eq!(
            crate::ui::dropdown::filter_items(&names, "ftrp")
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>(),
            vec!["feature/ref-picker"]
        );
    }

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<Vec<String>>,
        status: Vec<u8>,
    }

    impl HomeGitRunner for RecordingRunner {
        fn run(&mut self, _directory: &Path, args: &[String]) -> Result<Vec<u8>, String> {
            self.calls.push(args.to_vec());
            Ok(if args.first().is_some_and(|arg| arg == "status") {
                self.status.clone()
            } else {
                Vec::new()
            })
        }
    }

    #[test]
    fn dirty_tree_refusal_keeps_prompt_and_checkout_uninvoked() {
        let mut runner = RecordingRunner {
            status: b" M src/main.rs\n".to_vec(),
            ..RecordingRunner::default()
        };
        let git_ref = HomeRef {
            name: "feature/topic".into(),
            oid: "1234567".into(),
            tag: None,
        };

        let result = prepare_current_checkout(&mut runner, Path::new("/repo"), &git_ref);
        assert_eq!(
            result,
            Err("working tree is dirty, commit or stash first".into())
        );
        assert_eq!(
            runner.calls,
            vec![vec!["status".to_string(), "--porcelain".to_string()]],
            "dirty status must prevent checkout"
        );

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut home = crate::app::home::HomeState::test_with_prompt("keep this prompt");
        home.selected_ref = Some(git_ref);
        let plan = home.dispatch_plan().expect("fixed prompt dispatches");
        home.pending_dispatch = Some(plan.clone());
        app.state.home = Some(home);

        assert!(app.handle_home_checkout_finished(plan, result));
        let home = app
            .state
            .home
            .as_ref()
            .expect("dirty refusal keeps Home open");
        assert_eq!(home.prompt, "keep this prompt");
        assert_eq!(
            home.dispatch_error.as_deref(),
            Some("working tree is dirty, commit or stash first")
        );
    }

    #[test]
    fn remote_ref_checkout_creates_tracking_branch() {
        let mut runner = RecordingRunner::default();
        let git_ref = HomeRef {
            name: "origin/feature/topic".into(),
            oid: "1234567".into(),
            tag: Some(HomeRefTag::Remote),
        };

        prepare_current_checkout(&mut runner, Path::new("/repo"), &git_ref)
            .expect("clean remote checkout");

        assert_eq!(
            runner.calls[1],
            vec![
                "checkout",
                "-b",
                "feature/topic",
                "--track",
                "origin/feature/topic"
            ]
        );
    }
}
