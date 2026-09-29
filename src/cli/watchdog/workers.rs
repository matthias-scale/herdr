use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::schema::{
        AgentSessionInfo, Method, NotificationShowParams, NotificationShowSound, PaneListParams,
        Request,
    },
    watchdog::workers::{
        self as worker_watchdog, ParentState, StallAction, WorkerMemory, WorkerObservation,
        WorkerStallDecision,
    },
};

const DEFAULT_INTERVAL_SECS: u64 = 30;
const DEFAULT_STALL_MINUTES: u64 = 30;

#[derive(Debug, Clone)]
struct WorkerOptions {
    once: bool,
    dry_run: bool,
    json: bool,
    interval_secs: u64,
    stall_secs: u64,
    runs_dir: PathBuf,
    claude_projects_dir: PathBuf,
    state_file: PathBuf,
    log_file: PathBuf,
}

#[derive(Debug, Deserialize)]
struct PaneEntry {
    pane_id: String,
    #[serde(default)]
    agent_session: Option<AgentSessionInfo>,
}

#[derive(Debug, Serialize)]
struct WorkerSummary {
    workers: usize,
    stalled: usize,
    notified: usize,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct WorkerStallLog {
    timestamp: u64,
    source: String,
    worker_id: String,
    parent_session: String,
    last_activity: u64,
    age_secs: u64,
    parent_state: ParentState,
}

pub(super) fn run_worker_watchdog_command(args: &[String]) -> io::Result<i32> {
    if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
    {
        println!(
            "usage: herdr watchdog workers [--once] [--dry-run] [--interval-secs N] \
             [--stall-minutes N] [--runs-dir PATH] [--claude-projects-dir PATH]"
        );
        return Ok(0);
    }
    let options = match parse_worker_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    if options.once {
        return run_worker_scan(&options);
    }

    loop {
        if let Err(error) = run_worker_scan(&options) {
            eprintln!("worker watchdog scan failed: {error}");
        }
        thread::sleep(Duration::from_secs(options.interval_secs));
    }
}

fn parse_worker_options(args: &[String]) -> Result<WorkerOptions, String> {
    let home = home_dir();
    let mut options = WorkerOptions {
        once: false,
        dry_run: false,
        json: false,
        interval_secs: DEFAULT_INTERVAL_SECS,
        stall_secs: DEFAULT_STALL_MINUTES * 60,
        runs_dir: home.join(".agents/runs"),
        claude_projects_dir: home.join(".claude/projects"),
        state_file: crate::config::state_dir().join("worker-watchdog.json"),
        log_file: crate::config::state_dir().join("worker-watchdog.jsonl"),
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--once" => options.once = true,
            "--dry-run" => options.dry_run = true,
            "--json" => options.json = true,
            "--interval-secs" => {
                options.interval_secs = super::parse_value(args, &mut index, "--interval-secs")?;
                if options.interval_secs == 0 {
                    return Err("--interval-secs must be greater than zero".into());
                }
            }
            "--stall-minutes" => {
                let minutes = super::parse_value::<u64>(args, &mut index, "--stall-minutes")?;
                if minutes == 0 {
                    return Err("--stall-minutes must be greater than zero".into());
                }
                options.stall_secs = minutes
                    .checked_mul(60)
                    .ok_or_else(|| "--stall-minutes is too large".to_string())?;
            }
            "--runs-dir" => {
                options.runs_dir =
                    PathBuf::from(super::parse_string(args, &mut index, "--runs-dir")?);
            }
            "--claude-projects-dir" => {
                options.claude_projects_dir = PathBuf::from(super::parse_string(
                    args,
                    &mut index,
                    "--claude-projects-dir",
                )?);
            }
            "--state-file" => {
                options.state_file =
                    PathBuf::from(super::parse_string(args, &mut index, "--state-file")?);
            }
            "--log-file" => {
                options.log_file =
                    PathBuf::from(super::parse_string(args, &mut index, "--log-file")?);
            }
            unknown => return Err(format!("unknown worker watchdog option: {unknown}")),
        }
        index += 1;
    }
    Ok(options)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn run_worker_scan(options: &WorkerOptions) -> io::Result<i32> {
    let mut memory = load_memory(&options.state_file)?;
    let mut observations = discover_workers(&options.runs_dir, &options.claude_projects_dir)?;
    let pane_list = super::super::send_request(&Request {
        id: super::next_request_id("worker-watchdog-pane-list"),
        method: Method::PaneList(PaneListParams { workspace_id: None }),
    })?;
    super::ensure_api_success(&pane_list)?;
    let panes: Vec<PaneEntry> =
        serde_json::from_value(pane_list["result"]["panes"].clone()).map_err(io::Error::other)?;
    let parents = parent_panes(&panes);
    for worker in &mut observations {
        worker.parent_state = if parents.contains_key(&worker.parent_session) {
            ParentState::Present
        } else if worker.parent_scope_local {
            ParentState::Absent
        } else {
            ParentState::Unknown
        };
    }
    let now = unix_seconds()?;

    let decisions = worker_watchdog::process_stalls(
        &observations,
        &mut memory,
        now,
        options.stall_secs,
        options.dry_run,
        |worker, age_secs| notify_parent(worker, age_secs, &parents),
        |worker, age_secs| append_stall_log(&options.log_file, worker, age_secs),
    );
    save_memory(&options.state_file, &memory)?;
    print_worker_scan(observations.len(), &decisions, options)?;
    Ok(
        if decisions.iter().any(|decision| {
            decision.action == StallAction::NotifyFailed || decision.error.is_some()
        }) {
            1
        } else {
            0
        },
    )
}

fn parent_panes(panes: &[PaneEntry]) -> std::collections::HashMap<String, String> {
    let mut parents = std::collections::HashMap::new();
    for pane in panes {
        parents.insert(pane.pane_id.clone(), pane.pane_id.clone());
        if let Some(session) = &pane.agent_session {
            parents.insert(session.value.clone(), pane.pane_id.clone());
        }
    }
    parents
}

fn notify_parent(
    worker: &WorkerObservation,
    age_secs: u64,
    parents: &std::collections::HashMap<String, String>,
) -> io::Result<()> {
    let pane_id = parents.get(&worker.parent_session);
    if pane_id.is_none() && worker.parent_state != ParentState::Absent {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("parent session {} is not connected", worker.parent_session),
        ));
    }
    let text = format!(
        "Worker {} on {}: no semantic progress for {} minutes; parent session {}.",
        worker.worker_id,
        worker.source,
        age_secs / 60,
        worker.parent_session
    );
    let response = super::super::send_request(&Request {
        id: super::next_request_id("worker-stall-notification"),
        method: Method::NotificationShow(NotificationShowParams {
            title: if worker.parent_state == ParentState::Absent {
                "Orphaned worker".into()
            } else {
                "Worker watchdog".into()
            },
            body: Some(match pane_id {
                Some(pane_id) => format!("Parent {pane_id}: {text}"),
                None => format!("Operator queue: {text}"),
            }),
            position: None,
            sound: NotificationShowSound::None,
        }),
    })?;
    super::ensure_api_success(&response)
}

fn append_stall_log(path: &Path, worker: &WorkerObservation, age_secs: u64) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let record = WorkerStallLog {
        timestamp: unix_seconds()?,
        source: worker.source.clone(),
        worker_id: worker.worker_id.clone(),
        parent_session: worker.parent_session.clone(),
        last_activity: worker.last_activity,
        age_secs,
        parent_state: worker.parent_state,
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, &record).map_err(io::Error::other)?;
    file.write_all(b"\n")
}

fn print_worker_scan(
    workers: usize,
    decisions: &[WorkerStallDecision],
    options: &WorkerOptions,
) -> io::Result<()> {
    if options.json {
        let output = serde_json::json!({
            "decisions": decisions,
            "summary": WorkerSummary {
                workers,
                stalled: decisions.len(),
                notified: decisions.iter().filter(|decision| {
                    decision.action == StallAction::Notified
                }).count(),
                dry_run: options.dry_run,
            },
        });
        println!(
            "{}",
            serde_json::to_string(&output).map_err(io::Error::other)?
        );
        return Ok(());
    }

    for decision in decisions {
        let status = match decision.action {
            StallAction::ParentChecking => "checking parent",
            StallAction::ParentUnknown => "parent unknown",
            StallAction::Orphaned => "orphaned",
            StallAction::Confirming => "confirming stall",
            StallAction::WouldNotify => "would notify",
            StallAction::Notified => "notified",
            StallAction::AlreadyNotified => "already notified",
            StallAction::NotifyFailed => "notify failed",
        };
        println!(
            "worker {} parent {} stalled {}m: {}{}",
            decision.worker_id,
            decision.parent_session,
            decision.age_secs / 60,
            status,
            decision
                .error
                .as_ref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        );
    }
    println!(
        "worker watchdog summary: workers={} stalled={} notified={}{}",
        workers,
        decisions.len(),
        decisions
            .iter()
            .filter(|decision| decision.action == StallAction::Notified)
            .count(),
        if options.dry_run { " dry-run" } else { "" }
    );
    Ok(())
}

fn discover_workers(
    runs_dir: &Path,
    claude_projects_dir: &Path,
) -> io::Result<Vec<WorkerObservation>> {
    let mut workers = discover_codex_runs(runs_dir)?;
    workers.extend(discover_claude_subagents(claude_projects_dir)?);
    Ok(workers)
}

fn discover_codex_runs(runs_dir: &Path) -> io::Result<Vec<WorkerObservation>> {
    let mut workers = Vec::new();
    let entries = match fs::read_dir(runs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(workers),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_dir = entry.path();
        let state_path = run_dir.join("state.json");
        let state: Value = match fs::read(&state_path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    tracing::debug!(path = %state_path.display(), %error, "skipping malformed worker state");
                    continue;
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let parent_session = json_string(&state, &["parent_session"])
            .or_else(|| json_string(&state, &["parent", "session"]))
            .or_else(|| json_string(&state, &["parent"]))
            .or_else(|| launch_parent_session(&run_dir));
        let Some(parent_session) = parent_session else {
            continue;
        };
        let worker_id = entry.file_name().to_string_lossy().into_owned();
        let last_activity = codex_last_activity(&run_dir, &state)?;
        let state_name = json_string(&state, &["state"]).unwrap_or_default();
        let worker_host = json_string(&state, &["host"]);
        let parent_host = json_string(&state, &["parent", "host"]);
        let parent_scope_local = parent_host
            .as_deref()
            .is_none_or(|parent_host| worker_host.as_deref() == Some(parent_host));
        let finished = matches!(
            state_name.to_ascii_lowercase().as_str(),
            "complete"
                | "completed"
                | "done"
                | "failed"
                | "error"
                | "cancelled"
                | "stopped"
                | "success"
        ) || state.get("exit_code").is_some_and(|value| !value.is_null())
            || state
                .get("finished_at")
                .is_some_and(|value| !value.is_null());
        workers.push(WorkerObservation {
            source: "codex".into(),
            worker_id,
            parent_session,
            last_activity,
            finished,
            parent_scope_local,
            parent_state: ParentState::Unknown,
        });
    }
    Ok(workers)
}

fn launch_parent_session(run_dir: &Path) -> Option<String> {
    let metadata = fs::read_to_string(run_dir.join("launch-metadata")).ok()?;
    if let Ok(value) = serde_json::from_str::<Value>(&metadata) {
        if let Some(parent) = json_string(&value, &["parent_session"]) {
            return Some(parent);
        }
    }
    metadata.lines().find_map(|line| {
        line.trim()
            .strip_prefix("parent_session=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn codex_last_activity(run_dir: &Path, state: &Value) -> io::Result<u64> {
    // Heartbeat and atomic state rewrites prove liveness, not semantic progress.
    let mut last_activity = state
        .get("progress_at")
        .and_then(json_timestamp)
        .unwrap_or_default();
    for name in ["trace.log", "out.log", ".session-log"] {
        if let Some(modified) = modified_unix_seconds(&run_dir.join(name))? {
            last_activity = last_activity.max(modified);
        }
    }
    let turns_dir = run_dir.join("turns");
    if let Ok(turns) = fs::read_dir(turns_dir) {
        for turn in turns {
            let turn = turn?;
            if !turn.file_type()?.is_dir() {
                continue;
            }
            let trace = turn.path().join("trace.log");
            if let Some(modified) = modified_unix_seconds(&trace)? {
                last_activity = last_activity.max(modified);
            }
        }
    }
    Ok(last_activity)
}

fn discover_claude_subagents(projects_dir: &Path) -> io::Result<Vec<WorkerObservation>> {
    let mut workers = Vec::new();
    let projects = match fs::read_dir(projects_dir) {
        Ok(projects) => projects,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(workers),
        Err(error) => return Err(error),
    };
    for project in projects {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for session in fs::read_dir(project.path())? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            let Some(parent_session) = session.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let subagents_dir = session.path().join("subagents");
            let subagents = match fs::read_dir(subagents_dir) {
                Ok(subagents) => subagents,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for subagent in subagents {
                let subagent = subagent?;
                if !subagent.file_type()?.is_file()
                    || subagent.path().extension().is_none_or(|ext| ext != "jsonl")
                {
                    continue;
                }
                let worker_id = subagent
                    .path()
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string();
                let Some(last_activity) = modified_unix_seconds(&subagent.path())? else {
                    continue;
                };
                workers.push(WorkerObservation {
                    source: "claude".into(),
                    worker_id,
                    parent_session: parent_session.clone(),
                    last_activity,
                    finished: claude_subagent_finished(&subagent.path())?,
                    parent_scope_local: true,
                    parent_state: ParentState::Unknown,
                });
            }
        }
    }
    Ok(workers)
}

fn claude_subagent_finished(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    file.seek_read_tail(length)?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let tail = String::from_utf8_lossy(&tail);
    let Some(line) = tail.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Ok(false);
    };
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Ok(false);
    };
    let stop_reason = record
        .get("message")
        .and_then(|message| message.get("stop_reason"))
        .and_then(Value::as_str);
    Ok(stop_reason == Some("end_turn")
        || json_string(&record, &["type"]).as_deref() == Some("result"))
}

trait ReadTail {
    fn seek_read_tail(&mut self, length: u64) -> io::Result<()>;
}

impl ReadTail for fs::File {
    fn seek_read_tail(&mut self, length: u64) -> io::Result<()> {
        use std::io::Seek;
        self.seek(std::io::SeekFrom::Start(length.saturating_sub(16 * 1024)))
            .map(|_| ())
    }
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    let mut nested = value;
    for key in keys {
        nested = nested.get(*key)?;
    }
    nested
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_timestamp(value: &Value) -> Option<u64> {
    if let Some(timestamp) = value.as_u64() {
        return Some(timestamp);
    }
    let raw = value.as_str()?;
    let timestamp =
        time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
            .ok()?
            .unix_timestamp();
    u64::try_from(timestamp).ok()
}

fn modified_unix_seconds(path: &Path) -> io::Result<Option<u64>> {
    match fs::metadata(path) {
        Ok(metadata) => metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|duration| Some(duration.as_secs()))
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn load_memory(path: &Path) -> io::Result<WorkerMemory> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WorkerMemory::default()),
        Err(error) => Err(error),
    }
}

fn save_memory(path: &Path, memory: &WorkerMemory) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(memory).map_err(io::Error::other)?;
    bytes.push(b'\n');
    fs::write(path, bytes)
}

fn unix_seconds() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "herdr-worker-watchdog-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn codex_discovery_uses_parent_and_trace_activity() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-1");
        fs::create_dir_all(run_dir.join("turns/turn-1")).expect("create run files");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent_session":"parent-1","state":"active","exit_code":null}"#,
        )
        .expect("write run state");
        fs::write(run_dir.join("turns/turn-1/trace.log"), "tool call").expect("write trace");
        let workers = discover_codex_runs(dir.path()).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-1");
        assert_eq!(workers[0].source, "codex");
        assert!(!workers[0].finished);
        assert!(workers[0].last_activity > 0);
    }

    #[test]
    fn codex_discovery_reads_nested_parent_and_rfc3339_progress() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-nested");
        fs::create_dir_all(&run_dir).expect("create run directory");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent":{"session":"parent-nested"},"state":"active","last_heartbeat":"2026-09-29T07:00:00Z","progress_at":"2026-09-29T06:45:00Z"}"#,
        )
        .expect("write run state");
        let workers = discover_codex_runs(dir.path()).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-nested");
        assert_eq!(workers[0].last_activity, 1_790_664_300);
    }

    #[test]
    fn codex_discovery_ignores_non_directory_turn_entries() {
        let dir = TestDir::new();
        let run_dir = dir.path().join("ra-run-file-turn");
        fs::create_dir_all(run_dir.join("turns")).expect("create turns directory");
        fs::write(
            run_dir.join("state.json"),
            r#"{"parent_session":"parent-1","state":"active","exit_code":null}"#,
        )
        .expect("write run state");
        fs::write(run_dir.join("turns/receipt.json"), "{}").expect("write receipt");
        let workers = discover_codex_runs(dir.path()).expect("discover workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "parent-1");
    }

    #[test]
    fn claude_discovery_ignores_finished_subagent_transcripts() {
        let dir = TestDir::new();
        let transcript = dir.path().join("project/session-1/subagents/agent-1.jsonl");
        fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("create subagent directory");
        fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}\n",
        )
        .expect("write transcript");
        let workers = discover_claude_subagents(dir.path()).expect("discover Claude workers");
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].parent_session, "session-1");
        assert!(workers[0].finished);
    }

    #[test]
    fn worker_options_support_the_separate_dry_run_and_stall_threshold() {
        let options = parse_worker_options(&[
            "--once".into(),
            "--dry-run".into(),
            "--stall-minutes".into(),
            "7".into(),
        ])
        .expect("parse worker options");
        assert!(options.once);
        assert!(options.dry_run);
        assert_eq!(options.interval_secs, 30);
        assert_eq!(options.stall_secs, 420);
    }
}
