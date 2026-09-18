use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::api::schema::AgentStatus;
use crate::layout::PaneId;

pub(crate) const MAX_LINKS: usize = 200;

#[derive(Debug, Default)]
pub(crate) struct LinkExtractionGate {
    dirty: AtomicBool,
    extractions: AtomicU64,
}

impl LinkExtractionGate {
    pub(crate) fn observe_chunk(&self, bytes: &[u8]) {
        if bytes.windows(3).any(|window| window == b"://") {
            self.dirty.store(true, Ordering::Release);
        }
    }

    pub(crate) fn take_dirty(&self) -> bool {
        if self.dirty.swap(false, Ordering::AcqRel) {
            self.extractions.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn extraction_count(&self) -> u64 {
        self.extractions.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentTask {
    pub text: String,
    pub status: AgentTaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSubagentSource {
    Observed,
    Reported,
}

fn reported_source() -> AgentSubagentSource {
    AgentSubagentSource::Reported
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSubagent {
    pub name: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default = "reported_source")]
    pub source: AgentSubagentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentLinkSource {
    Output,
    Osc8,
    Transcript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentLink {
    pub url: String,
    pub domain: String,
    pub first_seen: String,
    pub last_seen: String,
    pub source: AgentLinkSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedAgentLink {
    pub(crate) url: String,
    pub(crate) first_seen: SystemTime,
    pub(crate) last_seen: SystemTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentReportPayload {
    #[serde(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub tasks: Vec<AgentTask>,
    #[serde(default)]
    pub subagents: Vec<AgentSubagent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStateSnapshot {
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub last_acted_at: Option<String>,
    pub tasks: Vec<AgentTask>,
    pub subagents: Vec<AgentSubagent>,
    pub links: Vec<AgentLink>,
}

#[derive(Debug, Clone, Default)]
struct PaneAgentState {
    status_text: Option<String>,
    goal: Option<String>,
    last_report_at: Option<SystemTime>,
    last_working_at: Option<SystemTime>,
    last_transcript_at: Option<SystemTime>,
    reported_tasks: Vec<AgentTask>,
    transcript_tasks: Option<Vec<AgentTask>>,
    reported_subagents: Vec<AgentSubagent>,
    observed_subagents: Vec<AgentSubagent>,
    links: Vec<StoredLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredLink {
    url: String,
    domain: String,
    first_seen: SystemTime,
    last_seen: SystemTime,
    source: AgentLinkSource,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentStateStore {
    panes: HashMap<PaneId, PaneAgentState>,
}

impl AgentStateStore {
    pub(crate) fn remove(&mut self, pane_id: PaneId) {
        self.panes.remove(&pane_id);
    }

    pub(crate) fn report(
        &mut self,
        pane_id: PaneId,
        payload: AgentReportPayload,
        observed_at: SystemTime,
    ) -> Result<(), String> {
        validate_report(&payload)?;

        let pane = self.panes.entry(pane_id).or_default();
        pane.status_text = payload.status_text;
        pane.goal = payload.goal;
        pane.last_report_at = Some(observed_at);
        pane.reported_tasks = payload.tasks;
        pane.reported_subagents = payload
            .subagents
            .into_iter()
            .map(|mut subagent| {
                subagent.source = AgentSubagentSource::Reported;
                subagent
            })
            .collect();
        Ok(())
    }

    pub(crate) fn snapshot(&self, pane_id: PaneId, status: AgentStatus) -> AgentStateSnapshot {
        let Some(pane) = self.panes.get(&pane_id) else {
            return AgentStateSnapshot {
                status,
                status_text: None,
                goal: None,
                last_acted_at: None,
                tasks: Vec::new(),
                subagents: Vec::new(),
                links: Vec::new(),
            };
        };
        let last_acted_at = [
            pane.last_report_at,
            pane.last_working_at,
            pane.last_transcript_at,
        ]
        .into_iter()
        .flatten()
        .max()
        .and_then(format_rfc3339);
        let mut subagents = pane.observed_subagents.clone();
        subagents.extend(pane.reported_subagents.clone());
        AgentStateSnapshot {
            status,
            status_text: pane.status_text.clone(),
            goal: pane.goal.clone(),
            last_acted_at,
            tasks: pane
                .transcript_tasks
                .clone()
                .unwrap_or_else(|| pane.reported_tasks.clone()),
            subagents,
            links: pane
                .links
                .iter()
                .filter_map(|link| {
                    Some(AgentLink {
                        url: link.url.clone(),
                        domain: link.domain.clone(),
                        first_seen: format_rfc3339(link.first_seen)?,
                        last_seen: format_rfc3339(link.last_seen)?,
                        source: link.source,
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn observe_working(&mut self, pane_id: PaneId, observed_at: SystemTime) {
        self.panes.entry(pane_id).or_default().last_working_at = Some(observed_at);
    }

    pub(crate) fn observe_transcript(
        &mut self,
        pane_id: PaneId,
        observed_at: Option<SystemTime>,
        tasks: Option<Vec<AgentTask>>,
        subagents: Vec<AgentSubagent>,
        links: Vec<ObservedAgentLink>,
    ) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        let previous_last = pane.last_transcript_at;
        let previous_tasks = pane.transcript_tasks.clone();
        let previous_subagents = pane.observed_subagents.clone();
        if let Some(observed_at) = observed_at {
            pane.last_transcript_at = Some(
                pane.last_transcript_at
                    .map_or(observed_at, |current| current.max(observed_at)),
            );
        }
        if let Some(tasks) = tasks {
            pane.transcript_tasks = Some(tasks);
        }
        pane.observed_subagents = subagents;
        let links_changed = observe_transcript_links_in_pane(pane, links);
        previous_last != pane.last_transcript_at
            || previous_tasks != pane.transcript_tasks
            || previous_subagents != pane.observed_subagents
            || links_changed
    }

    pub(crate) fn observe_links(
        &mut self,
        pane_id: PaneId,
        urls: impl IntoIterator<Item = String>,
        source: AgentLinkSource,
        observed_at: SystemTime,
    ) {
        let _ = observe_links_in_pane(
            self.panes.entry(pane_id).or_default(),
            urls,
            source,
            observed_at,
        );
    }
}

fn validate_report(payload: &AgentReportPayload) -> Result<(), String> {
    if payload
        .tasks
        .iter()
        .any(|task| task.text.trim().is_empty() || task.text.chars().any(char::is_control))
    {
        return Err("task text must not be empty or contain control characters".into());
    }
    if payload.subagents.iter().any(|subagent| {
        subagent.name.trim().is_empty() || subagent.name.chars().any(char::is_control)
    }) {
        return Err("subagent name must not be empty or contain control characters".into());
    }
    if payload.subagents.iter().any(|subagent| {
        subagent
            .last_active_at
            .as_ref()
            .is_some_and(|value| parse_rfc3339(value).is_none())
    }) {
        return Err("subagent last_active_at must be an RFC3339 timestamp".into());
    }
    Ok(())
}

pub(crate) fn format_rfc3339(value: SystemTime) -> Option<String> {
    time::OffsetDateTime::from(value)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

pub(crate) fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(SystemTime::from)
}

fn observe_links_in_pane(
    pane: &mut PaneAgentState,
    urls: impl IntoIterator<Item = String>,
    source: AgentLinkSource,
    observed_at: SystemTime,
) -> bool {
    let before = pane.links.clone();
    for url in urls {
        let Some(domain) = url_domain(&url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|link| link.url == url) {
            existing.last_seen = existing.last_seen.max(observed_at);
            continue;
        }
        pane.links.push(StoredLink {
            url,
            domain,
            first_seen: observed_at,
            last_seen: observed_at,
            source,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

fn observe_transcript_links_in_pane(
    pane: &mut PaneAgentState,
    links: Vec<ObservedAgentLink>,
) -> bool {
    let before = pane.links.clone();
    for link in links {
        let Some(domain) = url_domain(&link.url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|stored| stored.url == link.url) {
            existing.first_seen = existing.first_seen.min(link.first_seen);
            existing.last_seen = existing.last_seen.max(link.last_seen);
            continue;
        }
        pane.links.push(StoredLink {
            url: link.url,
            domain,
            first_seen: link.first_seen,
            last_seen: link.last_seen,
            source: AgentLinkSource::Transcript,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s<>\"'\x00-\x1f]+"#).expect("static URL regex")
});

pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    URL_RE
        .find_iter(text)
        .map(|matched| {
            matched
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}'])
                .to_string()
        })
        .filter(|url| url_domain(url).is_some())
        .collect()
}

fn url_domain(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if host_port.starts_with('[') {
        host_port.split_once(']')?.0.trim_start_matches('[')
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_round_trips_goal_status_tasks_and_subagents() {
        let pane_id = PaneId::from_raw(7);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    status_text: Some("checking tests".into()),
                    goal: Some("ship MAT-160".into()),
                    tasks: vec![AgentTask {
                        text: "run focused tests".into(),
                        status: AgentTaskStatus::InProgress,
                    }],
                    subagents: vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: None,
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }],
                },
                now,
            )
            .expect("valid report");

        let state = store.snapshot(pane_id, AgentStatus::Working);
        assert_eq!(state.goal.as_deref(), Some("ship MAT-160"));
        assert_eq!(state.status_text.as_deref(), Some("checking tests"));
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.subagents.len(), 1);
        assert_eq!(state.status, AgentStatus::Working);
        assert!(state.last_acted_at.is_some());
    }

    #[test]
    fn last_acted_at_uses_latest_report_working_or_transcript_observation() {
        let pane_id = PaneId::from_raw(8);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(pane_id, AgentReportPayload::default(), base)
            .expect("valid report");
        store.observe_working(pane_id, base + std::time::Duration::from_secs(5));
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(10)),
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store
                .snapshot(pane_id, AgentStatus::Idle)
                .last_acted_at
                .as_deref(),
            format_rfc3339(base + std::time::Duration::from_secs(10)).as_deref()
        );
    }

    #[test]
    fn chunks_without_scheme_marker_do_not_trigger_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output");
        assert!(!gate.take_dirty());
        assert_eq!(gate.extraction_count(), 0);

        gate.observe_chunk(b"visit https://example.test/path");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn links_dedupe_update_last_seen_and_drop_oldest_over_limit() {
        let pane_id = PaneId::from_raw(9);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base,
        );
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base + std::time::Duration::from_secs(1),
        );
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_ne!(state.links[0].first_seen, state.links[0].last_seen);

        for index in 0..=MAX_LINKS {
            store.observe_links(
                pane_id,
                [format!("https://links.test/{index}")],
                AgentLinkSource::Osc8,
                base + std::time::Duration::from_secs(index as u64 + 2),
            );
        }
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), MAX_LINKS);
        assert!(state
            .links
            .iter()
            .all(|link| link.url != "https://example.test/first"));
        assert!(state.links.iter().any(|link| {
            link.url == format!("https://links.test/{MAX_LINKS}")
                && link.source == AgentLinkSource::Osc8
        }));
    }

    #[test]
    fn transcript_link_preserves_its_row_times_across_later_rows() {
        let pane_id = PaneId::from_raw(12);
        let first = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let last = first + std::time::Duration::from_secs(3);
        let mut store = AgentStateStore::default();
        let link = ObservedAgentLink {
            url: "https://transcript.test/task".into(),
            first_seen: first,
            last_seen: last,
        };
        store.observe_transcript(pane_id, Some(last), None, Vec::new(), vec![link.clone()]);
        store.observe_transcript(
            pane_id,
            Some(last + std::time::Duration::from_secs(30)),
            None,
            Vec::new(),
            vec![link],
        );

        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_eq!(state.links[0].source, AgentLinkSource::Transcript);
        assert_eq!(state.links[0].first_seen, format_rfc3339(first).unwrap());
        assert_eq!(state.links[0].last_seen, format_rfc3339(last).unwrap());
    }

    #[test]
    fn url_extraction_trims_terminal_punctuation() {
        assert_eq!(
            extract_urls("open (https://example.test/path), then git://host/repo."),
            vec!["https://example.test/path", "git://host/repo"]
        );
    }

    #[test]
    fn observed_and_reported_subagents_are_both_projected() {
        let pane_id = PaneId::from_raw(10);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    subagents: vec![AgentSubagent {
                        name: "reported reviewer".into(),
                        status: AgentStatus::Blocked,
                        last_active_at: None,
                        pane_id: Some("w1:p2".into()),
                        source: AgentSubagentSource::Observed,
                    }],
                    ..AgentReportPayload::default()
                },
                now,
            )
            .unwrap();
        store.observe_transcript(
            pane_id,
            Some(now),
            None,
            vec![AgentSubagent {
                name: "native worker".into(),
                status: AgentStatus::Working,
                last_active_at: format_rfc3339(now),
                pane_id: None,
                source: AgentSubagentSource::Observed,
            }],
            Vec::new(),
        );

        let subagents = store.snapshot(pane_id, AgentStatus::Working).subagents;
        assert_eq!(subagents.len(), 2);
        assert_eq!(subagents[0].source, AgentSubagentSource::Observed);
        assert_eq!(subagents[1].source, AgentSubagentSource::Reported);
    }

    #[test]
    fn invalid_report_is_rejected_without_changing_state() {
        let pane_id = PaneId::from_raw(11);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some("keep me".into()),
                    ..AgentReportPayload::default()
                },
                now,
            )
            .expect("valid initial report");

        let error = store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some("replace me".into()),
                    subagents: vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: Some("yesterday".into()),
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }],
                    ..AgentReportPayload::default()
                },
                now + std::time::Duration::from_secs(1),
            )
            .expect_err("invalid timestamp must reject the entire report");

        assert!(error.contains("RFC3339"));
        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Idle).goal.as_deref(),
            Some("keep me")
        );
    }
}
