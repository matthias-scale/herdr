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
const REMOTE_HOST_MARKER: &[u8] = b"\x1eHERDR_FLEET_HOST_V1\x1e\n";
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MIN_REFRESH_INTERVAL_MS: u64 = 100;
type ParsedRemoteOutput = (
    Result<Vec<AgentInfo>, String>,
    Vec<Result<RunState, String>>,
    HostRuntime,
);

pub(crate) fn run_fleet_command(args: &[String]) -> std::io::Result<i32> {
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
    let mut expanded = Vec::with_capacity(args.len());
    for arg in args {
        if let Some(value) = arg.strip_prefix("--hosts=") {
            expanded.extend(["--hosts".to_string(), value.to_string()]);
        } else {
            expanded.push(arg.clone());
        }
    }
    let mut options = StatusOptions::default();
    let mut index = 0;
    while index < expanded.len() {
        match expanded[index].as_str() {
            "--hosts" => {
                let value = expanded
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

pub(crate) fn select_hosts(
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
    if fleet.refresh_interval_ms < MIN_REFRESH_INTERVAL_MS {
        return Err(format!(
            "remote.fleet.refresh_interval_ms must be at least {MIN_REFRESH_INTERVAL_MS}"
        ));
    }
    if fleet.heartbeat_stale_ms < MIN_TIMEOUT_MS {
        return Err(format!(
            "remote.fleet.heartbeat_stale_ms must be at least {MIN_TIMEOUT_MS}"
        ));
    }
    if fleet
        .self_name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty() || name.contains("::"))
    {
        return Err("remote.fleet.self_name must be non-empty and must not contain `::`".into());
    }

    let mut names = HashSet::new();
    for host in &fleet.hosts {
        if host.name.trim().is_empty() {
            return Err("every fleet host needs a non-empty name".into());
        }
        if host.name.contains("::") {
            return Err(format!(
                "fleet host {} has an invalid name: `::` is reserved for agent references",
                host.name
            ));
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HostRuntime {
    version: Option<String>,
    protocol: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostState {
    Reachable,
    Unreachable,
    VersionSkew,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HostSnapshot {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) local: bool,
    pub(crate) session: Option<String>,
    /// How to reach the host's server once ssh lands. It is connection detail
    /// rather than a runtime fact, so it stays out of the published snapshot
    /// while the local commands that dial the host can still read it.
    #[serde(skip)]
    pub(crate) socket: Option<String>,
    pub(crate) state: HostState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) protocol: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) entries: Vec<FleetRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Snapshot {
    pub(crate) polled: bool,
    #[serde(skip)]
    pub(crate) refreshed_at: Option<SystemTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refreshed_at_unix_ms: Option<u64>,
    pub(crate) configured_hosts: Vec<String>,
    pub(crate) hosts: Vec<HostSnapshot>,
}

impl Snapshot {
    pub(crate) fn unpolled(hosts: &[FleetHostConfig]) -> Self {
        Self {
            configured_hosts: hosts.iter().map(|host| host.name.clone()).collect(),
            ..Self::default()
        }
    }

    pub(crate) fn into_rows(self) -> Vec<FleetRow> {
        self.hosts
            .into_iter()
            .flat_map(|host| host.entries)
            .collect()
    }
}

pub(crate) fn poll(fleet: &FleetConfig) -> Snapshot {
    if fleet.hosts.is_empty() {
        return snapshot_from_evidence(&[], fleet, Vec::new(), SystemTime::now());
    }
    match select_hosts(fleet, None) {
        Ok(hosts) => collect_snapshot(&hosts, fleet),
        Err(error) => {
            let refreshed_at = SystemTime::now();
            Snapshot {
                polled: true,
                refreshed_at: Some(refreshed_at),
                refreshed_at_unix_ms: refreshed_at
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
                configured_hosts: fleet.hosts.iter().map(|host| host.name.clone()).collect(),
                hosts: fleet
                    .hosts
                    .iter()
                    .map(|host| HostSnapshot {
                        name: host.name.clone(),
                        target: host.target.clone(),
                        local: host.local,
                        session: host.session.clone(),
                        socket: host.socket.clone(),
                        state: HostState::Unreachable,
                        version: None,
                        protocol: None,
                        error: Some(error.clone()),
                        entries: Vec::new(),
                    })
                    .collect(),
            }
        }
    }
}

pub(crate) fn host_attach_argv(host: &HostSnapshot) -> Result<Vec<String>, String> {
    if host.local {
        return Err(format!("{} is the local host", host.name));
    }
    if host.target.trim().is_empty() {
        return Err(format!("{} has no SSH target", host.name));
    }
    let mut argv = vec![
        "herdr".to_string(),
        "--remote".to_string(),
        host.target.clone(),
    ];
    if let Some(session) = host.session.as_ref().filter(|session| !session.is_empty()) {
        argv.extend(["--session".to_string(), session.clone()]);
    }
    Ok(argv)
}

/// Build the argv that attaches one remote agent's terminal into a local pane.
///
/// `herdr --remote <target>` starts a *second* herdr TUI inside the pane and
/// offers to sync binaries with the remote host, which would stop a server that
/// is running live agents. Attaching a single agent instead streams that one
/// remote terminal and touches nothing else on the host.
pub(crate) fn agent_attach_argv(host: &HostSnapshot, agent: &str) -> Result<Vec<String>, String> {
    if host.local {
        return Err(format!("{} is the local host", host.name));
    }
    if host.target.trim().is_empty() {
        return Err(format!("{} has no SSH target", host.name));
    }
    if agent.trim().is_empty() {
        return Err(format!("{} has no agent target", host.name));
    }
    Ok(vec![
        "ssh".to_string(),
        "-t".to_string(),
        host.target.clone(),
        remote_attach_command(host.socket.as_deref(), host.session.as_deref(), agent),
    ])
}

fn remote_attach_command(socket: Option<&str>, session: Option<&str>, agent: &str) -> String {
    let socket = socket
        .map(|path| format!("HERDR_SOCKET_PATH={} ", shell_quote(path)))
        .unwrap_or_default();
    let session = session
        .map(|name| format!("HERDR_SESSION={} ", shell_quote(name)))
        .unwrap_or_default();
    format!("{socket}{session}herdr agent attach {}", shell_quote(agent))
}

pub(crate) fn start_poller(
    fleet: FleetConfig,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(move || loop {
        let snapshot = poll(&fleet);
        match event_tx.try_send(crate::events::AppEvent::FleetRefreshed { snapshot }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("dropped fleet refresh because the event queue is full");
            }
        }
        std::thread::sleep(Duration::from_millis(
            fleet.refresh_interval_ms.max(MIN_REFRESH_INTERVAL_MS),
        ));
    });
}

#[derive(Debug)]
struct HostEvidence {
    host: FleetHostConfig,
    agents: Result<Vec<AgentInfo>, String>,
    runs: Vec<Result<RunState, String>>,
    runtime: HostRuntime,
}

pub(crate) fn collect_rows(hosts: &[FleetHostConfig], fleet: &FleetConfig) -> Vec<FleetRow> {
    collect_snapshot(hosts, fleet).into_rows()
}

pub(crate) fn collect_snapshot(hosts: &[FleetHostConfig], fleet: &FleetConfig) -> Snapshot {
    collect_snapshot_with(&SystemHostReader, hosts, fleet)
}

fn collect_snapshot_with(
    reader: &impl HostReader,
    hosts: &[FleetHostConfig],
    fleet: &FleetConfig,
) -> Snapshot {
    let timeout = Duration::from_millis(fleet.timeout_ms);
    let evidence = std::thread::scope(|scope| {
        let handles = hosts
            .iter()
            .cloned()
            .map(|host| {
                let name = host.name.clone();
                (
                    name,
                    scope.spawn(move || fetch_host_with(reader, host, timeout)),
                )
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
                    runtime: HostRuntime::default(),
                },
            })
            .collect::<Vec<_>>()
    });

    snapshot_from_evidence(hosts, fleet, evidence, SystemTime::now())
}

fn snapshot_from_evidence(
    configured_hosts: &[FleetHostConfig],
    fleet: &FleetConfig,
    evidence: Vec<HostEvidence>,
    refreshed_at: SystemTime,
) -> Snapshot {
    let now_s = unix_seconds(refreshed_at);
    let heartbeat_stale_s = fleet.heartbeat_stale_ms.div_ceil(1_000);
    let mut hosts = Vec::with_capacity(evidence.len());
    for evidence in evidence {
        let error = evidence.agents.as_ref().err().cloned();
        let mut entries = Vec::new();
        match evidence.agents {
            Ok(agents) => {
                entries.extend(agents.into_iter().filter_map(|agent| {
                    FleetRow::from_agent(&evidence.host.name, evidence.host.local, agent, now_s)
                }));
            }
            Err(error) => {
                if let Some(row) = FleetRow::host_unknown(&evidence.host.name, error) {
                    entries.push(row);
                }
            }
        }
        for run in evidence.runs {
            match run {
                Ok(run) => {
                    if let Some(row) =
                        FleetRow::from_run(&evidence.host.name, run, now_s, heartbeat_stale_s)
                    {
                        entries.push(row);
                    }
                }
                Err(error) => {
                    if let Some(row) = FleetRow::run_error(&evidence.host.name, error) {
                        entries.push(row);
                    }
                }
            }
        }
        let version_skew = evidence
            .runtime
            .version
            .as_deref()
            .is_some_and(|version| version != crate::build_info::version())
            || evidence
                .runtime
                .protocol
                .is_some_and(|protocol| protocol != crate::protocol::PROTOCOL_VERSION);
        let state = if error.is_some() {
            HostState::Unreachable
        } else if version_skew {
            HostState::VersionSkew
        } else {
            HostState::Reachable
        };
        hosts.push(HostSnapshot {
            name: evidence.host.name,
            target: evidence.host.target,
            local: evidence.host.local,
            session: evidence.host.session,
            socket: evidence.host.socket,
            state,
            version: evidence.runtime.version,
            protocol: evidence.runtime.protocol,
            error,
            entries,
        });
    }

    let mut rows = hosts
        .iter_mut()
        .flat_map(|host| std::mem::take(&mut host.entries))
        .collect::<Vec<_>>();
    score_descendant_closure(&mut rows);
    for row in rows {
        if let Some(host) = hosts.iter_mut().find(|host| host.name == row.host) {
            host.entries.push(row);
        }
    }

    Snapshot {
        polled: true,
        refreshed_at: Some(refreshed_at),
        refreshed_at_unix_ms: refreshed_at
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        configured_hosts: configured_hosts
            .iter()
            .map(|host| host.name.clone())
            .collect(),
        hosts,
    }
}

trait HostReader: Sync {
    fn fetch_local(&self, host: FleetHostConfig, timeout: Duration) -> HostEvidence;
    fn fetch_remote(&self, host: FleetHostConfig, timeout: Duration) -> HostEvidence;
}

struct SystemHostReader;

impl HostReader for SystemHostReader {
    fn fetch_local(&self, host: FleetHostConfig, timeout: Duration) -> HostEvidence {
        fetch_local_host(host, timeout)
    }

    fn fetch_remote(&self, host: FleetHostConfig, timeout: Duration) -> HostEvidence {
        fetch_remote_host(host, timeout)
    }
}

fn fetch_host_with(
    reader: &impl HostReader,
    host: FleetHostConfig,
    timeout: Duration,
) -> HostEvidence {
    if host.local {
        reader.fetch_local(host, timeout)
    } else {
        reader.fetch_remote(host, timeout)
    }
}

fn fetch_local_host(host: FleetHostConfig, timeout: Duration) -> HostEvidence {
    let client = host.socket.as_ref().map_or_else(
        || ApiClient::for_target(ConnectionTarget::LocalSession(host.session.clone())),
        |path| ApiClient::for_target(ConnectionTarget::SocketPath(PathBuf::from(path))),
    );
    let request = Request {
        id: "fleet:collect:local".into(),
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
    let runtime = fetch_local_runtime(&client, timeout);
    let runs = local_run_states();
    HostEvidence {
        host,
        agents,
        runs,
        runtime,
    }
}

fn fetch_local_runtime(client: &ApiClient, timeout: Duration) -> HostRuntime {
    let request = Request {
        id: "fleet:collect:runtime".into(),
        method: Method::Ping(crate::api::schema::PingParams::default()),
    };
    client
        .request_value_with_timeout(&request, timeout)
        .ok()
        .and_then(|value| crate::api::client::parse_response_value(value).ok())
        .and_then(|response| match response.result {
            ResponseResult::Pong {
                version, protocol, ..
            } => Some(HostRuntime {
                version: Some(version),
                protocol: Some(protocol),
            }),
            _ => None,
        })
        .unwrap_or_default()
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
    let script = remote_read_script(host.socket.as_deref(), host.session.as_deref());
    let output = run_ssh_with_timeout(&host.target, &script, timeout);
    let (agents, runs, runtime) = match output {
        Ok(output) => parse_remote_output(&output),
        Err(error) => (Err(error), Vec::new(), HostRuntime::default()),
    };
    HostEvidence {
        host,
        agents,
        runs,
        runtime,
    }
}

fn remote_read_script(socket: Option<&str>, session: Option<&str>) -> String {
    let socket = socket
        .map(|path| format!("export HERDR_SOCKET_PATH={}\n", shell_quote(path)))
        .unwrap_or_default();
    let session = session
        .map(|name| format!("export HERDR_SESSION={}\n", shell_quote(name)))
        .unwrap_or_default();
    format!(
        "set -u\n{socket}{session}herdr agent list || exit $?\nprintf '\\036HERDR_FLEET_RUNS_V1\\036\\n'\nif [ -d \"$HOME/.agents/runs\" ]; then\n  find \"$HOME/.agents/runs\" -mindepth 2 -maxdepth 2 -type f -name state.json -exec cat {{}} \\; -exec printf '\\n' \\;\nfi\nprintf '\\036HERDR_FLEET_HOST_V1\\036\\n'\nherdr status server --json || true\n"
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
            HostRuntime::default(),
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
    let trailer = &output[marker_at + REMOTE_RUNS_MARKER.len()..];
    let (state_bytes, runtime) = trailer
        .windows(REMOTE_HOST_MARKER.len())
        .position(|window| window == REMOTE_HOST_MARKER)
        .map_or((trailer, HostRuntime::default()), |host_marker_at| {
            (
                &trailer[..host_marker_at],
                parse_host_runtime(
                    trailer[host_marker_at + REMOTE_HOST_MARKER.len()..].trim_ascii(),
                ),
            )
        });
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
    (response, runs, runtime)
}

fn parse_host_runtime(bytes: &[u8]) -> HostRuntime {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return HostRuntime::default();
    };
    HostRuntime {
        version: value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        protocol: value
            .get("protocol")
            .and_then(serde_json::Value::as_u64)
            .and_then(|protocol| u32::try_from(protocol).ok()),
    }
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
pub(crate) enum EvidenceSource {
    Herdr,
    RunState,
    Host,
}

impl EvidenceSource {
    pub(crate) fn table_label(&self) -> &'static str {
        match self {
            Self::Herdr => "pane",
            Self::RunState => "run",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct FleetGate {
    n: u32,
    label: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recommendation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FleetRow {
    pub(crate) host: String,
    pub(crate) agent_ref: crate::api::schema::AgentRef,
    pub(crate) source: EvidenceSource,
    pub(crate) handle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    work: Option<String>,
    pub(crate) state: String,
    raw_state: String,
    liveness: Liveness,
    pub(crate) blocked: bool,
    closure_liveness: Liveness,
    closure_blocked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) age_s: Option<u64>,
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
    pub(crate) error: Option<String>,
    #[serde(skip)]
    native_session: Option<String>,
    #[serde(skip)]
    agent_info: Option<AgentInfo>,
}

pub(crate) fn counts_as_live_agent(entry: &FleetRow) -> bool {
    entry.source != EvidenceSource::Host && entry.state != "status_unknown"
}

impl FleetRow {
    #[cfg(test)]
    pub(crate) fn test_agent_row(host: &str, name: &str) -> Self {
        Self::test_agent_row_with_id(host, name, name)
    }

    #[cfg(test)]
    pub(crate) fn test_agent_row_with_id(host: &str, name: &str, agent_id: &str) -> Self {
        let agent_ref =
            crate::api::schema::AgentRef::new(host, agent_id).expect("valid test agent reference");
        Self::unknown(host, EvidenceSource::Herdr, agent_ref, String::new())
            .with_test_name(name)
            .with_test_state("working")
    }

    #[cfg(test)]
    pub(crate) fn test_agent_info_row(host: &str, agent: AgentInfo) -> Self {
        Self::from_agent(host, false, agent, 0).expect("valid test agent row")
    }

    #[cfg(test)]
    pub(crate) fn test_local_agent_info_row(host: &str, agent: AgentInfo) -> Self {
        Self::from_agent(host, true, agent, 0).expect("valid local test agent row")
    }

    #[cfg(test)]
    pub(crate) fn test_run_row(host: &str, run_id: &str, blocked: bool) -> Self {
        let agent_ref =
            crate::api::schema::AgentRef::new(host, run_id).expect("valid test run reference");
        let mut row = Self::unknown(host, EvidenceSource::RunState, agent_ref, String::new());
        row.error = None;
        row.name = Some(run_id.to_string());
        row.agent = Some("codex".to_string());
        row.state = if blocked { "blocked" } else { "active" }.to_string();
        row.blocked = blocked;
        row
    }

    #[cfg(test)]
    pub(crate) fn test_agent_row_with_state(host: &str, name: &str, state: &str) -> Self {
        Self::test_agent_row(host, name).with_test_state(state)
    }

    #[cfg(test)]
    fn with_test_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self.agent = Some("codex".to_string());
        self
    }

    #[cfg(test)]
    fn with_test_state(mut self, state: &str) -> Self {
        self.state = state.to_string();
        self
    }

    fn from_agent(host: &str, host_is_local: bool, agent: AgentInfo, now_s: u64) -> Option<Self> {
        let agent_info = agent.clone();
        let projection = agent.agent_projection();
        let liveness = if projection.settled {
            Liveness::Terminal
        } else {
            match agent.agent_status {
                AgentStatus::Idle | AgentStatus::Working | AgentStatus::Blocked => Liveness::Live,
                AgentStatus::Done => Liveness::Terminal,
                AgentStatus::Stale | AgentStatus::Unknown => Liveness::Unknown,
            }
        };
        let blocked = projection.counts_as_blocked();
        let raw_state = if projection.settled {
            "done"
        } else if projection.attention_tier == crate::terminal::state::AttentionTier::Attention {
            "attention"
        } else {
            agent_status_str(agent.agent_status)
        }
        .to_string();
        let state = effective_state(&raw_state, liveness, blocked);
        let reported_at = agent.reported_at.clone();
        let age_s = reported_at
            .as_deref()
            .and_then(parse_utc_timestamp)
            .and_then(|reported| now_s.checked_sub(reported));
        let gates = agent
            .gates
            .iter()
            .filter(|_| projection.open_blockers)
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
        let agent_ref = if host_is_local {
            agent
                .agent_ref
                .clone()
                .or_else(|| crate::api::schema::AgentRef::new(host, agent.pane_id.clone()).ok())?
        } else {
            // The configured alias is authoritative for rows fetched from a
            // remote host. Its self-reported host name is descriptive only.
            let agent_id = agent.agent_ref.as_ref().map_or_else(
                || agent.pane_id.clone(),
                |agent_ref| agent_ref.agent.clone(),
            );
            crate::api::schema::AgentRef::new(host, agent_id).ok()?
        };
        let handle = agent_ref.to_string();
        let model = agent.tokens.get("model").cloned();
        let effort = agent.tokens.get("effort").cloned();
        let work = agent_work(&agent);
        let title = agent_title(&agent);
        let native_session = agent
            .agent_session
            .as_ref()
            .map(|session| session.value.clone());
        Some(Self {
            host: host.into(),
            agent_ref,
            source: EvidenceSource::Herdr,
            handle,
            agent: agent.agent,
            name: Some(id),
            title,
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
            agent_info: Some(agent_info),
        })
    }

    fn from_run(host: &str, run: RunState, now_s: u64, heartbeat_stale_s: u64) -> Option<Self> {
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
            .and_then(|run_id| crate::api::schema::AgentRef::new(&run.parent.host, run_id).ok())
            .map(|agent_ref| agent_ref.to_string());
        let agent_ref = crate::api::schema::AgentRef::new(host, run.run_id.clone()).ok()?;
        let handle = agent_ref.to_string();
        Some(Self {
            host: host.into(),
            agent_ref,
            source: EvidenceSource::RunState,
            handle,
            agent: Some(run.agent),
            name: Some(run.run_id),
            title: None,
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
            agent_info: None,
        })
    }

    fn host_unknown(host: &str, error: String) -> Option<Self> {
        Some(Self::unknown(
            host,
            EvidenceSource::Host,
            crate::api::schema::AgentRef::new(host, "STATUS_UNKNOWN").ok()?,
            error,
        ))
    }

    fn run_error(host: &str, error: String) -> Option<Self> {
        Some(Self::unknown(
            host,
            EvidenceSource::RunState,
            crate::api::schema::AgentRef::new(host, "RUN_STATE_ERROR").ok()?,
            error,
        ))
    }

    fn unknown(
        host: &str,
        source: EvidenceSource,
        agent_ref: crate::api::schema::AgentRef,
        error: String,
    ) -> Self {
        let handle = agent_ref.to_string();
        Self {
            host: host.into(),
            agent_ref,
            source,
            handle,
            agent: None,
            name: None,
            title: None,
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
            agent_info: None,
        }
    }

    pub(crate) fn agent_info(&self) -> Option<&AgentInfo> {
        self.agent_info.as_ref()
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

/// Resolve the title once when fleet evidence arrives. Older hosts do not send
/// `display_title`, so their fallback cannot recover manual pane labels or know
/// the remote user's home directory. It deliberately treats HOME as unknown.
fn agent_title(agent: &AgentInfo) -> Option<String> {
    if agent.display_title.is_some() {
        return agent.display_title.clone();
    }
    let title = agent.work_context.session_name.clone().or_else(|| {
        crate::workspace::agent_title_from_terminal_or_work(crate::workspace::AgentTitleContext {
            terminal_title: agent.terminal_title_stripped.as_deref(),
            work_title: agent.work_context.work_title.as_deref(),
            cwd: agent.cwd.as_deref().map(Path::new),
            home: None,
            agent_name: agent.name.as_deref(),
            agent_label: agent.agent.as_deref(),
            display_agent: agent.display_agent.as_deref(),
            detected_agent: agent
                .agent
                .as_deref()
                .and_then(crate::detect::parse_agent_label),
        })
    });
    let projection = crate::workspace::TabDisplayProjection::Derived {
        agent: None,
        ticket: agent.work_context.primary_ticket().map(str::to_string),
        binding: None,
        title,
    };
    crate::workspace::session_title(Some(&projection), None)
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

fn unix_seconds(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeReader {
        local_calls: AtomicUsize,
        remote_calls: AtomicUsize,
        agents: Result<Vec<AgentInfo>, String>,
        runtime: HostRuntime,
    }

    impl HostReader for FakeReader {
        fn fetch_local(&self, host: FleetHostConfig, _timeout: Duration) -> HostEvidence {
            self.local_calls.fetch_add(1, Ordering::Relaxed);
            HostEvidence {
                host,
                agents: self.agents.clone(),
                runs: Vec::new(),
                runtime: self.runtime.clone(),
            }
        }

        fn fetch_remote(&self, host: FleetHostConfig, _timeout: Duration) -> HostEvidence {
            self.remote_calls.fetch_add(1, Ordering::Relaxed);
            HostEvidence {
                host,
                agents: self.agents.clone(),
                runs: Vec::new(),
                runtime: self.runtime.clone(),
            }
        }
    }

    fn host(name: &str, local: bool) -> FleetHostConfig {
        FleetHostConfig {
            name: name.to_string(),
            target: if local {
                String::new()
            } else {
                name.to_string()
            },
            local,
            ..FleetHostConfig::default()
        }
    }

    fn fake_reader(agents: Result<Vec<AgentInfo>, String>, runtime: HostRuntime) -> FakeReader {
        FakeReader {
            local_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            agents,
            runtime,
        }
    }

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
        let row =
            FleetRow::from_agent("ub1", false, agent, 1_777_000_000).expect("valid fleet row");
        assert!(row.blocked);
        assert_eq!(row.liveness, Liveness::Unknown);
        assert_eq!(row.state, "blocked_liveness_unknown");
        assert_eq!(
            row.gate_summary.as_deref(),
            Some("Ship? | (a-rec) ship after CI")
        );
    }

    #[test]
    fn agent_attention_projection_excludes_questions_and_settled_panes_from_blocked() {
        let mut answer = agent(AgentStatus::Blocked, serde_json::json!([]));
        answer.items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Answer".into(),
            text: "Choose a lane".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let answer_row =
            FleetRow::from_agent("ub1", false, answer, 1_777_000_000).expect("valid attention row");
        assert!(!answer_row.blocked);
        assert_eq!(answer_row.state, "attention");

        let mut settled = agent(
            AgentStatus::Blocked,
            serde_json::json!([{"n": 1, "label": "Gate", "text": "Approve"}]),
        );
        settled.settled_at = Some(1_777_000_000);
        let settled_row =
            FleetRow::from_agent("ub1", false, settled, 1_777_000_000).expect("valid settled row");
        assert!(!settled_row.blocked);
        assert_eq!(settled_row.state, "done");
    }

    #[test]
    fn legacy_agent_without_ref_uses_configured_host_and_pane_identity() {
        let agent = agent(AgentStatus::Idle, serde_json::json!([]));
        assert!(agent.agent_ref.is_none());

        let row = FleetRow::from_agent("configured/alias", false, agent, 1_777_000_000)
            .expect("valid fleet row");

        assert_eq!(
            row.agent_ref,
            crate::api::schema::AgentRef::new("configured/alias", "p1")
                .expect("valid expected agent reference")
        );
    }

    #[test]
    fn configured_alias_replaces_the_host_reported_in_agent_identity() {
        let hosts = vec![host("office", false)];
        let mut reported = agent(AgentStatus::Idle, serde_json::json!([]));
        reported.agent_ref = Some(
            crate::api::schema::AgentRef::new("laptop", "p1")
                .expect("valid reported agent reference"),
        );
        let reader = fake_reader(Ok(vec![reported]), HostRuntime::default());

        let snapshot = collect_snapshot_with(&reader, &hosts, &FleetConfig::default());

        assert_eq!(snapshot.hosts[0].name, "office");
        assert_eq!(
            snapshot.hosts[0].entries[0].agent_ref,
            crate::api::schema::AgentRef::new("office", "p1")
                .expect("valid configured agent reference")
        );
    }

    #[test]
    fn fleet_names_reserve_the_agent_ref_separator() {
        let mut fleet = FleetConfig {
            self_name: Some("local::invalid".to_string()),
            hosts: vec![host("ub2", false)],
            ..FleetConfig::default()
        };
        assert!(select_hosts(&fleet, None).is_err());

        fleet.self_name = Some("local".to_string());
        fleet.hosts[0].name = "ub::2".to_string();
        assert!(select_hosts(&fleet, None).is_err());
    }

    #[test]
    fn live_agent_membership_keeps_supported_states_only() {
        let cases = [
            (AgentStatus::Idle, serde_json::json!([]), true),
            (AgentStatus::Working, serde_json::json!([]), true),
            (AgentStatus::Blocked, serde_json::json!([]), true),
            (AgentStatus::Done, serde_json::json!([]), true),
            (
                AgentStatus::Stale,
                serde_json::json!([{"n": 1, "label": "Gate", "text": "answer"}]),
                true,
            ),
            (AgentStatus::Stale, serde_json::json!([]), false),
            (AgentStatus::Unknown, serde_json::json!([]), false),
        ];

        for (status, gates, expected) in cases {
            let row = FleetRow::from_agent("ub2", false, agent(status, gates), 1_777_000_000)
                .expect("valid fleet row");
            assert_eq!(counts_as_live_agent(&row), expected, "{}", row.state);
        }

        let host_entry = FleetRow::host_unknown("ub2", "offline".to_string())
            .expect("valid host row")
            .with_test_state("working");
        assert!(!counts_as_live_agent(&host_entry));
    }

    #[test]
    fn unpolled_snapshot_differs_from_polled_empty_fleet() {
        let config = FleetConfig::default();
        let unpolled = Snapshot::unpolled(&config.hosts);
        let polled = poll(&config);
        assert!(!unpolled.polled);
        assert!(polled.polled);
        assert!(polled.hosts.is_empty());
    }

    #[test]
    fn unreachable_host_retains_reader_error() {
        let hosts = vec![host("ub2", false)];
        let reader = fake_reader(
            Err("ssh: connection refused".to_string()),
            HostRuntime::default(),
        );
        let snapshot = collect_snapshot_with(&reader, &hosts, &FleetConfig::default());
        assert_eq!(snapshot.hosts[0].state, HostState::Unreachable);
        assert_eq!(
            snapshot.hosts[0].error.as_deref(),
            Some("ssh: connection refused")
        );
    }

    #[test]
    fn remote_version_or_protocol_difference_marks_version_skew() {
        let hosts = vec![host("ub2", false)];
        let version_reader = fake_reader(
            Ok(Vec::new()),
            HostRuntime {
                version: Some("0.0.0-test".to_string()),
                protocol: Some(crate::protocol::PROTOCOL_VERSION),
            },
        );
        let version_snapshot =
            collect_snapshot_with(&version_reader, &hosts, &FleetConfig::default());
        assert_eq!(version_snapshot.hosts[0].state, HostState::VersionSkew);
        assert_eq!(
            version_snapshot.hosts[0].version.as_deref(),
            Some("0.0.0-test")
        );

        let protocol_reader = fake_reader(
            Ok(Vec::new()),
            HostRuntime {
                version: Some(crate::build_info::version().to_string()),
                protocol: Some(crate::protocol::PROTOCOL_VERSION + 1),
            },
        );
        let protocol_snapshot =
            collect_snapshot_with(&protocol_reader, &hosts, &FleetConfig::default());
        assert_eq!(protocol_snapshot.hosts[0].state, HostState::VersionSkew);
    }

    #[test]
    fn local_host_uses_local_reader_without_ssh() {
        let hosts = vec![host("laptop", true)];
        let reader = fake_reader(
            Ok(Vec::new()),
            HostRuntime {
                version: Some(crate::build_info::version().to_string()),
                protocol: Some(crate::protocol::PROTOCOL_VERSION),
            },
        );
        let snapshot = collect_snapshot_with(&reader, &hosts, &FleetConfig::default());
        assert_eq!(snapshot.hosts[0].state, HostState::Reachable);
        assert_eq!(reader.local_calls.load(Ordering::Relaxed), 1);
        assert_eq!(reader.remote_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn agent_attach_argv_streams_one_remote_agent_instead_of_a_nested_herdr() {
        let host = HostSnapshot {
            name: "workbox".to_string(),
            target: "you@workbox".to_string(),
            local: false,
            session: Some("agents".to_string()),
            socket: Some("/home/you/.config/herdr/herdr.sock".to_string()),
            state: HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            entries: Vec::new(),
        };

        assert_eq!(
            agent_attach_argv(&host, "w1:p2").expect("attach argv"),
            [
                "ssh",
                "-t",
                "you@workbox",
                "HERDR_SOCKET_PATH='/home/you/.config/herdr/herdr.sock' HERDR_SESSION='agents' herdr agent attach 'w1:p2'"
            ]
        );
    }

    #[test]
    fn agent_attach_command_quotes_a_target_that_carries_a_quote() {
        assert_eq!(
            remote_attach_command(None, None, "pane'; rm -rf /"),
            "herdr agent attach 'pane'\\''; rm -rf /'"
        );
    }

    #[test]
    fn agent_attach_argv_rejects_an_empty_agent() {
        let host = HostSnapshot {
            name: "workbox".to_string(),
            target: "you@workbox".to_string(),
            local: false,
            session: None,
            socket: None,
            state: HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            entries: Vec::new(),
        };

        assert!(agent_attach_argv(&host, "  ").is_err());
    }

    #[test]
    fn host_attach_argv_includes_configured_session() {
        let host = HostSnapshot {
            name: "workbox".to_string(),
            target: "you@workbox".to_string(),
            local: false,
            session: Some("agents".to_string()),
            socket: None,
            state: HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            entries: Vec::new(),
        };
        assert_eq!(
            host_attach_argv(&host).expect("attach argv"),
            ["herdr", "--remote", "you@workbox", "--session", "agents"]
        );
    }

    #[test]
    fn stale_agent_without_gate_is_status_unknown() {
        let row = FleetRow::from_agent(
            "ub1",
            false,
            agent(AgentStatus::Stale, serde_json::json!([])),
            1_777_000_000,
        )
        .expect("valid fleet row");
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
    fn rejected_run_state_is_listed_but_not_counted_as_live() {
        let configured_host = host("ub2", false);
        let rejected = parse_run_state(br#"{"schema":2}"#, "fixture");
        let snapshot = snapshot_from_evidence(
            std::slice::from_ref(&configured_host),
            &FleetConfig::default(),
            vec![HostEvidence {
                host: configured_host.clone(),
                agents: Ok(Vec::new()),
                runs: vec![rejected],
                runtime: HostRuntime::default(),
            }],
            SystemTime::UNIX_EPOCH,
        );

        let entries = &snapshot.hosts[0].entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, "status_unknown");
        assert!(entries[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("schema 2 is unsupported")));
        assert!(!counts_as_live_agent(&entries[0]));
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
        let (agents, runs, runtime) = parse_remote_output(&output);
        assert!(agents.unwrap().is_empty());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].as_ref().unwrap().run_id, "ra-260826-test-a1b2c3d");
        assert_eq!(runtime, HostRuntime::default());
    }

    #[test]
    fn remote_agent_list_applies_local_title_rules_and_legacy_fallbacks() {
        let parse_row = |agent: serde_json::Value| {
            let response = serde_json::json!({
                "id": "x",
                "result": {"type": "agent_list", "agents": [agent]}
            });
            let output = [
                serde_json::to_vec(&response).expect("agent response JSON"),
                REMOTE_RUNS_MARKER.to_vec(),
            ]
            .concat();
            let (agents, runs, _) = parse_remote_output(&output);
            assert!(runs.is_empty());
            FleetRow::from_agent(
                "ub1",
                false,
                agents
                    .expect("valid remote agent list")
                    .into_iter()
                    .next()
                    .expect("one remote agent"),
                0,
            )
            .expect("valid fleet row")
        };
        let base = serde_json::json!({
            "terminal_id": "term-1",
            "work_context": {"work_title": "Cost levers from work context"},
            "name": "cl-ceea66cc",
            "agent": "codex",
            "agent_status": "working",
            "workspace_id": "w23",
            "tab_id": "t1",
            "pane_id": "w23:p1E",
            "focused": false,
            "revision": 1
        });
        let mut with_terminal_title = base.clone();
        with_terminal_title["terminal_title_stripped"] =
            serde_json::json!("Scalable V2 cost levers handoff");

        assert_eq!(
            parse_row(with_terminal_title).title.as_deref(),
            Some("Scalable V2 cost levers handoff")
        );
        let mut with_composed_title = base.clone();
        with_composed_title["terminal_title_stripped"] = serde_json::json!("codex — Fix billing");
        assert_eq!(
            parse_row(with_composed_title).title.as_deref(),
            Some("Fix billing"),
            "the fleet projection strips the same leading agent identity as a local tab"
        );
        let mut with_cwd_title = base.clone();
        with_cwd_title["cwd"] = serde_json::json!("/work/herdr");
        with_cwd_title["terminal_title_stripped"] = serde_json::json!("codex — herdr");
        assert_eq!(
            parse_row(with_cwd_title).title.as_deref(),
            Some("Cost levers from work context"),
            "an agent-and-cwd title yields to the declared work title"
        );
        assert_eq!(
            parse_row(base.clone()).title.as_deref(),
            Some("Cost levers from work context"),
            "older agent-list JSON without a terminal title still parses"
        );
        let mut without_title = base;
        without_title["work_context"] = serde_json::json!({});
        without_title["terminal_title_stripped"] = serde_json::json!("Codex");
        let fallback = parse_row(without_title);
        assert_eq!(fallback.title, None);
        assert_eq!(fallback.name.as_deref(), Some("cl-ceea66cc"));
    }

    #[test]
    fn remote_agent_title_prefers_host_display_title_verbatim() {
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "term-1",
            "work_context": {
                "ticket_ids": ["SCA-1"],
                "work_title": "collector fallback"
            },
            "name": "cl-ceea66cc",
            "agent": "codex",
            "title": "runtime title",
            "display_title": "SCA-9: exact host title",
            "terminal_title_stripped": "terminal fallback",
            "agent_status": "working",
            "workspace_id": "w23",
            "tab_id": "t1",
            "pane_id": "w23:p1E",
            "focused": false,
            "revision": 1
        }))
        .expect("valid agent info");

        let row = FleetRow::from_agent("ub1", false, agent, 0).expect("valid fleet row");
        assert_eq!(row.title.as_deref(), Some("SCA-9: exact host title"));
    }

    #[test]
    fn legacy_remote_tilde_title_does_not_use_collector_home() {
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "term-1",
            "work_context": {"work_title": "collector fallback"},
            "name": "cl-ceea66cc",
            "agent": "codex",
            "terminal_title_stripped": "~/projects/herdr",
            "agent_status": "working",
            "workspace_id": "w23",
            "tab_id": "t1",
            "pane_id": "w23:p1E",
            "focused": false,
            "cwd": "/srv/remote-user/projects/herdr",
            "revision": 1
        }))
        .expect("valid legacy agent info");

        let row = FleetRow::from_agent("ub1", false, agent, 0).expect("valid fleet row");
        assert_eq!(row.title.as_deref(), Some("~/projects/herdr"));
    }

    #[test]
    fn legacy_remote_tilde_title_ignores_matching_collector_home() {
        let collector_home = std::env::var_os("HOME").expect("test collector HOME");
        let remote_cwd = PathBuf::from(collector_home).join("fleet-title-project");
        let agent: AgentInfo = serde_json::from_value(serde_json::json!({
            "terminal_id": "term-1",
            "work_context": {"work_title": "collector fallback"},
            "name": "cl-ceea66cc",
            "agent": "codex",
            "terminal_title_stripped": "~/fleet-title-project",
            "agent_status": "working",
            "workspace_id": "w23",
            "tab_id": "t1",
            "pane_id": "w23:p1E",
            "focused": false,
            "cwd": remote_cwd,
            "revision": 1
        }))
        .expect("valid legacy agent info");

        let row = FleetRow::from_agent("ub1", false, agent, 0).expect("valid fleet row");
        assert_eq!(row.title.as_deref(), Some("~/fleet-title-project"));
    }

    #[test]
    fn done_parent_scores_live_and_blocked_descendant_closure() {
        let mut parent = FleetRow::unknown(
            "mac",
            EvidenceSource::RunState,
            crate::api::schema::AgentRef::new("mac", "parent").expect("valid parent reference"),
            "fixture".into(),
        );
        parent.liveness = Liveness::Terminal;
        parent.closure_liveness = Liveness::Terminal;
        let mut child = FleetRow::unknown(
            "ub1",
            EvidenceSource::RunState,
            crate::api::schema::AgentRef::new("ub1", "child").expect("valid child reference"),
            "fixture".into(),
        );
        child.liveness = Liveness::Live;
        child.closure_liveness = Liveness::Live;
        child.blocked = true;
        child.closure_blocked = true;
        child.parent_handle = Some("mac::parent".into());
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
            false,
            agent(
                AgentStatus::Stale,
                serde_json::json!([{
                    "n": 1, "label": "Gate", "text": "answer"
                }]),
            ),
            1_777_000_000,
        )
        .expect("valid blocked fleet row");
        blocked.handle = "ub1/b".into();
        let working = FleetRow::from_agent(
            "mac",
            false,
            agent(AgentStatus::Working, serde_json::json!([])),
            1_777_000_000,
        )
        .expect("valid working fleet row");
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
