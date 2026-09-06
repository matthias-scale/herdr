//! Per-repository scratchpad: a plain `scratchpad.md` an agent and its operator
//! both write, surfaced read-only in the dock so neither side has to attach to a
//! pane to read the other's notes. Editing is deliberately delegated to `$EDITOR`
//! (see [`crate::ui::dock`]); nothing here owns a text buffer.

use std::path::{Path, PathBuf};

use crate::events::AppEvent;
use crate::work_context::{
    extract_missive_urls, extract_pr_urls, extract_preview_urls, extract_ticket_ids,
    work_link_candidates, PaneWorkContext, WorkLinkCandidate,
};
use notify::Watcher;

pub(crate) const SCRATCHPAD_RELATIVE_PATH: &str = ".herdr/scratchpad.md";

/// A scratchpad larger than this is almost certainly not a hand-written note, and
/// rendering it would stall the frame it is read on.
const MAX_SCRATCHPAD_BYTES: u64 = 512 * 1024;

/// The scratchpad file belonging to a repository root or worktree checkout.
pub(crate) fn scratchpad_path(repo_root: &Path) -> PathBuf {
    repo_root.join(SCRATCHPAD_RELATIVE_PATH)
}

/// What the dock renders. A missing file is an ordinary empty state, not an
/// error: the common case is a repository whose scratchpad has never been used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScratchpadDoc {
    /// `None` when the focused pane belongs to no repository.
    pub(crate) path: Option<PathBuf>,
    pub(crate) body: String,
    /// Set only for a file that exists but could not be read.
    pub(crate) error: Option<String>,
    pub(crate) exists: bool,
}

impl ScratchpadDoc {
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    path: Some(path),
                    ..Self::default()
                };
            }
            Err(error) => {
                return Self {
                    path: Some(path),
                    error: Some(error.to_string()),
                    ..Self::default()
                };
            }
        };
        if metadata.len() > MAX_SCRATCHPAD_BYTES {
            return Self {
                path: Some(path),
                error: Some(format!(
                    "scratchpad is larger than {}KB",
                    MAX_SCRATCHPAD_BYTES / 1024
                )),
                exists: true,
                ..Self::default()
            };
        }
        match std::fs::read_to_string(&path) {
            Ok(body) => Self {
                path: Some(path),
                body,
                error: None,
                exists: true,
            },
            Err(error) => Self {
                path: Some(path),
                error: Some(error.to_string()),
                exists: true,
                ..Self::default()
            },
        }
    }

    /// Reuses the work-context extractors so a link written into the scratchpad
    /// resolves exactly like the same link observed in a pane.
    pub(crate) fn link_candidates(&self) -> Vec<WorkLinkCandidate> {
        if self.body.is_empty() {
            return Vec::new();
        }
        let context = PaneWorkContext {
            repo: None,
            ticket_ids: extract_ticket_ids(&self.body),
            pr_urls: extract_pr_urls(&self.body),
            preview_urls: extract_preview_urls(&self.body),
            missive_urls: extract_missive_urls(&self.body),
            branch: None,
            work_title: None,
            session_name: None,
            role: None,
            active_owner: false,
        };
        work_link_candidates(&context)
    }
}

/// The repository the focused workspace belongs to. A worktree checkout owns its
/// own scratchpad, matching how the dock's Context tab labels a worktree.
pub(crate) fn focused_repo_root(app: &crate::app::AppState) -> Option<PathBuf> {
    let workspace = app.active.and_then(|index| app.workspaces.get(index))?;
    if let Some(worktree) = workspace.worktree_space() {
        return Some(worktree.checkout_path.clone());
    }
    workspace.git_space().map(|space| space.repo_root.clone())
}

/// Ensure the scratchpad and its parent directory exist so `$EDITOR` opens a real
/// file rather than creating one at an unexpected path.
pub(crate) fn ensure_scratchpad_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
}

/// Watches the scratchpad's parent directory. The parent, not the file, because a
/// file that does not exist yet cannot be watched and editors replace rather than
/// rewrite the inode.
pub(crate) fn watch_scratchpad(
    path: &Path,
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> Option<notify::RecommendedWatcher> {
    let parent = path.parent()?.to_path_buf();
    if std::fs::create_dir_all(&parent).is_err() {
        // A repository we cannot write to still renders; it just will not live-refresh.
        if !parent.is_dir() {
            return None;
        }
    }
    let target = path.to_path_buf();
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let relevant = matches!(
            event.kind,
            notify::EventKind::Any
                | notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_)
        );
        if relevant
            && event
                .paths
                .iter()
                .any(|candidate| candidate.file_name() == target.file_name())
        {
            let _ = event_tx.try_send(AppEvent::ScratchpadChanged);
        }
    });
    let mut watcher = watcher.ok()?;
    if let Err(error) = watcher.watch(&parent, notify::RecursiveMode::NonRecursive) {
        tracing::warn!(
            path = %parent.display(),
            error = %error,
            "failed to register scratchpad watcher"
        );
        return None;
    }
    Some(watcher)
}

impl crate::app::App {
    /// Follows focus: a different repository means a different scratchpad, a fresh
    /// read, and a watcher pointed somewhere else. Same repository is a no-op, so
    /// this is safe to call every tick.
    pub(crate) fn ensure_scratchpad(&mut self) {
        let path = focused_repo_root(&self.state).map(|root| scratchpad_path(&root));
        if path == self.scratchpad_watched_path {
            return;
        }
        self.state.scratchpad = ScratchpadDoc::load(path.clone());
        self.scratchpad_watcher = if cfg!(test) {
            None
        } else {
            path.as_deref()
                .and_then(|path| watch_scratchpad(path, self.event_tx.clone()))
        };
        self.scratchpad_watched_path = path;
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    /// Returns whether the reload changed anything, so an editor writing the file
    /// repeatedly does not force a repaint per write.
    pub(crate) fn reload_scratchpad(&mut self) -> bool {
        let next = ScratchpadDoc::load(self.scratchpad_watched_path.clone());
        if next == self.state.scratchpad {
            return false;
        }
        self.state.scratchpad = next;
        true
    }
}

pub(crate) fn editor_argv_candidates(path: Option<&std::path::Path>) -> Vec<Vec<String>> {
    let mut candidates = Vec::new();
    if let Ok(editor) = std::env::var("EDITOR") {
        if let Some(argv) = parse_editor_command(&editor) {
            candidates.push(argv);
        }
    }
    candidates.push(vec!["nvim".to_string()]);
    #[cfg(windows)]
    candidates.push(vec!["notepad.exe".to_string()]);
    #[cfg(not(windows))]
    candidates.push(vec!["vi".to_string()]);
    if let Some(path) = path {
        let argument = path.to_string_lossy().into_owned();
        for candidate in candidates.iter_mut() {
            candidate.push(argument.clone());
        }
    }
    candidates
}

fn parse_editor_command(command: &str) -> Option<Vec<String>> {
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                argv.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        argv.push(word);
    }
    (!argv.is_empty()).then_some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_requested_file_is_appended_to_every_editor_candidate() {
        let path = std::path::Path::new("/repo/.herdr/scratchpad.md");
        let with_path = editor_argv_candidates(Some(path));
        let without_path = editor_argv_candidates(None);

        assert_eq!(with_path.len(), without_path.len());
        assert!(
            with_path
                .iter()
                .all(|argv| argv.last().map(String::as_str) == Some(path.to_str().unwrap())),
            "candidates: {with_path:?}"
        );
        assert!(
            without_path
                .iter()
                .all(|argv| argv.last().map(String::as_str) != Some(path.to_str().unwrap())),
            "candidates: {without_path:?}"
        );
    }

    #[test]
    fn editor_command_parser_preserves_quoted_arguments() {
        assert_eq!(
            parse_editor_command("nvim --cmd \"set title\""),
            Some(vec![
                "nvim".to_string(),
                "--cmd".to_string(),
                "set title".to_string()
            ])
        );
    }

    /// The repository has no `tempfile` dependency; unique temp roots follow the
    /// same convention the UI tests use.
    fn temp_repo_root(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("herdr-scratchpad-{tag}-{unique}"));
        std::fs::create_dir_all(&root).expect("create temp repo root");
        root
    }

    #[test]
    fn scratchpad_lives_under_the_repository_root() {
        assert_eq!(
            scratchpad_path(Path::new("/repo")),
            PathBuf::from("/repo/.herdr/scratchpad.md")
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_state_and_not_an_error() {
        let root = temp_repo_root("missing");
        let doc = ScratchpadDoc::load(Some(scratchpad_path(&root)));
        assert!(!doc.exists);
        assert!(doc.error.is_none());
        assert!(doc.body.is_empty());
    }

    #[test]
    fn no_repository_yields_no_path() {
        let doc = ScratchpadDoc::load(None);
        assert!(doc.path.is_none());
        assert!(!doc.exists);
        assert!(doc.error.is_none());
    }

    #[test]
    fn an_existing_file_is_read_verbatim() {
        let root = temp_repo_root("read");
        let path = scratchpad_path(&root);
        ensure_scratchpad_file(&path).expect("create scratchpad");
        std::fs::write(&path, "## Progress\nhalfway\n").expect("write");
        let doc = ScratchpadDoc::load(Some(path));
        assert!(doc.exists);
        assert_eq!(doc.body, "## Progress\nhalfway\n");
    }

    #[test]
    fn an_oversized_scratchpad_reports_an_error_instead_of_rendering() {
        let root = temp_repo_root("oversized");
        let path = scratchpad_path(&root);
        ensure_scratchpad_file(&path).expect("create scratchpad");
        std::fs::write(&path, vec![b'x'; (MAX_SCRATCHPAD_BYTES + 1) as usize]).expect("write");
        let doc = ScratchpadDoc::load(Some(path));
        assert!(doc.exists);
        assert!(doc.body.is_empty());
        assert!(doc.error.is_some());
    }

    #[test]
    fn ensuring_the_file_creates_the_parent_directory_and_preserves_content() {
        let root = temp_repo_root("ensure");
        let path = scratchpad_path(&root);
        ensure_scratchpad_file(&path).expect("create scratchpad");
        assert!(path.exists());
        std::fs::write(&path, "kept").expect("write");
        ensure_scratchpad_file(&path).expect("second ensure");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "kept");
    }

    #[test]
    fn markdown_links_resolve_through_the_work_context_extractors() {
        let doc = ScratchpadDoc {
            path: Some(PathBuf::from("/repo/.herdr/scratchpad.md")),
            body: "MAT-128 needs https://github.com/o/r/pull/7 reviewed".to_string(),
            error: None,
            exists: true,
        };
        let candidates = doc.link_candidates();
        let labels: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.label.as_str())
            .collect();
        assert!(labels.contains(&"MAT-128"), "labels: {labels:?}");
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.url == "https://github.com/o/r/pull/7"),
            "candidates: {candidates:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_write_from_outside_herdr_is_adopted_by_the_next_reload() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let root = temp_repo_root("reload");
        let path = scratchpad_path(&root);
        ensure_scratchpad_file(&path).expect("create scratchpad");
        app.scratchpad_watched_path = Some(path.clone());

        assert!(app.reload_scratchpad(), "the empty file should be adopted");
        std::fs::write(&path, "## For you\ncheck the diff\n").expect("external write");
        assert!(app.reload_scratchpad(), "the external write should be seen");
        assert!(app.state.scratchpad.body.contains("check the diff"));
        assert!(
            !app.reload_scratchpad(),
            "an unchanged file must not force a repaint"
        );
    }

    #[test]
    fn a_pane_outside_any_repository_resolves_no_root() {
        let app = crate::app::state::AppState::test_new();
        assert!(focused_repo_root(&app).is_none());
    }

    #[test]
    fn an_empty_scratchpad_has_no_links() {
        assert!(ScratchpadDoc::default().link_candidates().is_empty());
    }
}
