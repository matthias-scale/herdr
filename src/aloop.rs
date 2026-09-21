//! Aloops: scheduled condition loops running on a producer host (default ub2).
//!
//! A loop fires on a schedule, appends one JSON line per invocation to
//! `~/.agents/aloop/runs/<loop>.jsonl` (run record schema, MAT-159 SCH3) and
//! drops one JSON file per finding into
//! `~/.agents/aloop/findings/<loop>-<stable_id>.json` (finding schema, MAT-159
//! SCH1). The fleet collector carries bounded excerpts across SSH; this module
//! owns the schemas, the caps, the local readers, and the sidebar projection.
//! Everything here is pure data: reading or projecting never starts an agent.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Pending findings listed in the sidebar, newest first (AC2).
pub(crate) const MAX_PENDING_FINDINGS: usize = 20;
/// Newest finding files emitted by the remote wire reader. Local reads inspect
/// every matching file so malformed or launched files cannot hide a pending
/// finding before the display cap is applied.
pub(crate) const MAX_FINDING_FILES: usize = 40;
pub(crate) const MAX_FINDING_BYTES: usize = 64 * 1024;
/// Loop run-log files read per poll.
/// Thirty-two loop files fit the independent 4 MiB aloop read budget while
/// covering the producer's normal loop set.
pub(crate) const MAX_RUN_FILES: usize = 32;
/// Run records kept per loop file, counted from the end. Sixty-four records
/// show roughly two months of daily runs without exceeding that same budget.
pub(crate) const MAX_RUN_TAIL_LINES: usize = 64;
/// Bytes read from the end of one loop run file.
pub(crate) const MAX_RUN_FILE_BYTES: usize = 32 * 1024;
/// SCH3 bounds log_excerpt to 4 KiB; the parser enforces it defensively.
pub(crate) const MAX_LOG_EXCERPT_BYTES: usize = 4 * 1024;
/// loops.md and one finding are both capped at the same budget on the wire.
pub(crate) const MAX_REGISTRY_BYTES: usize = 64 * 1024;

pub(crate) const FINDINGS_RELATIVE_DIR: &str = ".agents/aloop/findings";
pub(crate) const RUNS_RELATIVE_DIR: &str = ".agents/aloop/runs";

// Remote read markers, emitted by the fleet read script only for the producer
// host and parsed back from its output. Same framing as the fleet run markers.
pub(crate) const REMOTE_ALOOP_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_V1\x1e\n";
pub(crate) const REMOTE_ALOOP_FINDING_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_FINDING_V1\x1e\n";
pub(crate) const REMOTE_ALOOP_RUNS_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_RUNS_V1\x1e\n";
pub(crate) const REMOTE_ALOOP_RUN_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_RUN_V1:";
pub(crate) const REMOTE_ALOOP_REGISTRY_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_REGISTRY_V1\x1e\n";
pub(crate) const REMOTE_ALOOP_END_MARKER: &[u8] = b"\x1eHERDR_FLEET_ALOOP_END_V1\x1e\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindingStatus {
    Pending,
    Launched,
}

/// One finding file (SCH1), validated. `created_at` is an RFC3339 timestamp;
/// the parser rejects anything else so the age column never guesses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub(crate) loop_name: String,
    pub(crate) source: String,
    pub(crate) stable_id: String,
    pub(crate) title: String,
    pub(crate) url: Option<String>,
    pub(crate) evidence: String,
    pub(crate) prompt: String,
    pub(crate) created_at: String,
    pub(crate) created_at_unix_s: u64,
    pub(crate) status: FindingStatus,
}

/// One line of `~/.agents/aloop/runs/<loop>.jsonl` (SCH3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRecord {
    pub(crate) at: String,
    pub(crate) at_unix_s: u64,
    pub(crate) duration_ms: u64,
    pub(crate) exit: i32,
    pub(crate) findings: u32,
    pub(crate) stable_ids: Vec<String>,
    pub(crate) log_excerpt: String,
}

/// The tail of one loop's run file, newest record first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopRuns {
    pub(crate) loop_name: String,
    pub(crate) runs: Vec<Arc<RunRecord>>,
    pub(crate) skipped_lines: u64,
}

/// Everything one poll read from the producer host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostData {
    pub(crate) findings: Vec<Arc<Finding>>,
    pub(crate) skipped_findings: u64,
    pub(crate) loops: Vec<LoopRuns>,
    pub(crate) registry: crate::loop_runs::LoopRegistry,
}

/// What the sidebar can say about the producer host (AC6). `Unconfigured`
/// means `remote.fleet.aloop_host` names no configured host and is not this
/// machine; `Unreachable` means the read itself failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProducerState {
    Unconfigured,
    Unreachable(String),
    Read,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProducerSnapshot {
    pub(crate) host: String,
    pub(crate) state: ProducerState,
    pub(crate) data: HostData,
}

impl ProducerSnapshot {
    pub(crate) fn read(host: String, data: HostData) -> Self {
        Self {
            host,
            state: ProducerState::Read,
            data,
        }
    }

    pub(crate) fn unreachable(host: String, error: String) -> Self {
        Self {
            host,
            state: ProducerState::Unreachable(error),
            data: HostData::default(),
        }
    }

    pub(crate) fn unconfigured(host: String) -> Self {
        Self {
            host,
            state: ProducerState::Unconfigured,
            data: HostData::default(),
        }
    }

    pub(crate) fn reachable(&self) -> bool {
        matches!(self.state, ProducerState::Read)
    }
}

/// Loop and stable_id become sidebar keys and remote file names, so they stay
/// inside one conservative alphabet; `:` is excluded because it separates the
/// key segments.
fn valid_name_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        && value != "."
        && value != ".."
}

fn required_string(value: &serde_json::Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing or empty `{field}`"))
}

/// Parse one finding file. Malformed files are skipped by the caller (AC2);
/// nothing here panics on unexpected input.
pub(crate) fn parse_finding(bytes: &[u8], source_label: &str) -> Result<Finding, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid finding {source_label}: {error}"))?;
    let invalid = |reason: String| format!("invalid finding {source_label}: {reason}");
    let loop_name = required_string(&value, "loop").map_err(invalid)?;
    let stable_id = required_string(&value, "stable_id").map_err(invalid)?;
    if !valid_name_part(&loop_name) || !valid_name_part(&stable_id) {
        return Err(invalid("loop or stable_id has unsafe characters".into()));
    }
    let source = required_string(&value, "source").map_err(invalid)?;
    let title = required_string(&value, "title").map_err(invalid)?;
    let evidence = required_string(&value, "evidence").map_err(invalid)?;
    let prompt = required_string(&value, "prompt").map_err(invalid)?;
    let url = match value.get("url") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(url)) => Some(url.clone()),
        Some(_) => return Err(invalid("`url` must be a string or null".into())),
    };
    let created_at = required_string(&value, "created_at").map_err(invalid)?;
    let Some(created_at_unix_s) = crate::fleet::parse_utc_timestamp(&created_at) else {
        return Err(invalid("created_at must be RFC3339".into()));
    };
    let status = match value.get("status").and_then(serde_json::Value::as_str) {
        Some("pending") => FindingStatus::Pending,
        Some("launched") => FindingStatus::Launched,
        _ => return Err(invalid("status must be `pending` or `launched`".into())),
    };
    Ok(Finding {
        loop_name,
        source,
        stable_id,
        title,
        url,
        evidence,
        prompt,
        created_at,
        created_at_unix_s,
        status,
    })
}

fn truncate_bytes_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Parse one run-record line. Lines wider than the field budget are truncated
/// by the reader, so this only sees complete JSON values.
pub(crate) fn parse_run_record(line: &[u8], source_label: &str) -> Result<RunRecord, String> {
    let value: serde_json::Value = serde_json::from_slice(line)
        .map_err(|error| format!("invalid aloop run record {source_label}: {error}"))?;
    let invalid = |reason: String| format!("invalid aloop run record {source_label}: {reason}");
    let at = required_string(&value, "at").map_err(invalid)?;
    let Some(at_unix_s) = crate::fleet::parse_utc_timestamp(&at) else {
        return Err(invalid("at must be RFC3339".into()));
    };
    let duration_ms = value
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid("missing `duration_ms`".into()))?;
    let exit = value
        .get("exit")
        .and_then(serde_json::Value::as_i64)
        .and_then(|exit| i32::try_from(exit).ok())
        .ok_or_else(|| invalid("missing or out-of-range `exit`".into()))?;
    let findings = value
        .get("findings")
        .and_then(serde_json::Value::as_u64)
        .and_then(|findings| u32::try_from(findings).ok())
        .ok_or_else(|| invalid("missing or out-of-range `findings`".into()))?;
    let Some(ids) = value
        .get("stable_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return Err(invalid("missing or mistyped `stable_ids`".into()));
    };
    let mut stable_ids = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(id) = id.as_str().filter(|id| valid_name_part(id)) else {
            return Err(invalid("`stable_ids` must contain valid names".into()));
        };
        stable_ids.push(id.to_string());
    }
    let Some(log_excerpt) = value.get("log_excerpt").and_then(serde_json::Value::as_str) else {
        return Err(invalid("missing or mistyped `log_excerpt`".into()));
    };
    let log_excerpt = truncate_bytes_utf8(log_excerpt, MAX_LOG_EXCERPT_BYTES);
    Ok(RunRecord {
        at,
        at_unix_s,
        duration_ms,
        exit,
        findings,
        stable_ids,
        log_excerpt,
    })
}

/// Parse the tail of one loop run file: keep the newest records first and
/// skip malformed lines instead of failing the file (AC2's rule, mirrored).
pub(crate) fn parse_run_tail(bytes: &[u8], loop_name: &str) -> LoopRuns {
    let mut runs = Vec::new();
    let mut skipped_lines = 0u64;
    let mut lines: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.trim_ascii().is_empty())
        .collect();
    if lines.len() > MAX_RUN_TAIL_LINES {
        lines = lines.split_off(lines.len() - MAX_RUN_TAIL_LINES);
    }
    for (index, line) in lines.iter().enumerate() {
        match parse_run_record(line, &format!("{loop_name}.jsonl:{}", index + 1)) {
            Ok(record) => runs.push(Arc::new(record)),
            Err(_) => skipped_lines += 1,
        }
    }
    runs.sort_by(|left, right| {
        right
            .at_unix_s
            .cmp(&left.at_unix_s)
            .then_with(|| right.at.cmp(&left.at))
    });
    LoopRuns {
        loop_name: loop_name.to_string(),
        runs,
        skipped_lines,
    }
}

fn read_capped_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    match crate::platform::read_limited_reader(file, max_bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
    {
        crate::platform::LimitedRead::Empty => Ok(Vec::new()),
        crate::platform::LimitedRead::Complete(bytes) => Ok(bytes),
        crate::platform::LimitedRead::Oversized => {
            Err(format!("{} exceeds {max_bytes} bytes", path.display()))
        }
    }
}

/// Read the tail of one run file: the last `max_bytes`, with the first
/// partial line dropped when the read started mid-line.
fn read_run_tail_file(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("cannot stat {}: {error}", path.display()))?
        .len();
    let mut bytes = Vec::new();
    if len > MAX_RUN_FILE_BYTES as u64 {
        file.seek(SeekFrom::Start(len - MAX_RUN_FILE_BYTES as u64))
            .map_err(|error| format!("cannot seek {}: {error}", path.display()))?;
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    } else {
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    }
    Ok(bytes)
}

fn newest_first_json_files(dir: &Path, extension: &str, cap: usize) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %dir.display(), error = %error, "cannot read aloop directory");
            return Vec::new();
        }
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .filter_map(|path| {
            let modified = path.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    paths.into_iter().take(cap).map(|(_, path)| path).collect()
}

/// Read the local findings directory: newest files first, malformed or
/// oversized files counted as skipped (AC2).
pub(crate) fn read_findings_dir(dir: &Path) -> (Vec<Arc<Finding>>, u64) {
    let mut findings = Vec::new();
    let mut skipped = 0u64;
    for path in newest_first_json_files(dir, "json", usize::MAX) {
        let label = path.display().to_string();
        match read_capped_file(&path, MAX_FINDING_BYTES)
            .and_then(|bytes| parse_finding(&bytes, &label))
        {
            Ok(finding) => findings.push(Arc::new(finding)),
            Err(error) => {
                tracing::debug!(error = %error, "skipping aloop finding");
                skipped += 1;
            }
        }
    }
    (findings, skipped)
}

/// Read each loop's run file tail from the local runs directory.
pub(crate) fn read_runs_dir(dir: &Path) -> Vec<LoopRuns> {
    let mut loops = Vec::new();
    for path in newest_first_json_files(dir, "jsonl", MAX_RUN_FILES) {
        let Some(loop_name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| valid_name_part(stem))
        else {
            continue;
        };
        match read_run_tail_file(&path) {
            Ok(bytes) => loops.push(parse_run_tail(&bytes, loop_name)),
            Err(error) => tracing::debug!(error = %error, "skipping aloop run file"),
        }
    }
    loops
}

/// Read the producer's finding, run, and loop-definition stores from this
/// machine. `home` is a parameter so tests never touch the real HOME.
pub(crate) fn read_host_data_from(home: &Path) -> HostData {
    let (findings, skipped_findings) = read_findings_dir(&home.join(FINDINGS_RELATIVE_DIR));
    let loops = read_runs_dir(&home.join(RUNS_RELATIVE_DIR));
    let registry = crate::loop_runs::read_loop_registry(
        &home.join(crate::loop_runs::LOOP_REGISTRY_RELATIVE_PATH),
    );
    HostData {
        findings,
        skipped_findings,
        loops,
        registry,
    }
}

pub(crate) fn read_local_host_data() -> Result<HostData, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is unavailable; cannot read ~/.agents/aloop".to_string())?;
    Ok(read_host_data_from(&home))
}

fn marker_positions(bytes: &[u8], marker: &[u8]) -> Vec<usize> {
    bytes
        .windows(marker.len())
        .enumerate()
        .filter_map(|(at, window)| (window == marker).then_some(at))
        .collect()
}

/// Parse the block the remote read script emits for the producer host. Every
/// section is optional so an older or partially failed remote still yields
/// whatever it did send; malformed entries are skipped, not fatal.
pub(crate) fn parse_remote_block(bytes: &[u8]) -> HostData {
    let runs_at = marker_positions(bytes, REMOTE_ALOOP_RUNS_MARKER)
        .first()
        .copied();
    let registry_at = marker_positions(bytes, REMOTE_ALOOP_REGISTRY_MARKER)
        .first()
        .copied();
    let end_at = marker_positions(bytes, REMOTE_ALOOP_END_MARKER)
        .first()
        .copied()
        .unwrap_or(bytes.len());

    let section_end = |start: usize, candidates: &[Option<usize>]| {
        candidates
            .iter()
            .flatten()
            .copied()
            .filter(|at| *at >= start)
            .min()
            .unwrap_or(bytes.len())
    };
    let findings_end = section_end(0, &[runs_at, registry_at, Some(end_at)]);
    let mut findings = Vec::new();
    let mut skipped_findings = 0u64;
    let finding_marks = marker_positions(&bytes[..findings_end], REMOTE_ALOOP_FINDING_MARKER);
    for (index, mark) in finding_marks.iter().enumerate() {
        let start = mark + REMOTE_ALOOP_FINDING_MARKER.len();
        let end = finding_marks
            .get(index + 1)
            .copied()
            .unwrap_or(findings_end);
        let content = bytes[start..end].trim_ascii();
        if content.is_empty() {
            continue;
        }
        let label = format!("remote finding #{}", index + 1);
        if content.len() > MAX_FINDING_BYTES {
            skipped_findings += 1;
            continue;
        }
        match parse_finding(content, &label) {
            Ok(finding) => findings.push(Arc::new(finding)),
            Err(_) => skipped_findings += 1,
        }
    }

    let mut loops = Vec::new();
    if let Some(runs_at) = runs_at {
        let runs_start = runs_at + REMOTE_ALOOP_RUNS_MARKER.len();
        let runs_end = section_end(runs_start, &[registry_at, Some(end_at)]);
        let section = bytes.get(runs_start..runs_end).unwrap_or_default();
        let run_marks = marker_positions(section, REMOTE_ALOOP_RUN_MARKER);
        for (index, mark) in run_marks.iter().enumerate() {
            let start = mark + REMOTE_ALOOP_RUN_MARKER.len();
            let end = run_marks.get(index + 1).copied().unwrap_or(section.len());
            let record = &section[start..end];
            let Some(header_end) = record.windows(2).position(|window| window == b"\x1e\n") else {
                continue;
            };
            let Ok(loop_name) = std::str::from_utf8(&record[..header_end]) else {
                continue;
            };
            if !valid_name_part(loop_name) {
                continue;
            }
            loops.push(parse_run_tail(&record[header_end + 2..], loop_name));
        }
    }

    let registry = registry_at
        .map(|at| {
            let start = at + REMOTE_ALOOP_REGISTRY_MARKER.len();
            let end = section_end(start, &[Some(end_at)]);
            let content = bytes.get(start..end).unwrap_or_default();
            crate::loop_runs::parse_loop_registry(&String::from_utf8_lossy(
                &content[..content.len().min(MAX_REGISTRY_BYTES)],
            ))
        })
        .unwrap_or_default();

    HostData {
        findings,
        skipped_findings,
        loops,
        registry,
    }
}

/// A run that produced findings (SCH3 `findings` > 0) plus the pending
/// findings its `stable_ids` claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunProjection {
    pub(crate) run: Arc<RunRecord>,
    pub(crate) pending: Vec<Arc<Finding>>,
}

/// One loop's section content. `runs` holds hit runs newest first (AC7);
/// `clean_runs` are the runs folded behind the summary line; findings no
/// fetched run claims stay listed so the pending cap (AC2) always shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoopProjection {
    pub(crate) name: String,
    pub(crate) state: Option<crate::loop_runs::LoopState>,
    pub(crate) interval: Option<String>,
    pub(crate) unlinked_findings: Vec<Arc<Finding>>,
    pub(crate) runs: Vec<RunProjection>,
    pub(crate) clean_runs: Vec<Arc<RunRecord>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionProjection {
    pub(crate) host: String,
    pub(crate) reachable: bool,
    /// Why the producer is not readable, for the section's unreachable line.
    pub(crate) error: Option<String>,
    pub(crate) pending_count: usize,
    /// Pending findings across loops, newest first, capped (AC2).
    pub(crate) findings: Vec<Arc<Finding>>,
    pub(crate) loops: Vec<LoopProjection>,
}

/// Project the Aloops section from the latest fleet poll. Pure: it allocates
/// display structure from the snapshot and starts nothing (AC5).
pub(crate) fn project(snapshot: &crate::fleet::Snapshot) -> Option<SectionProjection> {
    let aloop = snapshot.aloop.as_ref()?;
    if !aloop.reachable() {
        return Some(SectionProjection {
            host: aloop.host.clone(),
            reachable: false,
            error: match &aloop.state {
                ProducerState::Unreachable(error) => Some(error.clone()),
                ProducerState::Unconfigured | ProducerState::Read => None,
            },
            pending_count: 0,
            findings: Vec::new(),
            loops: Vec::new(),
        });
    }
    let data = &aloop.data;
    let mut pending: Vec<Arc<Finding>> = data
        .findings
        .iter()
        .filter(|finding| finding.status == FindingStatus::Pending)
        .cloned()
        .collect();
    pending.sort_by(|left, right| {
        right
            .created_at_unix_s
            .cmp(&left.created_at_unix_s)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.loop_name.cmp(&right.loop_name))
            .then_with(|| left.stable_id.cmp(&right.stable_id))
    });
    let pending_count = pending.len();
    pending.truncate(MAX_PENDING_FINDINGS);

    let mut names: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for name in pending
        .iter()
        .map(|finding| finding.loop_name.clone())
        .chain(
            data.loops
                .iter()
                .map(|loop_runs| loop_runs.loop_name.clone()),
        )
        .chain(
            data.registry
                .loops
                .iter()
                .map(|definition| definition.loop_id.clone()),
        )
    {
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }

    let registry: HashMap<&str, &crate::loop_runs::LoopDefinition> = data
        .registry
        .loops
        .iter()
        .map(|definition| (definition.loop_id.as_str(), definition))
        .collect();
    let run_files: HashMap<&str, &LoopRuns> = data
        .loops
        .iter()
        .map(|loop_runs| (loop_runs.loop_name.as_str(), loop_runs))
        .collect();
    let pending_by_loop: HashMap<&str, Vec<&Arc<Finding>>> =
        pending.iter().fold(HashMap::new(), |mut by_loop, finding| {
            by_loop
                .entry(finding.loop_name.as_str())
                .or_default()
                .push(finding);
            by_loop
        });

    let mut loops = Vec::new();
    for name in names {
        let loop_pending = pending_by_loop
            .get(name.as_str())
            .cloned()
            .unwrap_or_default();
        let mut claimed: HashSet<&str> = HashSet::new();
        let mut runs = Vec::new();
        let mut clean_runs = Vec::new();
        if let Some(loop_runs) = run_files.get(name.as_str()) {
            for run in &loop_runs.runs {
                if run.findings == 0 {
                    clean_runs.push(Arc::clone(run));
                    continue;
                }
                let mut linked: Vec<Arc<Finding>> = Vec::new();
                for finding in &loop_pending {
                    if !claimed.contains(finding.stable_id.as_str())
                        && run.stable_ids.iter().any(|id| id == &finding.stable_id)
                    {
                        claimed.insert(finding.stable_id.as_str());
                        linked.push(Arc::clone(*finding));
                    }
                }
                linked.sort_by(|left, right| {
                    right
                        .created_at_unix_s
                        .cmp(&left.created_at_unix_s)
                        .then_with(|| left.stable_id.cmp(&right.stable_id))
                });
                runs.push(RunProjection {
                    run: Arc::clone(run),
                    pending: linked,
                });
            }
        }
        let unlinked_findings: Vec<Arc<Finding>> = loop_pending
            .iter()
            .filter(|finding| !claimed.contains(finding.stable_id.as_str()))
            .map(|finding| Arc::clone(*finding))
            .collect();
        let definition = registry.get(name.as_str());
        let activity = run_files
            .get(name.as_str())
            .and_then(|loop_runs| loop_runs.runs.first())
            .map(|run| run.at_unix_s)
            .unwrap_or(0)
            .max(
                loop_pending
                    .first()
                    .map(|finding| finding.created_at_unix_s)
                    .unwrap_or(0),
            );
        loops.push((
            activity,
            LoopProjection {
                name: name.clone(),
                state: definition.map(|definition| definition.state),
                interval: definition.and_then(|definition| definition.fields.get("every").cloned()),
                unlinked_findings,
                runs,
                clean_runs,
            },
        ));
    }
    // Loops with pending findings or fresh runs sort first; the rest follow by
    // name so the section does not reshuffle between identical polls.
    loops.sort_by(|(left_activity, left), (right_activity, right)| {
        right_activity
            .cmp(left_activity)
            .then_with(|| left.name.cmp(&right.name))
    });

    Some(SectionProjection {
        host: aloop.host.clone(),
        reachable: true,
        error: None,
        pending_count,
        findings: pending,
        loops: loops
            .into_iter()
            .map(|(_, projection)| projection)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_json(loop_name: &str, stable_id: &str, status: &str, created_at: &str) -> Vec<u8> {
        format!(
            r#"{{"loop":"{loop_name}","source":"sentry","stable_id":"{stable_id}","title":"boom {stable_id}","url":null,"evidence":"stacktrace","prompt":"fix it","created_at":"{created_at}","status":"{status}"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn finding_fixture_parses_all_fields() {
        let finding = parse_finding(
            &finding_json(
                "sentry-error-loop",
                "abc-123",
                "pending",
                "2026-09-18T09:50:00Z",
            ),
            "fixture",
        )
        .expect("valid finding");
        assert_eq!(finding.loop_name, "sentry-error-loop");
        assert_eq!(finding.stable_id, "abc-123");
        assert_eq!(finding.source, "sentry");
        assert_eq!(finding.status, FindingStatus::Pending);
        assert_eq!(finding.url, None);
        assert_eq!(
            finding.created_at_unix_s,
            crate::fleet::parse_utc_timestamp("2026-09-18T09:50:00Z").expect("timestamp")
        );
    }

    #[test]
    fn malformed_findings_are_rejected_not_misparsed() {
        for (label, bytes) in [
            ("not json", b"nope".as_slice().to_vec()),
            (
                "bad status",
                finding_json("loop", "a1", "done", "2026-09-18T09:50:00Z"),
            ),
            (
                "missing status",
                br#"{"loop":"loop","source":"s","stable_id":"a1","title":"t","created_at":"2026-09-18T09:50:00Z"}"#.to_vec(),
            ),
            (
                "bad created_at",
                finding_json("loop", "a1", "pending", "yesterday"),
            ),
            (
                "unsafe stable_id",
                finding_json("loop", "../etc", "pending", "2026-09-18T09:50:00Z"),
            ),
            (
                "unsafe loop name",
                finding_json("a/b", "a1", "pending", "2026-09-18T09:50:00Z"),
            ),
        ] {
            assert!(parse_finding(&bytes, label).is_err(), "{label}");
        }

        let valid = serde_json::from_slice::<serde_json::Value>(&finding_json(
            "loop",
            "a1",
            "pending",
            "2026-09-18T09:50:00Z",
        ))
        .expect("finding JSON");
        for field in ["evidence", "prompt"] {
            let mut missing = valid.clone();
            missing.as_object_mut().expect("object").remove(field);
            assert!(parse_finding(&serde_json::to_vec(&missing).expect("JSON"), field).is_err());
        }
        let mut mistyped_url = valid;
        mistyped_url["url"] = serde_json::json!(42);
        assert!(parse_finding(&serde_json::to_vec(&mistyped_url).expect("JSON"), "url").is_err());
    }

    #[test]
    fn run_record_parses_and_truncates_log_excerpt() {
        let long_log = "x".repeat(MAX_LOG_EXCERPT_BYTES + 512);
        let line = format!(
            r#"{{"at":"2026-09-18T09:59:00Z","duration_ms":1200,"exit":0,"findings":0,"stable_ids":[],"log_excerpt":"{long_log}"}}"#
        );
        let record = parse_run_record(line.as_bytes(), "fixture").expect("valid run record");
        assert_eq!(record.exit, 0);
        assert_eq!(record.duration_ms, 1200);
        assert_eq!(record.log_excerpt.len(), MAX_LOG_EXCERPT_BYTES);

        assert!(parse_run_record(b"not json", "fixture").is_err());
        assert!(
            parse_run_record(
                br#"{"at":"2026-09-18T09:59:00Z","duration_ms":1,"exit":0}"#,
                "fixture"
            )
            .is_err(),
            "missing findings"
        );
        assert!(parse_run_record(
            br#"{"at":"2026-09-18T09:59:00Z","duration_ms":1,"exit":0,"findings":0,"log_excerpt":""}"#,
            "missing stable_ids"
        )
        .is_err());
        assert!(parse_run_record(
            br#"{"at":"2026-09-18T09:59:00Z","duration_ms":1,"exit":0,"findings":0,"stable_ids":[],"log_excerpt":3}"#,
            "mistyped log_excerpt"
        )
        .is_err());
        assert!(parse_run_record(
            br#"{"at":"2026-09-18T09:59:00Z","duration_ms":1,"exit":0,"findings":0,"stable_ids":[3],"log_excerpt":""}"#,
            "mistyped stable_ids"
        )
        .is_err());
    }

    #[test]
    fn run_tail_keeps_newest_lines_sorted_and_skips_malformed() {
        let mut content = String::new();
        for minute in 0..(MAX_RUN_TAIL_LINES as u32 + 8) {
            content.push_str(&format!(
                "{{\"at\":\"2026-09-{:02}T{:02}:00:00Z\",\"duration_ms\":1000,\"exit\":0,\"findings\":0,\"stable_ids\":[],\"log_excerpt\":\"\"}}\n",
                18 + minute / 24,
                minute % 24,
            ));
        }
        content.push_str("garbage line\n");
        let tail = parse_run_tail(content.as_bytes(), "ci-checks");
        // The cap bounds the input lines read (the shell side tails the same
        // count); the garbage line lands inside the tail and is skipped, so
        // one fewer record survives.
        assert_eq!(tail.runs.len(), MAX_RUN_TAIL_LINES - 1);
        assert_eq!(tail.runs[0].at, "2026-09-20T23:00:00Z");
        assert_eq!(tail.skipped_lines, 1);
    }

    fn finding(
        loop_name: &str,
        stable_id: &str,
        status: FindingStatus,
        created_at: &str,
    ) -> Arc<Finding> {
        Arc::new(
            parse_finding(
                &finding_json(
                    loop_name,
                    stable_id,
                    match status {
                        FindingStatus::Pending => "pending",
                        FindingStatus::Launched => "launched",
                    },
                    created_at,
                ),
                "fixture",
            )
            .expect("valid finding"),
        )
    }

    fn run(at: &str, findings: u32, stable_ids: &[&str]) -> Arc<RunRecord> {
        let ids = stable_ids
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(",");
        Arc::new(
            parse_run_record(
                format!(
                    "{{\"at\":\"{at}\",\"duration_ms\":1400,\"exit\":0,\"findings\":{findings},\"stable_ids\":[{ids}],\"log_excerpt\":\"ok\"}}"
                )
                .as_bytes(),
                "fixture",
            )
            .expect("valid run record"),
        )
    }

    fn snapshot_with(data: HostData) -> crate::fleet::Snapshot {
        crate::fleet::Snapshot {
            polled: true,
            aloop: Some(ProducerSnapshot::read("ub2".to_string(), data)),
            ..crate::fleet::Snapshot::default()
        }
    }

    #[test]
    fn projection_filters_pending_sorts_newest_first_and_caps_at_twenty() {
        let mut findings = Vec::new();
        for index in 0..25u32 {
            findings.push(finding(
                "loop-a",
                &format!("id-{index:02}"),
                FindingStatus::Pending,
                &format!("2026-09-18T09:{index:02}:00Z"),
            ));
        }
        findings.push(finding(
            "loop-a",
            "launched-1",
            FindingStatus::Launched,
            "2026-09-18T10:00:00Z",
        ));
        let projection = project(&snapshot_with(HostData {
            findings,
            ..HostData::default()
        }))
        .expect("projection");

        assert_eq!(projection.pending_count, 25);
        assert_eq!(projection.findings.len(), MAX_PENDING_FINDINGS);
        assert!(projection
            .findings
            .iter()
            .all(|finding| finding.status == FindingStatus::Pending));
        assert_eq!(projection.findings[0].stable_id, "id-24");
        assert_eq!(
            projection.findings[MAX_PENDING_FINDINGS - 1].stable_id,
            "id-05"
        );
    }

    #[test]
    fn projection_folds_clean_runs_and_links_hit_run_findings() {
        let data = HostData {
            findings: vec![
                finding(
                    "loop-a",
                    "hit-1",
                    FindingStatus::Pending,
                    "2026-09-18T09:50:00Z",
                ),
                finding(
                    "loop-a",
                    "orphan",
                    FindingStatus::Pending,
                    "2026-09-18T09:55:00Z",
                ),
            ],
            loops: vec![LoopRuns {
                loop_name: "loop-a".to_string(),
                runs: vec![
                    run("2026-09-18T09:59:00Z", 0, &[]),
                    run("2026-09-18T09:50:00Z", 1, &["hit-1"]),
                    run("2026-09-18T09:41:00Z", 0, &[]),
                ],
                skipped_lines: 0,
            }],
            ..HostData::default()
        };
        let projection = project(&snapshot_with(data)).expect("projection");
        let loop_projection = &projection.loops[0];
        assert_eq!(loop_projection.runs.len(), 1);
        assert_eq!(loop_projection.runs[0].run.at, "2026-09-18T09:50:00Z");
        assert_eq!(loop_projection.runs[0].pending[0].stable_id, "hit-1");
        assert_eq!(loop_projection.clean_runs.len(), 2);
        assert_eq!(loop_projection.clean_runs[0].at, "2026-09-18T09:59:00Z");
        assert_eq!(loop_projection.unlinked_findings.len(), 1);
        assert_eq!(loop_projection.unlinked_findings[0].stable_id, "orphan");
    }

    #[test]
    fn projection_reads_registry_state_and_interval() {
        let registry = crate::loop_runs::parse_loop_registry(
            "## Armed\n- \x60loop-a\x60 sentry errors · every:10m\n## Disarmed\n- \x60loop-b\x60 ci\n",
        );
        let projection = project(&snapshot_with(HostData {
            registry,
            ..HostData::default()
        }))
        .expect("projection");
        let loop_a = projection
            .loops
            .iter()
            .find(|loop_projection| loop_projection.name == "loop-a")
            .expect("loop-a");
        assert_eq!(loop_a.state, Some(crate::loop_runs::LoopState::Armed));
        assert_eq!(loop_a.interval.as_deref(), Some("10m"));
        let loop_b = projection
            .loops
            .iter()
            .find(|loop_projection| loop_projection.name == "loop-b")
            .expect("loop-b");
        assert_eq!(loop_b.state, Some(crate::loop_runs::LoopState::Disarmed));
    }

    #[test]
    fn unreachable_and_unconfigured_producers_project_empty_sections() {
        let mut snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(ProducerSnapshot::unreachable(
                "ub2".to_string(),
                "timeout".to_string(),
            )),
            ..crate::fleet::Snapshot::default()
        };
        let projection = project(&snapshot).expect("projection");
        assert!(!projection.reachable);
        assert_eq!(projection.pending_count, 0);
        assert!(projection.loops.is_empty());

        snapshot.aloop = Some(ProducerSnapshot::unconfigured("ghost".to_string()));
        assert!(!project(&snapshot).expect("projection").reachable);

        snapshot.aloop = None;
        assert!(project(&snapshot).is_none());
    }

    #[test]
    fn remote_block_parses_findings_runs_and_registry() {
        let finding = finding_json("loop-a", "f1", "pending", "2026-09-18T09:50:00Z");
        let mut block = Vec::new();
        block.extend_from_slice(REMOTE_ALOOP_FINDING_MARKER);
        block.extend_from_slice(&finding);
        block.push(b'\n');
        block.extend_from_slice(REMOTE_ALOOP_FINDING_MARKER);
        block.extend_from_slice(b"{broken\n");
        block.extend_from_slice(REMOTE_ALOOP_RUNS_MARKER);
        block.extend_from_slice(b"\x1eHERDR_FLEET_ALOOP_RUN_V1:loop-a\x1e\n");
        block.extend_from_slice(
            b"{\"at\":\"2026-09-18T09:59:00Z\",\"duration_ms\":800,\"exit\":0,\"findings\":1,\"stable_ids\":[\"f1\"],\"log_excerpt\":\"done\"}\n",
        );
        block.extend_from_slice(REMOTE_ALOOP_REGISTRY_MARKER);
        block.extend_from_slice("## Armed\n- \x60loop-a\x60 title · every:10m\n".as_bytes());
        block.extend_from_slice(REMOTE_ALOOP_END_MARKER);

        let data = parse_remote_block(&block);
        assert_eq!(data.findings.len(), 1);
        assert_eq!(data.skipped_findings, 1);
        assert_eq!(data.loops.len(), 1);
        assert_eq!(data.loops[0].loop_name, "loop-a");
        assert_eq!(data.loops[0].runs.len(), 1);
        assert_eq!(data.registry.loops.len(), 1);
    }

    #[test]
    fn remote_block_reordered_or_truncated_markers_do_not_panic() {
        let reordered = [
            REMOTE_ALOOP_END_MARKER,
            REMOTE_ALOOP_RUNS_MARKER,
            REMOTE_ALOOP_RUN_MARKER,
            REMOTE_ALOOP_REGISTRY_MARKER,
        ]
        .concat();
        let truncated = [REMOTE_ALOOP_RUNS_MARKER, b"\x1eHERDR_FLEET_ALOOP_RUN_V1:"].concat();
        for bytes in [reordered, truncated] {
            assert!(std::panic::catch_unwind(|| parse_remote_block(&bytes)).is_ok());
        }
    }

    #[test]
    fn local_readers_skip_malformed_and_oversized_files() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-aloop-test-{}",
            crate::config::test_unique_suffix()
        ));
        let findings_dir = dir.join(FINDINGS_RELATIVE_DIR);
        let runs_dir = dir.join(RUNS_RELATIVE_DIR);
        std::fs::create_dir_all(&findings_dir).expect("create findings dir");
        std::fs::create_dir_all(&runs_dir).expect("create runs dir");
        std::fs::write(
            findings_dir.join("loop-a-good.json"),
            finding_json("loop-a", "good", "pending", "2026-09-18T09:50:00Z"),
        )
        .expect("write finding");
        std::fs::write(findings_dir.join("loop-a-bad.json"), b"{broken").expect("write bad");
        std::fs::write(
            findings_dir.join("loop-a-huge.json"),
            vec![b'x'; MAX_FINDING_BYTES + 1],
        )
        .expect("write huge");
        std::fs::write(
            runs_dir.join("loop-a.jsonl"),
            b"{\"at\":\"2026-09-18T09:59:00Z\",\"duration_ms\":1,\"exit\":0,\"findings\":0,\"stable_ids\":[],\"log_excerpt\":\"\"}\nbroken\n",
        )
        .expect("write runs");

        let data = read_host_data_from(&dir);
        assert_eq!(data.findings.len(), 1);
        assert_eq!(data.skipped_findings, 2);
        assert_eq!(data.loops.len(), 1);
        assert_eq!(data.loops[0].runs.len(), 1);
        assert_eq!(data.loops[0].skipped_lines, 1);
        std::fs::remove_dir_all(&dir).expect("remove fixture dir");
    }

    #[test]
    fn local_findings_filter_before_the_display_cap() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-aloop-finding-order-{}",
            crate::config::test_unique_suffix()
        ));
        let findings_dir = dir.join(FINDINGS_RELATIVE_DIR);
        std::fs::create_dir_all(&findings_dir).expect("create findings dir");
        std::fs::write(
            findings_dir.join("old-pending.json"),
            finding_json("loop", "still-pending", "pending", "2026-09-18T09:00:00Z"),
        )
        .expect("write pending finding");
        for index in 0..=MAX_FINDING_FILES {
            std::fs::write(
                findings_dir.join(format!("new-launched-{index:02}.json")),
                finding_json(
                    "loop",
                    &format!("launched-{index:02}"),
                    "launched",
                    "2026-09-18T10:00:00Z",
                ),
            )
            .expect("write launched finding");
        }
        let (findings, skipped) = read_findings_dir(&findings_dir);
        assert_eq!(skipped, 0);
        assert_eq!(findings.len(), MAX_FINDING_FILES + 2);
        let projection = project(&snapshot_with(HostData {
            findings,
            ..HostData::default()
        }))
        .expect("projection");
        assert_eq!(projection.pending_count, 1);
        assert_eq!(projection.findings[0].stable_id, "still-pending");
        std::fs::remove_dir_all(dir).expect("remove fixture");
    }
}
