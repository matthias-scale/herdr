use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::api::client::{ApiClient, ConnectionTarget};
use crate::api::schema::{AgentInfo, AgentStatus, EmptyParams, Method, Request, ResponseResult};
use crate::config::{FleetConfig, FleetHostConfig};

const REMOTE_RUNS_MARKER: &[u8] = b"\x1eHERDR_FLEET_RUNS_V1\x1e\n";
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 60_000;
type ParsedRemoteOutput = (
    Result<Vec<AgentInfo>, String>,
    Vec<Result<RunState, String>>,
);

pub(super) fn run_fleet_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("status") => fleet_status(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_fleet_help();
            Ok(0)
        }
        _ => {
            print_fleet_help();
            Ok(2)
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct StatusOptions {
    hosts: Option<HashSet<String>>,
    json: bool,
    blocked_only: bool,
    watch: bool,
}

fn fleet_status(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_status_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "usage: herdr fleet status [--hosts NAMES] [--json] [--blocked-only] [--watch]"
            );
            return Ok(2);
        }
    };
    let loaded = crate::config::Config::load();
    let fleet = loaded.config.remote.fleet;
    let hosts = match select_hosts(&fleet, options.hosts.as_ref()) {
        Ok(hosts) => hosts,
        Err(message) => {
            eprintln!("fleet config error: {message}");
            return Ok(2);
        }
    };

    if options.watch {
        return watch_status(hosts, fleet, options.blocked_only);
    }

    let mut rows = collect_rows(&hosts, &fleet);
    if options.blocked_only {
        rows.retain(|row| row.blocked);
    }
    sort_rows(&mut rows);
    if options.json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print_table(&rows);
    }
    Ok(0)
}

fn parse_status_options(args: &[String]) -> Result<StatusOptions, String> {
    let args = super::expand_equals_args(args, &["--hosts"]);
    let mut options = StatusOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--hosts" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --hosts".to_string())?;
                let names = value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect::<HashSet<_>>();
                if names.is_empty() {
                    return Err("--hosts requires at least one host name".into());
                }
                options.hosts = Some(names);
                index += 2;
            }
            "--json" => {
                options.json = true;
                index += 1;
            }
            "--blocked-only" => {
                options.blocked_only = true;
                index += 1;
            }
            "--watch" => {
                options.watch = true;
                index += 1;
            }
            "help" | "--help" | "-h" => return Err("help requested".into()),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    if options.watch && options.json {
        return Err("--watch already emits JSON Lines; omit --json".into());
    }
    Ok(options)
}

fn select_hosts(
    fleet: &FleetConfig,
    selected: Option<&HashSet<String>>,
) -> Result<Vec<FleetHostConfig>, String> {
    if fleet.hosts.is_empty() {
        return Err("no [[remote.fleet.hosts]] entries configured".into());
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&fleet.timeout_ms) {
        return Err(format!(
            "remote.fleet.timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
        ));
    }
    if fleet.heartbeat_stale_ms < MIN_TIMEOUT_MS {
        return Err(format!(
            "remote.fleet.heartbeat_stale_ms must be at least {MIN_TIMEOUT_MS}"
        ));
    }

    let mut names = HashSet::new();
    for host in &fleet.hosts {
        if host.name.trim().is_empty() {
            return Err("every fleet host needs a non-empty name".into());
        }
        if !names.insert(host.name.clone()) {
            return Err(format!("duplicate fleet host name: {}", host.name));
        }
        if !host.local && host.target.trim().is_empty() {
            return Err(format!("fleet host {} needs an SSH target", host.name));
        }
        if host.target.starts_with('-') {
            return Err(format!(
                "fleet host {} has an invalid SSH target",
                host.name
            ));
        }
    }

    if let Some(selected) = selected {
        let unknown = selected.difference(&names).cloned().collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(format!("unknown fleet host(s): {}", unknown.join(",")));
        }
    }

    let hosts = fleet
        .hosts
        .iter()
        .filter(|host| selected.is_none_or(|names| names.contains(&host.name)))
        .cloned()
        .collect::<Vec<_>>();
    if hosts.is_empty() {
        return Err("host selection is empty".into());
    }
    Ok(hosts)
}

#[derive(Debug)]
struct HostEvidence {
    host: FleetHostConfig,
    agents: Result<Vec<AgentInfo>, String>,
    runs: Vec<Result<RunState, String>>,
}

fn collect_rows(hosts: &[FleetHostConfig], fleet: &FleetConfig) -> Vec<FleetRow> {
    let timeout = Duration::from_millis(fleet.timeout_ms);
    let evidence = std::thread::scope(|scope| {
        let handles = hosts
            .iter()
            .cloned()
            .map(|host| {
                let name = host.name.clone();
                (name, scope.spawn(move || fetch_host(host, timeout)))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(name, handle)| match handle.join() {
                Ok(evidence) => evidence,
                Err(_) => HostEvidence {
                    host: FleetHostConfig {
                        name,
                        ..FleetHostConfig::default()
                    },
                    agents: Err("host reader panicked".into()),
                    runs: Vec::new(),
                },
            })
            .collect::<Vec<_>>()
    });

    let now_s = unix_now_s();
    let heartbeat_stale_s = fleet.heartbeat_stale_ms.div_ceil(1_000);
    let mut rows = Vec::new();
    for evidence in evidence {
        match evidence.agents {
            Ok(agents) => {
                rows.extend(
                    agents
                        .into_iter()
                        .map(|agent| FleetRow::from_agent(&evidence.host.name, agent, now_s)),
                );
            }
            Err(error) => rows.push(FleetRow::host_unknown(&evidence.host.name, error)),
        }
        for run in evidence.runs {
            match run {
                Ok(run) => rows.push(FleetRow::from_run(
                    &evidence.host.name,
                    run,
                    now_s,
                    heartbeat_stale_s,
                )),
                Err(error) => rows.push(FleetRow::run_error(&evidence.host.name, error)),
            }
        }
    }
    score_descendant_closure(&mut rows);
    rows
}

fn fetch_host(host: FleetHostConfig, timeout: Duration) -> HostEvidence {
    if host.local {
        fetch_local_host(host, timeout)
    } else {
        fetch_remote_host(host, timeout)
    }
}

fn fetch_local_host(host: FleetHostConfig, timeout: Duration) -> HostEvidence {
    let client = host.socket.as_ref().map_or_else(ApiClient::local, |path| {
        ApiClient::for_target(ConnectionTarget::SocketPath(PathBuf::from(path)))
    });
    let request = Request {
        id: "cli:fleet:status:local".into(),
        method: Method::AgentList(EmptyParams::default()),
    };
    let agents = client
        .request_value_with_timeout(&request, timeout)
        .map_err(|error| error.to_string())
        .and_then(|value| {
            crate::api::client::parse_response_value(value)
                .map_err(|error| error.to_string())
                .and_then(|response| match response.result {
                    ResponseResult::AgentList { agents } => Ok(agents),
                    other => Err(format!("unexpected agent-list response: {other:?}")),
                })
        });
    let runs = local_run_states();
    HostEvidence { host, agents, runs }
}

fn local_run_states() -> Vec<Result<RunState, String>> {
    let Some(home) = std::env::var_os("HOME") else {
        return vec![Err("HOME is unavailable; cannot read ~/.agents/runs".into())];
    };
    read_run_state_dir(&PathBuf::from(home).join(".agents/runs"))
}

fn read_run_state_dir(root: &Path) -> Vec<Result<RunState, String>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => return vec![Err(format!("cannot read {}: {error}", root.display()))],
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("state.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            std::fs::read(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))
                .and_then(|bytes| parse_run_state(&bytes, &path.display().to_string()))
        })
        .collect()
}

fn fetch_remote_host(host: FleetHostConfig, timeout: Duration) -> HostEvidence {
    let script = remote_read_script(host.socket.as_deref());
    let output = run_ssh_with_timeout(&host.target, &script, timeout);
    let (agents, runs) = match output {
        Ok(output) => parse_remote_output(&output),
        Err(error) => (Err(error), Vec::new()),
    };
    HostEvidence { host, agents, runs }
}

fn remote_read_script(socket: Option<&str>) -> String {
    let socket = socket
        .map(|path| format!("export HERDR_SOCKET_PATH={}\n", shell_quote(path)))
        .unwrap_or_default();
    format!(
        "set -u\n{socket}herdr agent list || exit $?\nprintf '\\036HERDR_FLEET_RUNS_V1\\036\\n'\nif [ -d \"$HOME/.agents/runs\" ]; then\n  find \"$HOME/.agents/runs\" -mindepth 2 -maxdepth 2 -type f -name state.json -exec cat {{}} \\; -exec printf '\\n' \\;\nfi\n"
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run_ssh_with_timeout(target: &str, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let connect_timeout = timeout.as_secs().max(1).to_string();
    let mut child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o"])
        .arg(format!("ConnectTimeout={connect_timeout}"))
        .args(["-o", "ServerAliveInterval=2", "-o", "ServerAliveCountMax=1"])
        .arg(target)
        .args(["sh", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start ssh: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|error| format!("failed to send remote read script: {error}"))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ssh stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ssh stderr unavailable".to_string())?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(format!(
                    "STATUS UNKNOWN: host read timed out after {}ms",
                    timeout.as_millis()
                ));
            }
            Err(error) => return Err(format!("failed to wait for ssh: {error}")),
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "ssh stdout reader panicked".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "ssh stderr reader panicked".to_string())??;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("STATUS UNKNOWN: ssh exited with {status}")
        } else {
            format!("STATUS UNKNOWN: {detail}")
        });
    }
    Ok(stdout)
}

fn read_all(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn parse_remote_output(output: &[u8]) -> ParsedRemoteOutput {
    let Some(marker_at) = output
        .windows(REMOTE_RUNS_MARKER.len())
        .position(|window| window == REMOTE_RUNS_MARKER)
    else {
        return (
            Err("remote output did not include the fleet read marker".into()),
            Vec::new(),
        );
    };
    let response = serde_json::from_slice(output[..marker_at].trim_ascii())
        .map_err(|error| format!("invalid remote agent-list JSON: {error}"))
        .and_then(|value| {
            crate::api::client::parse_response_value(value)
                .map_err(|error| error.to_string())
                .and_then(|response| match response.result {
                    ResponseResult::AgentList { agents } => Ok(agents),
                    other => Err(format!("unexpected agent-list response: {other:?}")),
                })
        });
    let state_bytes = &output[marker_at + REMOTE_RUNS_MARKER.len()..];
    let runs = serde_json::Deserializer::from_slice(state_bytes)
        .into_iter::<serde_json::Value>()
        .enumerate()
        .map(|(index, value)| {
            value
                .map_err(|error| format!("invalid remote run state #{}: {error}", index + 1))
                .and_then(|value| {
                    serde_json::to_vec(&value)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            parse_run_state(&bytes, &format!("remote run state #{}", index + 1))
                        })
                })
        })
        .collect();
    (response, runs)
}

fn parse_run_state(bytes: &[u8], source: &str) -> Result<RunState, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid run state {source}: {error}"))?;
    let schema = value.get("schema").and_then(serde_json::Value::as_u64);
    if schema != Some(1) {
        return Err(format!(
            "rejected run state {source}: schema {} is unsupported; expected 1",
            schema.map_or_else(|| "missing".into(), |value| value.to_string())
        ));
    }
    let run: RunState = serde_json::from_value(value)
        .map_err(|error| format!("invalid run state {source}: {error}"))?;
    if run.run_id.len() > 64
        || run.run_id.is_empty()
        || !run
            .run_id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        || !run.run_id.starts_with("ra-")
    {
        return Err(format!("invalid run state {source}: invalid run_id"));
    }
    for (field, timestamp) in [
        ("started_at", Some(run.started_at.as_str())),
        ("last_heartbeat", Some(run.last_heartbeat.as_str())),
        ("blocked_since", run.blocked_since.as_deref()),
    ] {
        if timestamp.is_some_and(|timestamp| parse_utc_timestamp(timestamp).is_none()) {
            return Err(format!(
                "invalid run state {source}: {field} must be RFC3339 UTC with Z"
            ));
        }
    }
    Ok(run)
}

// Schema v1 fields are intentionally all deserialized even when the current table does not
// display them. This makes malformed producer output fail at the consumer boundary.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct RunState {
    schema: u32,
    run_id: String,
    host: String,
    agent: String,
    model: String,
    effort: String,
    label: String,
    task: String,
    cwd: String,
    repo: String,
    branch: String,
    pid: u32,
    started_at: String,
    last_heartbeat: String,
    phase: String,
    state: RunStateKind,
    blocked_reason: Option<BlockedReason>,
    blocked_since: Option<String>,
    exit_code: Option<i32>,
    exit_reason: Option<ExitReason>,
    log_path: String,
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    cost_usd: Option<f64>,
    tool_calls: Option<u64>,
    parent: RunParent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum RunStateKind {
    Active,
    Blocked,
    Waiting,
    Done,
    Failed,
    Unknown,
}

impl RunStateKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum BlockedReason {
    Approval,
    Stalled,
    Loop,
}

impl BlockedReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Stalled => "stalled",
            Self::Loop => "loop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExitReason {
    Completed,
    Failed,
    Killed,
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
struct RunParent {
    host: String,
    run_id: Option<String>,
    session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Liveness {
    Live,
    Terminal,
    Unknown,
}

impl Liveness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Terminal => "terminal",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct DescendantScore {
    live: usize,
    terminal: usize,
    unknown: usize,
    blocked: usize,
}

impl DescendantScore {
    fn observe(&mut self, liveness: Liveness, blocked: bool) {
        match liveness {
            Liveness::Live => self.live += 1,
            Liveness::Terminal => self.terminal += 1,
            Liveness::Unknown => self.unknown += 1,
        }
        self.blocked += usize::from(blocked);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceSource {
    Herdr,
    RunState,
    Host,
}

impl EvidenceSource {
    fn table_label(&self) -> &'static str {
        match self {
            Self::Herdr => "pane",
            Self::RunState => "run",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct FleetGate {
    n: u32,
    label: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recommendation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FleetRow {
    host: String,
    source: EvidenceSource,
    handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work: Option<String>,
    state: String,
    raw_state: String,
    liveness: Liveness,
    blocked: bool,
    closure_liveness: Liveness,
    closure_blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    gates: Vec<FleetGate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_change_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_handle: Option<String>,
    descendants: DescendantScore,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip)]
    native_session: Option<String>,
}

impl FleetRow {
    fn from_agent(host: &str, agent: AgentInfo, now_s: u64) -> Self {
        let liveness = match agent.agent_status {
            AgentStatus::Idle | AgentStatus::Working | AgentStatus::Blocked => Liveness::Live,
            AgentStatus::Done => Liveness::Terminal,
            AgentStatus::Stale | AgentStatus::Unknown => Liveness::Unknown,
        };
        let blocked = agent.agent_status == AgentStatus::Blocked || !agent.gates.is_empty();
        let raw_state = agent_status_str(agent.agent_status).to_string();
        let state = effective_state(&raw_state, liveness, blocked);
        let reported_at = agent.reported_at.clone();
        let age_s = reported_at
            .as_deref()
            .and_then(parse_utc_timestamp)
            .and_then(|reported| now_s.checked_sub(reported));
        let gates = agent
            .gates
            .iter()
            .map(|gate| FleetGate {
                n: gate.n,
                label: gate.label.clone(),
                text: gate.text.clone(),
                pr: gate.pr,
                recommendation: gate_recommendation(&gate.text),
            })
            .collect::<Vec<_>>();
        let gate_summary = gates.first().map(gate_summary);
        let id = agent.name.clone().unwrap_or_else(|| agent.pane_id.clone());
        let handle = format!("{host}/{id}");
        let model = agent.tokens.get("model").cloned();
        let effort = agent.tokens.get("effort").cloned();
        let work = agent_work(&agent);
        let native_session = agent
            .agent_session
            .as_ref()
            .map(|session| session.value.clone());
        Self {
            host: host.into(),
            source: EvidenceSource::Herdr,
            handle,
            agent: agent.agent,
            name: Some(id),
            model,
            effort,
            work,
            state,
            raw_state,
            liveness,
            blocked,
            closure_liveness: liveness,
            closure_blocked: blocked,
            age_s,
            reported_at,
            gates,
            gate_summary,
            blocked_reason: None,
            state_change_seq: Some(agent.state_change_seq),
            parent_handle: None,
            descendants: DescendantScore::default(),
            error: None,
            native_session,
        }
    }

    fn from_run(host: &str, run: RunState, now_s: u64, heartbeat_stale_s: u64) -> Self {
        let heartbeat_at = parse_utc_timestamp(&run.last_heartbeat);
        let age_s = heartbeat_at.and_then(|heartbeat| now_s.checked_sub(heartbeat));
        let fresh = age_s.is_some_and(|age| age <= heartbeat_stale_s);
        let blocked = run.state == RunStateKind::Blocked;
        let liveness = match run.state {
            RunStateKind::Active | RunStateKind::Blocked | RunStateKind::Waiting if fresh => {
                Liveness::Live
            }
            RunStateKind::Done | RunStateKind::Failed => Liveness::Terminal,
            RunStateKind::Active
            | RunStateKind::Blocked
            | RunStateKind::Waiting
            | RunStateKind::Unknown => Liveness::Unknown,
        };
        let raw_state = run.state.as_str().to_string();
        let state = effective_state(&raw_state, liveness, blocked);
        let blocked_reason = run.blocked_reason.map(|reason| reason.as_str().to_string());
        let gate_summary = blocked_reason
            .as_ref()
            .map(|reason| format!("windowless run blocked: {reason}"));
        let parent_handle = run
            .parent
            .run_id
            .as_ref()
            .map(|run_id| format!("{}/{run_id}", run.parent.host));
        let handle = format!("{host}/{}", run.run_id);
        Self {
            host: host.into(),
            source: EvidenceSource::RunState,
            handle,
            agent: Some(run.agent),
            name: Some(run.run_id),
            model: Some(run.model),
            effort: Some(run.effort),
            work: Some(if run.branch.is_empty() {
                run.task
            } else {
                run.branch
            }),
            state,
            raw_state,
            liveness,
            blocked,
            closure_liveness: liveness,
            closure_blocked: blocked,
            age_s,
            reported_at: Some(run.last_heartbeat),
            gates: Vec::new(),
            gate_summary,
            blocked_reason,
            state_change_seq: None,
            parent_handle: parent_handle.or_else(|| {
                run.parent
                    .session
                    .map(|session| format!("session:{}/{}", run.parent.host, session))
            }),
            descendants: DescendantScore::default(),
            error: None,
            native_session: None,
        }
    }

    fn host_unknown(host: &str, error: String) -> Self {
        Self::unknown(
            host,
            EvidenceSource::Host,
            format!("{host}/STATUS_UNKNOWN"),
            error,
        )
    }

    fn run_error(host: &str, error: String) -> Self {
        Self::unknown(
            host,
            EvidenceSource::RunState,
            format!("{host}/RUN_STATE_ERROR"),
            error,
        )
    }

    fn unknown(host: &str, source: EvidenceSource, handle: String, error: String) -> Self {
        Self {
            host: host.into(),
            source,
            handle,
            agent: None,
            name: None,
            model: None,
            effort: None,
            work: None,
            state: "status_unknown".into(),
            raw_state: "unknown".into(),
            liveness: Liveness::Unknown,
            blocked: false,
            closure_liveness: Liveness::Unknown,
            closure_blocked: false,
            age_s: None,
            reported_at: None,
            gates: Vec::new(),
            gate_summary: None,
            blocked_reason: None,
            state_change_seq: None,
            parent_handle: None,
            descendants: DescendantScore::default(),
            error: Some(error),
            native_session: None,
        }
    }
}

fn agent_status_str(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Stale => "stale",
        AgentStatus::Unknown => "unknown",
    }
}

fn effective_state(raw: &str, liveness: Liveness, blocked: bool) -> String {
    match (blocked, liveness) {
        (true, Liveness::Unknown) => "blocked_liveness_unknown".into(),
        (true, _) => "blocked".into(),
        (false, Liveness::Unknown) => "status_unknown".into(),
        _ => raw.into(),
    }
}

fn agent_work(agent: &AgentInfo) -> Option<String> {
    agent
        .work_context
        .ticket_ids
        .first()
        .cloned()
        .or_else(|| agent.work_context.branch.clone())
        .or_else(|| {
            agent.work_context.pr_urls.first().map(|url| {
                url.rsplit_once("/pull/")
                    .map_or_else(|| url.clone(), |(_, number)| format!("PR #{number}"))
            })
        })
        .or_else(|| agent.work_context.work_title.clone())
}

fn gate_recommendation(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains("(a-rec)"))
        .map(normalize_line)
}

fn gate_summary(gate: &FleetGate) -> String {
    let first = gate
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(normalize_line)
        .unwrap_or_else(|| gate.label.clone());
    match gate.recommendation.as_deref() {
        Some(recommendation) if recommendation != first => format!("{first} | {recommendation}"),
        _ => first,
    }
}

fn normalize_line(line: &str) -> String {
    line.replace("**", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn score_descendant_closure(rows: &mut [FleetRow]) {
    let mut handles = HashMap::new();
    let mut sessions = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        handles.insert(row.handle.clone(), index);
        if let Some(session) = &row.native_session {
            sessions.insert(
                format!("session:{}/{}", row.host, session),
                row.handle.clone(),
            );
        }
    }
    for row in rows.iter_mut() {
        if let Some(parent) = row
            .parent_handle
            .as_ref()
            .and_then(|parent| sessions.get(parent))
        {
            row.parent_handle = Some(parent.clone());
        }
    }

    let observations = rows
        .iter()
        .map(|row| (row.parent_handle.clone(), row.liveness, row.blocked))
        .collect::<Vec<_>>();
    for (child_index, (parent, liveness, blocked)) in observations.into_iter().enumerate() {
        let mut parent = parent;
        let mut visited = HashSet::from([child_index]);
        while let Some(parent_handle) = parent {
            let Some(&parent_index) = handles.get(&parent_handle) else {
                break;
            };
            if !visited.insert(parent_index) {
                break;
            }
            rows[parent_index].descendants.observe(liveness, blocked);
            rows[parent_index].closure_blocked |= blocked;
            rows[parent_index].closure_liveness =
                closure_liveness(rows[parent_index].liveness, rows[parent_index].descendants);
            parent = rows[parent_index].parent_handle.clone();
        }
    }
}

fn closure_liveness(own: Liveness, descendants: DescendantScore) -> Liveness {
    if own == Liveness::Live || descendants.live > 0 {
        Liveness::Live
    } else if own == Liveness::Unknown || descendants.unknown > 0 {
        Liveness::Unknown
    } else {
        Liveness::Terminal
    }
}

fn sort_rows(rows: &mut [FleetRow]) {
    rows.sort_by(|left, right| {
        row_priority(left)
            .cmp(&row_priority(right))
            .then_with(|| left.host.cmp(&right.host))
            .then_with(|| left.handle.cmp(&right.handle))
    });
}

fn row_priority(row: &FleetRow) -> u8 {
    if row.blocked {
        0
    } else if matches!(row.raw_state.as_str(), "working" | "active") {
        1
    } else {
        2
    }
}

fn print_table(rows: &[FleetRow]) {
    println!(
        "{:<7} {:<5} {:<20} {:<19} {:<20} {:<25} {:>5}  GATE",
        "HOST", "SRC", "AGENT", "MODEL/EFFORT", "BRANCH/TICKET", "STATE", "AGE"
    );
    for row in rows {
        let identity = row
            .name
            .as_deref()
            .unwrap_or_else(|| row.error.as_deref().unwrap_or("STATUS UNKNOWN"));
        let model = match (row.model.as_deref(), row.effort.as_deref()) {
            (Some(model), Some(effort)) => format!("{model}/{effort}"),
            (Some(model), None) => model.into(),
            _ => "-".into(),
        };
        let state = if row.closure_liveness != row.liveness || row.closure_blocked != row.blocked {
            format!(
                "{} [tree:{}/{}]",
                human_state(&row.state),
                row.closure_liveness.as_str(),
                if row.closure_blocked {
                    "blocked"
                } else {
                    "clear"
                }
            )
        } else {
            human_state(&row.state)
        };
        let gate = row
            .gate_summary
            .as_deref()
            .or(row.error.as_deref())
            .unwrap_or("-");
        println!(
            "{:<7} {:<5} {:<20} {:<19} {:<20} {:<25} {:>5}  {}",
            truncate(&row.host, 7),
            row.source.table_label(),
            truncate(identity, 20),
            truncate(&model, 19),
            truncate(row.work.as_deref().unwrap_or("-"), 20),
            truncate(&state, 25),
            age_label(row.age_s),
            truncate(gate, 70),
        );
    }
}

fn human_state(state: &str) -> String {
    match state {
        "blocked_liveness_unknown" => "BLOCKED · UNKNOWN".into(),
        "status_unknown" => "STATUS UNKNOWN".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.into();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .chain(['…'])
        .collect()
}

fn age_label(age_s: Option<u64>) -> String {
    let Some(seconds) = age_s else {
        return "--".into();
    };
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedState {
    state: String,
    liveness: Liveness,
    blocked: bool,
    seq: Option<u64>,
    gate_summary: Option<String>,
}

impl From<&FleetRow> for ObservedState {
    fn from(row: &FleetRow) -> Self {
        Self {
            state: row.state.clone(),
            liveness: row.liveness,
            blocked: row.blocked,
            seq: row.state_change_seq,
            gate_summary: row.gate_summary.clone(),
        }
    }
}

#[derive(Serialize)]
struct TransitionEvent<'a> {
    handle: &'a str,
    old_state: &'a str,
    new_state: &'a str,
    timestamp: String,
    liveness: Liveness,
    blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_change_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate_summary: Option<&'a str>,
}

fn watch_status(
    hosts: Vec<FleetHostConfig>,
    fleet: FleetConfig,
    blocked_only: bool,
) -> std::io::Result<i32> {
    let running = Arc::new(AtomicBool::new(true));
    let signal = Arc::clone(&running);
    ctrlc::set_handler(move || signal.store(false, Ordering::SeqCst))
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let initial = collect_rows(&hosts, &fleet);
    let mut previous = initial
        .iter()
        .map(|row| (row.handle.clone(), ObservedState::from(row)))
        .collect::<HashMap<_, _>>();

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(WATCH_INTERVAL);
        if !running.load(Ordering::SeqCst) {
            break;
        }
        let current_rows = collect_rows(&hosts, &fleet);
        let current = current_rows
            .iter()
            .map(|row| (row.handle.clone(), ObservedState::from(row)))
            .collect::<HashMap<_, _>>();

        for (handle, observed) in &current {
            let Some(old) = previous.get(handle) else {
                continue;
            };
            if same_observed_state(old, observed)
                || (blocked_only && !old.blocked && !observed.blocked)
            {
                continue;
            }
            emit_transition(handle, old, observed)?;
        }
        for (handle, observed) in &current {
            if previous.contains_key(handle) || (blocked_only && !observed.blocked) {
                continue;
            }
            let absent = ObservedState {
                state: "absent".into(),
                liveness: Liveness::Unknown,
                blocked: false,
                seq: None,
                gate_summary: None,
            };
            emit_transition(handle, &absent, observed)?;
        }
        let missing = previous
            .keys()
            .filter(|handle| !current.contains_key(*handle))
            .cloned()
            .collect::<Vec<_>>();
        let mut next = current;
        for handle in missing {
            let old = &previous[&handle];
            let unknown = ObservedState {
                state: "status_unknown".into(),
                liveness: Liveness::Unknown,
                blocked: false,
                seq: None,
                gate_summary: None,
            };
            if !same_observed_state(old, &unknown) && (!blocked_only || old.blocked) {
                emit_transition(&handle, old, &unknown)?;
            }
            next.insert(handle, unknown);
        }
        previous = next;
    }
    Ok(0)
}

fn same_observed_state(left: &ObservedState, right: &ObservedState) -> bool {
    left.state == right.state && left.liveness == right.liveness && left.blocked == right.blocked
}

fn emit_transition(handle: &str, old: &ObservedState, new: &ObservedState) -> std::io::Result<()> {
    let event = TransitionEvent {
        handle,
        old_state: &old.state,
        new_state: &new.state,
        timestamp: format_utc_timestamp(unix_now_s()),
        liveness: new.liveness,
        blocked: new.blocked,
        state_change_seq: new.seq,
        gate_summary: (new.blocked && !old.blocked)
            .then_some(new.gate_summary.as_deref())
            .flatten(),
    };
    println!("{}", serde_json::to_string(&event)?);
    std::io::stdout().flush()
}

fn unix_now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn parse_utc_timestamp(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    if date.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let mut time = time.split(':');
    let hour = time.next()?.parse::<i64>().ok()?;
    let minute = time.next()?.parse::<i64>().ok()?;
    let second = time.next()?.split('.').next()?.parse::<i64>().ok()?;
    if time.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let max_day = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

fn format_utc_timestamp(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let seconds_of_day = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn print_fleet_help() {
    eprintln!("herdr fleet commands:");
    eprintln!("  herdr fleet status [--hosts NAMES] [--json] [--blocked-only] [--watch]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(status: AgentStatus, gates: serde_json::Value) -> AgentInfo {
        serde_json::from_value(serde_json::json!({
            "terminal_id": "term_1",
            "work_context": {"ticket_ids": ["SCA-1"], "pr_urls": []},
            "name": "worker",
            "agent": "codex",
            "agent_status": status,
            "reported_at": "2026-08-26T14:00:00Z",
            "gates": gates,
            "workspace_id": "w1",
            "tab_id": "t1",
            "pane_id": "p1",
            "focused": false,
            "state_change_seq": 7,
            "revision": 1
        }))
        .unwrap()
    }

    #[test]
    fn stale_agent_with_gate_is_blocked_with_unknown_liveness() {
        let agent = agent(
            AgentStatus::Stale,
            serde_json::json!([{
                "n": 1,
                "label": "Gate",
                "text": "Ship?\n(a-rec) ship after CI",
                "pr": 42
            }]),
        );
        let row = FleetRow::from_agent("ub1", agent, 1_777_000_000);
        assert!(row.blocked);
        assert_eq!(row.liveness, Liveness::Unknown);
        assert_eq!(row.state, "blocked_liveness_unknown");
        assert_eq!(
            row.gate_summary.as_deref(),
            Some("Ship? | (a-rec) ship after CI")
        );
    }

    #[test]
    fn stale_agent_without_gate_is_status_unknown() {
        let row = FleetRow::from_agent(
            "ub1",
            agent(AgentStatus::Stale, serde_json::json!([])),
            1_777_000_000,
        );
        assert!(!row.blocked);
        assert_eq!(row.liveness, Liveness::Unknown);
        assert_eq!(row.state, "status_unknown");
    }

    #[test]
    fn run_schema_mismatch_is_rejected_loudly() {
        let error = parse_run_state(br#"{"schema":2}"#, "fixture").unwrap_err();
        assert!(error.contains("schema 2 is unsupported; expected 1"));
    }

    #[test]
    fn remote_output_keeps_agent_and_run_evidence_separate() {
        let agent_response = serde_json::json!({
            "id": "x",
            "result": {"type": "agent_list", "agents": []}
        });
        let run = serde_json::json!({
            "schema": 1,
            "run_id": "ra-260826-test-a1b2c3d",
            "host": "ub1",
            "agent": "codex",
            "model": "gpt-5.6-sol",
            "effort": "high",
            "label": "[cx]",
            "task": "test",
            "cwd": "/tmp/test",
            "repo": "repo",
            "branch": "feat/test",
            "pid": 1,
            "started_at": "2026-08-26T14:00:00Z",
            "last_heartbeat": "2026-08-26T14:00:01Z",
            "phase": "implement",
            "state": "active",
            "blocked_reason": null,
            "blocked_since": null,
            "exit_code": null,
            "exit_reason": null,
            "log_path": "/tmp/out.log",
            "tokens_in": null,
            "tokens_out": null,
            "cost_usd": null,
            "tool_calls": null,
            "parent": {"host": "mac", "run_id": null, "session": null},
            "extra": true
        });
        let output = [
            serde_json::to_vec(&agent_response).unwrap(),
            REMOTE_RUNS_MARKER.to_vec(),
            serde_json::to_vec(&run).unwrap(),
        ]
        .concat();
        let (agents, runs) = parse_remote_output(&output);
        assert!(agents.unwrap().is_empty());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].as_ref().unwrap().run_id, "ra-260826-test-a1b2c3d");
    }

    #[test]
    fn done_parent_scores_live_and_blocked_descendant_closure() {
        let mut parent = FleetRow::unknown(
            "mac",
            EvidenceSource::RunState,
            "mac/parent".into(),
            "fixture".into(),
        );
        parent.liveness = Liveness::Terminal;
        parent.closure_liveness = Liveness::Terminal;
        let mut child = FleetRow::unknown(
            "ub1",
            EvidenceSource::RunState,
            "ub1/child".into(),
            "fixture".into(),
        );
        child.liveness = Liveness::Live;
        child.closure_liveness = Liveness::Live;
        child.blocked = true;
        child.closure_blocked = true;
        child.parent_handle = Some("mac/parent".into());
        let mut rows = vec![parent, child];
        score_descendant_closure(&mut rows);
        assert_eq!(rows[0].closure_liveness, Liveness::Live);
        assert!(rows[0].closure_blocked);
        assert_eq!(rows[0].descendants.live, 1);
        assert_eq!(rows[0].descendants.blocked, 1);
    }

    #[test]
    fn rfc3339_round_trip_at_contract_date() {
        let timestamp = "2026-08-26T14:05:00Z";
        let seconds = parse_utc_timestamp(timestamp).unwrap();
        assert_eq!(format_utc_timestamp(seconds), timestamp);
    }

    #[test]
    fn blocked_rows_sort_before_working_rows() {
        let mut blocked = FleetRow::from_agent(
            "ub1",
            agent(
                AgentStatus::Stale,
                serde_json::json!([{
                    "n": 1, "label": "Gate", "text": "answer"
                }]),
            ),
            1_777_000_000,
        );
        blocked.handle = "ub1/b".into();
        let working = FleetRow::from_agent(
            "mac",
            agent(AgentStatus::Working, serde_json::json!([])),
            1_777_000_000,
        );
        let mut rows = vec![working, blocked];
        sort_rows(&mut rows);
        assert_eq!(rows[0].handle, "ub1/b");
    }

    #[test]
    fn watch_deduplicates_same_state_even_when_sequence_advances() {
        let old = ObservedState {
            state: "working".into(),
            liveness: Liveness::Live,
            blocked: false,
            seq: Some(7),
            gate_summary: None,
        };
        let mut new = old.clone();
        new.seq = Some(8);
        assert!(same_observed_state(&old, &new));

        new.liveness = Liveness::Unknown;
        new.state = "status_unknown".into();
        assert!(!same_observed_state(&old, &new));
    }
}
