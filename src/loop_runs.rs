use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::events::AppEvent;
use notify::Watcher;

const RECEIPT_RELATIVE_PATH: &str = ".local/state/herdr/run-receipts.jsonl";
const LOOP_REGISTRY_RELATIVE_PATH: &str = "workspaces/scalable/loops.md";
pub(crate) const ALL_LOOPS_ID: &str = "__all__";

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct RunHistory {
    pub(crate) runs: Vec<RunRecord>,
    pub(crate) skipped_lines: u64,
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunRecord {
    pub(crate) run_id: String,
    pub(crate) skill: String,
    pub(crate) session: Option<String>,
    pub(crate) pr: Option<u64>,
    pub(crate) ticket: Option<String>,
    pub(crate) loop_id: Option<String>,
    pub(crate) start: String,
    pub(crate) end: Option<String>,
    pub(crate) wall_min: Option<f64>,
    pub(crate) blocked_min: Option<f64>,
    pub(crate) gates: Vec<GateRecord>,
    pub(crate) human_touches: Option<u64>,
    pub(crate) touches_by_type: BTreeMap<String, u64>,
    pub(crate) interrupted_focus: Option<bool>,
    pub(crate) review_rounds: Option<u64>,
    pub(crate) out_tokens: Option<u64>,
    pub(crate) outcome: RunOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateRecord {
    pub(crate) kind: String,
    pub(crate) defaulted: bool,
    pub(crate) recommendation_matched: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    InFlight,
    Terminal(String),
}
impl RunOutcome {
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::InFlight => "in_flight",
            Self::Terminal(value) => value.as_str(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LoopRegistry {
    pub(crate) loops: Vec<LoopDefinition>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopDefinition {
    pub(crate) loop_id: String,
    pub(crate) title: String,
    pub(crate) state: LoopState,
    pub(crate) fields: BTreeMap<String, String>,
    pub(crate) recent_runs: Vec<RecentRun>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopState {
    Armed,
    Disarmed,
}
impl LoopState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Disarmed => "disarmed",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecentRun {
    pub(crate) run_id: String,
    pub(crate) stable_id: Option<String>,
    pub(crate) outcome: String,
    pub(crate) epoch: Option<u64>,
    pub(crate) at: Option<String>,
}

pub(crate) fn default_receipt_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(RECEIPT_RELATIVE_PATH))
}
pub(crate) fn default_loop_registry_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LOOP_REGISTRY_RELATIVE_PATH))
}

pub(crate) fn watch_receipts(
    path: Option<PathBuf>,
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> Option<notify::RecommendedWatcher> {
    let path = path?;
    let parent = path.parent()?.to_path_buf();
    let target = path.clone();
    let watched_parent = parent.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        let relevant = matches!(
            event.kind,
            notify::EventKind::Any
                | notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_)
        );
        if relevant
            && event.paths.iter().any(|candidate| {
                candidate == &target
                    || candidate == &watched_parent
                    || candidate.file_name() == target.file_name()
            })
        {
            let _ = event_tx.try_send(AppEvent::LoopRunHistoryChanged);
        }
    })
    .ok()?;
    watcher
        .watch(&parent, notify::RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

#[cfg(test)]
pub(crate) fn parse_receipts(contents: &str) -> RunHistory {
    parse_receipt_bytes(contents.as_bytes())
}

#[derive(Debug)]
pub(crate) struct ReceiptReader {
    path: PathBuf,
    offset: u64,
    pending: Vec<u8>,
    history: RunHistory,
    indexes: HashMap<String, usize>,
}

impl ReceiptReader {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            pending: Vec::new(),
            history: RunHistory::default(),
            indexes: HashMap::new(),
        }
    }

    pub(crate) fn history(&self) -> &RunHistory {
        &self.history
    }

    pub(crate) fn refresh(&mut self) -> bool {
        let length = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let changed = self.offset != 0 || !self.history.runs.is_empty();
                self.reset();
                return changed;
            }
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %error,
                    "failed to stat loop run receipts"
                );
                return false;
            }
        };

        if length < self.offset {
            self.reset();
        }
        if length == self.offset {
            return false;
        }

        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %error,
                    "failed to open loop run receipts"
                );
                return false;
            }
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut bytes = Vec::new();
        if let Err(error) = file.read_to_end(&mut bytes) {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "failed to read appended loop run receipts"
            );
            return false;
        }
        self.offset = length;
        self.pending.extend(bytes);

        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline).collect::<Vec<_>>();
            process_receipt_line(&mut self.history, &mut self.indexes, &line[..newline]);
        }
        self.history
            .runs
            .sort_by(|left, right| right.start.cmp(&left.start));
        self.rebuild_indexes();
        true
    }

    fn rebuild_indexes(&mut self) {
        self.indexes = self
            .history
            .runs
            .iter()
            .enumerate()
            .map(|(index, run)| (run.run_id.clone(), index))
            .collect();
    }

    fn reset(&mut self) {
        self.offset = 0;
        self.pending.clear();
        self.history = RunHistory::default();
        self.indexes.clear();
    }
}

#[cfg(test)]
fn parse_receipt_bytes(bytes: &[u8]) -> RunHistory {
    let mut history = RunHistory::default();
    let mut indexes = HashMap::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        process_receipt_line(&mut history, &mut indexes, line);
    }
    history
        .runs
        .sort_by(|left, right| right.start.cmp(&left.start));
    history
}

fn process_receipt_line(
    history: &mut RunHistory,
    indexes: &mut HashMap<String, usize>,
    line: &[u8],
) {
    let Ok(line) = std::str::from_utf8(line) else {
        history.skipped_lines += 1;
        return;
    };
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Some(value) = serde_json::from_str::<serde_json::Value>(line).ok() else {
        history.skipped_lines += 1;
        return;
    };
    let Some(run_id) = string_field(&value, "run_id") else {
        history.skipped_lines += 1;
        return;
    };
    let is_end = match string_field(&value, "event") {
        Some(event) if event == "end" => true,
        Some(event) if event == "start" => false,
        Some(_) => {
            history.skipped_lines += 1;
            return;
        }
        None => value.get("end").is_some() || value.get("outcome").is_some(),
    };
    if is_end {
        let Some(record) = parse_terminal_record(&value, &run_id) else {
            history.skipped_lines += 1;
            return;
        };
        if let Some(index) = indexes.get(&run_id).copied() {
            if same_identity(&history.runs[index], &record) {
                apply_terminal(&mut history.runs[index], record);
            } else {
                history.skipped_lines += 1;
            }
        } else {
            indexes.insert(run_id, history.runs.len());
            history.runs.push(record);
        }
    } else {
        let Some(record) = parse_start_record(&value, &run_id) else {
            history.skipped_lines += 1;
            return;
        };
        if let Some(index) = indexes.get(&run_id).copied() {
            if same_identity(&history.runs[index], &record) {
                apply_start(&mut history.runs[index], record);
            } else {
                history.skipped_lines += 1;
            }
        } else {
            indexes.insert(run_id, history.runs.len());
            history.runs.push(record);
        }
    }
}

pub(crate) fn runs_for_loop(history: &RunHistory, loop_id: Option<&str>) -> Vec<RunRecord> {
    let Some(loop_id) = loop_id else {
        return history.runs.clone();
    };
    let has_loop_identity = history.runs.iter().any(|run| run.loop_id.is_some());
    history
        .runs
        .iter()
        .filter(|run| !has_loop_identity || run.loop_id.as_deref() == Some(loop_id))
        .cloned()
        .collect()
}

pub(crate) fn read_default_registry() -> LoopRegistry {
    default_loop_registry_path()
        .map(|path| read_loop_registry(&path))
        .unwrap_or_default()
}
pub(crate) fn read_loop_registry(path: &Path) -> LoopRegistry {
    let Ok(contents) = fs::read_to_string(path) else {
        return LoopRegistry::default();
    };
    parse_loop_registry(&contents)
}
pub(crate) fn parse_loop_registry(contents: &str) -> LoopRegistry {
    let mut registry = LoopRegistry::default();
    let mut state = None;
    let mut recent_runs = Vec::new();
    for line in contents.lines() {
        match line.trim() {
            "## Armed" => state = Some(LoopState::Armed),
            "## Disarmed" => state = Some(LoopState::Disarmed),
            "## Recent runs" => state = None,
            _ => {}
        }
        let trimmed = line.trim_start();
        if let Some(loop_state) = state {
            if let Some(row) = trimmed.strip_prefix("- \x60") {
                let Some((loop_id, remainder)) = row.split_once('\x60') else {
                    continue;
                };
                if loop_id.is_empty() {
                    continue;
                }
                let segments = remainder
                    .trim()
                    .trim_start_matches('·')
                    .split('·')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>();
                let title = segments.first().copied().unwrap_or(loop_id).to_string();
                let fields = segments
                    .iter()
                    .filter_map(|item| item.split_once(':'))
                    .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
                    .collect();
                registry.loops.push(LoopDefinition {
                    loop_id: loop_id.to_string(),
                    title,
                    state: loop_state,
                    fields,
                    recent_runs: Vec::new(),
                });
            }
        } else if let Some(row) = trimmed.strip_prefix("- run:") {
            let mut fields = parse_key_value_segments(row);
            let run_id = row.split('·').next().unwrap_or("").trim().to_string();
            if run_id.is_empty() {
                continue;
            }
            fields.insert("run".to_string(), run_id);
            recent_runs.push(RecentRun {
                run_id: fields.get("run").cloned().unwrap_or_default(),
                stable_id: fields.get("stable_id").cloned(),
                outcome: fields
                    .get("outcome")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                epoch: fields.get("epoch").and_then(|value| value.parse().ok()),
                at: fields.get("at").cloned(),
            });
        }
    }
    for recent in recent_runs {
        let loop_id = recent
            .run_id
            .strip_prefix("loop:")
            .unwrap_or(&recent.run_id);
        if let Some(definition) = registry
            .loops
            .iter_mut()
            .find(|item| item.loop_id == loop_id)
        {
            definition.recent_runs.push(recent);
        }
    }
    registry
}

fn parse_start_record(value: &serde_json::Value, run_id: &str) -> Option<RunRecord> {
    let start = string_field(value, "start")?;
    parse_timestamp_seconds(&start)?;
    Some(RunRecord {
        run_id: run_id.to_string(),
        skill: string_field(value, "skill")?,
        session: optional_string_field(value, "session"),
        pr: optional_u64_field(value, "pr"),
        ticket: optional_string_field(value, "ticket"),
        loop_id: optional_loop_id(value),
        start,
        end: None,
        wall_min: None,
        blocked_min: None,
        gates: Vec::new(),
        human_touches: None,
        touches_by_type: BTreeMap::new(),
        interrupted_focus: None,
        review_rounds: None,
        out_tokens: None,
        outcome: RunOutcome::InFlight,
    })
}
fn parse_terminal_record(value: &serde_json::Value, run_id: &str) -> Option<RunRecord> {
    let mut record = parse_start_record(value, run_id)?;
    let end = string_field(value, "end")?;
    parse_timestamp_seconds(&end)?;
    record.end = Some(end);
    record.wall_min = optional_f64_field(value, "wall_min");
    record.blocked_min = optional_f64_field(value, "blocked_min");
    record.gates = parse_gates(value);
    record.human_touches = optional_u64_field(value, "human_touches");
    record.touches_by_type = parse_touches_by_type(value);
    record.interrupted_focus = optional_bool_field(value, "interrupted_focus");
    record.review_rounds = optional_u64_field(value, "review_rounds");
    record.out_tokens = optional_u64_field(value, "out_tokens");
    record.outcome = RunOutcome::Terminal(string_field(value, "outcome")?);
    Some(record)
}
fn apply_start(existing: &mut RunRecord, start: RunRecord) {
    existing.session = existing.session.clone().or(start.session);
    existing.pr = existing.pr.or(start.pr);
    existing.ticket = existing.ticket.clone().or(start.ticket);
    existing.loop_id = existing.loop_id.clone().or(start.loop_id);
    if existing.skill.is_empty() {
        existing.skill = start.skill;
    }
    if existing.start.is_empty() {
        existing.start = start.start;
    }
}

fn same_identity(left: &RunRecord, right: &RunRecord) -> bool {
    left.run_id == right.run_id
        && left.skill == right.skill
        && left.session == right.session
        && left.pr == right.pr
        && left.ticket == right.ticket
        && left.loop_id == right.loop_id
        && left.start == right.start
}

fn apply_terminal(existing: &mut RunRecord, terminal: RunRecord) {
    apply_start(existing, terminal.clone());
    existing.end = terminal.end;
    existing.wall_min = terminal.wall_min;
    existing.blocked_min = terminal.blocked_min;
    existing.gates = terminal.gates;
    existing.human_touches = terminal.human_touches;
    existing.touches_by_type = terminal.touches_by_type;
    existing.interrupted_focus = terminal.interrupted_focus;
    existing.review_rounds = terminal.review_rounds;
    existing.out_tokens = terminal.out_tokens;
    existing.outcome = terminal.outcome;
}
fn parse_gates(value: &serde_json::Value) -> Vec<GateRecord> {
    value
        .get("gates")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|gate| {
            Some(GateRecord {
                kind: string_field(gate, "kind")?,
                defaulted: optional_bool_field(gate, "defaulted").unwrap_or(false),
                recommendation_matched: optional_bool_field(gate, "recommendation_matched"),
            })
        })
        .collect()
}
fn parse_touches_by_type(value: &serde_json::Value) -> BTreeMap<String, u64> {
    value
        .get("touches_by_type")
        .and_then(serde_json::Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}
fn parse_key_value_segments(value: &str) -> BTreeMap<String, String> {
    value
        .split('·')
        .filter_map(|segment| segment.trim().split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}
fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
fn optional_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}
fn optional_loop_id(value: &serde_json::Value) -> Option<String> {
    ["loop_id", "loop", "loop_name"]
        .iter()
        .find_map(|field| string_field(value, field))
}
fn optional_u64_field(value: &serde_json::Value, field: &str) -> Option<u64> {
    value.get(field).and_then(serde_json::Value::as_u64)
}
fn optional_f64_field(value: &serde_json::Value, field: &str) -> Option<f64> {
    value
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
}
fn optional_bool_field(value: &serde_json::Value, field: &str) -> Option<bool> {
    value.get(field).and_then(serde_json::Value::as_bool)
}
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub(crate) fn duration_label(record: &RunRecord, now_unix_seconds: Option<i64>) -> String {
    if let Some(minutes) = record.wall_min {
        return format_minutes(minutes);
    }
    if matches!(record.outcome, RunOutcome::InFlight) {
        if let (Some(now), Some(start)) = (now_unix_seconds, parse_timestamp_seconds(&record.start))
        {
            return format!(
                "in flight ({})",
                format_minutes(now.saturating_sub(start).max(0) as f64 / 60.0)
            );
        }
        return "in flight".to_string();
    }
    "—".to_string()
}
fn format_minutes(minutes: f64) -> String {
    if minutes < 1.0 {
        format!("{:.0}s", minutes * 60.0)
    } else {
        format!("{:.0}m", minutes)
    }
}
fn parse_timestamp_seconds(value: &str) -> Option<i64> {
    let (date, time_and_zone) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let days_in_month = match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    let zone_start = time_and_zone
        .find(['Z', '+', '-'])
        .unwrap_or(time_and_zone.len());
    let time = &time_and_zone[..zone_start];
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.split('.').next()?.parse().ok()?;
    if time_parts.next().is_some()
        || hour >= 24
        || minute >= 60
        || second >= 60
        || hour < 0
        || minute < 0
        || second < 0
    {
        return None;
    }
    let zone = &time_and_zone[zone_start..];
    let zone_offset = if zone.is_empty() || zone == "Z" {
        0
    } else if zone.starts_with(['+', '-']) {
        let sign: i64 = if zone.starts_with('-') { -1 } else { 1 };
        let mut parts = zone[1..].split(':');
        let hours = parts.next()?.parse::<i64>().ok()?;
        let minutes = parts.next()?.parse::<i64>().ok()?;
        if parts.next().is_some() || hours >= 24 || minutes >= 60 || hours < 0 || minutes < 0 {
            return None;
        }
        sign.checked_mul(
            hours
                .checked_mul(3600)?
                .checked_add(minutes.checked_mul(60)?)?,
        )?
    } else {
        return None;
    };
    days_from_civil(year, month, day)?
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?
        .checked_sub(zone_offset)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let year = i128::from(year) - i128::from(month <= 2);
    let era = if year >= 0 {
        year / 400
    } else {
        (year - 399) / 400
    };
    let year_of_era = year - era * 400;
    let month_index = i128::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_index + 2) / 5 + i128::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    i64::try_from(days).ok()
}

pub(crate) fn run_info(record: &RunRecord) -> crate::api::schema::LoopRunInfo {
    crate::api::schema::LoopRunInfo {
        run_id: record.run_id.clone(),
        skill: record.skill.clone(),
        session: record.session.clone(),
        pr: record.pr,
        ticket: record.ticket.clone(),
        loop_id: record.loop_id.clone(),
        start: record.start.clone(),
        end: record.end.clone(),
        wall_min: record.wall_min,
        blocked_min: record.blocked_min,
        gates: record
            .gates
            .iter()
            .map(|gate| crate::api::schema::LoopGateInfo {
                kind: gate.kind.clone(),
                defaulted: gate.defaulted,
                recommendation_matched: gate.recommendation_matched,
            })
            .collect(),
        human_touches: record.human_touches,
        touches_by_type: record.touches_by_type.clone(),
        interrupted_focus: record.interrupted_focus,
        review_rounds: record.review_rounds,
        out_tokens: record.out_tokens,
        outcome: record.outcome.label().to_string(),
    }
}
pub(crate) fn loop_info(definition: &LoopDefinition) -> crate::api::schema::LoopInfo {
    crate::api::schema::LoopInfo {
        loop_id: definition.loop_id.clone(),
        title: definition.title.clone(),
        state: definition.state.label().to_string(),
        fields: definition.fields.clone(),
        recent_runs: definition
            .recent_runs
            .iter()
            .map(|run| crate::api::schema::LoopRecentRun {
                run_id: run.run_id.clone(),
                stable_id: run.stable_id.clone(),
                outcome: run.outcome.clone(),
                epoch: run.epoch,
                at: run.at.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    const RECEIPTS: &str = r#"
{"v":1,"event":"start","run_id":"run-1","skill":"aship","session":"s","pr":800,"start":"2026-08-10T10:00:00Z"}
not-json
{"v":1,"event":"end","run_id":"run-1","skill":"aship","session":"s","pr":800,"start":"2026-08-10T10:00:00Z","end":"2026-08-10T13:34:00Z","wall_min":214,"blocked_min":63,"gates":[{"kind":"preference","defaulted":true,"recommendation_matched":null}],"human_touches":9,"touches_by_type":{"gate":6,"correction":2,"nudge":1},"interrupted_focus":false,"review_rounds":3,"out_tokens":412000,"outcome":"merged"}
{"v":1,"event":"start","run_id":"run-2","skill":"amerge","start":"2026-08-10T11:00:00Z"}
{"v":1,"event":"end","run_id":"run-3","skill":"aship","start":"2026-08-10T09:00:00Z","end":"2026-08-10T09:01:00Z","outcome":"vanished"}
{"v":1,"event":"end","run_id":"run-bad","skill":"aship","outcome":"failed"}
{"v":1,"event":"start","run_id":"run-4","skill":"aship","start":"2026-08-10T08:00:00Z"
{"v":1,"event":"start","run_id":"run-5","skill":"aship","start":"2026-08-10T07:00:00Z"}
"#;
    #[test]
    fn ac1_receipt_lines_parse_into_typed_run_records() {
        let history = parse_receipts(RECEIPTS);
        assert_eq!(history.runs.len(), 4);
        let merged = history
            .runs
            .iter()
            .find(|run| run.run_id == "run-1")
            .expect("merged run");
        assert_eq!(merged.outcome, RunOutcome::Terminal("merged".to_string()));
        assert_eq!(merged.gates.len(), 1);
        assert_eq!(merged.human_touches, Some(9));
        assert_eq!(merged.touches_by_type.get("gate"), Some(&6));
        assert_eq!(
            history
                .runs
                .iter()
                .find(|run| run.run_id == "run-2")
                .map(|run| &run.outcome),
            Some(&RunOutcome::InFlight)
        );
        assert_eq!(
            history
                .runs
                .iter()
                .find(|run| run.run_id == "run-3")
                .map(|run| run.outcome.label()),
            Some("vanished")
        );
        assert_eq!(history.skipped_lines, 3);
    }

    #[test]
    fn ac2_unmatched_start_stub_is_in_flight() {
        let history = parse_receipts(
            "{\"run_id\":\"run\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}",
        );
        assert_eq!(history.runs.len(), 1);
        assert_eq!(history.runs[0].outcome, RunOutcome::InFlight);
        assert_eq!(history.runs[0].end, None);
    }
    #[test]
    fn ac4_malformed_lines_do_not_drop_surrounding_runs() {
        let history = parse_receipts("{\"run_id\":\"before\",\"skill\":\"aship\",\"start\":\"2026-08-10T08:00:00Z\"}\ntruncated\n{\"run_id\":\"after\",\"skill\":\"aship\",\"start\":\"2026-08-10T09:00:00Z\"}");
        assert_eq!(history.runs.len(), 2);
        assert_eq!(history.skipped_lines, 1);
    }

    #[test]
    fn invalid_utf8_lines_are_skipped_instead_of_lossily_admitted() {
        let path = std::env::temp_dir().join(format!(
            "herdr-loop-runs-invalid-utf8-{}-{}.jsonl",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let bytes = b"{\"event\":\"start\",\"run_id\":\"valid\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}\n{\"event\":\"start\",\"run_id\":\"bad\",\"skill\":\"bad\xFFskill\",\"start\":\"2026-08-10T10:01:00Z\"}\n";
        fs::write(&path, bytes).expect("write temporary receipt fixture");

        let mut reader = ReceiptReader::new(path.clone());
        assert!(reader.refresh());
        let history = reader.history();

        assert_eq!(history.runs.len(), 1);
        assert_eq!(history.runs[0].run_id, "valid");
        assert_eq!(history.skipped_lines, 1);
        fs::remove_file(path).expect("remove temporary receipt fixture");
    }

    #[test]
    fn extreme_timestamps_are_skipped_before_duration_arithmetic() {
        let history = parse_receipts(
            r#"{"event":"start","run_id":"extreme","start":"9223372036854775807-12-31T23:59:59Z"}"#,
        );

        assert!(history.runs.is_empty());
        assert_eq!(history.skipped_lines, 1);
    }

    #[test]
    fn duplicate_run_ids_with_different_identity_are_not_hybridized() {
        let history = parse_receipts(
            r#"
{"event":"start","run_id":"duplicate","skill":"aship","loop_id":"loop-a","session":"session-a","start":"2026-08-10T10:00:00Z"}
{"event":"start","run_id":"duplicate","skill":"aship","loop_id":"loop-b","session":"session-b","start":"2026-08-10T11:00:00Z"}
{"event":"end","run_id":"duplicate","skill":"aship","loop_id":"loop-b","session":"session-b","start":"2026-08-10T11:00:00Z","end":"2026-08-10T11:30:00Z","outcome":"done"}
"#,
        );

        assert_eq!(history.runs.len(), 1);
        assert_eq!(history.runs[0].loop_id.as_deref(), Some("loop-a"));
        assert_eq!(history.runs[0].session.as_deref(), Some("session-a"));
        assert!(matches!(history.runs[0].outcome, RunOutcome::InFlight));
        assert_eq!(history.skipped_lines, 2);
    }

    #[test]
    fn incremental_receipt_reader_consumes_only_appended_lines() {
        let path = std::env::temp_dir().join(format!(
            "herdr-loop-runs-incremental-{}-{}.jsonl",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(
            &path,
            b"{\"event\":\"start\",\"run_id\":\"first\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}\n",
        )
        .expect("write initial receipt fixture");

        let mut reader = ReceiptReader::new(path.clone());
        assert!(reader.refresh());
        assert_eq!(reader.history().runs.len(), 1);

        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open receipt fixture for append");
        writeln!(
            file,
            "{{\"event\":\"start\",\"run_id\":\"second\",\"skill\":\"aship\",\"start\":\"2026-08-10T11:00:00Z\"}}"
        )
        .expect("append receipt fixture");

        assert!(reader.refresh());
        assert_eq!(reader.history().runs.len(), 2);

        writeln!(
            file,
            "{{\"event\":\"end\",\"run_id\":\"first\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\",\"end\":\"2026-08-10T10:30:00Z\",\"outcome\":\"done\"}}"
        )
        .expect("append terminal receipt fixture");
        assert!(reader.refresh());
        let first = reader
            .history()
            .runs
            .iter()
            .find(|run| run.run_id == "first")
            .expect("first run");
        assert!(matches!(first.outcome, RunOutcome::Terminal(_)));
        let second = reader
            .history()
            .runs
            .iter()
            .find(|run| run.run_id == "second")
            .expect("second run");
        assert_eq!(second.outcome, RunOutcome::InFlight);
        assert!(!reader.refresh());
        fs::remove_file(path).expect("remove temporary receipt fixture");
    }

    #[test]
    fn ac5_absent_and_empty_files_are_empty_histories() {
        let mut reader = ReceiptReader::new(PathBuf::from("/definitely/missing/receipts.jsonl"));
        assert!(!reader.refresh());
        assert_eq!(reader.history(), &RunHistory::default());
        assert_eq!(parse_receipts("\n"), RunHistory::default());
    }
    #[test]
    fn parses_loop_registry_membership_and_recent_runs() {
        let registry = parse_loop_registry("## Armed\n- \x60daily\x60 title · policy:auto · max_iterations:2\n## Disarmed\n- \x60old\x60 title\n## Recent runs\n- run:daily · stable_id:stable-1 · outcome:fired · epoch:12 · at:2026-08-10T00:00:00Z\n");
        assert_eq!(registry.loops.len(), 2);
        assert_eq!(registry.loops[0].state, LoopState::Armed);
        assert_eq!(
            registry.loops[0].fields.get("policy"),
            Some(&"auto".to_string())
        );
        assert_eq!(registry.loops[0].recent_runs.len(), 1);
    }
    #[test]
    fn in_flight_duration_uses_injected_clock() {
        let history = parse_receipts(
            "{\"run_id\":\"run\",\"skill\":\"aship\",\"start\":\"2026-08-10T10:00:00Z\"}",
        );
        assert_eq!(duration_label(&history.runs[0], None), "in flight");
        assert_eq!(
            duration_label(
                &history.runs[0],
                parse_timestamp_seconds("2026-08-10T10:30:00Z")
            ),
            "in flight (30m)"
        );
    }
}
