use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::api::schema::AgentStatus;
use crate::layout::PaneId;

pub(crate) const MAX_LINKS: usize = 200;

const LINK_LOOKBEHIND_BYTES: usize = 256;
const MAX_PENDING_LINK_BYTES: usize = 2 * 1024 * 1024;
const LINK_QUIET_PERIOD: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct LinkExtractionGate {
    pending: Mutex<PendingLinkBytes>,
    active: AtomicBool,
    marker_tail: AtomicU64,
    extractions: AtomicU64,
    #[cfg(test)]
    lock_acquisitions: AtomicU64,
}

#[derive(Debug)]
struct PendingLinkBytes {
    bytes: Vec<u8>,
    dirty: bool,
    last_observed_at: Option<Instant>,
    queued_output_urls: Vec<String>,
    queued_osc8_urls: Vec<String>,
}

impl Default for PendingLinkBytes {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            dirty: false,
            last_observed_at: None,
            queued_output_urls: Vec::new(),
            queued_osc8_urls: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExtractedAgentLinks {
    pub(crate) output_urls: Vec<String>,
    pub(crate) osc8_urls: Vec<String>,
}

impl LinkExtractionGate {
    pub(crate) fn observe_chunk(&self, bytes: &[u8]) {
        self.observe_chunk_at(bytes, Instant::now());
    }

    fn observe_chunk_at(&self, bytes: &[u8], observed_at: Instant) {
        let marker_tail = self.marker_tail.load(Ordering::Acquire);
        let colon = memchr::memchr(b':', bytes);
        if !self.active.load(Ordering::Acquire)
            && colon.is_none()
            && !marker_tail_can_continue(marker_tail)
        {
            self.marker_tail
                .store(append_marker_tail(marker_tail, bytes), Ordering::Release);
            return;
        }
        #[cfg(test)]
        self.lock_acquisitions.fetch_add(1, Ordering::Relaxed);
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if pending.dirty {
            append_pending(&mut pending, bytes);
            pending.last_observed_at = Some(observed_at);
            return;
        }

        let (tail, tail_len) = unpack_marker_tail(marker_tail);
        let mut candidate = Vec::with_capacity(tail_len + bytes.len());
        candidate.extend_from_slice(&tail[..tail_len]);
        candidate.extend_from_slice(bytes);
        if let Some(marker_start) = find_scheme_start(&candidate) {
            pending.dirty = true;
            pending.last_observed_at = Some(observed_at);
            append_pending(&mut pending, &candidate[marker_start..]);
            self.active.store(true, Ordering::Release);
            self.marker_tail.store(0, Ordering::Release);
        } else {
            self.marker_tail.store(
                pack_marker_tail(&candidate[candidate.len().saturating_sub(7)..]),
                Ordering::Release,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn take_dirty(&self) -> bool {
        self.take_links_at(Instant::now() + LINK_QUIET_PERIOD)
            .is_some()
    }

    pub(crate) fn take_links(&self) -> Option<ExtractedAgentLinks> {
        self.take_links_at(Instant::now())
    }

    fn take_links_at(&self, now: Instant) -> Option<ExtractedAgentLinks> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let mut pending = self.pending.lock().ok()?;
        if !pending.dirty {
            return None;
        }
        let quiet = pending.last_observed_at.is_some_and(|observed_at| {
            now.saturating_duration_since(observed_at) >= LINK_QUIET_PERIOD
        });
        let mut extracted = extract_agent_links(&pending.bytes, quiet);
        if extracted.unterminated_url {
            return None;
        }
        extracted
            .links
            .output_urls
            .append(&mut pending.queued_output_urls);
        extracted
            .links
            .osc8_urls
            .append(&mut pending.queued_osc8_urls);
        extracted.links.output_urls.sort_unstable();
        extracted.links.output_urls.dedup();
        extracted.links.osc8_urls.sort_unstable();
        extracted.links.osc8_urls.dedup();
        pending.dirty = false;
        self.active.store(false, Ordering::Release);
        pending.bytes.clear();
        pending.last_observed_at = None;
        drop(pending);
        self.extractions.fetch_add(1, Ordering::Relaxed);
        Some(extracted.links)
    }

    #[cfg(test)]
    pub(crate) fn extraction_count(&self) -> u64 {
        self.extractions.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn lock_acquisition_count(&self) -> u64 {
        self.lock_acquisitions.load(Ordering::Relaxed)
    }
}

fn append_pending(pending: &mut PendingLinkBytes, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let remaining = MAX_PENDING_LINK_BYTES.saturating_sub(pending.bytes.len());
        let appended = bytes.len().min(remaining);
        pending.bytes.extend_from_slice(&bytes[..appended]);
        bytes = &bytes[appended..];
        if bytes.is_empty() {
            break;
        }

        let mut extracted = extract_agent_links(&pending.bytes, false);
        queue_links(
            &mut pending.queued_output_urls,
            &mut extracted.links.output_urls,
        );
        queue_links(
            &mut pending.queued_osc8_urls,
            &mut extracted.links.osc8_urls,
        );
        let tail_start = pending.bytes.len().saturating_sub(LINK_LOOKBEHIND_BYTES);
        pending.bytes.drain(..tail_start);
    }
}

fn queue_links(queue: &mut Vec<String>, links: &mut Vec<String>) {
    queue.append(links);
    queue.sort_unstable();
    queue.dedup();
    if queue.len() > MAX_LINKS {
        queue.drain(..queue.len() - MAX_LINKS);
    }
}

fn pack_marker_tail(bytes: &[u8]) -> u64 {
    let bytes = &bytes[bytes.len().saturating_sub(7)..];
    let mut packed = (bytes.len() as u64) << 56;
    for (index, byte) in bytes.iter().enumerate() {
        packed |= u64::from(*byte) << (index * 8);
    }
    packed
}

fn unpack_marker_tail(packed: u64) -> ([u8; 7], usize) {
    let len = ((packed >> 56) as usize).min(7);
    let mut bytes = [0; 7];
    for (index, byte) in bytes[..len].iter_mut().enumerate() {
        *byte = ((packed >> (index * 8)) & 0xff) as u8;
    }
    (bytes, len)
}

fn append_marker_tail(packed: u64, bytes: &[u8]) -> u64 {
    if bytes.len() >= 7 {
        return pack_marker_tail(&bytes[bytes.len() - 7..]);
    }
    let (previous, previous_len) = unpack_marker_tail(packed);
    let keep_previous = 7_usize.saturating_sub(bytes.len());
    let previous_start = previous_len.saturating_sub(keep_previous);
    let kept = previous_len - previous_start;
    let mut tail = [0; 7];
    tail[..kept].copy_from_slice(&previous[previous_start..previous_len]);
    tail[kept..kept + bytes.len()].copy_from_slice(bytes);
    pack_marker_tail(&tail[..kept + bytes.len()])
}

fn marker_tail_can_continue(packed: u64) -> bool {
    let (tail, len) = unpack_marker_tail(packed);
    tail[..len].ends_with(b":") || tail[..len].ends_with(b":/")
}

fn find_scheme_start(bytes: &[u8]) -> Option<usize> {
    memchr::memchr_iter(b':', bytes).find_map(|colon| {
        if bytes.get(colon + 1..colon + 3) != Some(b"//") {
            return None;
        }
        let start = bytes[..colon]
            .iter()
            .rposition(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'.' | b'-'))
            .map_or(0, |index| index + 1);
        let scheme_start = bytes
            .get(start)
            .is_some_and(u8::is_ascii_alphabetic)
            .then_some(start)?;
        Some(
            bytes[..scheme_start]
                .windows(4)
                .rposition(|window| window == b"\x1b]8;")
                .unwrap_or(scheme_start),
        )
    })
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
    pub tasks: Option<Vec<AgentTask>>,
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
    reported_tasks_at: Option<SystemTime>,
    transcript_tasks: Option<Vec<AgentTask>>,
    transcript_tasks_at: Option<SystemTime>,
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
        if let Some(tasks) = payload.tasks {
            pane.reported_tasks = tasks;
            pane.reported_tasks_at = Some(observed_at);
        }
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
            tasks: if pane.transcript_tasks_at > pane.reported_tasks_at {
                pane.transcript_tasks.clone().unwrap_or_default()
            } else {
                pane.reported_tasks.clone()
            },
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
        tasks_observed_at: Option<SystemTime>,
        subagents: Vec<AgentSubagent>,
        links: Vec<ObservedAgentLink>,
    ) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        let previous_last = pane.last_transcript_at;
        let previous_tasks = pane.transcript_tasks.clone();
        let previous_tasks_at = pane.transcript_tasks_at;
        let previous_subagents = pane.observed_subagents.clone();
        if let Some(observed_at) = observed_at {
            pane.last_transcript_at = Some(
                pane.last_transcript_at
                    .map_or(observed_at, |current| current.max(observed_at)),
            );
        }
        if let (Some(tasks), Some(tasks_observed_at)) = (tasks, tasks_observed_at) {
            pane.transcript_tasks = Some(tasks);
            pane.transcript_tasks_at = Some(tasks_observed_at);
        }
        pane.observed_subagents = subagents;
        let links_changed = observe_transcript_links_in_pane(pane, links);
        previous_last != pane.last_transcript_at
            || previous_tasks != pane.transcript_tasks
            || previous_tasks_at != pane.transcript_tasks_at
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
        .as_deref()
        .unwrap_or_default()
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
        .map(|matched| trim_url_suffix(matched.as_str()).to_string())
        .filter(|url| url_domain(url).is_some())
        .collect()
}

fn trim_url_suffix(mut url: &str) -> &str {
    loop {
        let Some(last) = url.chars().next_back() else {
            return url;
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?') {
            url = &url[..url.len() - last.len_utf8()];
            continue;
        }
        let opener = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return url,
        };
        if url.chars().filter(|character| *character == last).count()
            > url.chars().filter(|character| *character == opener).count()
        {
            url = &url[..url.len() - last.len_utf8()];
            continue;
        }
        return url;
    }
}

struct AgentLinkExtraction {
    links: ExtractedAgentLinks,
    unterminated_url: bool,
}

fn extract_agent_links(bytes: &[u8], finalize_tail: bool) -> AgentLinkExtraction {
    let (visible_bytes, mut osc8_urls) = strip_terminal_sequences(bytes);
    let visible = String::from_utf8_lossy(&visible_bytes);
    let mut unterminated_url = false;
    let mut output_urls = URL_RE
        .find_iter(&visible)
        .filter_map(|matched| {
            if matched.end() == visible.len() && !finalize_tail {
                unterminated_url = true;
                return None;
            }
            let url = trim_url_suffix(matched.as_str()).to_string();
            url_domain(&url).is_some().then_some(url)
        })
        .collect::<Vec<_>>();
    if !finalize_tail && has_unterminated_url_candidate(&visible) {
        unterminated_url = true;
    }
    output_urls.sort_unstable();
    output_urls.dedup();
    osc8_urls.sort_unstable();
    osc8_urls.dedup();
    AgentLinkExtraction {
        links: ExtractedAgentLinks {
            output_urls,
            osc8_urls,
        },
        unterminated_url,
    }
}

fn has_unterminated_url_candidate(text: &str) -> bool {
    let tail_start = text
        .char_indices()
        .rev()
        .find(|(_, character)| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '"' | '\'' | '<' | '>')
        })
        .map_or(0, |(index, character)| index + character.len_utf8());
    find_scheme_start(&text.as_bytes()[tail_start..]).is_some()
}

fn strip_terminal_sequences(bytes: &[u8]) -> (Vec<u8>, Vec<String>) {
    let mut visible_bytes = Vec::with_capacity(bytes.len());
    let mut osc8_urls = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'\x1b' {
            visible_bytes.push(bytes[offset]);
            offset += 1;
            continue;
        }

        match bytes.get(offset + 1).copied() {
            Some(b'[') => {
                let Some(end) = bytes[offset + 2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map(|relative| offset + 2 + relative + 1)
                else {
                    break;
                };
                offset = end;
            }
            Some(b']') => {
                let content_start = offset + 2;
                let Some((relative_end, terminator_len)) = osc_terminator(&bytes[content_start..])
                else {
                    break;
                };
                let content_end = content_start + relative_end;
                let content = &bytes[content_start..content_end];
                if let Some(osc8) = content.strip_prefix(b"8;") {
                    if let Some(parameter_end) = osc8.iter().position(|byte| *byte == b';') {
                        osc8_urls.extend(extract_urls(&String::from_utf8_lossy(
                            &osc8[parameter_end + 1..],
                        )));
                    }
                }
                offset = content_end + terminator_len;
            }
            Some(b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/') => {
                if offset + 2 >= bytes.len() {
                    break;
                }
                offset += 3;
            }
            Some(_) => offset += 2,
            None => break,
        }
    }
    (visible_bytes, osc8_urls)
}

fn osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.iter().enumerate().find_map(|(index, byte)| {
        if *byte == b'\x07' {
            Some((index, 1))
        } else if *byte == b'\x1b' && bytes.get(index + 1) == Some(&b'\\') {
            Some((index, 2))
        } else {
            None
        }
    })
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
                    tasks: Some(vec![AgentTask {
                        text: "run focused tests".into(),
                        status: AgentTaskStatus::InProgress,
                    }]),
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
    fn marker_free_chunk_does_not_take_pending_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output without a marker");
        assert_eq!(gate.lock_acquisition_count(), 0);

        gate.observe_chunk(b"visit https://example.test");
        assert_eq!(gate.lock_acquisition_count(), 1);
        gate.observe_chunk(b"/continued\n");
        assert_eq!(gate.lock_acquisition_count(), 2);
    }

    #[test]
    fn scheme_marker_split_across_chunks_triggers_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https:");
        assert!(!gate.take_dirty());

        gate.observe_chunk(b"//split.example.test/path\n");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn scheme_prefix_accumulates_across_three_chunks() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ht");
        gate.observe_chunk(b"tps:");
        gate.observe_chunk(b"//split.example.test/path\n");

        let links = gate.take_links().expect("link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn unfinished_url_waits_for_terminator_before_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://split.example.test");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"/path\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn scheme_only_tail_survives_a_detection_tick_between_chunks() {
        let gate = LinkExtractionGate::default();
        let started = Instant::now();
        gate.observe_chunk_at(b"https://", started);

        assert!(gate
            .take_links_at(started + Duration::from_millis(700))
            .is_none());

        gate.observe_chunk_at(
            b"split.example.test/path\n",
            started + Duration::from_millis(700),
        );
        let links = gate
            .take_links_at(started + Duration::from_millis(701))
            .expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn quiet_period_finalizes_an_unterminated_url_tail() {
        let gate = LinkExtractionGate::default();
        let started = Instant::now();
        gate.observe_chunk_at(b"https://quiet.example.test/path", started);

        assert!(gate
            .take_links_at(started + LINK_QUIET_PERIOD - Duration::from_millis(1))
            .is_none());
        let links = gate
            .take_links_at(started + LINK_QUIET_PERIOD)
            .expect("quiet link extraction");
        assert_eq!(links.output_urls, vec!["https://quiet.example.test/path"]);
    }

    #[test]
    fn styled_url_ignores_csi_osc_and_charset_select_sequences() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(
            concat!(
                "https://sty",
                "\x1b[31m",
                "led.",
                "\x1b]0;terminal title\x07",
                "example.test",
                "\x1b(B",
                "/path\n"
            )
            .as_bytes(),
        );

        let links = gate.take_links().expect("styled link extraction");
        assert_eq!(links.output_urls, vec!["https://styled.example.test/path"]);
    }

    #[test]
    fn pending_buffer_cap_extracts_then_keeps_scanning_later_output() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://before-cap.example.test/path\n");
        gate.observe_chunk(&vec![b'x'; MAX_PENDING_LINK_BYTES]);
        gate.observe_chunk(b"\nhttps://after-cap.example.test/path\n");

        let links = gate.take_links().expect("bounded link extraction");
        assert_eq!(
            links.output_urls,
            vec![
                "https://after-cap.example.test/path",
                "https://before-cap.example.test/path",
            ]
        );
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
        store.observe_transcript(
            pane_id,
            Some(last),
            None,
            None,
            Vec::new(),
            vec![link.clone()],
        );
        store.observe_transcript(
            pane_id,
            Some(last + std::time::Duration::from_secs(30)),
            None,
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
    fn url_extraction_preserves_balanced_trailing_delimiters() {
        assert_eq!(
            extract_urls(concat!(
                "https://en.wikipedia.org/wiki/Function_(mathematics) ",
                "https://example.test/unbalanced), ",
                "https://example.test/list[item] ",
                "https://example.test/trailing]."
            )),
            vec![
                "https://en.wikipedia.org/wiki/Function_(mathematics)",
                "https://example.test/unbalanced",
                "https://example.test/list[item]",
                "https://example.test/trailing",
            ]
        );
    }

    #[test]
    fn newer_reported_tasks_override_older_transcript_tasks() {
        let pane_id = PaneId::from_raw(13);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let transcript_task = AgentTask {
            text: "old transcript task".into(),
            status: AgentTaskStatus::Pending,
        };
        let reported_task = AgentTask {
            text: "new reported task".into(),
            status: AgentTaskStatus::InProgress,
        };
        let mut store = AgentStateStore::default();
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(2)),
            Some(vec![transcript_task]),
            Some(base),
            Vec::new(),
            Vec::new(),
        );
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task.clone()]),
                    ..AgentReportPayload::default()
                },
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid report");

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![reported_task]
        );
    }

    #[test]
    fn newer_transcript_tasks_override_reported_tasks() {
        let pane_id = PaneId::from_raw(14);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let reported_task = AgentTask {
            text: "old reported task".into(),
            status: AgentTaskStatus::Pending,
        };
        let transcript_task = AgentTask {
            text: "new transcript task".into(),
            status: AgentTaskStatus::Completed,
        };
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task]),
                    ..AgentReportPayload::default()
                },
                base,
            )
            .expect("valid report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(1)),
            Some(vec![transcript_task.clone()]),
            Some(base + std::time::Duration::from_secs(1)),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![transcript_task]
        );
    }

    #[test]
    fn explicit_empty_report_and_newer_empty_transcript_clear_tasks() {
        let pane_id = PaneId::from_raw(15);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                serde_json::from_str(
                    r#"{"tasks":[{"text":"reported task","status":"in_progress"}]}"#,
                )
                .expect("report payload"),
                base,
            )
            .expect("valid report");
        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[]}"#).expect("empty report payload"),
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid empty report");
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());

        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[{"text":"new report","status":"pending"}]}"#)
                    .expect("replacement report payload"),
                base + std::time::Duration::from_secs(2),
            )
            .expect("valid replacement report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(3)),
            Some(Vec::new()),
            Some(base + std::time::Duration::from_secs(3)),
            Vec::new(),
            Vec::new(),
        );
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());
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
