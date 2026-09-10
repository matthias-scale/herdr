//! Projects: named groups of checkouts the composer can dispatch into.
//!
//! Herdr already knows how to browse to a directory, but browsing is the wrong
//! affordance for the question "which of my repos is this for?". The answer is
//! a short list that barely changes, and typing a path to reach it every time
//! is friction the composer can remove.
//!
//! A project is deliberately not a workspace and not a directory. Workspaces
//! are session organisation and a directory is one path; a project is the
//! grouping a human already has in their head ("scalable", "personal") and
//! which the filesystem does not record anywhere. That grouping therefore has
//! to be configured, so this module reads it rather than guessing it.
//!
//! Scanning happens here, once, at config-resolve time. Nothing in this module
//! may be called from view computation or render: it touches the filesystem,
//! and that cost is multiplicative with panes and clients.

use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;

/// Where repos live when no project is configured.
const DEFAULT_ROOT: &str = "~/Repos";

/// The id and label a single implicit project carries.
const DEFAULT_PROJECT: &str = "repos";

/// One checkout inside a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRepo {
    /// The directory name, which is what the picker shows.
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

/// A named group of checkouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) label: String,
    /// Resolved at config time and then immutable. Empty when the configured
    /// roots do not exist yet, which is not an error: a project can be declared
    /// before its checkouts land.
    pub(crate) repos: Vec<ProjectRepo>,
}

/// Expand a leading `~/` against `$HOME`. Anything else is returned unchanged,
/// including a bare `~`, which is a legal directory name.
fn expand_home(value: &str) -> PathBuf {
    let Some(rest) = value.strip_prefix("~/") else {
        return PathBuf::from(value);
    };
    match std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => Path::new(&home).join(rest),
        None => PathBuf::from(value),
    }
}

/// A directory is a checkout when it carries a `.git` entry. A worktree's
/// `.git` is a file rather than a directory, so both count.
fn is_checkout(path: &Path) -> bool {
    path.join(".git").exists()
}

fn repo_at(path: PathBuf) -> Option<ProjectRepo> {
    if !is_checkout(&path) {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(ProjectRepo { name, path })
}

/// Every checkout directly inside `root`. One level only: nesting a scan is how
/// a stray `node_modules` turns a config read into a disk crawl.
fn scan_root(root: &Path) -> Vec<ProjectRepo> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut repos: Vec<ProjectRepo> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| repo_at(entry.path()))
        .collect();
    repos.sort_by(|left, right| left.name.cmp(&right.name));
    repos
}

/// Build the composer's project list from config, scanning each project's roots.
///
/// An empty config yields one project over `~/Repos`, so the picker is useful
/// before anyone configures anything.
pub(crate) fn resolve(configured: &[ProjectConfig]) -> Vec<Project> {
    if configured.is_empty() {
        return vec![Project {
            id: DEFAULT_PROJECT.into(),
            label: DEFAULT_PROJECT.into(),
            repos: scan_root(&expand_home(DEFAULT_ROOT)),
        }];
    }

    let mut projects = Vec::with_capacity(configured.len());
    for entry in configured {
        if entry.id.trim().is_empty() {
            tracing::warn!("ignoring a project with no id");
            continue;
        }
        let mut repos: Vec<ProjectRepo> = entry
            .roots
            .iter()
            .flat_map(|root| scan_root(&expand_home(root)))
            .chain(entry.repos.iter().filter_map(|repo| {
                let path = expand_home(repo);
                let path = if path.is_absolute() {
                    path
                } else {
                    expand_home(DEFAULT_ROOT).join(path)
                };
                repo_at(path)
            }))
            .collect();
        // A repo reachable through two roots is still one repo.
        repos.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        repos.dedup_by(|left, right| left.path == right.path);

        let label = if entry.label.trim().is_empty() {
            entry.id.clone()
        } else {
            entry.label.clone()
        };
        projects.push(Project {
            id: entry.id.clone(),
            label,
            repos,
        });
    }
    projects
}

impl crate::app::state::AppState {
    /// The project the sidebar is scoped to. An id that no longer resolves
    /// reads as no scope, so deleting a project from the config shows
    /// everything again instead of emptying the sidebar.
    pub(crate) fn scoped_project(&self) -> Option<&Project> {
        let id = self.sidebar_work_filter.project.as_deref()?;
        self.projects.iter().find(|project| project.id == id)
    }

    /// Whether the sidebar offers a project scope at all. One project is every
    /// project, and scoping to it would filter nothing.
    pub(crate) fn sidebar_project_scope_available(&self) -> bool {
        self.projects.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repository has no `tempfile` dependency; unique temp roots follow the
    /// same convention the rest of the tests use.
    fn temp_root(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("herdr-projects-{tag}-{unique}"));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn checkout(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.join(".git")).expect("create checkout");
        path
    }

    fn names(project: &Project) -> Vec<&str> {
        project
            .repos
            .iter()
            .map(|repo| repo.name.as_str())
            .collect()
    }

    #[test]
    fn a_root_contributes_its_checkouts_and_skips_plain_directories() {
        let root = temp_root("root");
        checkout(&root, "herdr");
        checkout(&root, "dotfiles");
        std::fs::create_dir_all(root.join("notes")).expect("plain dir");

        let projects = resolve(&[ProjectConfig {
            id: "work".into(),
            label: "Work".into(),
            roots: vec![root.display().to_string()],
            repos: Vec::new(),
        }]);

        assert_eq!(
            names(&projects[0]),
            ["dotfiles", "herdr"],
            "sorted, checkouts only"
        );
        assert_eq!(projects[0].label, "Work");
    }

    #[test]
    fn an_explicit_repo_joins_the_project_without_duplicating_a_scanned_one() {
        let root = temp_root("mixed");
        let herdr = checkout(&root, "herdr");
        let elsewhere = temp_root("elsewhere");
        let solo = checkout(&elsewhere, "solo");

        let projects = resolve(&[ProjectConfig {
            id: "mixed".into(),
            label: String::new(),
            roots: vec![root.display().to_string()],
            repos: vec![herdr.display().to_string(), solo.display().to_string()],
        }]);

        assert_eq!(names(&projects[0]), ["herdr", "solo"]);
        // An empty label falls back to the id rather than rendering blank.
        assert_eq!(projects[0].label, "mixed");
    }

    #[test]
    fn a_project_with_no_id_is_dropped_and_a_missing_root_is_not_an_error() {
        let projects = resolve(&[
            ProjectConfig {
                id: "  ".into(),
                ..ProjectConfig::default()
            },
            ProjectConfig {
                id: "later".into(),
                roots: vec!["/nonexistent/path/for/tests".into()],
                ..ProjectConfig::default()
            },
        ]);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "later");
        assert!(projects[0].repos.is_empty());
    }
}
