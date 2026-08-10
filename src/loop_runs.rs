use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

const RECEIPT_RELATIVE_PATH: &str = ".local/state/herdr/run-receipts.jsonl";
const LOOP_REGISTRY_RELATIVE_PATH: &str = "workspaces/scalable/loops.md";

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
pub(crate) fn read_default_receipts() -> RunHistory {
    default_receipt_path()
        .map(|path| read_receipts(&path))
        .unwrap_or_default()
}
pub(crate) fn read_receipts(path: &Path) -> RunHistory {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return RunHistory::default(),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to read loop run receipts");
            return RunHistory::default();
        }
    };
    parse_receipts(&String::from_utf8_lossy(&bytes))
}

pub(crate) fn parse_receipts(contents: &str) -> RunHistory {
    let mut history = RunHistory::default();
    let mut indexes = HashMap::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(value) = serde_json::from_str::<serde_json::Value>(line).ok() else {
            history.skipped_lines += 1;
            continue;
        };
        let Some(run_id) = string_field(&value, "run_id") else {
            history.skipped_lines += 1;
            continue;
        };
        let is_end = match string_field(&value, "event") {
            Some(event) if event == "end" => true,
            Some(event) if event == "start" => false,
            Some(_) => {
                history.skipped_lines += 1;
                continue;
            }
            None => value.get("end").is_some() || value.get("outcome").is_some(),
        };
        if is_end {
            let Some(record) = parse_terminal_record(&value, &run_id) else {
                history.skipped_lines += 1;
                continue;
            };
            if let Some(index) = indexes.get(&run_id).copied() {
                apply_terminal(&mut history.runs[index], record);
            } else {
                indexes.insert(run_id, history.runs.len());
                history.runs.push(record);
            }
        } else {
            let Some(record) = parse_start_record(&value, &run_id) else {
                history.skipped_lines += 1;
                continue;
            };
            if let Some(index) = indexes.get(&run_id).copied() {
                apply_start(&mut history.runs[index], record);
            } else {
                indexes.insert(run_id, history.runs.len());
                history.runs.push(record);
            }
        }
    }
    history
        .runs
        .sort_by(|left, right| right.start.cmp(&left.start));
    history
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
    Some(RunRecord {
        run_id: run_id.to_string(),
        skill: string_field(value, "skill")?,
        session: optional_string_field(value, "session"),
        pr: optional_u64_field(value, "pr"),
        ticket: optional_string_field(value, "ticket"),
        loop_id: optional_loop_id(value),
        start: string_field(value, "start")?,
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
    record.end = Some(string_field(value, "end")?);
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
                format_minutes((now - start).max(0) as f64 / 60.0)
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
    let zone_start = time_and_zone
        .find(['Z', '+', '-'])
        .unwrap_or(time_and_zone.len());
    let mut time_parts = time_and_zone[..zone_start].split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.split('.').next()?.parse().ok()?;
    let zone = &time_and_zone[zone_start..];
    let zone_offset = if zone.is_empty() || zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let mut parts = zone[1..].split(':');
        sign * (parts.next()?.parse::<i64>().ok()? * 3600
            + parts.next().unwrap_or("0").parse::<i64>().ok()? * 60)
    };
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - zone_offset,
    )
}
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
        let history = parse_receipts("{\"run_id\":\"before\",\"skill\":\"aship\",\"start\":\"a\"}\ntruncated\n{\"run_id\":\"after\",\"skill\":\"aship\",\"start\":\"b\"}");
        assert_eq!(history.runs.len(), 2);
        assert_eq!(history.skipped_lines, 1);
    }
    #[test]
    fn ac5_absent_and_empty_files_are_empty_histories() {
        assert_eq!(
            read_receipts(Path::new("/definitely/missing/receipts.jsonl")),
            RunHistory::default()
        );
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
