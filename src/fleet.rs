use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::api::client::{ApiClient, ConnectionTarget};
use crate::api::schema::{AgentInfo, AgentStatus, EmptyParams, Method, Request, ResponseResult};
use crate::config::{FleetConfig, FleetHostConfig};

const REMOTE_RUNS_MARKER: &[u8] = b"\x1eHERDR_FLEET_RUNS_V1\x1e\n";
const REMOTE_RUN_RECORD_MARKER: &[u8] = b"\x1eHERDR_FLEET_RUN_V1:";
const REMOTE_HOST_MARKER: &[u8] = b"\x1eHERDR_FLEET_HOST_V1\x1e\n";
const REMOTE_GROUPS_MARKER: &[u8] = b"\x1eHERDR_FLEET_GROUPS_V1\x1e\n";
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MIN_REFRESH_INTERVAL_MS: u64 = 100;
// ub2 currently carries about 1,000 retained runs. Inspect enough entries to
// select its newest records while keeping malformed or unbounded stores capped.
const MAX_RUN_DIRECTORY_ENTRIES: usize = 2_048;
const MAX_REMOTE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const AUTHORITY_ACCEPTANCE_LEDGER_VERSION: u32 = 2;
#[cfg(not(test))]
const AUTHORITY_ACCEPTANCE_LEDGER_FILE: &str = "authority-acceptance-ledger-v2.json";
#[cfg(not(test))]
const LEGACY_GROUP_CATALOG_CACHE_FILE: &str = "remote-group-catalogs-v1.json";
type ParsedRemoteOutput = (
    Result<Vec<AgentInfo>, String>,
    Result<crate::groups::GroupAuthoritySnapshot, String>,
    Vec<Result<crate::agent_runs::Observation, String>>,
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
    if let Some(host) = fleet.symphony_host.as_deref() {
        if !names.contains(host) {
            return Err(format!(
                "remote.fleet.symphony_host names unknown host: {host}"
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
    /// The host identity reported by the remote server's agent inventory.
    /// This is deliberately not part of the public fleet response: rows keep
    /// the configured alias, while remote focus uses this fact only on the
    /// wire after checking that the connection tuple still matches.
    #[serde(skip)]
    pub(crate) remote_identity: Option<String>,
    pub(crate) entries: Vec<FleetRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Snapshot {
    pub(crate) polled: bool,
    #[serde(skip)]
    pub(crate) refreshed_at: Option<SystemTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) refreshed_at_unix_ms: Option<u64>,
    /// Configuration generation used when this poll began. It is not part of
    /// the public fleet response; consumers use it to reject observations
    /// that raced a configuration reload.
    #[serde(skip)]
    pub(crate) config_generation: u64,
    pub(crate) configured_hosts: Vec<String>,
    pub(crate) hosts: Vec<HostSnapshot>,
    /// Complete owner catalogs observed by the periodic fleet poll. Admission
    /// happens once when the refresh reaches the app event loop.
    #[serde(skip)]
    pub(crate) group_catalogs: Vec<GroupCatalog>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GroupCatalogState {
    Fresh,
    Stale,
    IdentityConflict,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GroupCatalog {
    pub(crate) host: String,
    pub(crate) target: String,
    pub(crate) local: bool,
    pub(crate) session: Option<String>,
    #[serde(skip)]
    pub(crate) socket: Option<String>,
    pub(crate) state: GroupCatalogState,
    #[serde(skip)]
    pub(crate) observed_authority_id: Option<crate::groups::AuthorityId>,
    pub(crate) snapshot: Option<crate::groups::GroupAuthoritySnapshot>,
    pub(crate) error: Option<String>,
}

impl GroupCatalog {
    fn matches_connection(&self, other: &Self) -> bool {
        self.target == other.target
            && self.local == other.local
            && self.session == other.session
            && self.socket == other.socket
    }

    pub(crate) fn authority_id(&self) -> Option<&crate::groups::AuthorityId> {
        self.observed_authority_id.as_ref().or_else(|| {
            self.snapshot
                .as_ref()
                .map(|snapshot| &snapshot.authority_id)
        })
    }

    pub(crate) fn is_fresh(&self) -> bool {
        self.state == GroupCatalogState::Fresh
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthorityAcceptanceLedgerFile {
    version: u32,
    authorities: Vec<crate::groups::GroupAuthoritySnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyGroupCatalogCacheEntry {
    target: String,
    local: bool,
    session: Option<String>,
    socket: Option<String>,
    snapshot: crate::groups::GroupAuthoritySnapshot,
}

impl LegacyGroupCatalogCacheEntry {
    fn into_snapshot(self) -> crate::groups::GroupAuthoritySnapshot {
        let Self {
            target: _,
            local: _,
            session: _,
            socket: _,
            snapshot,
        } = self;
        snapshot
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyGroupCatalogCacheFile {
    version: u32,
    entries: Vec<LegacyGroupCatalogCacheEntry>,
}

#[derive(Debug, Deserialize)]
struct AuthorityAcceptanceLedgerHeader {
    version: u32,
}

/// Durable, authority-owned admission history. Connection catalogs describe
/// the latest poll; this ledger records every authority snapshot accepted by
/// this server and cannot be narrowed by the current fleet configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AuthorityAcceptanceLedger {
    authorities: BTreeMap<crate::groups::AuthorityId, crate::groups::GroupAuthoritySnapshot>,
}

impl AuthorityAcceptanceLedger {
    fn canonical_snapshot(
        mut snapshot: crate::groups::GroupAuthoritySnapshot,
    ) -> crate::groups::GroupAuthoritySnapshot {
        snapshot.groups.sort_by_key(|record| record.id.local);
        snapshot.memberships.clear();
        snapshot
    }

    fn from_snapshots(
        snapshots: impl IntoIterator<Item = crate::groups::GroupAuthoritySnapshot>,
    ) -> Result<Self, String> {
        let mut by_authority: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for snapshot in snapshots {
            let snapshot = Self::canonical_snapshot(snapshot);
            by_authority
                .entry(snapshot.authority_id.clone())
                .or_default()
                .push(snapshot);
        }
        let mut ledger = Self::default();
        for snapshots in by_authority.values_mut() {
            snapshots.sort_by_key(|snapshot| snapshot.revision);
            for snapshot in snapshots.drain(..) {
                ledger.advance(&snapshot)?;
            }
        }
        Ok(ledger)
    }

    pub(crate) fn advance(
        &mut self,
        snapshot: &crate::groups::GroupAuthoritySnapshot,
    ) -> Result<bool, String> {
        let candidate = Self::canonical_snapshot(snapshot.clone());
        let retained = self.authorities.get(&candidate.authority_id);
        crate::groups::admit_authority_snapshot(retained, &candidate)
            .map_err(|error| error.to_string())?;
        if retained == Some(&candidate) {
            return Ok(false);
        }
        self.authorities
            .insert(candidate.authority_id.clone(), candidate);
        Ok(true)
    }

    pub(crate) fn accepted(
        &self,
        authority: &crate::groups::AuthorityId,
    ) -> Option<&crate::groups::GroupAuthoritySnapshot> {
        self.authorities.get(authority)
    }

    fn contains_snapshot(&self, snapshot: &crate::groups::GroupAuthoritySnapshot) -> bool {
        let snapshot = Self::canonical_snapshot(snapshot.clone());
        self.authorities.get(&snapshot.authority_id) == Some(&snapshot)
    }

    fn snapshots(&self) -> Vec<crate::groups::GroupAuthoritySnapshot> {
        self.authorities.values().cloned().collect()
    }
}

#[cfg(not(test))]
pub(crate) fn authority_acceptance_ledger_path() -> PathBuf {
    crate::session::data_dir().join(AUTHORITY_ACCEPTANCE_LEDGER_FILE)
}

#[cfg(not(test))]
pub(crate) fn legacy_group_catalog_cache_path() -> PathBuf {
    crate::session::data_dir().join(LEGACY_GROUP_CATALOG_CACHE_FILE)
}

pub(crate) fn load_authority_acceptance_ledger_with_legacy(
    path: &Path,
    legacy_path: &Path,
) -> Result<AuthorityAcceptanceLedger, String> {
    match path.try_exists() {
        Ok(true) => load_authority_acceptance_ledger(path),
        Ok(false) => match legacy_path.try_exists() {
            Ok(true) => {
                let ledger = load_authority_acceptance_ledger(legacy_path)?;
                save_authority_acceptance_ledger(path, &ledger).map_err(|error| {
                    format!(
                        "cannot migrate legacy authority acceptance ledger to {}: {error}",
                        path.display()
                    )
                })?;
                Ok(ledger)
            }
            Ok(false) => Ok(AuthorityAcceptanceLedger::default()),
            Err(error) => Err(format!(
                "cannot inspect legacy authority acceptance ledger: {error}"
            )),
        },
        Err(error) => Err(format!(
            "cannot inspect authority acceptance ledger: {error}"
        )),
    }
}

pub(crate) fn load_authority_acceptance_ledger(
    path: &Path,
) -> Result<AuthorityAcceptanceLedger, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AuthorityAcceptanceLedger::default());
        }
        Err(error) => return Err(format!("cannot read authority acceptance ledger: {error}")),
    };
    let header: AuthorityAcceptanceLedgerHeader = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse authority acceptance ledger: {error}"))?;
    let snapshots = match header.version {
        1 => {
            let file: LegacyGroupCatalogCacheFile = serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot parse legacy group catalog cache: {error}"))?;
            file.entries
                .into_iter()
                .map(LegacyGroupCatalogCacheEntry::into_snapshot)
                .collect::<Vec<_>>()
        }
        AUTHORITY_ACCEPTANCE_LEDGER_VERSION => {
            let file: AuthorityAcceptanceLedgerFile = serde_json::from_slice(&bytes)
                .map_err(|error| format!("cannot parse authority acceptance ledger: {error}"))?;
            file.authorities
        }
        version => {
            return Err(format!(
                "unsupported authority acceptance ledger version {version} (expected {AUTHORITY_ACCEPTANCE_LEDGER_VERSION})"
            ));
        }
    };
    AuthorityAcceptanceLedger::from_snapshots(snapshots)
        .map_err(|error| format!("authority acceptance ledger has invalid history: {error}"))
}

pub(crate) fn save_authority_acceptance_ledger(
    path: &Path,
    ledger: &AuthorityAcceptanceLedger,
) -> std::io::Result<()> {
    let file = AuthorityAcceptanceLedgerFile {
        version: AUTHORITY_ACCEPTANCE_LEDGER_VERSION,
        authorities: ledger.snapshots(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(error) = write_authority_acceptance_ledger_contents(&mut file, &bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = crate::platform::replace_file_durably(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

trait AuthorityAcceptanceLedgerTempFile {
    fn write_contents(&mut self, contents: &[u8]) -> std::io::Result<()>;
    fn sync_contents(&mut self) -> std::io::Result<()>;
}

impl AuthorityAcceptanceLedgerTempFile for std::fs::File {
    fn write_contents(&mut self, contents: &[u8]) -> std::io::Result<()> {
        self.write_all(contents)
    }

    fn sync_contents(&mut self) -> std::io::Result<()> {
        self.sync_all()
    }
}

fn write_authority_acceptance_ledger_contents(
    file: &mut impl AuthorityAcceptanceLedgerTempFile,
    contents: &[u8],
) -> std::io::Result<()> {
    file.write_contents(contents)?;
    file.sync_contents()
}

struct AuthorityAcceptanceLedgerWrite {
    path: PathBuf,
    ledger: AuthorityAcceptanceLedger,
    snapshot: Snapshot,
}

type AuthorityAcceptanceLedgerSave =
    dyn Fn(&Path, &AuthorityAcceptanceLedger) -> std::io::Result<()> + Send + Sync + 'static;

/// Serial durable ledger writer. The app loop admits each candidate before it
/// enters this queue and publishes it only after the matching completion.
pub(crate) struct AuthorityAcceptanceLedgerWriter {
    sender: std::sync::Mutex<Option<std::sync::mpsc::Sender<AuthorityAcceptanceLedgerWrite>>>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    save: Arc<AuthorityAcceptanceLedgerSave>,
}

impl AuthorityAcceptanceLedgerWriter {
    pub(crate) fn new(event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>) -> Self {
        Self {
            sender: std::sync::Mutex::new(None),
            event_tx,
            save: Arc::new(save_authority_acceptance_ledger),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_save(
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        save: impl Fn(&Path, &AuthorityAcceptanceLedger) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sender: std::sync::Mutex::new(None),
            event_tx,
            save: Arc::new(save),
        }
    }

    pub(crate) fn enqueue(
        &self,
        path: PathBuf,
        ledger: AuthorityAcceptanceLedger,
        snapshot: Snapshot,
    ) -> Result<(), String> {
        let mut sender = self
            .sender
            .lock()
            .map_err(|_| "authority acceptance ledger queue is unavailable".to_string())?;
        if sender.is_none() {
            let (write_tx, write_rx) = std::sync::mpsc::channel::<AuthorityAcceptanceLedgerWrite>();
            let event_tx = self.event_tx.clone();
            let save = Arc::clone(&self.save);
            std::thread::Builder::new()
                .name("herdr-group-ledger".into())
                .spawn(move || {
                    while let Ok(job) = write_rx.recv() {
                        let result = save(&job.path, &job.ledger).map_err(|error| {
                            format!(
                                "cannot persist authority acceptance ledger at {}: {error}",
                                job.path.display()
                            )
                        });
                        if event_tx
                            .blocking_send(
                                crate::events::AppEvent::AuthorityAcceptanceLedgerPersisted {
                                    ledger: job.ledger,
                                    snapshot: Box::new(job.snapshot),
                                    result,
                                },
                            )
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .map_err(|error| {
                    format!("cannot start authority acceptance ledger writer: {error}")
                })?;
            *sender = Some(write_tx);
        }
        let Some(sender) = sender.as_ref() else {
            return Err("authority acceptance ledger queue did not initialize".into());
        };
        sender
            .send(AuthorityAcceptanceLedgerWrite {
                path,
                ledger,
                snapshot,
            })
            .map_err(|_| "authority acceptance ledger writer stopped".to_string())
    }
}

struct RoutedApiRequest {
    catalog: GroupCatalog,
    config_generation: u64,
    route: AuthorityRoute,
    mutation_route: MutationRoute,
    route_lease: u64,
    request: Request,
    respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum MutationRoute {
    Authority(AuthorityRoute),
    Pane {
        route: AuthorityRoute,
        pane_id: String,
        pane_incarnation: String,
    },
}

impl MutationRoute {
    fn from_request(route: AuthorityRoute, request: &Request) -> Result<Self, String> {
        let crate::api::schema::Method::GroupAuthorityMutate(params) = &request.method else {
            return Ok(Self::Authority(route));
        };
        let crate::api::schema::AuthorityMutation::PaneGroupSet(params) = &params.mutation else {
            return Ok(Self::Authority(route));
        };
        let pane_incarnation = params
            .expected_pane_incarnation
            .clone()
            .ok_or_else(|| "remote pane mutation has no expected incarnation".to_string())?;
        Ok(Self::Pane {
            route,
            pane_id: params.pane_id.clone(),
            pane_incarnation,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AuthorityRoute {
    authority: crate::groups::AuthorityId,
    target: String,
    local: bool,
    session: Option<String>,
    socket: Option<String>,
}

impl AuthorityRoute {
    fn from_catalog(catalog: &GroupCatalog) -> Option<Self> {
        Some(Self {
            authority: catalog.observed_authority_id.clone()?,
            target: catalog.target.clone(),
            local: catalog.local,
            session: catalog.session.clone(),
            socket: catalog.socket.clone(),
        })
    }
}

#[derive(Debug, Default)]
struct AuthorityRouteLeases {
    next: u64,
    valid: BTreeMap<MutationRoute, u64>,
}

impl AuthorityRouteLeases {
    fn issue(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        if self.next == 0 {
            self.valid.clear();
            self.next = 1;
        }
        self.next
    }

    fn observe(&mut self, routes: BTreeMap<MutationRoute, ()>) {
        self.valid.retain(|route, _| routes.contains_key(route));
        for route in routes.into_keys() {
            if self.valid.contains_key(&route) {
                continue;
            }
            let lease = self.issue();
            self.valid.insert(route, lease);
        }
    }
}

/// FIFO worker for remote authority mutations. One worker preserves API arrival
/// order while keeping every IPC or SSH wait off the app event loop.
pub(crate) struct AuthorityMutationRouter {
    sender: std::sync::Mutex<Option<std::sync::mpsc::Sender<RoutedApiRequest>>>,
    config_generation: Arc<std::sync::atomic::AtomicU64>,
    route_leases: Arc<std::sync::Mutex<AuthorityRouteLeases>>,
    ssh_program: std::ffi::OsString,
    timeout: Duration,
}

impl Default for AuthorityMutationRouter {
    fn default() -> Self {
        Self {
            sender: std::sync::Mutex::new(None),
            config_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            route_leases: Arc::new(std::sync::Mutex::new(AuthorityRouteLeases::default())),
            ssh_program: std::ffi::OsString::from("ssh"),
            timeout: Duration::from_secs(5),
        }
    }
}

impl AuthorityMutationRouter {
    #[cfg(test)]
    pub(crate) fn with_ssh_program(program: PathBuf, timeout: Duration) -> Self {
        Self {
            sender: std::sync::Mutex::new(None),
            config_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            route_leases: Arc::new(std::sync::Mutex::new(AuthorityRouteLeases::default())),
            ssh_program: program.into_os_string(),
            timeout,
        }
    }

    pub(crate) fn reconfigure(&self, config_generation: u64) {
        self.config_generation
            .store(config_generation, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn observe_snapshot(&self, snapshot: &Snapshot) {
        let conflicts = snapshot.identity_conflicts();
        let mut routes = BTreeMap::new();
        for catalog in snapshot.group_catalogs.iter().filter(|catalog| {
            catalog.state == GroupCatalogState::Fresh
                && catalog
                    .observed_authority_id
                    .as_ref()
                    .is_some_and(|authority| !conflicts.contains(authority))
        }) {
            let Some(route) = AuthorityRoute::from_catalog(catalog) else {
                continue;
            };
            routes.insert(MutationRoute::Authority(route.clone()), ());
            for membership in catalog
                .snapshot
                .iter()
                .flat_map(|snapshot| &snapshot.memberships)
                .filter(|membership| !membership.pane_incarnation.is_empty())
            {
                routes.insert(
                    MutationRoute::Pane {
                        route: route.clone(),
                        pane_id: membership.pane_id.clone(),
                        pane_incarnation: membership.pane_incarnation.clone(),
                    },
                    (),
                );
            }
        }
        if let Ok(mut leases) = self.route_leases.lock() {
            leases.observe(routes);
        }
    }

    pub(crate) fn enqueue(
        &self,
        catalog: GroupCatalog,
        config_generation: u64,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        let route = AuthorityRoute::from_catalog(&catalog)
            .ok_or_else(|| "authority route has no observed authority".to_string())?;
        let mutation_route = MutationRoute::from_request(route.clone(), &request)?;
        let route_lease = self
            .route_leases
            .lock()
            .map_err(|_| "authority route leases are unavailable".to_string())?
            .valid
            .get(&mutation_route)
            .copied()
            .ok_or_else(|| format!("authority {} route is no longer fresh", route.authority))?;
        let mut sender = self
            .sender
            .lock()
            .map_err(|_| "remote authority mutation queue is unavailable".to_string())?;
        if sender.is_none() {
            let (request_tx, request_rx) = std::sync::mpsc::channel::<RoutedApiRequest>();
            let ssh_program = self.ssh_program.clone();
            let timeout = self.timeout;
            let current_generation = Arc::clone(&self.config_generation);
            let current_route_leases = Arc::clone(&self.route_leases);
            std::thread::Builder::new()
                .name("herdr-group-mutations".into())
                .spawn(move || {
                    while let Ok(job) = request_rx.recv() {
                        let id = job.request.id.clone();
                        if job.config_generation
                            != current_generation.load(std::sync::atomic::Ordering::Acquire)
                        {
                            let response = serde_json::to_string(
                                &crate::api::schema::ErrorResponse {
                                    id,
                                    error: crate::api::schema::ErrorBody {
                                        code: "authority_not_fresh".into(),
                                        message: format!(
                                            "authority {} fleet configuration changed before the queued mutation could run",
                                            job.route.authority
                                        ),
                                    },
                                },
                            )
                            .unwrap_or_else(|_| "{}".to_string());
                            let _ = job.respond_to.send(response);
                            continue;
                        }
                        let route_is_current = current_route_leases
                            .lock()
                            .is_ok_and(|leases| {
                                leases.valid.get(&job.mutation_route) == Some(&job.route_lease)
                            });
                        if !route_is_current {
                            let response = serde_json::to_string(
                                &crate::api::schema::ErrorResponse {
                                    id,
                                    error: crate::api::schema::ErrorBody {
                                        code: "authority_not_fresh".into(),
                                        message: format!(
                                            "authority {} route changed before the queued mutation could run",
                                            job.route.authority
                                        ),
                                    },
                                },
                            )
                            .unwrap_or_else(|_| "{}".to_string());
                            let _ = job.respond_to.send(response);
                            continue;
                        }
                        let response = route_api_request_with_ssh_program(
                            &job.catalog,
                            &job.request,
                            timeout,
                            &ssh_program,
                        )
                        .unwrap_or_else(|error| {
                            serde_json::to_string(&crate::api::schema::ErrorResponse {
                                id,
                                error: crate::api::schema::ErrorBody {
                                    code: "authority_unreachable".into(),
                                    message: format!(
                                        "authority {} is unreachable: {error}",
                                        job.route.authority
                                    ),
                                },
                            })
                            .unwrap_or_else(|_| "{}".to_string())
                        });
                        let _ = job.respond_to.send(response);
                    }
                })
                .map_err(|error| {
                    format!("cannot start remote authority mutation worker: {error}")
                })?;
            *sender = Some(request_tx);
        }
        let Some(sender) = sender.as_ref() else {
            return Err("remote authority mutation queue did not initialize".into());
        };
        sender
            .send(RoutedApiRequest {
                catalog,
                config_generation,
                route,
                mutation_route,
                route_lease,
                request,
                respond_to,
            })
            .map_err(|_| "remote authority mutation worker stopped".to_string())
    }
}

fn route_api_request_with_ssh_program(
    catalog: &GroupCatalog,
    request: &Request,
    timeout: Duration,
    ssh_program: impl AsRef<OsStr>,
) -> Result<String, String> {
    let value = if catalog.local {
        api_client_for_catalog(catalog)
            .request_value_with_timeout(request, timeout)
            .map_err(|error| error.to_string())?
    } else {
        let request_json = serde_json::to_string(request).map_err(|error| error.to_string())?;
        let socket = catalog
            .socket
            .as_deref()
            .map(|path| format!("export HERDR_SOCKET_PATH={}\n", shell_quote(path)))
            .unwrap_or_default();
        let session = catalog
            .session
            .as_deref()
            .map(|name| format!("export HERDR_SESSION={}\n", shell_quote(name)))
            .unwrap_or_default();
        let script = format!(
            "set -u\n{socket}{session}printf '%s\\n' {} | herdr api relay\n",
            shell_quote(&request_json)
        );
        let output = run_ssh_program_with_timeout(ssh_program, &catalog.target, &script, timeout)?;
        serde_json::from_slice(output.trim_ascii())
            .map_err(|error| format!("invalid authority mutation response: {error}"))?
    };
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

fn api_client_for_catalog(catalog: &GroupCatalog) -> ApiClient {
    catalog.socket.as_ref().map_or_else(
        || ApiClient::for_target(ConnectionTarget::LocalSession(catalog.session.clone())),
        |path| ApiClient::for_target(ConnectionTarget::SocketPath(PathBuf::from(path))),
    )
}

impl Snapshot {
    fn identity_conflicts(&self) -> HashSet<crate::groups::AuthorityId> {
        let mut reports_by_authority = HashMap::new();
        for catalog in &self.group_catalogs {
            let Some(authority) = catalog.observed_authority_id.as_ref() else {
                continue;
            };
            *reports_by_authority
                .entry(authority.clone())
                .or_insert(0_usize) += 1;
        }
        reports_by_authority
            .into_iter()
            .filter_map(|(authority, reports)| (reports > 1).then_some(authority))
            .collect()
    }

    fn mark_identity_conflicts(&mut self, durable_history_available: bool) {
        let conflicts = self.identity_conflicts();
        for catalog in &mut self.group_catalogs {
            let Some(authority) = catalog.observed_authority_id.as_ref() else {
                continue;
            };
            if conflicts.contains(authority) {
                catalog.state = GroupCatalogState::IdentityConflict;
                catalog.error = Some(if durable_history_available {
                    format!(
                        "authority identity conflict: {authority} is reported by multiple connections"
                    )
                } else {
                    format!(
                        "authority identity conflict while durable history is unavailable: {authority}"
                    )
                });
            }
        }
    }

    pub(crate) fn unpolled(hosts: &[FleetHostConfig]) -> Self {
        Self {
            configured_hosts: hosts.iter().map(|host| host.name.clone()).collect(),
            ..Self::default()
        }
    }

    pub(crate) fn reconcile_after_config_reload(
        &self,
        fleet: &FleetConfig,
        config_generation: u64,
    ) -> Self {
        let previous_hosts = self
            .hosts
            .iter()
            .map(|host| (host.name.as_str(), host))
            .collect::<HashMap<_, _>>();
        let hosts = fleet
            .hosts
            .iter()
            .filter_map(|configured| {
                previous_hosts
                    .get(configured.name.as_str())
                    .copied()
                    .filter(|observed| observed.matches_config(configured))
                    .cloned()
            })
            .collect();
        let matches_connection = |catalog: &GroupCatalog, configured: &FleetHostConfig| {
            catalog.target == configured.target
                && catalog.local == configured.local
                && catalog.session == configured.session
                && catalog.socket == configured.socket
        };
        let mut group_catalogs = self
            .group_catalogs
            .iter()
            .filter(|catalog| {
                fleet.hosts.iter().any(|configured| {
                    configured.name == catalog.host && matches_connection(catalog, configured)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        for catalog in self.group_catalogs.iter().filter(|catalog| {
            !fleet.hosts.iter().any(|configured| {
                configured.name == catalog.host && matches_connection(catalog, configured)
            })
        }) {
            let Some(configured) = fleet.hosts.iter().find(|configured| {
                matches_connection(catalog, configured)
                    && !group_catalogs.iter().any(|current| {
                        current.host == configured.name
                            && current.matches_connection(catalog)
                            && current.authority_id() == catalog.authority_id()
                    })
            }) else {
                continue;
            };
            let mut retained = catalog.clone();
            retained.host.clone_from(&configured.name);
            group_catalogs.push(retained);
        }

        Self {
            polled: self.polled,
            refreshed_at: self.refreshed_at,
            refreshed_at_unix_ms: self.refreshed_at_unix_ms,
            config_generation,
            configured_hosts: fleet.hosts.iter().map(|host| host.name.clone()).collect(),
            hosts,
            group_catalogs,
        }
    }

    pub(crate) fn into_rows(self) -> Vec<FleetRow> {
        self.hosts
            .into_iter()
            .flat_map(|host| host.entries)
            .collect()
    }

    /// Retain only a connection's last live answer when the current poll could
    /// not read one. Durable authority history lives in
    /// `AuthorityAcceptanceLedger`, never in this presentation snapshot.
    pub(crate) fn retain_unavailable_group_catalogs_from(&mut self, previous: &Self) {
        for catalog in &mut self.group_catalogs {
            if catalog.snapshot.is_some() {
                continue;
            }
            if let Some(retained) = previous
                .group_catalogs
                .iter()
                .find(|old| old.matches_connection(catalog) && old.snapshot.is_some())
            {
                catalog.snapshot = retained.snapshot.clone();
                catalog.state = GroupCatalogState::Stale;
            }
        }
    }

    /// Validate current authority reports against durable history and return
    /// the complete advance-only candidate ledger. Conflicted reports never
    /// advance history, and rejected reports remain visible only as stale
    /// current observations.
    pub(crate) fn admit_group_catalogs(
        &mut self,
        ledger: &AuthorityAcceptanceLedger,
    ) -> AuthorityAcceptanceLedger {
        self.mark_identity_conflicts(true);
        let mut candidate = ledger.clone();
        for catalog in &mut self.group_catalogs {
            if catalog.state != GroupCatalogState::Fresh {
                continue;
            }
            let Some(incoming) = catalog.snapshot.as_ref() else {
                catalog.state = GroupCatalogState::Unavailable;
                catalog.error = Some("fresh authority catalog had no snapshot".into());
                continue;
            };
            if catalog.observed_authority_id.as_ref() != Some(&incoming.authority_id) {
                catalog.state = GroupCatalogState::Unavailable;
                catalog.error = Some("reported authority does not match its snapshot".into());
                continue;
            }
            if let Err(error) = candidate.advance(incoming) {
                catalog.state = GroupCatalogState::Stale;
                catalog.error = Some(format!("catalog rejected: {error}"));
            }
        }
        candidate
    }

    pub(crate) fn reject_group_catalogs_without_durable_history(&mut self, error: &str) {
        for catalog in &mut self.group_catalogs {
            if catalog.state != GroupCatalogState::IdentityConflict {
                catalog.state = GroupCatalogState::Unavailable;
            }
            catalog.error = Some(error.to_string());
        }

        self.mark_identity_conflicts(false);
    }

    pub(crate) fn quarantine_unpersisted_advances(
        &mut self,
        ledger: &AuthorityAcceptanceLedger,
        error: &str,
    ) {
        for catalog in &mut self.group_catalogs {
            if catalog.state != GroupCatalogState::Fresh {
                continue;
            }
            let Some(snapshot) = catalog.snapshot.as_ref() else {
                catalog.state = GroupCatalogState::Unavailable;
                catalog.error = Some(error.to_string());
                continue;
            };
            if ledger.contains_snapshot(snapshot) {
                continue;
            }
            catalog.state = if ledger.accepted(&snapshot.authority_id).is_some() {
                GroupCatalogState::Stale
            } else {
                GroupCatalogState::Unavailable
            };
            catalog.error = Some(error.to_string());
        }
        self.mark_identity_conflicts(false);
    }

    pub(crate) fn fresh_group_catalog(
        &self,
        authority: &crate::groups::AuthorityId,
    ) -> Result<&GroupCatalog, String> {
        let matching = self
            .group_catalogs
            .iter()
            .filter(|catalog| catalog.observed_authority_id.as_ref() == Some(authority))
            .collect::<Vec<_>>();
        if self.identity_conflicts().contains(authority)
            || matching
                .iter()
                .any(|catalog| catalog.state == GroupCatalogState::IdentityConflict)
        {
            return Err(format!("authority {authority} has an identity conflict"));
        }
        let Some(catalog) = matching.first().copied() else {
            return Err(format!("authority {authority} has not been observed"));
        };
        if !catalog.is_fresh() {
            return Err(format!("authority {authority} is not fresh"));
        }
        Ok(catalog)
    }

    pub(crate) fn authority_has_identity_conflict(
        &self,
        authority: &crate::groups::AuthorityId,
    ) -> bool {
        self.identity_conflicts().contains(authority)
            || self.group_catalogs.iter().any(|catalog| {
                catalog.observed_authority_id.as_ref() == Some(authority)
                    && catalog.state == GroupCatalogState::IdentityConflict
            })
    }

    /// Keep the last observed remote inventory when a configured host cannot
    /// be polled. The fresh host state/error remains authoritative; only row
    /// identity and display metadata are retained for an honest unknown view.
    pub(crate) fn retain_unreachable_inventory_from(&mut self, previous: &Self) {
        let observed_at_unix_s = self
            .refreshed_at_unix_ms
            .map(|milliseconds| milliseconds / 1_000)
            .or_else(|| {
                self.refreshed_at.and_then(|refreshed_at| {
                    refreshed_at
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_secs())
                })
            });
        for host in &mut self.hosts {
            if host.local || host.state != HostState::Unreachable {
                continue;
            }
            let Some(old) = previous.hosts.iter().find(|old| {
                old.name == host.name
                    && old.target == host.target
                    && old.local == host.local
                    && old.session == host.session
                    && old.socket == host.socket
            }) else {
                continue;
            };
            let mut retained = old
                .entries
                .iter()
                .filter(|row| row.source != EvidenceSource::Host && row.error.is_none())
                .cloned()
                .collect::<Vec<_>>();
            if let Some(observed_at_unix_s) = observed_at_unix_s {
                for row in &mut retained {
                    row.age_s = row.age_seconds_at(observed_at_unix_s);
                }
            }
            retained.extend(std::mem::take(&mut host.entries));
            host.entries = retained;
            host.remote_identity = None;
        }
    }
}

impl HostSnapshot {
    pub(crate) fn matches_config(&self, configured: &FleetHostConfig) -> bool {
        self.local == configured.local
            && self.target == configured.target
            && self.socket == configured.socket
            && self.session == configured.session
    }
}

pub(crate) fn poll(fleet: &FleetConfig) -> Snapshot {
    poll_without_generation(fleet)
}

fn poll_with_generation(fleet: &FleetConfig, config_generation: u64) -> Snapshot {
    let mut snapshot = poll(fleet);
    snapshot.config_generation = config_generation;
    snapshot
}

fn poll_without_generation(fleet: &FleetConfig) -> Snapshot {
    let mut polling_fleet = fleet.clone();
    let self_name = polling_fleet.resolved_self_name();
    // Skip the synthetic local host when a configured host already carries the
    // resolved self name; select_hosts hard-errors on duplicate names.
    let implicit_local = !polling_fleet.hosts.iter().any(|host| host.local)
        && !polling_fleet
            .hosts
            .iter()
            .any(|host| host.name == self_name);
    if implicit_local {
        polling_fleet.hosts.insert(
            0,
            FleetHostConfig {
                name: self_name.clone(),
                local: true,
                ..FleetHostConfig::default()
            },
        );
    }
    match select_hosts(&polling_fleet, None) {
        Ok(hosts) => collect_snapshot_with_implicit_local(
            &SystemHostReader,
            &hosts,
            &polling_fleet,
            implicit_local.then(|| polling_fleet.resolved_self_name()),
        ),
        Err(error) => {
            let refreshed_at = SystemTime::now();
            Snapshot {
                polled: true,
                refreshed_at: Some(refreshed_at),
                refreshed_at_unix_ms: refreshed_at
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
                config_generation: 0,
                configured_hosts: polling_fleet
                    .hosts
                    .iter()
                    .map(|host| host.name.clone())
                    .collect(),
                hosts: polling_fleet
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
                        remote_identity: None,
                        entries: Vec::new(),
                    })
                    .collect(),
                group_catalogs: Vec::new(),
            }
        }
    }
}

/// The fleet host a local pane was attached to, read back from its launch
/// argv. Hosts may share an SSH target under different sessions or sockets, so
/// a match compares everything the attach encoded, not the target alone.
pub(crate) fn attached_host_name<'a>(snapshot: &'a Snapshot, argv: &[String]) -> Option<&'a str> {
    let target = match argv {
        [ssh, flag, target, _] if ssh == "ssh" && flag == "-t" => target,
        [herdr, flag, target, ..] if herdr == "herdr" && flag == "--remote" => target,
        _ => return None,
    };
    snapshot
        .hosts
        .iter()
        .filter(|host| !host.local && host.target == *target)
        .find(|host| match argv {
            [ssh, _, _, command] if ssh == "ssh" => {
                let agent_prefix =
                    remote_attach_command(host.socket.as_deref(), host.session.as_deref(), "");
                command.starts_with(agent_prefix.trim_end_matches("''"))
            }
            _ => host_attach_argv(host).is_ok_and(|expected| expected == argv),
        })
        .map(|host| host.name.as_str())
}

pub(crate) fn host_attach_argv(host: &HostSnapshot) -> Result<Vec<String>, String> {
    host_attach_argv_parts(&host.name, host.local, &host.target, host.session.as_ref())
}

pub(crate) fn host_attach_argv_from_config(host: &FleetHostConfig) -> Result<Vec<String>, String> {
    host_attach_argv_parts(&host.name, host.local, &host.target, host.session.as_ref())
}

fn host_attach_argv_parts(
    name: &str,
    local: bool,
    target: &str,
    session: Option<&String>,
) -> Result<Vec<String>, String> {
    if local {
        return Err(format!("{name} is the local host"));
    }
    if target.trim().is_empty() {
        return Err(format!("{name} has no SSH target"));
    }
    let mut argv = vec![
        "herdr".to_string(),
        "--remote".to_string(),
        target.to_string(),
    ];
    if let Some(session) = session.filter(|session| !session.is_empty()) {
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
#[cfg(test)]
pub(crate) fn agent_attach_argv(host: &HostSnapshot, agent: &str) -> Result<Vec<String>, String> {
    agent_attach_argv_parts(
        &host.name,
        host.local,
        &host.target,
        host.socket.as_deref(),
        host.session.as_deref(),
        agent,
    )
}

pub(crate) fn agent_attach_argv_from_config(
    host: &FleetHostConfig,
    agent: &str,
) -> Result<Vec<String>, String> {
    agent_attach_argv_parts(
        &host.name,
        host.local,
        &host.target,
        host.socket.as_deref(),
        host.session.as_deref(),
        agent,
    )
}

fn agent_attach_argv_parts(
    name: &str,
    local: bool,
    target: &str,
    socket: Option<&str>,
    session: Option<&str>,
    agent: &str,
) -> Result<Vec<String>, String> {
    if local {
        return Err(format!("{name} is the local host"));
    }
    if target.trim().is_empty() {
        return Err(format!("{name} has no SSH target"));
    }
    if agent.trim().is_empty() {
        return Err(format!("{name} has no agent target"));
    }
    Ok(vec![
        "ssh".to_string(),
        "-t".to_string(),
        target.to_string(),
        remote_attach_command(socket, session, agent),
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

#[derive(Debug, Clone)]
struct FleetPollerState {
    fleet: FleetConfig,
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct FleetPollerConfig {
    state: std::sync::Mutex<FleetPollerState>,
    changed: std::sync::Condvar,
}

pub(crate) type FleetPollerHandle = Arc<FleetPollerConfig>;

impl FleetPollerConfig {
    fn new(fleet: FleetConfig) -> Self {
        Self {
            state: std::sync::Mutex::new(FleetPollerState {
                fleet,
                generation: 0,
            }),
            changed: std::sync::Condvar::new(),
        }
    }

    pub(crate) fn replace(&self, fleet: FleetConfig) -> u64 {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.fleet = fleet;
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        self.changed.notify_all();
        generation
    }

    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    pub(crate) fn host(&self, name: &str) -> Option<FleetHostConfig> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(host) = state
            .fleet
            .hosts
            .iter()
            .find(|host| host.name == name)
            .cloned()
        {
            return Some(host);
        }
        (!state.fleet.hosts.iter().any(|host| host.local)
            && state.fleet.resolved_self_name() == name)
            .then(|| FleetHostConfig {
                name: name.to_string(),
                local: true,
                ..FleetHostConfig::default()
            })
    }

    pub(crate) fn symphony_target(&self) -> Result<(Option<FleetHostConfig>, Duration), String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let timeout =
            Duration::from_millis(state.fleet.timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS));
        let Some(name) = state.fleet.symphony_host.as_deref() else {
            return Ok((None, timeout));
        };
        let host = state
            .fleet
            .hosts
            .iter()
            .find(|host| host.name == name)
            .cloned()
            .ok_or_else(|| format!("Symphony host {name} is not configured"))?;
        if !host.local && (host.target.trim().is_empty() || host.target.starts_with('-')) {
            return Err(format!("Symphony host {name} has no valid SSH target"));
        }
        Ok(((!host.local).then_some(host), timeout))
    }

    fn snapshot(&self) -> FleetPollerState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn wait_for_change(&self, generation: u64, timeout: Duration) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _state = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.generation == generation)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

pub(crate) fn start_poller(
    fleet: FleetConfig,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) -> FleetPollerHandle {
    let poller_config = Arc::new(FleetPollerConfig::new(fleet));
    if cfg!(test) {
        return poller_config;
    }
    let poller_config_for_thread = Arc::clone(&poller_config);
    std::thread::spawn(move || loop {
        let state = poller_config_for_thread.snapshot();
        let snapshot = poll_with_generation(&state.fleet, state.generation);
        match event_tx.try_send(crate::events::AppEvent::FleetRefreshed { snapshot }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("dropped fleet refresh because the event queue is full");
            }
        }
        poller_config_for_thread.wait_for_change(
            state.generation,
            Duration::from_millis(state.fleet.refresh_interval_ms.max(MIN_REFRESH_INTERVAL_MS)),
        );
    });
    poller_config
}

#[derive(Debug)]
struct HostEvidence {
    host: FleetHostConfig,
    agents: Result<Vec<AgentInfo>, String>,
    groups: Option<Result<crate::groups::GroupAuthoritySnapshot, String>>,
    runs: Vec<Result<crate::agent_runs::Observation, String>>,
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
    collect_snapshot_with_implicit_local(reader, hosts, fleet, None)
}

fn collect_snapshot_with_implicit_local(
    reader: &impl HostReader,
    hosts: &[FleetHostConfig],
    fleet: &FleetConfig,
    implicit_local_name: Option<String>,
) -> Snapshot {
    let timeout = Duration::from_millis(fleet.timeout_ms);
    let evidence = std::thread::scope(|scope| {
        let handles = hosts
            .iter()
            .cloned()
            .map(|host| {
                let fallback = host.clone();
                let runs_only = host.local && implicit_local_name.as_deref() == Some(&host.name);
                (
                    fallback,
                    scope.spawn(move || {
                        if runs_only {
                            fetch_local_run_host(host)
                        } else {
                            fetch_host_with(reader, host, timeout)
                        }
                    }),
                )
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(host, handle)| match handle.join() {
                Ok(evidence) => evidence,
                Err(_) => HostEvidence {
                    host,
                    agents: Err("host reader panicked".into()),
                    groups: Some(Err("group catalog reader panicked".into())),
                    runs: Vec::new(),
                    runtime: HostRuntime::default(),
                },
            })
            .collect::<Vec<_>>()
    });

    snapshot_from_evidence(hosts, fleet, evidence, SystemTime::now())
}

fn fetch_local_run_host(host: FleetHostConfig) -> HostEvidence {
    HostEvidence {
        host,
        agents: Ok(Vec::new()),
        groups: None,
        runs: local_run_states(),
        runtime: HostRuntime::default(),
    }
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
    let mut group_catalogs = Vec::with_capacity(evidence.len());
    for evidence in evidence {
        if let Some(group_result) = evidence.groups.clone() {
            group_catalogs.push(GroupCatalog {
                host: evidence.host.name.clone(),
                target: evidence.host.target.clone(),
                local: evidence.host.local,
                session: evidence.host.session.clone(),
                socket: evidence.host.socket.clone(),
                state: if group_result.is_ok() {
                    GroupCatalogState::Fresh
                } else {
                    GroupCatalogState::Unavailable
                },
                observed_authority_id: group_result
                    .as_ref()
                    .ok()
                    .map(|snapshot| snapshot.authority_id.clone()),
                snapshot: group_result.as_ref().ok().cloned(),
                error: group_result.err(),
            });
        }
        let error = evidence.agents.as_ref().err().cloned();
        let remote_identity = (!evidence.host.local)
            .then(|| evidence.agents.as_ref().ok())
            .flatten()
            .and_then(|agents| remote_self_name(agents));
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
            remote_identity,
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
        config_generation: 0,
        configured_hosts: configured_hosts
            .iter()
            .map(|host| host.name.clone())
            .collect(),
        hosts,
        group_catalogs,
    }
}

/// Resolve a remote server's self identity from a complete agent inventory.
/// Every returned agent must carry the same host identity; an empty, legacy,
/// or mixed inventory is not safe enough to authorize a later control request.
fn remote_self_name(agents: &[AgentInfo]) -> Option<String> {
    let mut names = agents
        .iter()
        .map(|agent| {
            agent
                .agent_ref
                .as_ref()
                .map(|agent_ref| agent_ref.host.clone())
        })
        .collect::<Option<Vec<_>>>()?;
    names.sort_unstable();
    names.dedup();
    (names.len() == 1).then(|| names.remove(0))
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
    let client = api_client_for_host(&host);
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
    let groups = fetch_group_catalog(&client, timeout);
    let runs = local_run_states();
    HostEvidence {
        host,
        agents,
        groups: Some(groups),
        runs,
        runtime,
    }
}

fn api_client_for_host(host: &FleetHostConfig) -> ApiClient {
    host.socket.as_ref().map_or_else(
        || ApiClient::for_target(ConnectionTarget::LocalSession(host.session.clone())),
        |path| ApiClient::for_target(ConnectionTarget::SocketPath(PathBuf::from(path))),
    )
}

fn fetch_group_catalog(
    client: &ApiClient,
    timeout: Duration,
) -> Result<crate::groups::GroupAuthoritySnapshot, String> {
    let request = Request {
        id: "fleet:collect:groups".into(),
        method: Method::GroupHostSnapshot(EmptyParams::default()),
    };
    client
        .request_value_with_timeout(&request, timeout)
        .map_err(|error| error.to_string())
        .and_then(|value| {
            crate::api::client::parse_response_value(value)
                .map_err(|error| error.to_string())
                .and_then(|response| match response.result {
                    ResponseResult::GroupHostSnapshot { snapshot } => Ok(snapshot),
                    other => Err(format!("unexpected group snapshot response: {other:?}")),
                })
        })
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

fn local_run_states() -> Vec<Result<crate::agent_runs::Observation, String>> {
    let Some(home) = std::env::var_os("HOME") else {
        return vec![Err("HOME is unavailable; cannot read ~/.agents/runs".into())];
    };
    read_run_state_dir(&PathBuf::from(home).join(".agents/runs"))
}

fn read_run_state_dir(root: &Path) -> Vec<Result<crate::agent_runs::Observation, String>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => return vec![Err(format!("cannot read {}: {error}", root.display()))],
    };
    recent_run_state_paths(entries.map(|entry| entry.map(|entry| entry.path())))
        .into_iter()
        .map(|path| read_run_state_file(&path))
        .collect()
}

fn recent_run_state_paths(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
) -> Vec<PathBuf> {
    let mut paths = entries
        .into_iter()
        .take(MAX_RUN_DIRECTORY_ENTRIES)
        .filter_map(Result::ok)
        .map(|path| path.join("state.json"))
        .filter_map(|path| {
            let modified = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    paths
        .into_iter()
        .take(crate::agent_runs::MAX_RUNS_PER_HOST)
        .map(|(_, path)| path)
        .collect()
}

fn read_run_state_file(path: &Path) -> Result<crate::agent_runs::Observation, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let bytes =
        match crate::platform::read_limited_reader(file, crate::agent_runs::MAX_RUN_STATE_BYTES)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        {
            crate::platform::LimitedRead::Empty => Vec::new(),
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Oversized => {
                return Err(format!(
                    "run state {} exceeds {} bytes",
                    path.display(),
                    crate::agent_runs::MAX_RUN_STATE_BYTES
                ));
            }
        };
    crate::agent_runs::parse_state(&bytes, &path.display().to_string()).map(|state| {
        crate::agent_runs::Observation {
            pid_alive: crate::platform::process_exists(state.pid),
            state,
        }
    })
}

fn fetch_remote_host(host: FleetHostConfig, timeout: Duration) -> HostEvidence {
    let script = remote_read_script(host.socket.as_deref(), host.session.as_deref());
    let output = run_ssh_with_timeout(&host.target, &script, timeout);
    let (agents, groups, runs, runtime) = match output {
        Ok(output) => parse_remote_output(&output),
        Err(error) => (
            Err(error.clone()),
            Err(error),
            Vec::new(),
            HostRuntime::default(),
        ),
    };
    HostEvidence {
        host,
        agents,
        groups: Some(groups),
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
        "set -u\n{socket}{session}herdr agent list || true\nprintf '\\036HERDR_FLEET_GROUPS_V1\\036\\n'\nherdr api authority-snapshot || true\nprintf '\\036HERDR_FLEET_RUNS_V1\\036\\n'\nif [ -d \"$HOME/.agents/runs\" ]; then\n  ls -1t \"$HOME\"/.agents/runs/*/state.json 2>/dev/null | sed -n '1,{}p' | while IFS= read -r file; do\n    [ -f \"$file\" ] || continue\n    pid=$(head -c {} \"$file\" | sed -n 's/.*\"pid\"[[:space:]]*:[[:space:]]*\\([0-9][0-9]*\\).*/\\1/p' | head -n 1)\n    alive=0\n    if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then alive=1; fi\n    printf '\\036HERDR_FLEET_RUN_V1:%s\\036\\n' \"$alive\"\n    head -c {} \"$file\"\n    printf '\\n'\n  done\nfi\nprintf '\\036HERDR_FLEET_HOST_V1\\036\\n'\nherdr status server --json || true\n",
        crate::agent_runs::MAX_RUNS_PER_HOST,
        crate::agent_runs::MAX_RUN_STATE_BYTES + 1,
        crate::agent_runs::MAX_RUN_STATE_BYTES + 1,
    )
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn run_ssh_with_timeout(
    target: &str,
    script: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    run_ssh_program_with_timeout("ssh", target, script, timeout)
}

fn run_ssh_program_with_timeout(
    program: impl AsRef<OsStr>,
    target: &str,
    script: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let connect_timeout = timeout.as_secs().max(1).to_string();
    let mut command = crate::noninteractive_process::command(program);
    command
        .args(["-o", "BatchMode=yes", "-o"])
        .arg(format!("ConnectTimeout={connect_timeout}"))
        .args(["-o", "ServerAliveInterval=2", "-o", "ServerAliveCountMax=1"])
        .arg(target)
        .args(["sh", "-s"]);
    let output = crate::noninteractive_process::output_with_stdin_and_deadline_limited(
        command,
        script.as_bytes().to_vec(),
        Instant::now() + timeout,
        MAX_REMOTE_OUTPUT_BYTES,
    )
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::TimedOut => format!(
            "STATUS UNKNOWN: host read timed out after {}ms",
            timeout.as_millis()
        ),
        std::io::ErrorKind::FileTooLarge => {
            format!("STATUS UNKNOWN: host read exceeded {MAX_REMOTE_OUTPUT_BYTES} bytes")
        }
        _ => format!("failed to run ssh: {error}"),
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("STATUS UNKNOWN: ssh exited with {}", output.status)
        } else {
            format!("STATUS UNKNOWN: {detail}")
        });
    }
    Ok(output.stdout)
}

fn parse_remote_output(output: &[u8]) -> ParsedRemoteOutput {
    let Some(marker_at) = output
        .windows(REMOTE_RUNS_MARKER.len())
        .position(|window| window == REMOTE_RUNS_MARKER)
    else {
        return (
            Err("remote output did not include the fleet read marker".into()),
            Err("remote output did not include the group catalog marker".into()),
            Vec::new(),
            HostRuntime::default(),
        );
    };
    let pre_runs = &output[..marker_at];
    let (agent_bytes, group_bytes) = pre_runs
        .windows(REMOTE_GROUPS_MARKER.len())
        .position(|window| window == REMOTE_GROUPS_MARKER)
        .map_or((pre_runs, None), |groups_marker_at| {
            (
                &pre_runs[..groups_marker_at],
                Some(&pre_runs[groups_marker_at + REMOTE_GROUPS_MARKER.len()..]),
            )
        });
    let response = serde_json::from_slice(agent_bytes.trim_ascii())
        .map_err(|error| format!("invalid remote agent-list JSON: {error}"))
        .and_then(|value| {
            crate::api::client::parse_response_value(value)
                .map_err(|error| error.to_string())
                .and_then(|response| match response.result {
                    ResponseResult::AgentList { agents } => Ok(agents),
                    other => Err(format!("unexpected agent-list response: {other:?}")),
                })
        });
    let groups = group_bytes
        .ok_or_else(|| "remote output did not include the group catalog marker".to_string())
        .and_then(|bytes| {
            serde_json::from_slice(bytes.trim_ascii())
                .map_err(|error| format!("invalid remote group-catalog JSON: {error}"))
        })
        .and_then(|value| {
            crate::api::client::parse_response_value(value)
                .map_err(|error| error.to_string())
                .and_then(|response| match response.result {
                    ResponseResult::GroupHostSnapshot { snapshot } => Ok(snapshot),
                    other => Err(format!("unexpected group snapshot response: {other:?}")),
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
    let runs = parse_remote_run_records(state_bytes);
    (response, groups, runs, runtime)
}

fn parse_remote_run_records(bytes: &[u8]) -> Vec<Result<crate::agent_runs::Observation, String>> {
    let mut records = Vec::new();
    let mut remaining = bytes;
    while let Some(marker_at) = remaining
        .windows(REMOTE_RUN_RECORD_MARKER.len())
        .position(|window| window == REMOTE_RUN_RECORD_MARKER)
    {
        remaining = &remaining[marker_at + REMOTE_RUN_RECORD_MARKER.len()..];
        let Some(header_end) = remaining.windows(2).position(|window| window == b"\x1e\n") else {
            records.push(Err("invalid remote run marker".to_string()));
            break;
        };
        let pid_alive = &remaining[..header_end] == b"1";
        remaining = &remaining[header_end + 2..];
        let next_marker = remaining
            .windows(REMOTE_RUN_RECORD_MARKER.len())
            .position(|window| window == REMOTE_RUN_RECORD_MARKER)
            .unwrap_or(remaining.len());
        let state_bytes = remaining[..next_marker].trim_ascii();
        let source = format!("remote run state #{}", records.len() + 1);
        records.push(
            if state_bytes.len() > crate::agent_runs::MAX_RUN_STATE_BYTES {
                Err(format!(
                    "run state {source} exceeds {} bytes",
                    crate::agent_runs::MAX_RUN_STATE_BYTES
                ))
            } else {
                crate::agent_runs::parse_state(state_bytes, &source)
                    .map(|state| crate::agent_runs::Observation { state, pid_alive })
            },
        );
        remaining = &remaining[next_marker..];
    }
    records
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
    #[serde(skip)]
    run_summary: Option<std::sync::Arc<crate::agent_runs::Summary>>,
}

#[cfg(test)]
pub(crate) fn counts_as_live_agent(entry: &FleetRow) -> bool {
    entry.source != EvidenceSource::Host && entry.state != "status_unknown"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffectiveRemoteLifecycle<'a> {
    pub(crate) state_label: &'a str,
    pub(crate) state: crate::detect::AgentState,
    pub(crate) seen: bool,
    pub(crate) stale: bool,
    pub(crate) attention_tier: Option<crate::terminal::state::AttentionTier>,
    pub(crate) open_blockers: bool,
    pub(crate) usage_limited: bool,
    pub(crate) waiting_on_agents: bool,
    pub(crate) settled: bool,
    pub(crate) snoozed_until: Option<u64>,
}

impl FleetRow {
    pub(crate) fn age_seconds_at(&self, now_unix_s: u64) -> Option<u64> {
        self.reported_at
            .as_deref()
            .and_then(parse_utc_timestamp)
            .and_then(|reported_at| now_unix_s.checked_sub(reported_at))
            .or(self.age_s)
    }

    pub(crate) fn counts_as_live_agent(&self, host_state: HostState, now_unix_s: u64) -> bool {
        self.source != EvidenceSource::Host
            && self
                .effective_remote_lifecycle(host_state, now_unix_s)
                .state_label
                != "status_unknown"
    }

    /// Combine the owning host's reachability with the last observed agent
    /// lifecycle. Retained inventory stays useful during an outage, but stale
    /// gates never remain actionable and a snooze only lasts until its original
    /// server-owned deadline.
    pub(crate) fn effective_remote_lifecycle(
        &self,
        host_state: HostState,
        now_unix_s: u64,
    ) -> EffectiveRemoteLifecycle<'_> {
        let projection = self.agent_info.as_ref().map(AgentInfo::agent_projection);
        let settled = projection.is_some_and(|projection| projection.settled);
        let snoozed_until = self
            .agent_info
            .as_ref()
            .and_then(|info| info.snoozed_until)
            .filter(|deadline| *deadline > now_unix_s);

        if host_state == HostState::Unreachable {
            return EffectiveRemoteLifecycle {
                state_label: "status_unknown",
                state: crate::detect::AgentState::Unknown,
                seen: true,
                stale: true,
                attention_tier: Some(crate::terminal::state::AttentionTier::None),
                open_blockers: false,
                usage_limited: false,
                waiting_on_agents: false,
                settled,
                snoozed_until,
            };
        }

        if let Some(projection) = projection {
            return EffectiveRemoteLifecycle {
                state_label: &self.state,
                state: projection.state,
                seen: projection.seen,
                stale: projection.stale,
                attention_tier: Some(projection.attention_tier),
                open_blockers: projection.open_blockers,
                usage_limited: projection.usage_limited,
                waiting_on_agents: projection.waiting_on_agents,
                settled,
                snoozed_until,
            };
        }

        let state = if self.blocked {
            crate::detect::AgentState::Blocked
        } else {
            match self.state.as_str() {
                "active" | "working" => crate::detect::AgentState::Working,
                "waiting" | "done" | "failed" => crate::detect::AgentState::Idle,
                _ => crate::detect::AgentState::Unknown,
            }
        };
        EffectiveRemoteLifecycle {
            state_label: &self.state,
            state,
            seen: !matches!(self.state.as_str(), "done" | "failed"),
            stale: state == crate::detect::AgentState::Unknown,
            attention_tier: None,
            open_blockers: false,
            usage_limited: false,
            waiting_on_agents: false,
            settled: false,
            snoozed_until: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_agent_row(host: &str, name: &str) -> Self {
        Self::test_agent_row_with_id(host, name, name)
    }

    #[cfg(test)]
    pub(crate) fn test_agent_row_with_id(host: &str, name: &str, agent_id: &str) -> Self {
        let agent_ref =
            crate::api::schema::AgentRef::new(host, agent_id).expect("valid test agent reference");
        let mut row = Self::unknown(host, EvidenceSource::Herdr, agent_ref, String::new())
            .with_test_name(name)
            .with_test_state("working");
        row.error = None;
        row
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
    pub(crate) fn test_run_summary_row(summary: crate::agent_runs::Summary) -> Self {
        let mut row = Self::test_run_row(
            &summary.host,
            &summary.run_id,
            summary.state == crate::agent_runs::DisplayState::Blocked,
        );
        row.run_summary = Some(std::sync::Arc::new(summary));
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
            run_summary: None,
        })
    }

    fn from_run(
        host: &str,
        observation: crate::agent_runs::Observation,
        now_s: u64,
        heartbeat_stale_s: u64,
    ) -> Option<Self> {
        let run = observation.state.clone();
        let heartbeat_at = parse_utc_timestamp(&run.last_heartbeat);
        let age_s = heartbeat_at.and_then(|heartbeat| now_s.checked_sub(heartbeat));
        let fresh = age_s.is_some_and(|age| age <= heartbeat_stale_s);
        let blocked = run.state == crate::agent_runs::State::Blocked;
        let liveness = match run.state {
            crate::agent_runs::State::Active
            | crate::agent_runs::State::Blocked
            | crate::agent_runs::State::Waiting
                if fresh =>
            {
                Liveness::Live
            }
            crate::agent_runs::State::Done | crate::agent_runs::State::Failed => Liveness::Terminal,
            crate::agent_runs::State::Active
            | crate::agent_runs::State::Blocked
            | crate::agent_runs::State::Waiting
            | crate::agent_runs::State::Unknown
            | crate::agent_runs::State::Empty => Liveness::Unknown,
        };
        let raw_state = run.state.as_str().to_string();
        let state = effective_state(&raw_state, liveness, blocked);
        let blocked_reason = run.blocked_reason.clone();
        let gate_summary = blocked_reason
            .as_ref()
            .map(|reason| format!("windowless run blocked: {reason}"));
        let parent_handle = run
            .parent
            .run_id
            .as_ref()
            .and_then(|run_id| crate::api::schema::AgentRef::new(&run.parent.host, run_id).ok())
            .map(|agent_ref| agent_ref.to_string())
            .or_else(|| {
                run.parent
                    .session
                    .as_ref()
                    .map(|session| format!("session:{}/{}", run.parent.host, session))
            });
        let summary = std::sync::Arc::new(crate::agent_runs::summarize(
            host,
            observation,
            now_s,
            heartbeat_stale_s,
        ));
        let agent_ref = crate::api::schema::AgentRef::new(host, summary.run_id.clone()).ok()?;
        let handle = agent_ref.to_string();
        Some(Self {
            host: host.into(),
            agent_ref,
            source: EvidenceSource::RunState,
            handle,
            agent: Some(run.agent),
            name: Some(summary.run_id.clone()),
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
            parent_handle,
            descendants: DescendantScore::default(),
            error: None,
            native_session: None,
            agent_info: None,
            run_summary: Some(summary),
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
            run_summary: None,
        }
    }

    pub(crate) fn agent_info(&self) -> Option<&AgentInfo> {
        self.agent_info.as_ref()
    }

    pub(crate) fn run_summary(&self) -> Option<&std::sync::Arc<crate::agent_runs::Summary>> {
        self.run_summary.as_ref()
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

pub(crate) fn parse_utc_timestamp(value: &str) -> Option<u64> {
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

    fn run_fixture_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-fleet-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create fleet fixture directory");
        dir
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, contents).expect("write executable fixture");
        let mut permissions = std::fs::metadata(path)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("set executable fixture mode");
    }

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
                groups: Some(Err("group catalog unavailable in fake reader".into())),
                runs: Vec::new(),
                runtime: self.runtime.clone(),
            }
        }

        fn fetch_remote(&self, host: FleetHostConfig, _timeout: Duration) -> HostEvidence {
            self.remote_calls.fetch_add(1, Ordering::Relaxed);
            HostEvidence {
                host,
                agents: self.agents.clone(),
                groups: Some(Err("group catalog unavailable in fake reader".into())),
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

    fn group_catalog(
        host: &str,
        target: &str,
        seed: u8,
        revision: u64,
        groups: Vec<crate::groups::GroupRecord>,
    ) -> GroupCatalog {
        GroupCatalog {
            host: host.into(),
            target: target.into(),
            local: false,
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
            state: GroupCatalogState::Fresh,
            observed_authority_id: Some(crate::groups::AuthorityId::from_random_bytes([seed; 16])),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: crate::groups::AuthorityId::from_random_bytes([seed; 16]),
                revision,
                groups,
                memberships: Vec::new(),
            }),
            error: None,
        }
    }

    fn group_record(
        seed: u8,
        local: u64,
        revision: u64,
        deleted: bool,
    ) -> crate::groups::GroupRecord {
        crate::groups::GroupRecord {
            id: crate::groups::GroupId {
                owner: crate::groups::AuthorityId::from_random_bytes([seed; 16]),
                local,
            },
            revision,
            state: if deleted {
                crate::groups::GroupState::Deleted
            } else {
                crate::groups::GroupState::Active {
                    name: "Work".into(),
                }
            },
        }
    }

    fn ledger_with_deleted_group() -> AuthorityAcceptanceLedger {
        let mut ledger = AuthorityAcceptanceLedger::default();
        ledger
            .advance(
                group_catalog(
                    "history",
                    "ignored",
                    1,
                    2,
                    vec![group_record(1, 1, 2, true)],
                )
                .snapshot
                .as_ref()
                .expect("authority snapshot"),
            )
            .expect("accept tombstone");
        ledger
    }

    fn assert_rolled_back_group_is_not_fresh(
        ledger: &AuthorityAcceptanceLedger,
        host: &str,
        target: &str,
    ) {
        let mut rolled_back = Snapshot {
            polled: true,
            group_catalogs: vec![group_catalog(
                host,
                target,
                1,
                1,
                vec![group_record(1, 1, 1, false)],
            )],
            ..Snapshot::default()
        };
        let candidate = rolled_back.admit_group_catalogs(ledger);

        assert_eq!(candidate, *ledger);
        assert_eq!(
            rolled_back.group_catalogs[0].state,
            GroupCatalogState::Stale
        );
        assert!(matches!(
            ledger
                .accepted(&crate::groups::AuthorityId::from_random_bytes([1; 16]))
                .expect("accepted tombstone history")
                .groups[0]
                .state,
            crate::groups::GroupState::Deleted
        ));
        assert!(matches!(
            rolled_back.group_catalogs[0]
                .snapshot
                .as_ref()
                .expect("current rolled-back answer")
                .groups[0]
                .state,
            crate::groups::GroupState::Active { .. }
        ));
    }

    #[test]
    fn deleted_group_stays_deleted_after_connection_remove_and_readd() {
        let dir = run_fixture_dir("group-history-remove-readd");
        let path = dir.join("remote-group-catalogs.json");
        let ledger = ledger_with_deleted_group();
        save_authority_acceptance_ledger(&path, &ledger).expect("persist accepted tombstone");

        let removed = Snapshot::default().reconcile_after_config_reload(&FleetConfig::default(), 1);
        assert!(removed.group_catalogs.is_empty());

        let retained =
            load_authority_acceptance_ledger(&path).expect("reload after restoring connection");
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-a");
        std::fs::remove_dir_all(dir).expect("remove remove-readd fixture");
    }

    #[test]
    fn deleted_group_stays_deleted_after_connection_repoint() {
        let dir = run_fixture_dir("group-history-repoint");
        let path = dir.join("remote-group-catalogs.json");
        let ledger = ledger_with_deleted_group();
        save_authority_acceptance_ledger(&path, &ledger).expect("persist accepted tombstone");

        let retained =
            load_authority_acceptance_ledger(&path).expect("load after repointing the connection");
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-b");
        std::fs::remove_dir_all(dir).expect("remove repoint fixture");
    }

    struct PanickingReader;

    impl HostReader for PanickingReader {
        fn fetch_local(&self, _host: FleetHostConfig, _timeout: Duration) -> HostEvidence {
            panic!("fixture poll panic")
        }

        fn fetch_remote(&self, _host: FleetHostConfig, _timeout: Duration) -> HostEvidence {
            panic!("fixture poll panic")
        }
    }

    #[test]
    fn deleted_group_stays_deleted_after_poll_worker_panic() {
        let dir = run_fixture_dir("group-history-poll-panic");
        let path = dir.join("remote-group-catalogs.json");
        let configured = FleetHostConfig {
            name: "office".into(),
            target: "machine-a".into(),
            local: false,
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
        };
        let fleet = FleetConfig {
            hosts: vec![configured.clone()],
            ..FleetConfig::default()
        };
        let ledger = ledger_with_deleted_group();
        save_authority_acceptance_ledger(&path, &ledger).expect("persist accepted tombstone");

        let panicked = collect_snapshot_with(&PanickingReader, &[configured], &fleet);
        assert_eq!(panicked.group_catalogs[0].target, "machine-a");
        assert_eq!(
            panicked.group_catalogs[0].state,
            GroupCatalogState::Unavailable
        );

        let retained = load_authority_acceptance_ledger(&path).expect("reload after poll panic");
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-a");
        std::fs::remove_dir_all(dir).expect("remove poll panic fixture");
    }

    #[test]
    fn deleted_group_stays_deleted_after_invalid_fleet_configuration() {
        let dir = run_fixture_dir("group-history-invalid-config");
        let path = dir.join("remote-group-catalogs.json");
        let ledger = ledger_with_deleted_group();
        save_authority_acceptance_ledger(&path, &ledger).expect("persist accepted tombstone");

        let invalid = FleetConfig {
            self_name: Some("invalid::self".into()),
            ..FleetConfig::default()
        };
        let unusable = poll_without_generation(&invalid);
        assert!(unusable.group_catalogs.is_empty());

        let retained =
            load_authority_acceptance_ledger(&path).expect("reload after repairing fleet config");
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-a");
        std::fs::remove_dir_all(dir).expect("remove invalid config fixture");
    }

    #[test]
    fn deleted_group_stays_deleted_after_authority_a_then_b_then_rolled_back_a() {
        let dir = run_fixture_dir("group-history-a-b-a");
        let path = dir.join("remote-group-catalogs.json");
        let mut ledger = ledger_with_deleted_group();
        let authority_b = group_catalog(
            "office",
            "machine-b",
            2,
            1,
            vec![group_record(2, 1, 1, false)],
        );
        ledger
            .advance(authority_b.snapshot.as_ref().expect("authority B snapshot"))
            .expect("accept authority B");
        save_authority_acceptance_ledger(&path, &ledger).expect("persist both authority histories");

        let retained =
            load_authority_acceptance_ledger(&path).expect("load after authority switch");
        assert!(retained
            .accepted(&crate::groups::AuthorityId::from_random_bytes([2; 16]))
            .is_some());
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-b");
        std::fs::remove_dir_all(dir).expect("remove authority switch fixture");
    }

    #[test]
    fn rejected_catalog_retains_the_last_accepted_snapshot_as_stale() {
        let previous_catalog = group_catalog(
            "office",
            "machine-a",
            1,
            2,
            vec![group_record(1, 1, 2, true)],
        );
        let mut ledger = AuthorityAcceptanceLedger::default();
        ledger
            .advance(
                previous_catalog
                    .snapshot
                    .as_ref()
                    .expect("accepted snapshot"),
            )
            .expect("accept retained snapshot");
        let mut incoming = Snapshot {
            group_catalogs: vec![group_catalog("office", "machine-a", 1, 3, Vec::new())],
            ..Snapshot::default()
        };

        let candidate = incoming.admit_group_catalogs(&ledger);

        assert_eq!(candidate, ledger);
        assert_eq!(incoming.group_catalogs[0].state, GroupCatalogState::Stale);
        assert!(incoming.group_catalogs[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("lost observed tombstone")));
    }

    #[test]
    fn stale_catalog_retention_keys_on_authority_and_connection_not_alias() {
        let previous_catalog = group_catalog(
            "office",
            "machine-a",
            1,
            1,
            vec![group_record(1, 1, 1, false)],
        );
        let previous = Snapshot {
            group_catalogs: vec![previous_catalog.clone()],
            ..Snapshot::default()
        };
        let mut renamed_alias = previous_catalog.clone();
        renamed_alias.host = "laptop".into();
        renamed_alias.snapshot = None;
        renamed_alias.state = GroupCatalogState::Unavailable;
        renamed_alias.error = Some("offline".into());
        let mut same_connection = Snapshot {
            group_catalogs: vec![renamed_alias],
            ..Snapshot::default()
        };
        same_connection.retain_unavailable_group_catalogs_from(&previous);
        assert_eq!(
            same_connection.group_catalogs[0].state,
            GroupCatalogState::Stale
        );
        assert_eq!(
            same_connection.group_catalogs[0].snapshot,
            previous_catalog.snapshot
        );

        let mut repointed = same_connection.clone();
        repointed.group_catalogs[0].target = "machine-b".into();
        repointed.group_catalogs[0].snapshot = None;
        repointed.group_catalogs[0].state = GroupCatalogState::Unavailable;
        repointed.retain_unavailable_group_catalogs_from(&previous);
        assert!(repointed.group_catalogs[0].snapshot.is_none());
        assert_eq!(
            repointed.group_catalogs[0].state,
            GroupCatalogState::Unavailable
        );
    }

    #[test]
    fn authority_acceptance_ledger_syncs_contents_before_durable_replace() {
        #[derive(Default)]
        struct RecordingFile {
            operations: Vec<&'static str>,
        }

        impl AuthorityAcceptanceLedgerTempFile for RecordingFile {
            fn write_contents(&mut self, _contents: &[u8]) -> std::io::Result<()> {
                self.operations.push("write_all");
                Ok(())
            }

            fn sync_contents(&mut self) -> std::io::Result<()> {
                self.operations.push("sync_all");
                Ok(())
            }
        }

        let mut file = RecordingFile::default();
        write_authority_acceptance_ledger_contents(&mut file, b"ledger")
            .expect("write and sync ledger contents");

        assert_eq!(file.operations, ["write_all", "sync_all"]);
    }

    #[test]
    fn duplicate_authority_observations_are_identity_conflicts() {
        let mut snapshot = Snapshot {
            group_catalogs: vec![
                group_catalog("office", "machine-a", 1, 0, Vec::new()),
                group_catalog("home", "machine-b", 1, 0, Vec::new()),
            ],
            ..Snapshot::default()
        };

        snapshot.admit_group_catalogs(&AuthorityAcceptanceLedger::default());

        assert!(snapshot
            .group_catalogs
            .iter()
            .all(|catalog| catalog.state == GroupCatalogState::IdentityConflict));
        let authority = crate::groups::AuthorityId::from_random_bytes([1; 16]);
        assert!(snapshot
            .fresh_group_catalog(&authority)
            .expect_err("collision cannot be routed")
            .contains("identity conflict"));
    }

    #[test]
    fn reload_keeping_both_connection_aliases_preserves_identity_conflict() {
        let configured_host = |name: &str| FleetHostConfig {
            name: name.into(),
            target: "machine-a".into(),
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
            ..FleetHostConfig::default()
        };
        let fleet = FleetConfig {
            hosts: vec![configured_host("office"), configured_host("duplicate")],
            ..FleetConfig::default()
        };
        let authority = crate::groups::AuthorityId::from_random_bytes([1; 16]);
        let mut snapshot = Snapshot {
            group_catalogs: vec![
                group_catalog("office", "machine-a", 1, 1, Vec::new()),
                group_catalog("duplicate", "machine-a", 1, 1, Vec::new()),
            ],
            ..Snapshot::default()
        };
        snapshot.admit_group_catalogs(&AuthorityAcceptanceLedger::default());

        let reloaded = snapshot.reconcile_after_config_reload(&fleet, 2);

        assert_eq!(reloaded.group_catalogs.len(), 2);
        assert!(reloaded.authority_has_identity_conflict(&authority));
        assert!(reloaded
            .group_catalogs
            .iter()
            .all(|catalog| catalog.state == GroupCatalogState::IdentityConflict));
    }

    #[test]
    fn reload_renaming_both_connection_aliases_preserves_identity_conflict() {
        let configured_host = |name: &str| FleetHostConfig {
            name: name.into(),
            target: "machine-a".into(),
            session: Some("agents".into()),
            socket: Some("/tmp/herdr.sock".into()),
            ..FleetHostConfig::default()
        };
        let fleet = FleetConfig {
            hosts: vec![configured_host("work"), configured_host("backup")],
            ..FleetConfig::default()
        };
        let authority = crate::groups::AuthorityId::from_random_bytes([1; 16]);
        let mut snapshot = Snapshot {
            group_catalogs: vec![
                group_catalog("office", "machine-a", 1, 1, Vec::new()),
                group_catalog("duplicate", "machine-a", 1, 1, Vec::new()),
            ],
            ..Snapshot::default()
        };
        snapshot.admit_group_catalogs(&AuthorityAcceptanceLedger::default());

        let reloaded = snapshot.reconcile_after_config_reload(&fleet, 2);

        assert_eq!(
            reloaded
                .group_catalogs
                .iter()
                .map(|catalog| catalog.host.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["work", "backup"])
        );
        assert!(reloaded.authority_has_identity_conflict(&authority));
    }

    #[test]
    fn legacy_catalog_cache_migrates_history_without_connection_filtering() {
        let dir = run_fixture_dir("group-catalog-cache");
        let path = dir.join("remote-group-catalogs.json");
        let accepted = group_catalog(
            "office",
            "machine-a",
            1,
            2,
            vec![group_record(1, 1, 2, true)],
        );
        let accepted = accepted.snapshot.expect("accepted snapshot");
        let file = LegacyGroupCatalogCacheFile {
            version: 1,
            entries: vec![
                LegacyGroupCatalogCacheEntry {
                    target: "machine-a".into(),
                    local: false,
                    session: Some("agents".into()),
                    socket: Some("/tmp/herdr.sock".into()),
                    snapshot: accepted.clone(),
                },
                LegacyGroupCatalogCacheEntry {
                    target: "machine-b".into(),
                    local: false,
                    session: None,
                    socket: None,
                    snapshot: accepted,
                },
            ],
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&file).expect("serialize legacy cache"),
        )
        .expect("write legacy cache");

        let retained = load_authority_acceptance_ledger(&path).expect("migrate legacy cache");
        assert_rolled_back_group_is_not_fresh(&retained, "renamed", "machine-b");
        std::fs::remove_dir_all(dir).expect("remove catalog cache fixture");
    }

    #[test]
    fn unusable_poll_cannot_erase_retained_tombstone_history() {
        let dir = run_fixture_dir("unusable-poll-retains-tombstone");
        let path = dir.join("remote-group-catalogs.json");
        let fleet = FleetConfig {
            hosts: vec![FleetHostConfig {
                name: "office".into(),
                target: "machine-a".into(),
                local: false,
                session: Some("agents".into()),
                socket: Some("/tmp/herdr.sock".into()),
            }],
            ..FleetConfig::default()
        };
        let ledger = ledger_with_deleted_group();
        save_authority_acceptance_ledger(&path, &ledger)
            .expect("persist accepted tombstone history");

        let not_polled = Snapshot::unpolled(&fleet.hosts);
        assert!(not_polled.group_catalogs.is_empty());

        let mut unusable_config = fleet.clone();
        unusable_config.timeout_ms = 1;
        let unusable_poll = poll_without_generation(&unusable_config);
        assert!(unusable_poll.polled);
        assert!(unusable_poll.group_catalogs.is_empty());
        assert!(unusable_poll.hosts.iter().any(|host| {
            host.name == "office"
                && host.target == "machine-a"
                && host.session.as_deref() == Some("agents")
                && host.socket.as_deref() == Some("/tmp/herdr.sock")
        }));

        let retained = load_authority_acceptance_ledger(&path)
            .expect("load retained history after repairing config");
        assert_eq!(retained, ledger);
        assert_rolled_back_group_is_not_fresh(&retained, "office", "machine-a");

        std::fs::remove_dir_all(dir).expect("remove unusable poll fixture");
    }

    #[test]
    fn accepted_history_survives_a_connection_switching_authorities() {
        let mut ledger = ledger_with_deleted_group();
        let authority_b = group_catalog(
            "office",
            "machine-a",
            2,
            1,
            vec![group_record(2, 1, 1, false)],
        );
        ledger
            .advance(authority_b.snapshot.as_ref().expect("authority B snapshot"))
            .expect("accept authority B");

        let dir = run_fixture_dir("authority-switch-cache");
        let path = dir.join("remote-group-catalogs.json");
        save_authority_acceptance_ledger(&path, &ledger).expect("persist both authority histories");
        let retained_history = load_authority_acceptance_ledger(&path)
            .expect("load independent histories for the connection");
        assert!(retained_history
            .accepted(&crate::groups::AuthorityId::from_random_bytes([2; 16]))
            .is_some());
        assert_rolled_back_group_is_not_fresh(&retained_history, "office", "machine-a");
        std::fs::remove_dir_all(dir).expect("remove authority switch cache fixture");
    }

    #[test]
    fn retained_history_does_not_conflict_with_a_current_authority_report() {
        let authority_a = crate::groups::AuthorityId::from_random_bytes([1; 16]);
        let authority_b = crate::groups::AuthorityId::from_random_bytes([2; 16]);
        let mut x_reports_a = Snapshot {
            group_catalogs: vec![group_catalog(
                "x",
                "machine-x",
                1,
                1,
                vec![group_record(1, 1, 1, false)],
            )],
            ..Snapshot::default()
        };
        let ledger = x_reports_a.admit_group_catalogs(&AuthorityAcceptanceLedger::default());
        let mut x_reports_b = Snapshot {
            group_catalogs: vec![group_catalog(
                "x",
                "machine-x",
                2,
                1,
                vec![group_record(2, 1, 1, false)],
            )],
            ..Snapshot::default()
        };
        let ledger = x_reports_b.admit_group_catalogs(&ledger);
        assert_eq!(x_reports_b.group_catalogs.len(), 1);

        let mut y_reports_a = Snapshot {
            group_catalogs: vec![
                group_catalog("x", "machine-x", 2, 2, vec![group_record(2, 1, 2, false)]),
                group_catalog("y", "machine-y", 1, 2, vec![group_record(1, 1, 2, false)]),
            ],
            ..Snapshot::default()
        };
        let ledger = y_reports_a.admit_group_catalogs(&ledger);

        let current_a = y_reports_a
            .fresh_group_catalog(&authority_a)
            .expect("Y currently reports authority A");
        assert_eq!(current_a.host, "y");
        let current_b = y_reports_a
            .fresh_group_catalog(&authority_b)
            .expect("X currently reports authority B");
        assert_eq!(current_b.host, "x");
        assert!(!y_reports_a.authority_has_identity_conflict(&authority_a));
        assert_eq!(y_reports_a.group_catalogs.len(), 2);
        assert!(ledger.accepted(&authority_a).is_some());
        assert!(ledger.accepted(&authority_b).is_some());
    }

    #[test]
    fn authority_acceptance_ledger_rejects_unreadable_or_conflicting_history() {
        let dir = run_fixture_dir("invalid-group-catalog-cache");
        let path = dir.join("remote-group-catalogs.json");

        std::fs::write(&path, b"not json").expect("write corrupt cache");
        assert!(load_authority_acceptance_ledger(&path).is_err());

        let snapshot = group_catalog(
            "office",
            "machine-a",
            1,
            2,
            vec![group_record(1, 1, 2, false)],
        );
        let mut duplicate = snapshot.clone();
        duplicate.snapshot.as_mut().expect("snapshot").groups[0].state =
            crate::groups::GroupState::Active {
                name: "Focus".into(),
            };
        let file = AuthorityAcceptanceLedgerFile {
            version: AUTHORITY_ACCEPTANCE_LEDGER_VERSION,
            authorities: vec![
                snapshot.snapshot.expect("first snapshot"),
                duplicate.snapshot.expect("conflicting snapshot"),
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&file).expect("serialize cache"))
            .expect("write ambiguous cache");
        assert!(load_authority_acceptance_ledger(&path).is_err());

        std::fs::remove_dir_all(dir).expect("remove catalog cache fixture");
    }

    #[test]
    fn durable_history_quarantine_preserves_observed_identity_conflicts() {
        let mut snapshot = Snapshot {
            group_catalogs: vec![
                group_catalog("one", "machine-a", 1, 2, Vec::new()),
                group_catalog("two", "machine-b", 1, 2, Vec::new()),
            ],
            ..Snapshot::default()
        };
        snapshot.admit_group_catalogs(&AuthorityAcceptanceLedger::default());
        snapshot.reject_group_catalogs_without_durable_history("retained history is unreadable");

        let authority = crate::groups::AuthorityId::from_random_bytes([1; 16]);
        assert!(snapshot.authority_has_identity_conflict(&authority));
        assert!(snapshot.group_catalogs.iter().all(|catalog| {
            catalog.state == GroupCatalogState::IdentityConflict
                && catalog.authority_id() == Some(&authority)
                && catalog.snapshot.is_some()
        }));
    }

    #[test]
    fn retained_unreachable_inventory_advances_age_from_last_observation() {
        let mut row = FleetRow::test_agent_row("remote", "worker");
        row.reported_at = Some("1970-01-01T00:01:40Z".into());
        row.age_s = Some(1);
        let previous = Snapshot {
            hosts: vec![HostSnapshot {
                name: "remote".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                entries: vec![row],
            }],
            ..Snapshot::default()
        };
        let unreachable = |refreshed_at_unix_ms| Snapshot {
            refreshed_at_unix_ms: Some(refreshed_at_unix_ms),
            hosts: vec![HostSnapshot {
                name: "remote".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: HostState::Unreachable,
                version: None,
                protocol: None,
                error: Some("offline".into()),
                remote_identity: None,
                entries: Vec::new(),
            }],
            ..Snapshot::default()
        };

        let mut first = unreachable(130_000);
        first.retain_unreachable_inventory_from(&previous);
        assert_eq!(first.hosts[0].entries[0].age_s, Some(30));

        let mut second = unreachable(160_000);
        second.retain_unreachable_inventory_from(&first);
        assert_eq!(second.hosts[0].entries[0].age_s, Some(60));
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
    fn agent_attention_projection_blocks_action_points_but_not_settled_panes() {
        let mut answer = agent(AgentStatus::Blocked, serde_json::json!([]));
        answer.items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
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
        assert!(answer_row.blocked);
        assert_eq!(answer_row.state, "blocked");

        let mut verify = agent(AgentStatus::Idle, serde_json::json!([]));
        verify.items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "Verify".into(),
            text: "Optional check".into(),
            blocking: false,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let verify_row =
            FleetRow::from_agent("ub1", false, verify, 1_777_000_000).expect("valid verify row");
        assert!(verify_row.blocked);
        assert_eq!(verify_row.state, "blocked");

        let mut informational = agent(AgentStatus::Done, serde_json::json!([]));
        informational.items = vec![crate::api::schema::ClosingBlockItem {
            n: 1,
            label: "What to test".into(),
            text: "Run the smoke test".into(),
            blocking: true,
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let informational_row = FleetRow::from_agent("ub1", false, informational, 1_777_000_000)
            .expect("valid informational row");
        assert!(!informational_row.blocked);
        assert_eq!(informational_row.state, "done");

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
        assert_eq!(snapshot.hosts[0].remote_identity.as_deref(), Some("laptop"));
    }

    #[test]
    fn remote_identity_requires_a_complete_unambiguous_inventory() {
        let hosts = vec![host("office", false)];

        let legacy = collect_snapshot_with(
            &fake_reader(
                Ok(vec![agent(AgentStatus::Idle, serde_json::json!([]))]),
                HostRuntime::default(),
            ),
            &hosts,
            &FleetConfig::default(),
        );
        assert!(legacy.hosts[0].remote_identity.is_none());

        let mut first = agent(AgentStatus::Idle, serde_json::json!([]));
        first.agent_ref = Some(
            crate::api::schema::AgentRef::new("laptop", "p1").expect("valid first remote identity"),
        );
        let mut second = agent(AgentStatus::Working, serde_json::json!([]));
        second.agent_ref = Some(
            crate::api::schema::AgentRef::new("other-laptop", "p2")
                .expect("valid second remote identity"),
        );
        let ambiguous = collect_snapshot_with(
            &fake_reader(Ok(vec![first, second]), HostRuntime::default()),
            &hosts,
            &FleetConfig::default(),
        );
        assert!(ambiguous.hosts[0].remote_identity.is_none());
    }

    #[test]
    // A2: an empty reachable inventory records no remote identity.
    fn empty_remote_inventory_records_no_identity() {
        let hosts = vec![host("office", false)];
        let snapshot = collect_snapshot_with(
            &fake_reader(Ok(Vec::new()), HostRuntime::default()),
            &hosts,
            &FleetConfig::default(),
        );

        assert_eq!(snapshot.hosts[0].state, HostState::Reachable);
        assert!(snapshot.hosts[0].remote_identity.is_none());
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
        assert_eq!(polled.hosts.len(), 1);
        assert_eq!(polled.hosts[0].name, config.resolved_self_name());
    }

    #[test]
    fn configured_host_named_like_self_does_not_collide_with_implicit_local() {
        let config = FleetConfig {
            self_name: Some("laptop".to_string()),
            timeout_ms: MIN_TIMEOUT_MS,
            hosts: vec![host("laptop", false)],
            ..FleetConfig::default()
        };
        let polled = poll(&config);
        assert!(polled.polled);
        assert_eq!(polled.hosts.len(), 1);
        assert_eq!(polled.hosts[0].name, "laptop");
        assert_ne!(
            polled.hosts[0].error.as_deref(),
            Some("duplicate fleet host name: laptop")
        );
    }

    #[test]
    fn implicit_local_host_can_open_run_logs() {
        let config = FleetConfig {
            self_name: Some("laptop".to_string()),
            hosts: vec![host("ub2", false)],
            ..FleetConfig::default()
        };
        let poller = FleetPollerConfig::new(config);

        let local = poller.host("laptop").expect("implicit local host");
        assert!(local.local);
        assert_eq!(local.name, "laptop");
        assert_eq!(poller.host("ub2").expect("configured host").target, "ub2");
    }

    #[test]
    fn symphony_defaults_local_and_can_target_remote_temporal() {
        let local = FleetPollerConfig::new(FleetConfig::default());
        assert!(local.symphony_target().expect("local target").0.is_none());

        let remote = FleetPollerConfig::new(FleetConfig {
            symphony_host: Some("ub2".to_string()),
            hosts: vec![host("ub2", false)],
            ..FleetConfig::default()
        });
        let (target, _) = remote.symphony_target().expect("remote target");
        assert_eq!(target.expect("configured remote").target, "ub2");

        let invalid = FleetPollerConfig::new(FleetConfig {
            symphony_host: Some("unsafe".to_string()),
            hosts: vec![FleetHostConfig {
                name: "unsafe".to_string(),
                target: "-oProxyCommand=bad".to_string(),
                ..FleetHostConfig::default()
            }],
            ..FleetConfig::default()
        });
        assert!(invalid.symphony_target().is_err());
    }

    #[test]
    fn replacing_fleet_wakes_a_poller_wait_and_publishes_new_config() {
        let initial = FleetConfig {
            refresh_interval_ms: 60_000,
            ..FleetConfig::default()
        };
        let poller = Arc::new(FleetPollerConfig::new(initial));
        let initial_state = poller.snapshot();
        let waiter = Arc::clone(&poller);
        let (woke_tx, woke_rx) = std::sync::mpsc::channel();
        let wait_thread = std::thread::spawn(move || {
            waiter.wait_for_change(initial_state.generation, Duration::from_secs(60));
            woke_tx.send(()).expect("woken poller receiver");
        });

        let replacement = FleetConfig {
            refresh_interval_ms: 100,
            hosts: vec![FleetHostConfig {
                name: "new-host".into(),
                target: "new-host".into(),
                ..FleetHostConfig::default()
            }],
            ..FleetConfig::default()
        };
        poller.replace(replacement.clone());

        woke_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("fleet reload wakes the old interval wait");
        wait_thread.join().expect("poller wait thread");
        let state = poller.snapshot();
        assert_eq!(state.generation, 1);
        assert_eq!(
            state.fleet.refresh_interval_ms,
            replacement.refresh_interval_ms
        );
        assert_eq!(
            state.fleet.hosts.first().map(|host| host.name.as_str()),
            Some("new-host")
        );
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
    fn implicit_local_run_host_does_not_call_back_into_the_server() {
        let hosts = vec![host("laptop", true)];
        let reader = fake_reader(
            Ok(Vec::new()),
            HostRuntime {
                version: Some(crate::build_info::version().to_string()),
                protocol: Some(crate::protocol::PROTOCOL_VERSION),
            },
        );

        let snapshot = collect_snapshot_with_implicit_local(
            &reader,
            &hosts,
            &FleetConfig::default(),
            Some("laptop".to_string()),
        );

        assert_eq!(snapshot.hosts[0].state, HostState::Reachable);
        assert!(snapshot.group_catalogs.is_empty());
        assert_eq!(reader.local_calls.load(Ordering::Relaxed), 0);
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
            remote_identity: None,
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
    fn attached_host_name_reads_both_attach_shapes_back_to_the_host() {
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
            remote_identity: None,
            entries: Vec::new(),
        };
        let snapshot = Snapshot {
            hosts: vec![host.clone()],
            ..Snapshot::default()
        };
        let agent = agent_attach_argv(&host, "w1:p2").expect("attach argv");
        let whole = host_attach_argv(&host).expect("host argv");
        assert_eq!(attached_host_name(&snapshot, &agent), Some("workbox"));
        assert_eq!(attached_host_name(&snapshot, &whole), Some("workbox"));
        assert_eq!(
            attached_host_name(
                &snapshot,
                &["ssh".into(), "-t".into(), "elsewhere".into(), "x".into()]
            ),
            None
        );
        assert_eq!(attached_host_name(&snapshot, &["zsh".into()]), None);

        // A second alias on the same target under another session keeps its
        // own name instead of borrowing the first host's.
        let mut sibling = host.clone();
        sibling.name = "workbox-b".to_string();
        sibling.session = Some("batch".to_string());
        let both = Snapshot {
            hosts: vec![host.clone(), sibling.clone()],
            ..Snapshot::default()
        };
        let sibling_agent = agent_attach_argv(&sibling, "w1:p2").expect("attach argv");
        let sibling_whole = host_attach_argv(&sibling).expect("host argv");
        assert_eq!(attached_host_name(&both, &agent), Some("workbox"));
        assert_eq!(attached_host_name(&both, &sibling_agent), Some("workbox-b"));
        assert_eq!(attached_host_name(&both, &sibling_whole), Some("workbox-b"));
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
            remote_identity: None,
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
            remote_identity: None,
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
        let error = crate::agent_runs::parse_state(br#"{"schema":2}"#, "fixture").unwrap_err();
        assert!(error.contains("schema 2 is unsupported; expected 1"));
    }

    #[test]
    fn local_run_scan_caps_entries_before_metadata_and_keeps_newest_results() {
        let inspected = std::sync::Arc::new(AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&inspected);
        let entries = (0..MAX_RUN_DIRECTORY_ENTRIES + 20).map(move |index| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(PathBuf::from(format!("/missing/run-{index}")))
        });
        assert!(recent_run_state_paths(entries).is_empty());
        assert_eq!(inspected.load(Ordering::Relaxed), MAX_RUN_DIRECTORY_ENTRIES);

        let root = run_fixture_dir("newest-runs");
        let mut entries = Vec::new();
        for index in 0..crate::agent_runs::MAX_RUNS_PER_HOST + 5 {
            let run = root.join(format!("run-{index:02}"));
            std::fs::create_dir(&run).expect("create run directory");
            std::fs::write(run.join("state.json"), b"{}").expect("write run state");
            entries.push(Ok(run));
            std::thread::sleep(Duration::from_millis(2));
        }
        let newest = recent_run_state_paths(entries);
        assert_eq!(newest.len(), crate::agent_runs::MAX_RUNS_PER_HOST);
        assert!(newest.contains(&root.join("run-44").join("state.json")));
        assert!(!newest.contains(&root.join("run-00").join("state.json")));
        std::fs::remove_dir_all(root).expect("remove newest-runs fixture");
    }

    #[test]
    fn local_and_remote_run_states_reject_bytes_past_the_cap() {
        let root = run_fixture_dir("oversized-state");
        let run = root.join("run");
        std::fs::create_dir(&run).expect("create run fixture");
        std::fs::write(
            run.join("state.json"),
            vec![b'x'; crate::agent_runs::MAX_RUN_STATE_BYTES + 1],
        )
        .expect("write oversized state");
        let result = read_run_state_dir(&root);
        assert_eq!(result.len(), 1);
        assert!(result[0]
            .as_ref()
            .expect_err("oversized local state must fail")
            .contains("exceeds 65536 bytes"));

        let mut remote = REMOTE_RUN_RECORD_MARKER.to_vec();
        remote.extend_from_slice(b"0\x1e\n");
        remote.extend(std::iter::repeat_n(
            b'x',
            crate::agent_runs::MAX_RUN_STATE_BYTES + 1,
        ));
        let records = parse_remote_run_records(&remote);
        assert_eq!(records.len(), 1);
        assert!(records[0]
            .as_ref()
            .expect_err("oversized remote state must fail")
            .contains("exceeds 65536 bytes"));
        std::fs::remove_dir_all(root).expect("remove oversized-state fixture");
    }

    #[cfg(unix)]
    #[test]
    fn ssh_timeout_does_not_wait_for_descendant_pipe_holders() {
        let root = run_fixture_dir("ssh-descendant");
        let fake_ssh = root.join("ssh");
        write_executable(&fake_ssh, "#!/bin/sh\nsleep 30 &\nwait\n");

        let started = Instant::now();
        let error = run_ssh_program_with_timeout(
            &fake_ssh,
            "fixture",
            "exit 0",
            Duration::from_millis(500),
        )
        .expect_err("SSH wrapper with retained pipes must time out");

        assert!(error.contains("timed out after 500ms"));
        assert!(
            started.elapsed() < Duration::from_millis(1_250),
            "timeout must not block joining descendant-held pipes"
        );
        std::fs::remove_dir_all(root).expect("remove ssh-descendant fixture");
    }

    #[cfg(unix)]
    #[test]
    fn ssh_capture_caps_combined_stdout_and_stderr() {
        let root = run_fixture_dir("ssh-output-cap");
        let fake_ssh = root.join("ssh");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\nhead -c {} /dev/zero\nhead -c {} /dev/zero >&2\n",
                MAX_REMOTE_OUTPUT_BYTES / 2 + 1,
                MAX_REMOTE_OUTPUT_BYTES / 2 + 1,
            ),
        );

        let error =
            run_ssh_program_with_timeout(&fake_ssh, "fixture", "exit 0", Duration::from_secs(5))
                .expect_err("SSH output past the cap must fail");
        assert!(error.contains("exceeded 4194304 bytes"));
        std::fs::remove_dir_all(root).expect("remove ssh-output-cap fixture");
    }

    fn legacy_run(
        run_id: &str,
        state: &str,
        heartbeat: &str,
        blocked_reason: Option<&str>,
        parent_run_id: Option<&str>,
    ) -> crate::agent_runs::Observation {
        let value = serde_json::json!({
            "schema": 1,
            "run_id": run_id,
            "host": "probe",
            "agent": "codex",
            "model": "gpt-5.6-sol",
            "effort": "high",
            "label": "compatibility probe",
            "task": "preserve fleet status",
            "cwd": "/tmp/probe",
            "repo": "matthias-scale/herdr",
            "branch": "feat/probe",
            "pid": 4242,
            "started_at": "2026-09-17T08:00:00Z",
            "last_heartbeat": heartbeat,
            "phase": "verify",
            "state": state,
            "blocked_reason": blocked_reason,
            "blocked_since": blocked_reason.map(|_| "2026-09-17T08:01:00Z"),
            "exit_code": null,
            "exit_reason": null,
            "log_path": "/home/probe/.agents/runs/probe/out.log",
            "tokens_in": null,
            "tokens_out": null,
            "cost_usd": null,
            "tool_calls": null,
            "parent": {"host": "probe", "run_id": parent_run_id, "session": null}
        });
        crate::agent_runs::Observation {
            state: crate::agent_runs::parse_state(
                &serde_json::to_vec(&value).expect("legacy fixture JSON"),
                "legacy fixture",
            )
            .expect("legacy run state"),
            pid_alive: false,
        }
    }

    fn run_snapshot(runs: Vec<crate::agent_runs::Observation>) -> Snapshot {
        let configured_host = host("probe", false);
        let fleet = FleetConfig {
            heartbeat_stale_ms: 60_000,
            ..FleetConfig::default()
        };
        snapshot_from_evidence(
            std::slice::from_ref(&configured_host),
            &fleet,
            vec![HostEvidence {
                host: configured_host.clone(),
                agents: Ok(Vec::new()),
                groups: Some(Err("group catalog unavailable in run fixture".into())),
                runs: runs.into_iter().map(Ok).collect(),
                runtime: HostRuntime::default(),
            }],
            UNIX_EPOCH + Duration::from_secs(1_758_099_600), // 2025-09-17T09:00:00Z
        )
    }

    #[test]
    fn run_rows_preserve_stale_blocked_and_fresh_waiting_contract() {
        let snapshot = run_snapshot(vec![
            legacy_run(
                "ra-stale-blocked",
                "blocked",
                "2025-09-17T08:00:00Z",
                Some("approval"),
                Some("ra-parent"),
            ),
            legacy_run(
                "ra-fresh-waiting",
                "waiting",
                "2025-09-17T08:59:30Z",
                None,
                None,
            ),
        ]);
        let rows = serde_json::to_value(&snapshot.hosts[0].entries).expect("serialize rows");
        let rows = rows.as_array().expect("row array");
        let blocked = rows
            .iter()
            .find(|row| row["name"] == "ra-stale-blocked")
            .expect("blocked row");
        assert_eq!(blocked["state"], "blocked_liveness_unknown");
        assert_eq!(blocked["raw_state"], "blocked");
        assert_eq!(blocked["liveness"], "unknown");
        assert_eq!(blocked["blocked"], true);
        assert_eq!(blocked["blocked_reason"], "approval");
        assert_eq!(blocked["parent_handle"], "probe::ra-parent");

        let waiting = rows
            .iter()
            .find(|row| row["name"] == "ra-fresh-waiting")
            .expect("waiting row");
        assert_eq!(waiting["state"], "waiting");
        assert_eq!(waiting["raw_state"], "waiting");
        assert_eq!(waiting["liveness"], "live");
        assert_eq!(waiting["blocked"], false);
    }

    #[test]
    fn run_parent_closure_includes_live_child() {
        let snapshot = run_snapshot(vec![
            legacy_run("ra-parent", "done", "2025-09-17T08:00:00Z", None, None),
            legacy_run(
                "ra-child",
                "active",
                "2025-09-17T08:59:30Z",
                None,
                Some("ra-parent"),
            ),
        ]);
        let rows = serde_json::to_value(&snapshot.hosts[0].entries).expect("serialize rows");
        let parent = rows
            .as_array()
            .expect("row array")
            .iter()
            .find(|row| row["name"] == "ra-parent")
            .expect("parent row");
        assert_eq!(parent["closure_liveness"], "live");
        assert_eq!(parent["descendants"]["live"], 1);
    }

    #[test]
    fn rejected_run_state_is_listed_but_not_counted_as_live() {
        let configured_host = host("ub2", false);
        let rejected = crate::agent_runs::parse_state(br#"{"schema":2}"#, "fixture").map(|state| {
            crate::agent_runs::Observation {
                state,
                pid_alive: false,
            }
        });
        let snapshot = snapshot_from_evidence(
            std::slice::from_ref(&configured_host),
            &FleetConfig::default(),
            vec![HostEvidence {
                host: configured_host.clone(),
                agents: Ok(Vec::new()),
                groups: Some(Err("group catalog unavailable in run fixture".into())),
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
            b"\x1eHERDR_FLEET_RUN_V1:1\x1e\n".to_vec(),
            serde_json::to_vec(&run).unwrap(),
        ]
        .concat();
        let (agents, groups, runs, runtime) = parse_remote_output(&output);
        assert!(agents.unwrap().is_empty());
        assert!(groups.is_err());
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].as_ref().unwrap().state.run_id,
            "ra-260826-test-a1b2c3d"
        );
        assert!(runs[0].as_ref().unwrap().pid_alive);
        assert_eq!(runtime, HostRuntime::default());
    }

    #[test]
    fn remote_output_parses_a_complete_group_segment() {
        let agent_response = serde_json::json!({
            "id": "agents",
            "result": {"type": "agent_list", "agents": []}
        });
        let authority = crate::groups::AuthorityId::from_random_bytes([12; 16]);
        let expected = crate::groups::GroupAuthoritySnapshot {
            authority_id: authority.clone(),
            revision: 3,
            groups: vec![crate::groups::GroupRecord {
                id: crate::groups::GroupId {
                    owner: authority,
                    local: 7,
                },
                revision: 3,
                state: crate::groups::GroupState::Active {
                    name: "Shared".into(),
                },
            }],
            memberships: Vec::new(),
        };
        let group_response = crate::api::schema::SuccessResponse {
            id: "groups".into(),
            result: ResponseResult::GroupHostSnapshot {
                snapshot: expected.clone(),
            },
        };
        let output = [
            serde_json::to_vec(&agent_response).expect("agent response JSON"),
            REMOTE_GROUPS_MARKER.to_vec(),
            serde_json::to_vec(&group_response).expect("group response JSON"),
            REMOTE_RUNS_MARKER.to_vec(),
        ]
        .concat();

        let (agents, groups, runs, runtime) = parse_remote_output(&output);

        assert!(agents.expect("agent segment").is_empty());
        assert_eq!(groups.expect("group segment"), expected);
        assert!(runs.is_empty());
        assert_eq!(runtime, HostRuntime::default());
    }

    #[cfg(unix)]
    #[test]
    fn remote_authority_mutation_uses_api_relay_framing_over_ssh() {
        let root = run_fixture_dir("authority-mutation-relay");
        let fake_ssh = root.join("ssh");
        write_executable(
            &fake_ssh,
            r#"#!/bin/sh
input=$(cat)
case "$input" in *"herdr api relay"*) ;; *) exit 41 ;; esac
case "$input" in *"group.authority_mutate"*) ;; *) exit 42 ;; esac
case "$input" in *"HERDR_SOCKET_PATH"*) ;; *) exit 43 ;; esac
case "$input" in *"HERDR_SESSION"*) ;; *) exit 44 ;; esac
printf '%s\n' '{"id":"mutation","result":{"type":"ok"}}'
"#,
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([13; 16]);
        let catalog = GroupCatalog {
            host: "office".into(),
            target: "fixture".into(),
            local: false,
            session: Some("agents".into()),
            socket: Some("/tmp/herdr fixture.sock".into()),
            state: GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: None,
            error: None,
        };
        let request = Request {
            id: "mutation".into(),
            method: Method::GroupAuthorityMutate(crate::api::schema::AuthorityMutationParams {
                expected_authority: authority.clone(),
                forwarded: true,
                mutation: crate::api::schema::AuthorityMutation::Delete(
                    crate::api::schema::GroupDeleteParams {
                        group_id: crate::groups::GroupId {
                            owner: authority,
                            local: 7,
                        },
                        expected_revision: 3,
                    },
                ),
            }),
        };

        let response = route_api_request_with_ssh_program(
            &catalog,
            &request,
            Duration::from_secs(2),
            &fake_ssh,
        )
        .expect("fake SSH relay response");

        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).expect("response JSON"),
            serde_json::json!({"id": "mutation", "result": {"type": "ok"}})
        );
        std::fs::remove_dir_all(root).expect("remove fake SSH fixture");
    }

    #[cfg(unix)]
    #[test]
    fn authority_mutation_router_preserves_request_and_revision_order() {
        let root = run_fixture_dir("authority-mutation-order");
        let fake_ssh = root.join("ssh");
        let log = root.join("requests");
        write_executable(
            &fake_ssh,
            &format!(
                "#!/bin/sh\ncat >> '{}'\nprintf '%s\\n' '{{\"id\":\"ok\",\"result\":{{\"type\":\"ok\"}}}}'\n",
                log.display()
            ),
        );
        let authority = crate::groups::AuthorityId::from_random_bytes([15; 16]);
        let catalog = GroupCatalog {
            host: "office".into(),
            target: "fixture".into(),
            local: false,
            session: None,
            socket: None,
            state: GroupCatalogState::Fresh,
            observed_authority_id: Some(authority.clone()),
            snapshot: None,
            error: None,
        };
        let request = |id: &str, expected_revision| Request {
            id: id.into(),
            method: Method::GroupAuthorityMutate(crate::api::schema::AuthorityMutationParams {
                expected_authority: authority.clone(),
                forwarded: true,
                mutation: crate::api::schema::AuthorityMutation::Delete(
                    crate::api::schema::GroupDeleteParams {
                        group_id: crate::groups::GroupId {
                            owner: authority.clone(),
                            local: 7,
                        },
                        expected_revision,
                    },
                ),
            }),
        };
        let router = AuthorityMutationRouter::with_ssh_program(fake_ssh, Duration::from_secs(2));
        router.observe_snapshot(&Snapshot {
            group_catalogs: vec![catalog.clone()],
            ..Snapshot::default()
        });
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let (second_tx, second_rx) = std::sync::mpsc::channel();

        router
            .enqueue(catalog.clone(), 0, request("first", 3), first_tx)
            .expect("enqueue first mutation");
        router
            .enqueue(catalog, 0, request("second", 4), second_tx)
            .expect("enqueue second mutation");
        first_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first mutation response");
        second_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second mutation response");

        let requests = std::fs::read_to_string(&log).expect("captured mutation requests");
        let first = requests
            .find("\"expected_revision\":3")
            .expect("first expected revision");
        let second = requests
            .find("\"expected_revision\":4")
            .expect("second expected revision");
        assert!(first < second, "mutation queue reordered requests");
        std::fs::remove_dir_all(root).expect("remove mutation order fixture");
    }

    #[test]
    fn remote_run_read_is_bounded_and_survives_an_unavailable_herdr_socket() {
        let script = remote_read_script(None, None);

        assert!(script.contains("herdr agent list || true"));
        assert!(script.contains(&format!(
            "sed -n '1,{}p'",
            crate::agent_runs::MAX_RUNS_PER_HOST
        )));
        assert!(script.contains(&format!(
            "head -c {} \"$file\"",
            crate::agent_runs::MAX_RUN_STATE_BYTES + 1
        )));
        assert!(script.contains("kill -0 \"$pid\""));
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
            let (agents, groups, runs, _) = parse_remote_output(&output);
            assert!(runs.is_empty());
            assert!(groups.is_err());
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
