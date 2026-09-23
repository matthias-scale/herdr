//! Read-only session goals loaded from `.streams.json`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

pub(crate) const STREAMS_FILE_NAME: &str = ".streams.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoalsFile {
    version: u8,
    next_goal_id: u64,
    next_stream_id: u64,
    pub(crate) goals: BTreeMap<String, Goal>,
    pub(crate) streams: BTreeMap<String, Stream>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Goal {
    pub(crate) text: String,
    pub(crate) done_when: String,
    pub(crate) link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stream {
    pub(crate) goal: String,
    pub(crate) what: String,
    pub(crate) state: StreamState,
    pub(crate) owner: String,
    pub(crate) needs: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StreamState {
    Running,
    Waiting,
    Blocked,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalsLoad {
    Missing,
    Malformed(String),
    Ready(GoalsFile),
}

pub(crate) fn session_root(cwd: &Path) -> PathBuf {
    session_root_with(cwd, |path| path.exists())
}

fn session_root_with(cwd: &Path, is_git_marker: impl Fn(&Path) -> bool) -> PathBuf {
    cwd.ancestors()
        .find(|directory| is_git_marker(&directory.join(".git")))
        .unwrap_or(cwd)
        .to_path_buf()
}

pub(crate) fn load_from_cwd(cwd: &Path) -> GoalsLoad {
    let path = session_root(cwd).join(STREAMS_FILE_NAME);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return GoalsLoad::Missing,
        Err(error) => return GoalsLoad::Malformed(error.to_string()),
    };

    match parse(&contents) {
        Ok(file) => GoalsLoad::Ready(file),
        Err(error) => GoalsLoad::Malformed(error),
    }
}

pub(crate) fn parse(contents: &str) -> Result<GoalsFile, String> {
    let file: GoalsFile = serde_json::from_str(contents).map_err(|error| error.to_string())?;
    validate(&file)?;
    Ok(file)
}

fn validate(file: &GoalsFile) -> Result<(), String> {
    if file.version != 1 {
        return Err("version must be 1".into());
    }
    if file.next_goal_id == 0 || file.next_stream_id == 0 {
        return Err("next IDs must be positive".into());
    }

    for (id, goal) in &file.goals {
        validate_id(id, 'G')?;
        validate_line(&goal.text, "goal text")?;
        validate_nonempty(&goal.done_when, "goal done_when")?;
        if let Some(link) = &goal.link {
            validate_nonempty(link, "goal link")?;
        }
    }

    for (id, stream) in &file.streams {
        validate_id(id, 'S')?;
        validate_id(&stream.goal, 'G')?;
        validate_line(&stream.what, "stream what")?;
        validate_nonempty(&stream.owner, "stream owner")?;
        if let Some(needs) = &stream.needs {
            if needs.iter().any(|need| *need == 0) {
                return Err("stream needs entries must be positive".into());
            }
            let unique: BTreeSet<_> = needs.iter().collect();
            if unique.len() != needs.len() {
                return Err("stream needs entries must be unique".into());
            }
        }
    }
    Ok(())
}

fn validate_id(id: &str, prefix: char) -> Result<(), String> {
    let Some(number) = id.strip_prefix(prefix) else {
        return Err(format!("invalid {prefix} ID `{id}`"));
    };
    if number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("invalid {prefix} ID `{id}`"));
    }
    Ok(())
}

fn validate_nonempty(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() {
        Err(format!("{field} must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_line(value: &str, field: &str) -> Result<(), String> {
    validate_nonempty(value, field)?;
    if value.contains(['\r', '\n']) {
        Err(format!("{field} must be one line"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
        "version": 1,
        "next_goal_id": 3,
        "next_stream_id": 4,
        "goals": {
            "G1": {"text": "Plain replies", "done_when": "Replies stay plain"},
            "G2": {"text": "Goals visible", "done_when": "Panel renders", "link": "https://example.test"}
        },
        "streams": {
            "S1": {"goal": "G1", "what": "Style text", "state": "running", "owner": "agent"},
            "S2": {"goal": "G2", "what": "Preview", "state": "blocked", "owner": "agent", "needs": [1, 2]},
            "S3": {"goal": "G2", "what": "Tests", "state": "done", "owner": "agent"}
        }
    }"#;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "herdr-goals-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time after epoch")
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_v1_file() {
        let parsed = parse(VALID).expect("valid goals file");
        assert_eq!(parsed.goals["G1"].text, "Plain replies");
        assert_eq!(
            parsed.streams["S2"].needs.as_deref(),
            Some([1, 2].as_slice())
        );
        assert_eq!(parsed.streams["S1"].needs, None);
        assert_eq!(parsed.streams["S3"].state, StreamState::Done);
    }

    #[test]
    fn rejects_schema_violations() {
        let duplicate_needs = VALID.replace("[1, 2]", "[1, 1]");
        assert!(parse(&duplicate_needs).is_err());

        let newline = VALID.replace("Plain replies", "Plain\\nreplies");
        assert!(parse(&newline).is_err());

        let unknown = VALID.replace(
            "\"done_when\": \"Replies stay plain\"",
            "\"done_when\": \"Replies stay plain\", \"extra\": true",
        );
        assert!(parse(&unknown).is_err());
    }

    #[test]
    fn discovers_worktree_root_without_following_git_common_dir() {
        let temp = TestDir::new("root");
        let worktree = temp.0.join("worktree");
        let nested = worktree.join("one/two");
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::write(
            worktree.join(".git"),
            "gitdir: /elsewhere/common/worktrees/task\n",
        )
        .expect("write worktree git file");

        assert_eq!(session_root(&nested), worktree);
    }

    #[test]
    fn uses_cwd_when_outside_git() {
        let temp = TestDir::new("non-git");
        assert_eq!(session_root_with(&temp.0, |_| false), temp.0);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let temp = TestDir::new("missing");
        fs::write(temp.0.join(".git"), "gitdir: /elsewhere\n").expect("write worktree git file");
        assert_eq!(load_from_cwd(&temp.0), GoalsLoad::Missing);
    }

    #[test]
    fn malformed_file_returns_a_visible_error_state() {
        let temp = TestDir::new("malformed");
        fs::write(temp.0.join(".git"), "gitdir: /elsewhere\n").expect("write worktree git file");
        fs::write(temp.0.join(STREAMS_FILE_NAME), "{not json").expect("write malformed goals file");

        assert!(matches!(
            load_from_cwd(&temp.0),
            GoalsLoad::Malformed(error) if !error.is_empty()
        ));
    }
}
