use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTreeSnapshot {
    pub(crate) root: PathBuf,
    pub(crate) files: Vec<FileRecord>,
    pub(crate) fingerprint: u64,
}

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
    match git_repo_root(cwd, git_program) {
        Some(root) => build_git_file_tree(root, git_program),
        None => build_directory_file_tree(cwd.to_path_buf()),
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

fn build_directory_file_tree(root: PathBuf) -> FileTreeSnapshot {
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let mut paths = Vec::new();
    walk_directory(&root, &root, &mut paths);
    paths.sort();
    let files = paths
        .into_iter()
        .map(|path| FileRecord { path, status: None })
        .collect::<Vec<_>>();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    files.hash(&mut hasher);
    FileTreeSnapshot {
        root,
        files,
        fingerprint: hasher.finish(),
    }
}

fn walk_directory(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_directory(root, &path, paths);
        } else if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_path_buf());
        }
    }
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
