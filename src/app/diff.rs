use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::App;
use crate::app::state::{DiffCacheEntry, DiffCacheKey, DiffFileContent, DiffFileSummary};
use crate::events::AppEvent;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DiffRefreshRequest {
    pub(crate) cwd: PathBuf,
    pub(crate) pr_base: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) ignore_whitespace: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffRefreshTarget {
    Summary(DiffRefreshRequest),
    File {
        request: DiffRefreshRequest,
        key: DiffCacheKey,
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiffRefreshResult {
    Summary {
        request: DiffRefreshRequest,
        key: DiffCacheKey,
        entry: DiffCacheEntry,
    },
    File {
        request: DiffRefreshRequest,
        key: DiffCacheKey,
        path: String,
        content: Result<DiffFileContent, String>,
    },
}

/// Select the comparison ref in priority order. `origin_head` is the resolved
/// symbolic ref, for example `origin/main`, rather than the literal `origin/HEAD`.
pub(crate) fn resolve_diff_base(
    pr_base: Option<&str>,
    origin_head: Option<&str>,
    has_main: bool,
    has_master: bool,
) -> Option<String> {
    pr_base
        .filter(|base| !base.trim().is_empty())
        .or_else(|| origin_head.filter(|base| !base.trim().is_empty()))
        .map(str::to_string)
        .or_else(|| has_main.then(|| "main".to_string()))
        .or_else(|| has_master.then(|| "master".to_string()))
}

impl App {
    pub(crate) fn start_dock_diff_refresh_if_needed(&mut self) {
        if self.state.dock_collapsed
            || self.state.dock_tab != Some(crate::app::DockSurface::Diff)
            || self.diff_refresh_in_flight.is_some()
        {
            return;
        }
        let Some(request) = self.focused_diff_refresh_request() else {
            self.state.dock_diff_active_key = None;
            return;
        };

        if self.state.dock_diff_request.as_ref() != Some(&request) {
            self.state.dock_diff_request = Some(request.clone());
            self.state.dock_diff_active_key = None;
            self.state.dock_diff_selected = 0;
            self.state.dock_scroll = 0;
        }

        if self.state.dock_diff_active_key.is_none() {
            self.state.dock_diff_active_key = self
                .state
                .dock_diff_resolved_requests
                .get(&request)
                .filter(|key| self.state.dock_diff_cache.contains_key(*key))
                .cloned();
        }

        let target = if let Some(key) = self.state.dock_diff_active_key.clone() {
            let Some(entry) = self.state.dock_diff_cache.get(&key) else {
                self.state.dock_diff_active_key = None;
                return;
            };
            entry
                .files
                .iter()
                .find(|file| {
                    !self.state.dock_diff_collapsed.contains(&file.path)
                        && !entry.contents.contains_key(&file.path)
                })
                .map(|file| DiffRefreshTarget::File {
                    request,
                    key,
                    path: file.path.clone(),
                })
        } else {
            Some(DiffRefreshTarget::Summary(request))
        };
        let Some(target) = target else {
            return;
        };

        self.last_diff_refresh_generation = self.last_diff_refresh_generation.wrapping_add(1);
        let generation = self.last_diff_refresh_generation;
        self.diff_refresh_in_flight = Some((generation, target.clone()));
        let event_tx = self.event_tx.clone();
        let git_program = self.git_program_for_diff();
        std::thread::spawn(move || {
            let result = run_diff_refresh(target, &git_program);
            let _ = event_tx.blocking_send(AppEvent::DiffRefreshed {
                generation,
                result: Box::new(result),
            });
        });
    }

    fn git_program_for_diff(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(program) = self.git_program_override.as_ref() {
            return program.clone();
        }
        PathBuf::from("git")
    }

    fn focused_diff_refresh_request(&self) -> Option<DiffRefreshRequest> {
        let workspace = self
            .state
            .active
            .and_then(|index| self.state.workspaces.get(index))?;
        let pane_id = workspace.focused_pane_id()?;
        let terminal_id = workspace.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id)?;
        let context = terminal.effective_work_context();
        let pr_base = context.primary_pr().and_then(|pr_url| {
            let repo = context
                .repo
                .clone()
                .or_else(|| crate::work_context::repo_slug_from_pr_url(pr_url))?;
            let pr_number = pr_url.rsplit('/').next()?.parse().ok();
            let key = crate::app::state::WorkItemKey {
                repo,
                pr_number,
                pr_url: Some(pr_url.to_string()),
                ticket_id: None,
            };
            self.state
                .work_item_detail_cache
                .get(&key)
                .and_then(|detail| detail.base_ref_name.clone())
        });
        Some(DiffRefreshRequest {
            cwd: terminal.cwd.clone(),
            pr_base,
            branch: context.branch.clone(),
            ignore_whitespace: self.state.dock_diff_ignore_whitespace,
        })
    }

    pub(crate) fn handle_diff_refreshed(
        &mut self,
        generation: u64,
        result: DiffRefreshResult,
    ) -> bool {
        let Some((active_generation, target)) = self.diff_refresh_in_flight.take() else {
            return false;
        };
        if generation != active_generation {
            self.diff_refresh_in_flight = Some((active_generation, target));
            return false;
        }
        match result {
            DiffRefreshResult::Summary {
                request,
                key,
                entry,
            } => {
                self.state
                    .dock_diff_resolved_requests
                    .insert(request.clone(), key.clone());
                if self.state.dock_diff_request.as_ref() == Some(&request) {
                    self.state.dock_diff_active_key = Some(key.clone());
                }
                self.state.dock_diff_cache.insert(key, entry);
            }
            DiffRefreshResult::File {
                request,
                key,
                path,
                content,
            } => {
                if let Some(entry) = self.state.dock_diff_cache.get_mut(&key) {
                    match content {
                        Ok(content) => {
                            entry.contents.insert(path, content);
                        }
                        Err(error) => entry.error = Some(error),
                    }
                }
                if self.state.dock_diff_request.as_ref() != Some(&request) {
                    return false;
                }
            }
        }
        true
    }
}

fn run_diff_refresh(target: DiffRefreshTarget, git_program: &Path) -> DiffRefreshResult {
    match target {
        DiffRefreshTarget::Summary(request) => run_summary_refresh(request, git_program),
        DiffRefreshTarget::File { request, key, path } => {
            let content = run_file_refresh(&key, &path, git_program);
            DiffRefreshResult::File {
                request,
                key,
                path,
                content,
            }
        }
    }
}

fn run_summary_refresh(request: DiffRefreshRequest, git_program: &Path) -> DiffRefreshResult {
    let result = (|| {
        let root = git_text(git_program, &request.cwd, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim());
        let origin_head = git_text(
            git_program,
            &root,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        )
        .ok();
        let base = resolve_diff_base(
            request.pr_base.as_deref(),
            origin_head.as_deref().map(str::trim),
            git_ref_exists(git_program, &root, "main"),
            git_ref_exists(git_program, &root, "master"),
        )
        .ok_or_else(|| "no diff base found (origin/HEAD, main, or master)".to_string())?;
        let branch = request
            .branch
            .clone()
            .filter(|branch| !branch.trim().is_empty())
            .or_else(|| git_text(git_program, &root, &["branch", "--show-current"]).ok())
            .map(|branch| branch.trim().to_string())
            .filter(|branch| !branch.is_empty())
            .unwrap_or_else(|| "HEAD".to_string());
        let key = DiffCacheKey {
            root: root.clone(),
            base: base.clone(),
            ignore_whitespace: request.ignore_whitespace,
        };
        let committed = git_diff_stat(
            git_program,
            &root,
            &format!("{base}...HEAD"),
            request.ignore_whitespace,
        )?;
        let uncommitted = git_diff_stat(git_program, &root, "HEAD", request.ignore_whitespace)?;
        let files = merge_stat_files(committed, uncommitted);
        let contents = files
            .first()
            .and_then(|file| {
                run_file_refresh(&key, &file.path, git_program)
                    .ok()
                    .map(|content| (file.path.clone(), content))
            })
            .into_iter()
            .collect();
        Ok::<_, String>((
            key,
            DiffCacheEntry {
                branch,
                files,
                contents,
                error: None,
            },
        ))
    })();

    match result {
        Ok((key, entry)) => DiffRefreshResult::Summary {
            request,
            key,
            entry,
        },
        Err(error) => {
            let key = DiffCacheKey {
                root: request.cwd.clone(),
                base: request
                    .pr_base
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                ignore_whitespace: request.ignore_whitespace,
            };
            DiffRefreshResult::Summary {
                request,
                key,
                entry: DiffCacheEntry {
                    branch: "HEAD".to_string(),
                    files: Vec::new(),
                    contents: HashMap::new(),
                    error: Some(error),
                },
            }
        }
    }
}

fn run_file_refresh(
    key: &DiffCacheKey,
    path: &str,
    git_program: &Path,
) -> Result<DiffFileContent, String> {
    let committed = git_diff_file(
        git_program,
        &key.root,
        &format!("{}...HEAD", key.base),
        path,
        key.ignore_whitespace,
    )?;
    let uncommitted = git_diff_file(git_program, &key.root, "HEAD", path, key.ignore_whitespace)?;
    Ok(DiffFileContent {
        committed: parse_unified_diff_lines(&committed),
        uncommitted: parse_unified_diff_lines(&uncommitted),
    })
}

fn git_diff_stat(
    git_program: &Path,
    root: &Path,
    revision: &str,
    ignore_whitespace: bool,
) -> Result<Vec<DiffFileSummary>, String> {
    let mut args = vec!["diff"];
    if ignore_whitespace {
        args.push("-w");
    }
    args.extend(["--stat=1000", revision]);
    git_text(git_program, root, &args).map(|output| parse_diff_stat(&output))
}

fn git_diff_file(
    git_program: &Path,
    root: &Path,
    revision: &str,
    path: &str,
    ignore_whitespace: bool,
) -> Result<String, String> {
    let mut args = vec!["diff"];
    if ignore_whitespace {
        args.push("-w");
    }
    args.extend([revision, "--", path]);
    git_text(git_program, root, &args)
}

fn git_text(git_program: &Path, cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(git_program)
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|error| format!("git {} failed to start: {error}", args.join(" ")))?;
    output_text(output, args)
}

fn output_text(output: Output, args: &[&str]) -> Result<String, String> {
    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map_err(|error| format!("git {} returned non-UTF-8 output: {error}", args.join(" ")));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("git {} failed: {}", args.join(" "), stderr.trim()))
}

fn git_ref_exists(git_program: &Path, root: &Path, reference: &str) -> bool {
    Command::new(git_program)
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .status()
        .is_ok_and(|status| status.success())
}

pub(crate) fn parse_diff_stat(output: &str) -> Vec<DiffFileSummary> {
    output.lines().filter_map(parse_stat_line).collect()
}

fn parse_stat_line(line: &str) -> Option<DiffFileSummary> {
    let (display_path, detail) = line.rsplit_once(" | ")?;
    let display_path = display_path.trim();
    let detail = detail.trim();
    if display_path.is_empty() || detail.is_empty() {
        return None;
    }
    let binary = detail.starts_with("Bin ");
    let (additions, deletions) = if binary {
        (0, 0)
    } else {
        let graph = detail
            .split_once(' ')
            .map(|(_, graph)| graph)
            .unwrap_or_default();
        (
            graph.chars().filter(|character| *character == '+').count(),
            graph.chars().filter(|character| *character == '-').count(),
        )
    };
    Some(DiffFileSummary {
        path: rename_destination(display_path),
        display_path: display_path.to_string(),
        additions,
        deletions,
        binary,
    })
}

fn rename_destination(display_path: &str) -> String {
    let Some((before, after)) = display_path.split_once(" => ") else {
        return display_path.to_string();
    };
    if let Some(open) = before.rfind('{') {
        if let Some(close) = after.find('}') {
            return format!(
                "{}{}{}",
                &before[..open],
                &after[..close],
                &after[close + 1..]
            );
        }
    }
    after.to_string()
}

fn merge_stat_files(
    committed: Vec<DiffFileSummary>,
    uncommitted: Vec<DiffFileSummary>,
) -> Vec<DiffFileSummary> {
    let mut merged = committed;
    for file in uncommitted {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.path == file.path)
        {
            existing.additions = existing.additions.saturating_add(file.additions);
            existing.deletions = existing.deletions.saturating_add(file.deletions);
            existing.binary |= file.binary;
        } else {
            merged.push(file);
        }
    }
    merged
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Hunk,
    Added,
    Removed,
    Context,
    Binary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) text: String,
    pub(crate) kind: DiffLineKind,
}

pub(crate) fn parse_unified_diff_lines(output: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for line in output.lines() {
        let kind = if line.starts_with("@@") {
            Some(DiffLineKind::Hunk)
        } else if line.starts_with('+') && !line.starts_with("+++") {
            Some(DiffLineKind::Added)
        } else if line.starts_with('-') && !line.starts_with("---") {
            Some(DiffLineKind::Removed)
        } else if line.starts_with(' ') || line == "\\ No newline at end of file" {
            Some(DiffLineKind::Context)
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            Some(DiffLineKind::Binary)
        } else {
            None
        };
        if let Some(kind) = kind {
            lines.push(DiffLine {
                text: line.to_string(),
                kind,
            });
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_repo() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("herdr-diff-fixture-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create fixture repo");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .expect("run fixture git");
            assert!(status.success(), "git fixture command failed: {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Herdr test"]);
        git(&["config", "user.email", "herdr@example.test"]);
        std::fs::write(root.join("notes.txt"), "first\n").expect("write base file");
        git(&["add", "notes.txt"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("notes.txt"), "committed\n").expect("write committed diff");
        git(&["commit", "-q", "-am", "change"]);
        std::fs::write(root.join("notes.txt"), "committed\nuncommitted\n")
            .expect("write working diff");
        root
    }

    #[test]
    fn diff_base_resolution_follows_pr_origin_main_master_order() {
        assert_eq!(
            resolve_diff_base(Some("release"), Some("origin/trunk"), true, true).as_deref(),
            Some("release")
        );
        assert_eq!(
            resolve_diff_base(None, Some("origin/trunk"), true, true).as_deref(),
            Some("origin/trunk")
        );
        assert_eq!(
            resolve_diff_base(None, None, true, true).as_deref(),
            Some("main")
        );
        assert_eq!(
            resolve_diff_base(None, None, false, true).as_deref(),
            Some("master")
        );
        assert_eq!(resolve_diff_base(None, None, false, false), None);
    }

    #[test]
    fn stat_parser_keeps_renames_and_binary_files() {
        let files = parse_diff_stat(
            " src/{old.rs => new.rs} | 3 ++-\n assets/logo.png | Bin 12 -> 18 bytes\n 2 files changed, 2 insertions(+), 1 deletion(-)\n",
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].display_path, "src/{old.rs => new.rs}");
        assert_eq!((files[0].additions, files[0].deletions), (2, 1));
        assert_eq!(files[1].path, "assets/logo.png");
        assert!(files[1].binary);
    }

    #[test]
    fn unified_diff_hunk_parser_handles_renames_and_binary_files() {
        let fixture = concat!(
            "diff --git a/old.rs b/new.rs\n",
            "similarity index 80%\n",
            "rename from old.rs\n",
            "rename to new.rs\n",
            "--- a/old.rs\n",
            "+++ b/new.rs\n",
            "@@ -1,2 +1,2 @@\n",
            "-old\n",
            "+new\n",
            " context\n",
            "diff --git a/logo.png b/logo.png\n",
            "Binary files a/logo.png and b/logo.png differ\n",
        );
        let parsed = parse_unified_diff_lines(fixture);
        assert_eq!(
            parsed.iter().map(|line| line.kind).collect::<Vec<_>>(),
            vec![
                DiffLineKind::Hunk,
                DiffLineKind::Removed,
                DiffLineKind::Added,
                DiffLineKind::Context,
                DiffLineKind::Binary,
            ]
        );
        assert_eq!(parsed[0].text, "@@ -1,2 +1,2 @@");
    }

    #[test]
    fn collapse_state_is_independent_per_file() {
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("src/one.rs".to_string());
        assert!(collapsed.contains("src/one.rs"));
        assert!(!collapsed.contains("src/two.rs"));
        collapsed.insert("src/two.rs".to_string());
        collapsed.remove("src/one.rs");
        assert!(!collapsed.contains("src/one.rs"));
        assert!(collapsed.contains("src/two.rs"));
    }

    #[test]
    fn diff_refresh_merges_committed_and_uncommitted_sections() {
        let root = fixture_repo();
        let request = DiffRefreshRequest {
            cwd: root.clone(),
            pr_base: Some("main".into()),
            branch: Some("feature".into()),
            ignore_whitespace: false,
        };
        let DiffRefreshResult::Summary { key, entry, .. } =
            run_summary_refresh(request, Path::new("git"))
        else {
            panic!("expected summary");
        };
        assert_eq!(key.base, "main");
        assert_eq!(entry.branch, "feature");
        assert_eq!(entry.files.len(), 1);
        assert_eq!(entry.files[0].path, "notes.txt");
        assert_eq!((entry.files[0].additions, entry.files[0].deletions), (2, 1));

        let content =
            run_file_refresh(&key, "notes.txt", Path::new("git")).expect("load fixture file diff");
        assert!(content
            .committed
            .iter()
            .any(|line| line.text == "+committed"));
        assert!(content
            .uncommitted
            .iter()
            .any(|line| line.text == "+uncommitted"));
        std::fs::remove_dir_all(root).expect("remove fixture repo");
    }
}
