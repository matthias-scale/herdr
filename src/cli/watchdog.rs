use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::schema::{
        AgentStatus, Method, PaneAgentState, PaneListParams, PaneReadParams, PaneReportAgentParams,
        ReadFormat, ReadSource, Request,
    },
    watchdog::{
        self, DecisionStatus, Memory, PaneSample, ScanOptions, ScanResult, Verdict, WATCHDOG_SOURCE,
    },
};

const DEFAULT_INTERVAL_SECS: u64 = 300;
const DEFAULT_STALL_SECS: u64 = 600;
const DEFAULT_LINES: u32 = 40;
const DEFAULT_MAX_MODEL_CALLS: usize = 5;
const MODEL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct WatchdogOptions {
    once: bool,
    dry_run: bool,
    interval_secs: u64,
    stall_secs: u64,
    lines: u32,
    no_model: bool,
    max_model_calls: usize,
    codex_bin: PathBuf,
    state_file: PathBuf,
    json: bool,
}

#[derive(Debug, Deserialize)]
struct PaneEntry {
    pane_id: String,
    agent: Option<String>,
    agent_status: AgentStatus,
}

#[derive(Debug, Serialize)]
struct ScanSummary {
    panes: usize,
    blocked: usize,
    ambiguous: usize,
    ok: usize,
    model_calls: usize,
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn run_watchdog_command(args: &[String]) -> io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        return print_watchdog_help();
    }
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return Ok(2);
        }
    };
    if options.once {
        return run_scan(&options);
    }

    loop {
        if let Err(error) = run_scan(&options) {
            eprintln!("watchdog scan failed: {error}");
        }
        thread::sleep(Duration::from_secs(options.interval_secs));
    }
}

fn print_watchdog_help() -> io::Result<i32> {
    let mut command = super::spec::watchdog_command();
    command.set_bin_name("herdr watchdog");
    let mut stdout = io::stdout().lock();
    crate::platform::begin_cli_output();
    command.write_long_help(&mut stdout)?;
    writeln!(stdout)?;
    Ok(0)
}

fn parse_options(args: &[String]) -> Result<WatchdogOptions, String> {
    let mut options = WatchdogOptions {
        once: false,
        dry_run: false,
        interval_secs: DEFAULT_INTERVAL_SECS,
        stall_secs: DEFAULT_STALL_SECS,
        lines: DEFAULT_LINES,
        no_model: false,
        max_model_calls: DEFAULT_MAX_MODEL_CALLS,
        codex_bin: PathBuf::from("codex"),
        state_file: crate::config::state_dir().join("watchdog.json"),
        json: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--once" => options.once = true,
            "--dry-run" => options.dry_run = true,
            "--no-model" => options.no_model = true,
            "--json" => options.json = true,
            "--interval-secs" => {
                options.interval_secs = parse_value(args, &mut index, "--interval-secs")?;
                if options.interval_secs == 0 {
                    return Err("--interval-secs must be greater than zero".into());
                }
            }
            "--stall-secs" => {
                options.stall_secs = parse_value(args, &mut index, "--stall-secs")?;
            }
            "--lines" => options.lines = parse_value(args, &mut index, "--lines")?,
            "--max-model-calls" => {
                options.max_model_calls = parse_value(args, &mut index, "--max-model-calls")?;
            }
            "--codex-bin" => {
                options.codex_bin = PathBuf::from(parse_string(args, &mut index, "--codex-bin")?);
            }
            "--state-file" => {
                options.state_file = PathBuf::from(parse_string(args, &mut index, "--state-file")?);
            }
            unknown => return Err(format!("unknown watchdog option: {unknown}")),
        }
        index += 1;
    }
    Ok(options)
}

fn parse_value<T>(args: &[String], index: &mut usize, option: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = parse_string(args, index, option)?;
    raw.parse::<T>()
        .map_err(|error| format!("invalid value for {option}: {error}"))
}

fn parse_string(args: &[String], index: &mut usize, option: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("missing value for {option}"))
}

fn run_scan(options: &WatchdogOptions) -> io::Result<i32> {
    let mut memory = load_memory(&options.state_file)?;
    let pane_list = super::send_request(&Request {
        id: next_request_id("pane-list"),
        method: Method::PaneList(PaneListParams { workspace_id: None }),
    })?;
    ensure_api_success(&pane_list)?;
    let panes: Vec<PaneEntry> =
        serde_json::from_value(pane_list["result"]["panes"].clone()).map_err(io::Error::other)?;
    let listed_pane_ids = panes
        .iter()
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();

    let mut samples = Vec::new();
    for pane in panes {
        if pane.agent.is_none() || !watchdog::status_in_scope(pane.agent_status) {
            continue;
        }
        let (tail, read_error) = match read_tail(&pane.pane_id, options.lines) {
            Ok(tail) => (tail, None),
            Err(error) => (String::new(), Some(error)),
        };
        samples.push(PaneSample {
            pane_id: pane.pane_id,
            agent: pane.agent,
            status: pane.agent_status,
            tail,
            read_error,
        });
    }

    let result = watchdog::scan_decisions(
        &listed_pane_ids,
        &samples,
        &mut memory,
        unix_seconds()?,
        ScanOptions {
            stall_secs: options.stall_secs,
            max_model_calls: options.max_model_calls,
            no_model: options.no_model,
            dry_run: options.dry_run,
        },
        |agent, tail| {
            run_model_classifier(&options.codex_bin, agent, tail).map_err(|error| error.to_string())
        },
        mark_blocked,
    );

    if !options.dry_run {
        save_memory(&options.state_file, &memory)?;
    }
    print_scan(&result, options)?;
    Ok(
        if result
            .decisions
            .iter()
            .any(|decision| decision.write_error.is_some())
        {
            1
        } else {
            0
        },
    )
}

fn read_tail(pane_id: &str, lines: u32) -> Result<String, String> {
    let response = super::send_request(&Request {
        id: next_request_id("pane-read"),
        method: Method::PaneRead(PaneReadParams {
            pane_id: pane_id.to_string(),
            source: ReadSource::Recent,
            lines: Some(lines),
            format: ReadFormat::Text,
            strip_ansi: true,
            intent: Default::default(),
        }),
    })
    .map_err(|error| error.to_string())?;
    ensure_api_success(&response).map_err(|error| error.to_string())?;
    response["result"]["read"]["text"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "pane.read response did not contain result.read.text".into())
}

/// This is the single write path for watchdog blocked status. A sibling branch
/// adds a status-log watchdog write path; switch over here when that lands.
fn mark_blocked(pane_id: &str, agent: &str, reason: &str) -> io::Result<()> {
    let response = super::send_request(&Request {
        id: next_request_id("pane-report-agent"),
        method: Method::PaneReportAgent(PaneReportAgentParams {
            pane_id: pane_id.to_string(),
            source: WATCHDOG_SOURCE.to_string(),
            agent: agent.to_string(),
            state: PaneAgentState::Blocked,
            v: None,
            message: Some(reason.to_string()),
            seq: None,
            wait: None,
            eta_s: None,
            reported_at: None,
            agent_session_id: None,
            agent_session_path: None,
            gates: None,
            items: None,
            decisions: None,
            completion: None,
            external_wait: None,
            parse_status: None,
            workers_unknown: None,
            agents: None,
        }),
    })?;
    ensure_api_success(&response)
}

fn ensure_api_success(response: &Value) -> io::Result<()> {
    if response.get("error").is_some() {
        return Err(io::Error::other(format!(
            "Herdr API error: {}",
            response["error"]
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| response["error"].as_str().unwrap_or("request failed"))
        )));
    }
    Ok(())
}

fn next_request_id(operation: &str) -> String {
    let sequence = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    format!("cli:watchdog:{operation}:{sequence}")
}

fn load_memory(path: &Path) -> io::Result<Memory> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Memory::new()),
        Err(error) => Err(error),
    }
}

fn save_memory(path: &Path, memory: &Memory) -> io::Result<()> {
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
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs())
}

fn print_scan(result: &ScanResult, options: &WatchdogOptions) -> io::Result<()> {
    let summary = summarize(result);
    if options.json {
        let output = serde_json::json!({
            "decisions": result.decisions,
            "summary": summary,
            "dry_run": options.dry_run,
        });
        println!(
            "{}",
            serde_json::to_string(&output).map_err(io::Error::other)?
        );
        return Ok(());
    }

    for decision in &result.decisions {
        let status = match decision.agent_status {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Stale => "stale",
            AgentStatus::Unknown => "unknown",
        };
        let verdict = match decision.status {
            DecisionStatus::Blocked => "blocked",
            DecisionStatus::Ambiguous => "ambiguous",
            DecisionStatus::Ok => "ok",
        };
        let mut reason = decision.reason.clone();
        if let Some(error) = &decision.write_error {
            reason.push_str(&format!("; report failed: {error}"));
        }
        let dry_run = if options.dry_run { " [dry-run]" } else { "" };
        println!(
            "pane {} {} {} -> {} ({}){}",
            decision.pane_id, decision.agent, status, verdict, reason, dry_run
        );
    }
    println!(
        "watchdog summary: panes={} blocked={} ambiguous={} ok={} model_calls={}{}",
        summary.panes,
        summary.blocked,
        summary.ambiguous,
        summary.ok,
        summary.model_calls,
        if options.dry_run { " dry-run" } else { "" }
    );
    Ok(())
}

fn summarize(result: &ScanResult) -> ScanSummary {
    let mut summary = ScanSummary {
        panes: result.decisions.len(),
        blocked: 0,
        ambiguous: 0,
        ok: 0,
        model_calls: result.model_calls,
    };
    for decision in &result.decisions {
        match decision.status {
            DecisionStatus::Blocked => summary.blocked += 1,
            DecisionStatus::Ambiguous => summary.ambiguous += 1,
            DecisionStatus::Ok => summary.ok += 1,
        }
    }
    summary
}

struct TempOutput(PathBuf);

impl Drop for TempOutput {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::debug!(
                    path = %self.0.display(),
                    %error,
                    "failed to remove watchdog classifier output"
                );
            }
        }
    }
}

fn temp_output_file() -> io::Result<TempOutput> {
    for _ in 0..64 {
        let sequence = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "herdr-watchdog-{}-{sequence}.txt",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(TempOutput(path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary classifier output file",
    ))
}

fn run_model_classifier(codex_bin: &Path, agent: &str, tail: &str) -> io::Result<Verdict> {
    let output_file = temp_output_file()?;
    let prompt = watchdog::classifier_prompt(agent, tail);
    let mut child = Command::new(codex_bin)
        .arg("exec")
        .arg("-m")
        .arg("gpt-6-luna")
        .arg("-c")
        .arg("model_reasoning_effort=\"low\"")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--skip-git-repo-check")
        .arg("-o")
        .arg(&output_file.0)
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let status = wait_for_model(&mut child)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "codex classifier exited with {status}"
        )));
    }
    let reply = fs::read_to_string(&output_file.0)?;
    Ok(watchdog::parse_classifier_reply(&reply))
}

fn wait_for_model(child: &mut Child) -> io::Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= MODEL_TIMEOUT {
            let kill_result = child.kill();
            let wait_result = child.wait();
            kill_result?;
            wait_result?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "codex classifier exceeded 120 second timeout",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}
