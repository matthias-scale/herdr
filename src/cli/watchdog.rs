use std::{
    fs,
    io::{self, Read, Write},
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

mod workers;

const DEFAULT_INTERVAL_SECS: u64 = 300;
const DEFAULT_STALL_SECS: u64 = 600;
const DEFAULT_LINES: u32 = 40;
const MODEL_TIMEOUT: Duration = Duration::from_secs(120);
const STAGE2_MODEL_ID: &str = "gemini-3.1-flash-lite";

#[derive(Debug, Clone)]
struct WatchdogOptions {
    once: bool,
    dry_run: bool,
    interval_secs: u64,
    stall_secs: u64,
    lines: u32,
    no_model: bool,
    gemini_bin: PathBuf,
    state_file: PathBuf,
    status_log: PathBuf,
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
    corrected: usize,
    consistent: usize,
    ambiguous: usize,
    model_calls: usize,
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn run_watchdog_command(args: &[String]) -> io::Result<i32> {
    if args.first().is_some_and(|arg| arg == "workers") {
        return workers::run_worker_watchdog_command(&args[1..]);
    }
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
        gemini_bin: PathBuf::from("gemini"),
        state_file: crate::config::state_dir().join("watchdog.json"),
        status_log: crate::config::state_dir().join("watchdog-status-log.jsonl"),
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
            "--gemini-bin" => {
                options.gemini_bin = PathBuf::from(parse_string(args, &mut index, "--gemini-bin")?);
            }
            "--state-file" => {
                options.state_file = PathBuf::from(parse_string(args, &mut index, "--state-file")?);
            }
            "--status-log" => {
                options.status_log = PathBuf::from(parse_string(args, &mut index, "--status-log")?);
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
            no_model: options.no_model,
            dry_run: options.dry_run,
        },
        |samples| {
            run_model_classifier(&options.gemini_bin, samples).map_err(|error| error.to_string())
        },
        |pane_id, agent, old_state, new_state, evidence| {
            report_status(pane_id, agent, new_state, evidence)?;
            watchdog::append_status_correction(
                &options.status_log,
                pane_id,
                agent,
                old_state,
                new_state,
                evidence,
            )
        },
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

fn report_status(
    pane_id: &str,
    agent: &str,
    status: AgentStatus,
    evidence: &str,
) -> io::Result<()> {
    let state = match status {
        AgentStatus::Working => PaneAgentState::Working,
        AgentStatus::Done => PaneAgentState::Idle,
        AgentStatus::Blocked => PaneAgentState::Blocked,
        AgentStatus::Idle | AgentStatus::Stale | AgentStatus::Unknown => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "watchdog can only correct to working, done, or blocked",
            ));
        }
    };
    let response = super::send_request(&Request {
        id: next_request_id("pane-report-agent"),
        method: Method::PaneReportAgent(PaneReportAgentParams {
            pane_id: pane_id.to_string(),
            source: WATCHDOG_SOURCE.to_string(),
            agent: agent.to_string(),
            state,
            v: None,
            message: Some(evidence.to_string()),
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
        let old_state = match decision.old_state {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Stale => "stale",
            AgentStatus::Unknown => "unknown",
        };
        let new_state = decision
            .new_state
            .map_or("unverified", |state| match state {
                AgentStatus::Idle => "idle",
                AgentStatus::Working => "working",
                AgentStatus::Blocked => "blocked",
                AgentStatus::Done => "done",
                AgentStatus::Stale => "stale",
                AgentStatus::Unknown => "unknown",
            });
        let outcome = match decision.status {
            DecisionStatus::Corrected => "corrected",
            DecisionStatus::Consistent => "verified",
            DecisionStatus::Ambiguous => "ambiguous",
        };
        let mut evidence = decision.evidence.clone();
        if let Some(error) = &decision.write_error {
            evidence.push_str(&format!("; correction failed: {error}"));
        }
        let dry_run = if options.dry_run { " [dry-run]" } else { "" };
        println!(
            "pane {} {} {} -> {} {} ({}){}",
            decision.pane_id, decision.agent, old_state, new_state, outcome, evidence, dry_run
        );
    }
    println!(
        "watchdog summary: panes={} corrected={} consistent={} ambiguous={} model_calls={}{}",
        summary.panes,
        summary.corrected,
        summary.consistent,
        summary.ambiguous,
        summary.model_calls,
        if options.dry_run { " dry-run" } else { "" }
    );
    Ok(())
}

fn summarize(result: &ScanResult) -> ScanSummary {
    let mut summary = ScanSummary {
        panes: result.decisions.len(),
        corrected: 0,
        consistent: 0,
        ambiguous: 0,
        model_calls: result.model_calls,
    };
    for decision in &result.decisions {
        match decision.status {
            DecisionStatus::Corrected => summary.corrected += 1,
            DecisionStatus::Consistent => summary.consistent += 1,
            DecisionStatus::Ambiguous => summary.ambiguous += 1,
        }
    }
    summary
}

fn run_model_classifier(
    gemini_bin: &Path,
    samples: &[PaneSample],
) -> io::Result<std::collections::HashMap<String, watchdog::StatusClassification>> {
    let prompt = watchdog::classifier_prompt(samples);
    let mut child = Command::new(gemini_bin)
        .arg("--model")
        .arg(STAGE2_MODEL_ID)
        .arg("--prompt")
        .arg(prompt)
        .arg("--output-format")
        .arg("text")
        .arg("--approval-mode")
        .arg("default")
        .arg("--skip-trust")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Gemini classifier stdout was not captured"))?;
    let reader = thread::spawn(move || {
        let mut reply = String::new();
        stdout.read_to_string(&mut reply).map(|()| reply)
    });
    let status = wait_for_model(&mut child)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Gemini classifier exited with {status}"
        )));
    }
    let reply = reader
        .join()
        .map_err(|_| io::Error::other("Gemini classifier output reader panicked"))??;
    let pane_ids = samples
        .iter()
        .map(|sample| sample.pane_id.clone())
        .collect::<Vec<_>>();
    Ok(watchdog::parse_classifier_reply(&reply, &pane_ids))
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
                "Gemini classifier exceeded 120 second timeout",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}
