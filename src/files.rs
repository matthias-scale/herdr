use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FileStatus {
    Modified,
    Added,
    Untracked,
}

impl FileStatus {
    pub(crate) fn gutter(self) -> char {
        match self {
            Self::Modified => 'M',
            Self::Added => 'A',
            Self::Untracked => '?',
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileRecord {
    pub(crate) path: PathBuf,
    pub(crate) status: Option<FileStatus>,
}

/// Where a listing came from. Outside a git repository `git ls-files` has
/// nothing to say, so the tree is a plain directory walk and the surface says
/// so instead of pretending the repository is empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FileTreeSource {
    #[default]
    Git,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTreeSnapshot {
    pub(crate) root: PathBuf,
    pub(crate) files: Vec<FileRecord>,
    pub(crate) fingerprint: u64,
    pub(crate) source: FileTreeSource,
    /// Why the listing is incomplete. Shown as a row instead of the tree, so a
    /// walk that never finishes cannot leave the surface on its spinner.
    pub(crate) error: Option<String>,
}

/// How long a directory walk may run before the surface gives up on it.
pub(crate) const DIRECTORY_WALK_DEADLINE: Duration = Duration::from_secs(5);

/// Directories a plain walk never descends into: not interesting, and big
/// enough to spend the whole deadline on.
const IGNORED_DIRECTORIES: [&str; 3] = [".git", "target", "node_modules"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileTreeRowKind {
    Directory,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTreeRow {
    pub(crate) path: PathBuf,
    pub(crate) depth: usize,
    pub(crate) kind: FileTreeRowKind,
    pub(crate) status: Option<FileStatus>,
}

#[derive(Default)]
struct DirectoryNode {
    directories: BTreeMap<String, DirectoryNode>,
    files: BTreeMap<String, Option<FileStatus>>,
}

impl FileTreeSnapshot {
    pub(crate) fn rows(
        &self,
        collapsed: &std::collections::HashSet<PathBuf>,
        matched_files: Option<&std::collections::HashSet<PathBuf>>,
    ) -> Vec<FileTreeRow> {
        let mut root = DirectoryNode::default();
        for file in &self.files {
            if matched_files.is_some_and(|matches| !matches.contains(&file.path)) {
                continue;
            }
            insert_file(&mut root, &file.path, file.status);
        }

        let mut rows = Vec::new();
        append_rows(
            &root,
            Path::new(""),
            0,
            collapsed,
            matched_files.is_some(),
            &mut rows,
        );
        rows
    }
}

fn insert_file(root: &mut DirectoryNode, path: &Path, status: Option<FileStatus>) {
    let mut components = path.components().peekable();
    let mut directory = root;
    while let Some(component) = components.next() {
        let name = component.as_os_str().to_string_lossy().into_owned();
        if components.peek().is_none() {
            directory.files.insert(name, status);
        } else {
            directory = directory.directories.entry(name).or_default();
        }
    }
}

fn append_rows(
    node: &DirectoryNode,
    parent: &Path,
    depth: usize,
    collapsed: &std::collections::HashSet<PathBuf>,
    force_expanded: bool,
    rows: &mut Vec<FileTreeRow>,
) {
    for (name, child) in &node.directories {
        let path = parent.join(name);
        rows.push(FileTreeRow {
            path: path.clone(),
            depth,
            kind: FileTreeRowKind::Directory,
            status: None,
        });
        if force_expanded || !collapsed.contains(&path) {
            append_rows(child, &path, depth + 1, collapsed, force_expanded, rows);
        }
    }
    for (name, status) in &node.files {
        rows.push(FileTreeRow {
            path: parent.join(name),
            depth,
            kind: FileTreeRowKind::File,
            status: *status,
        });
    }
}

pub(crate) fn build_file_tree(cwd: &Path, git_program: &Path) -> FileTreeSnapshot {
    build_file_tree_until(cwd, git_program, Instant::now() + DIRECTORY_WALK_DEADLINE)
}

/// `build_file_tree` with an explicit deadline for the non-git walk.
pub(crate) fn build_file_tree_until(
    cwd: &Path,
    git_program: &Path,
    deadline: Instant,
) -> FileTreeSnapshot {
    match git_repo_root(cwd, git_program) {
        Some(root) => build_git_file_tree(root, git_program),
        None => build_directory_file_tree(cwd.to_path_buf(), deadline),
    }
}

pub(crate) fn git_file_fingerprint(root: &Path, git_program: &Path) -> Option<u64> {
    let paths = git_output(
        root,
        git_program,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let status = git_output(
        root,
        git_program,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Some(hash_git_outputs(&paths, &status))
}

fn git_repo_root(cwd: &Path, git_program: &Path) -> Option<PathBuf> {
    let output = git_output(cwd, git_program, &["rev-parse", "--show-toplevel"])?;
    let root = String::from_utf8_lossy(&output).trim().to_string();
    (!root.is_empty()).then(|| {
        let root = PathBuf::from(root);
        std::fs::canonicalize(&root).unwrap_or(root)
    })
}

fn build_git_file_tree(root: PathBuf, git_program: &Path) -> FileTreeSnapshot {
    let paths = git_output(
        &root,
        git_program,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )
    .unwrap_or_default();
    let porcelain = git_output(
        &root,
        git_program,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .unwrap_or_default();
    let statuses = parse_porcelain(&porcelain);
    let files = nul_paths(&paths)
        .into_iter()
        .map(|path| FileRecord {
            status: statuses.get(&path).copied(),
            path,
        })
        .collect();
    FileTreeSnapshot {
        root,
        files,
        fingerprint: hash_git_outputs(&paths, &porcelain),
        source: FileTreeSource::Git,
        error: None,
    }
}

fn git_output(cwd: &Path, git_program: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(git_program)
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn nul_paths(output: &[u8]) -> Vec<PathBuf> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
        .collect()
}

pub(crate) fn parse_porcelain(output: &[u8]) -> HashMap<PathBuf, FileStatus> {
    let records = output.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut statuses = HashMap::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let x = record[0] as char;
        let y = record[1] as char;
        let path = PathBuf::from(String::from_utf8_lossy(&record[3..]).into_owned());
        if let Some(status) = porcelain_status(x, y) {
            statuses.insert(path, status);
        }
        index += if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            2
        } else {
            1
        };
    }
    statuses
}

fn porcelain_status(x: char, y: char) -> Option<FileStatus> {
    if x == '?' && y == '?' {
        Some(FileStatus::Untracked)
    } else if x == 'A' || y == 'A' {
        Some(FileStatus::Added)
    } else if [x, y]
        .into_iter()
        .any(|status| matches!(status, 'M' | 'D' | 'R' | 'C' | 'U'))
    {
        Some(FileStatus::Modified)
    } else {
        None
    }
}

fn hash_git_outputs(paths: &[u8], status: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    paths.hash(&mut hasher);
    status.hash(&mut hasher);
    hasher.finish()
}

fn build_directory_file_tree(root: PathBuf, deadline: Instant) -> FileTreeSnapshot {
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let mut paths = Vec::new();
    let outcome = walk_directory(&root, &root, &mut paths, deadline);
    paths.sort();
    let files = paths
        .into_iter()
        .map(|path| FileRecord { path, status: None })
        .collect::<Vec<_>>();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    files.hash(&mut hasher);
    let error = match outcome {
        WalkOutcome::Complete => None,
        WalkOutcome::TimedOut => Some(format!(
            "listing timed out after {}s",
            DIRECTORY_WALK_DEADLINE.as_secs()
        )),
        WalkOutcome::Unreadable => Some(format!("cannot read {}", root.display())),
    };
    if let Some(error) = error.as_deref() {
        tracing::warn!(root = %root.display(), error, "directory file listing incomplete");
    }
    FileTreeSnapshot {
        root,
        files,
        fingerprint: hasher.finish(),
        source: FileTreeSource::Directory,
        error,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkOutcome {
    Complete,
    TimedOut,
    /// The walk root itself could not be read; a directory deeper down that is
    /// unreadable is skipped instead, the same as before.
    Unreadable,
}

fn walk_directory(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<PathBuf>,
    deadline: Instant,
) -> WalkOutcome {
    if Instant::now() >= deadline {
        return WalkOutcome::TimedOut;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return if directory == root {
            WalkOutcome::Unreadable
        } else {
            WalkOutcome::Complete
        };
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if Instant::now() >= deadline {
            return WalkOutcome::TimedOut;
        }
        if IGNORED_DIRECTORIES
            .iter()
            .any(|ignored| entry.file_name() == *ignored)
        {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if walk_directory(root, &path, paths, deadline) == WalkOutcome::TimedOut {
                return WalkOutcome::TimedOut;
            }
        } else if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_path_buf());
        }
    }
    WalkOutcome::Complete
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-files-{label}-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success());
    }

    #[test]
    fn a_cwd_outside_a_repository_falls_back_to_a_directory_walk() {
        let temp = TempDir::new("no-repo");
        let root = &temp.0;
        std::fs::write(root.join("notes.md"), "hello").expect("write file");
        for noise in ["target", "node_modules", ".git"] {
            std::fs::create_dir_all(root.join(noise)).expect("create dir");
            std::fs::write(root.join(noise).join("ignored.txt"), "x").expect("write file");
        }
        std::fs::create_dir_all(root.join("src")).expect("create dir");
        std::fs::write(root.join("src").join("lib.rs"), "fn main() {}").expect("write file");

        // A git program that cannot exist keeps the fixture independent of the
        // machine's git and of any repository above the temp directory.
        let snapshot = build_file_tree(root, Path::new("herdr-no-such-git"));

        assert_eq!(snapshot.source, FileTreeSource::Directory);
        assert_eq!(snapshot.error, None);
        let paths = snapshot
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        assert!(paths.contains(&PathBuf::from("notes.md")));
        assert!(paths.contains(&PathBuf::from("src/lib.rs")));
        for noise in ["target", "node_modules", ".git"] {
            assert!(
                !paths.iter().any(|path| path.starts_with(noise)),
                "{noise} should not be walked: {paths:?}"
            );
        }
    }

    #[test]
    fn a_walk_that_runs_past_its_deadline_reports_an_error() {
        let temp = TempDir::new("slow-walk");
        std::fs::write(temp.0.join("notes.md"), "hello").expect("write file");

        let snapshot = build_file_tree_until(
            &temp.0,
            Path::new("herdr-no-such-git"),
            Instant::now() - Duration::from_secs(1),
        );

        assert_eq!(snapshot.source, FileTreeSource::Directory);
        assert_eq!(
            snapshot.error.as_deref(),
            Some("listing timed out after 5s")
        );
    }

    #[test]
    fn an_unreadable_walk_root_reports_an_error() {
        let temp = TempDir::new("missing-root");
        let missing = temp.0.join("gone");

        let snapshot = build_file_tree(&missing, Path::new("herdr-no-such-git"));

        assert_eq!(snapshot.source, FileTreeSource::Directory);
        assert!(snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("cannot read ")));
    }

    #[test]
    fn tree_comes_from_real_git_fixture_and_honors_gitignore() {
        if !git_available() {
            return;
        }
        let temp = TempDir::new("fixture");
        git(&temp.0, &["init", "--quiet"]);
        std::fs::create_dir_all(temp.0.join("src/nested")).expect("create dirs");
        std::fs::write(temp.0.join("src/lib.rs"), "fn lib() {}\n").expect("write lib");
        std::fs::write(temp.0.join("src/nested/data.json"), "{}\n").expect("write json");
        std::fs::write(temp.0.join("ignored.log"), "ignored\n").expect("write ignored");
        std::fs::write(temp.0.join(".gitignore"), "*.log\n").expect("write ignore");
        git(&temp.0, &["add", "src/lib.rs", ".gitignore"]);

        let snapshot = build_file_tree(&temp.0.join("src"), Path::new("git"));
        let paths = snapshot
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<Vec<_>>();

        // git prints the canonical toplevel; on macOS the temp dir lives under
        // /var, a symlink to /private/var, so compare canonical paths.
        assert_eq!(
            snapshot.root.canonicalize().expect("canonical root"),
            temp.0.canonicalize().expect("canonical fixture")
        );
        assert!(paths.contains(&Path::new("src/lib.rs")));
        assert!(paths.contains(&Path::new("src/nested/data.json")));
        assert!(paths.contains(&Path::new(".gitignore")));
        assert!(!paths.contains(&Path::new("ignored.log")));
    }

    #[test]
    fn porcelain_gutter_mapping_handles_staged_unstaged_and_untracked() {
        let output = b" M src/lib.rs\0A  src/new.rs\0?? notes.md\0D  gone.txt\0";
        let statuses = parse_porcelain(output);
        assert_eq!(statuses[Path::new("src/lib.rs")], FileStatus::Modified);
        assert_eq!(statuses[Path::new("src/new.rs")], FileStatus::Added);
        assert_eq!(statuses[Path::new("notes.md")], FileStatus::Untracked);
        assert_eq!(statuses[Path::new("gone.txt")], FileStatus::Modified);
    }

    #[test]
    fn filtered_tree_expands_matching_parents() {
        let snapshot = FileTreeSnapshot {
            root: PathBuf::from("/repo"),
            files: vec![FileRecord {
                path: PathBuf::from("src/nested/lib.rs"),
                status: None,
            }],
            fingerprint: 1,
            source: FileTreeSource::Git,
            error: None,
        };
        let collapsed = [PathBuf::from("src"), PathBuf::from("src/nested")]
            .into_iter()
            .collect();
        let matched = [PathBuf::from("src/nested/lib.rs")].into_iter().collect();
        let rows = snapshot.rows(&collapsed, Some(&matched));
        assert_eq!(
            rows.iter()
                .map(|row| row.path.as_path())
                .collect::<Vec<_>>(),
            vec![
                Path::new("src"),
                Path::new("src/nested"),
                Path::new("src/nested/lib.rs")
            ]
        );
    }
}
