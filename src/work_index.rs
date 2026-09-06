use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::schema::{AgentInfo, AgentStatus};
use crate::config::{MissiveConfig, WorkIndexConfig};
use crate::work_context::{
    linear_ticket_url, normalize_repo_slug, normalize_ticket_id, repo_slug_from_pr_url,
    repo_slugs_match, PaneWorkRole,
};

pub(crate) const WORK_INDEX_BATCH_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const WORK_INDEX_TARGET_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const WORK_ITEM_DETAIL_CACHE_CAPACITY: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkIndexRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemDetailRefreshInFlight {
    pub(crate) keys: Vec<crate::app::state::WorkItemKey>,
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemCheckSummary {
    pub(crate) failing: usize,
    pub(crate) total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemComment {
    pub(crate) author: Option<String>,
    pub(crate) body: String,
    pub(crate) created_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemAction {
    pub(crate) name: String,
    pub(crate) state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemFile {
    pub(crate) path: String,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemCommit {
    pub(crate) short_id: String,
    pub(crate) subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkItemDetail {
    pub(crate) number: Option<u64>,
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) base_ref_name: Option<String>,
    pub(crate) head_ref_name: Option<String>,
    pub(crate) created_at: Option<SystemTime>,
    pub(crate) updated_at: Option<SystemTime>,
    pub(crate) labels: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) review_decision: Option<String>,
    pub(crate) is_draft: Option<bool>,
    pub(crate) reviewers: Vec<String>,
    pub(crate) mergeable: Option<String>,
    pub(crate) merge_state_status: Option<String>,
    pub(crate) head_sha: Option<String>,
    pub(crate) checks: Option<WorkItemCheckSummary>,
    pub(crate) comments: Vec<WorkItemComment>,
    pub(crate) actions: Vec<WorkItemAction>,
    pub(crate) files: Vec<WorkItemFile>,
    pub(crate) commits: Vec<WorkItemCommit>,
    /// GitHub's `gh pr view --json` payload does not expose review threads.
    /// Keep the absence explicit rather than substituting review count data.
    pub(crate) unresolved_review_threads: Option<usize>,
    pub(crate) unavailable: Option<String>,
    pub(crate) observed_at: SystemTime,
}

impl WorkItemDetail {
    /// An otherwise blank detail, for a source that fills only some fields.
    pub(crate) fn empty() -> Self {
        let mut detail = Self::unavailable(String::new());
        detail.unavailable = None;
        detail
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            number: None,
            title: None,
            body: None,
            author: None,
            base_ref_name: None,
            head_ref_name: None,
            created_at: None,
            updated_at: None,
            labels: Vec::new(),
            url: None,
            review_decision: None,
            is_draft: None,
            reviewers: Vec::new(),
            mergeable: None,
            merge_state_status: None,
            head_sha: None,
            checks: None,
            comments: Vec::new(),
            actions: Vec::new(),
            files: Vec::new(),
            commits: Vec::new(),
            unresolved_review_threads: None,
            unavailable: Some(message.into()),
            observed_at: SystemTime::now(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkItemDetailCache {
    entries: HashMap<crate::app::state::WorkItemKey, CachedWorkItemDetail>,
    order: VecDeque<crate::app::state::WorkItemKey>,
}

#[derive(Clone, Debug)]
struct CachedWorkItemDetail {
    detail: WorkItemDetail,
    refreshed_at: Instant,
}

impl WorkItemDetailCache {
    pub(crate) fn get(&self, key: &crate::app::state::WorkItemKey) -> Option<&WorkItemDetail> {
        self.entries.get(key).map(|cached| &cached.detail)
    }

    /// Forget one entry, so the next refresh re-reads it. Used after a write,
    /// where the cached copy is known to be stale the moment it succeeds.
    pub(crate) fn remove(&mut self, key: &crate::app::state::WorkItemKey) {
        self.entries.remove(key);
        self.order.retain(|entry| entry != key);
    }

    pub(crate) fn insert(&mut self, key: crate::app::state::WorkItemKey, detail: WorkItemDetail) {
        self.insert_at(key, detail, Instant::now());
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn insert_at(
        &mut self,
        key: crate::app::state::WorkItemKey,
        detail: WorkItemDetail,
        refreshed_at: Instant,
    ) {
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key.clone());
        self.entries.insert(
            key,
            CachedWorkItemDetail {
                detail,
                refreshed_at,
            },
        );
        while self.entries.len() > WORK_ITEM_DETAIL_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn is_fresh(
        &self,
        key: &crate::app::state::WorkItemKey,
        now: Instant,
        interval: Duration,
    ) -> bool {
        self.entries.get(key).is_some_and(|cached| {
            now.checked_duration_since(cached.refreshed_at)
                .is_some_and(|age| age < interval)
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemPane {
    pub pane_id: String,
    /// Human-facing agent label (`cc·opus·high`), not the pane id: the PR
    /// projection shows who owns the work, and a pane id names nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    pub workspace_id: String,
    pub tab_id: String,
    pub role: Option<PaneWorkRole>,
    pub active_owner: bool,
    pub agent_status: AgentStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemSource {
    pub github: bool,
    pub linear: bool,
    pub pane: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItem {
    pub repo: String,
    pub pr_number: Option<u64>,
    pub pr_url: Option<String>,
    pub pr_title: Option<String>,
    pub pr_state: Option<String>,
    pub draft: bool,
    pub review_decision: Option<String>,
    #[serde(default)]
    pub created_at: Option<SystemTime>,
    #[serde(default)]
    pub updated_at: Option<SystemTime>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub check_state: PrCheckState,
    #[serde(default)]
    pub audience: PrAudience,
    pub ticket_ids: Vec<String>,
    pub ticket_title: Option<String>,
    pub ticket_state: Option<String>,
    #[serde(default)]
    pub ticket_details: Vec<WorkTicket>,
    pub branch: Option<String>,
    pub preview_urls: Vec<String>,
    pub panes: Vec<WorkItemPane>,
    pub source: WorkItemSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrCheckState {
    Passing,
    Failing,
    Pending,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrAudience {
    Authored,
    Other,
    #[default]
    Unclassified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkTicket {
    pub(crate) identifier: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) assignee: Option<String>,
    #[serde(default)]
    pub(crate) priority: Option<u8>,
    #[serde(default)]
    pub(crate) cycle: Option<String>,
    #[serde(default)]
    pub(crate) group: TicketGroup,
    pub(crate) created_at: Option<SystemTime>,
    pub(crate) updated_at: Option<SystemTime>,
    pub(crate) branch: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) relations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveUser {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveEntry {
    pub(crate) id: String,
    pub(crate) author: Option<String>,
    pub(crate) preview: String,
    pub(crate) created_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MissiveConversation {
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) app_url: String,
    pub(crate) web_url: String,
    pub(crate) assignees: Vec<MissiveUser>,
    pub(crate) last_activity_at: Option<SystemTime>,
    pub(crate) closed: bool,
    #[serde(default)]
    pub(crate) pane_bound: bool,
    #[serde(default)]
    pub(crate) messages: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) notes: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) drafts: Vec<MissiveEntry>,
    #[serde(default)]
    pub(crate) posts: Vec<MissiveEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkIndexSource {
    Github,
    Linear,
    Missive,
}

impl WorkIndexSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Linear => "Linear",
            Self::Missive => "Missive",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkIndexUnavailable {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linear: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missive: Option<String>,
}

impl WorkIndexUnavailable {
    pub(crate) fn only(source: WorkIndexSource, reason: impl Into<String>) -> Self {
        let mut unavailable = Self::default();
        let reason = Self::normalized_reason(source, reason.into());
        match source {
            WorkIndexSource::Github => unavailable.github = Some(reason),
            WorkIndexSource::Linear => unavailable.linear = Some(reason),
            WorkIndexSource::Missive => unavailable.missive = Some(reason),
        }
        unavailable
    }

    fn record(&mut self, source: WorkIndexSource, reason: impl Into<String>) {
        if self.is_empty() {
            *self = Self::only(source, reason);
            return;
        }
        let destination = match source {
            WorkIndexSource::Github => &mut self.github,
            WorkIndexSource::Linear => &mut self.linear,
            WorkIndexSource::Missive => &mut self.missive,
        };
        if destination.is_none() {
            *destination = Some(Self::normalized_reason(source, reason.into()));
        }
    }

    fn normalized_reason(source: WorkIndexSource, reason: String) -> String {
        reason
            .strip_prefix(source.label())
            .map(|reason| reason.trim_start_matches([' ', ':']).to_string())
            .unwrap_or(reason)
    }

    pub(crate) fn reason(&self, source: WorkIndexSource) -> Option<&str> {
        match source {
            WorkIndexSource::Github => self.github.as_deref(),
            WorkIndexSource::Linear => self.linear.as_deref(),
            WorkIndexSource::Missive => self.missive.as_deref(),
        }
    }

    pub(crate) fn summary(&self) -> String {
        [
            WorkIndexSource::Github,
            WorkIndexSource::Linear,
            WorkIndexSource::Missive,
        ]
        .into_iter()
        .filter_map(|source| {
            self.reason(source)
                .map(|reason| format!("{}: {reason}", source.label()))
        })
        .collect::<Vec<_>>()
        .join("; ")
    }

    fn is_empty(&self) -> bool {
        self.github.is_none() && self.linear.is_none() && self.missive.is_none()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TicketGroup {
    #[default]
    Assigned,
    Triage,
    DoneThisCycle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub items: Vec<WorkItem>,
    #[serde(default)]
    pub conversations: Vec<MissiveConversation>,
    /// Session-only Missive identity directory. The app reuses it across
    /// refreshes, but the disk cache must not revive a previous token owner's
    /// identity in a later session.
    #[serde(skip)]
    pub missive_users: Vec<MissiveUser>,
    pub unavailable: Option<WorkIndexUnavailable>,
    pub observed_at: SystemTime,
}

impl Snapshot {
    pub(crate) fn unavailable_reason(&self, source: WorkIndexSource) -> Option<&str> {
        self.unavailable
            .as_ref()
            .and_then(|unavailable| unavailable.reason(source))
    }

    pub(crate) fn unavailable_summary(&self) -> Option<String> {
        self.unavailable
            .as_ref()
            .map(WorkIndexUnavailable::summary)
            .filter(|summary| !summary.is_empty())
    }
}

/// Provider identities and assignable users observed by the work-index job.
/// This is session runtime state, not part of the persisted work-index cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkIndexSession {
    pub(crate) linear: ProviderDirectory,
    pub(crate) github: ProviderDirectory,
    pub(crate) missive: ProviderDirectory,
    missive_users: Vec<MissiveUser>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderDirectory {
    pub(crate) viewer: Option<String>,
    pub(crate) assignees: Vec<String>,
    resolved: bool,
}

#[derive(Debug, Clone)]
struct GithubPullRequest {
    repo: String,
    number: u64,
    url: String,
    title: String,
    body: String,
    branch: String,
    draft: bool,
    state: String,
    review_decision: Option<String>,
    created_at: Option<SystemTime>,
    updated_at: Option<SystemTime>,
    additions: u64,
    deletions: u64,
    author: Option<String>,
    assignees: Vec<String>,
    labels: Vec<String>,
    check_state: PrCheckState,
    audience: PrAudience,
}

type LinearTicket = WorkTicket;

#[derive(Debug, Clone)]
struct Attachment {
    ticket_id: String,
    repo: String,
    number: u64,
    url: String,
    title: Option<String>,
    state: Option<String>,
    draft: bool,
    branch: Option<String>,
    preview_urls: Vec<String>,
}

#[derive(Debug)]
enum RefreshError {
    TimedOut,
    Failed(String),
}

const MISSIVE_API_BASE: &str = "https://public.missiveapp.com/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
enum MissiveRequest {
    Conversations { team: String },
    Conversation { id: String },
    ConversationMessages { id: String },
    Message { id: String },
    ConversationDrafts { id: String },
    ConversationPosts { id: String },
    ConversationNotes { id: String },
    Users { organization: Option<String> },
}

impl MissiveRequest {
    const fn method(&self) -> &'static str {
        "GET"
    }

    fn url(&self) -> String {
        match self {
            Self::Conversations { team } => format!(
                "{MISSIVE_API_BASE}/conversations?team_all={}",
                percent_encode(team)
            ),
            Self::Conversation { id } => {
                format!("{MISSIVE_API_BASE}/conversations/{}", percent_encode(id))
            }
            Self::ConversationMessages { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/messages",
                percent_encode(id)
            ),
            Self::Message { id } => {
                format!("{MISSIVE_API_BASE}/messages/{}", percent_encode(id))
            }
            Self::ConversationDrafts { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/drafts",
                percent_encode(id)
            ),
            Self::ConversationPosts { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/posts",
                percent_encode(id)
            ),
            Self::ConversationNotes { id } => format!(
                "{MISSIVE_API_BASE}/conversations/{}/comments",
                percent_encode(id)
            ),
            Self::Users { organization } => organization.as_ref().map_or_else(
                || format!("{MISSIVE_API_BASE}/users"),
                |organization| {
                    format!(
                        "{MISSIVE_API_BASE}/users?organization={}",
                        percent_encode(organization)
                    )
                },
            ),
        }
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::Conversations { .. } => "conversation list",
            Self::Conversation { .. } => "conversation",
            Self::ConversationMessages { .. } => "conversation messages",
            Self::Message { .. } => "message",
            Self::ConversationDrafts { .. } => "conversation drafts",
            Self::ConversationPosts { .. } => "conversation posts",
            Self::ConversationNotes { .. } => "conversation comments",
            Self::Users { .. } => "users",
        }
    }
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn run_missive_get(
    request: &MissiveRequest,
    config: &MissiveConfig,
    program: &Path,
    deadline: Instant,
) -> Result<Value, RefreshError> {
    let token = std::env::var(&config.token_env).map_err(|_| {
        RefreshError::Failed(format!(
            "Missive token environment variable {} is not set",
            config.token_env
        ))
    })?;
    if token.trim().is_empty() {
        return Err(RefreshError::Failed(format!(
            "Missive token environment variable {} is empty",
            config.token_env
        )));
    }
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "--fail-with-body",
        "--silent",
        "--show-error",
        "--request",
        request.method(),
        "--header",
        &format!("Authorization: Bearer {token}"),
        &request.url(),
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("Missive {} GET could not run", request.label()))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(format!(
            "Missive {} GET failed with status {}",
            request.label(),
            output.status
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| RefreshError::Failed("Missive GET returned invalid JSON".into()))
}

fn value_array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn missive_user(value: &Value) -> Option<MissiveUser> {
    let id = value.get("id")?.as_str()?.to_string();
    let email = value_text(value.get("email"));
    let name = value_text(value.get("name"))
        .or_else(|| value_text(value.get("display_name")))
        .or_else(|| email.clone())
        .unwrap_or_else(|| id.clone());
    Some(MissiveUser {
        id,
        name,
        email,
        is_me: value
            .get("me")
            .or_else(|| value.get("is_me"))
            .or_else(|| value.get("current"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn missive_time(value: Option<&Value>) -> Option<SystemTime> {
    if let Some(seconds) = value.and_then(Value::as_u64) {
        return Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
    }
    value
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_system_time)
}

fn missive_preview(value: &Value) -> String {
    let text = ["preview", "body", "text", "markdown", "subject"]
        .into_iter()
        .find_map(|key| value_text(value.get(key)))
        .or_else(|| {
            value
                .get("notification")
                .and_then(|notification| value_text(notification.get("body")))
        })
        .or_else(|| {
            value
                .get("notification")
                .and_then(|notification| value_text(notification.get("title")))
        })
        .unwrap_or_default();
    let mut plain = String::with_capacity(text.len().min(240));
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
        if plain.chars().count() >= 240 {
            break;
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn missive_author(value: &Value) -> Option<String> {
    value
        .get("author")
        .or_else(|| value.get("from_field"))
        .and_then(|author| {
            value_text(author.get("name")).or_else(|| value_text(author.get("address")))
        })
        .or_else(|| value_text(value.get("username")))
}

fn missive_entry(value: &Value) -> Option<MissiveEntry> {
    Some(MissiveEntry {
        id: value_text(value.get("id")).unwrap_or_default(),
        author: missive_author(value),
        preview: missive_preview(value),
        created_at: missive_time(
            value
                .get("delivered_at")
                .or_else(|| value.get("created_at")),
        ),
    })
}

fn parse_missive_entries(value: &Value, key: &str) -> Vec<MissiveEntry> {
    value_array(value, key)
        .iter()
        .filter_map(missive_entry)
        .collect()
}

fn parse_missive_conversation_for_user(
    value: &Value,
    current_user_id: Option<&str>,
) -> Option<MissiveConversation> {
    let id = value.get("id")?.as_str()?.to_string();
    let fallback_url = format!("https://mail.missiveapp.com/#inbox/conversations/{id}");
    let web_url = value_text(value.get("web_url")).unwrap_or_else(|| fallback_url.clone());
    let app_url = value_text(value.get("app_url")).unwrap_or_else(|| web_url.clone());
    let closed_for_user = current_user_id.is_some_and(|current_user_id| {
        value_array(value.get("users").unwrap_or(&Value::Null), "users")
            .iter()
            .find(|user| user.get("id").and_then(Value::as_str) == Some(current_user_id))
            .and_then(|user| user.get("closed"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    Some(MissiveConversation {
        subject: value_text(value.get("subject"))
            .or_else(|| value_text(value.get("latest_message_subject")))
            .unwrap_or_else(|| "(no subject)".into()),
        assignees: value_array(value.get("assignees").unwrap_or(&Value::Null), "users")
            .iter()
            .filter_map(missive_user)
            .collect(),
        last_activity_at: missive_time(value.get("last_activity_at")),
        closed: value
            .get("closed")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                value
                    .get("closed_at")
                    .is_some_and(|closed| !closed.is_null())
            })
            || closed_for_user,
        pane_bound: false,
        id,
        app_url,
        web_url,
        messages: Vec::new(),
        notes: Vec::new(),
        drafts: Vec::new(),
        posts: Vec::new(),
    })
}

fn parse_missive_conversations_for_user(
    value: &Value,
    current_user_id: Option<&str>,
) -> Vec<MissiveConversation> {
    value_array(value, "conversations")
        .iter()
        .filter_map(|value| parse_missive_conversation_for_user(value, current_user_id))
        .collect()
}

fn missive_resource<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value.get(key) {
        Some(Value::Array(values)) => values.first(),
        Some(value) => Some(value),
        None if value.is_object() => Some(value),
        None => None,
    }
}

fn missive_conversation_id(url: &str) -> Option<&str> {
    url.split("/conversations/")
        .nth(1)
        .and_then(|tail| tail.split(['?', '#', '/']).next())
        .filter(|id| !id.is_empty())
}

fn fetch_missive_conversation_detail(
    id: &str,
    current_user_id: Option<&str>,
    config: &MissiveConfig,
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
) -> Result<MissiveConversation, RefreshError> {
    let get = |request| {
        run_missive_get(
            &request,
            config,
            program,
            target_deadline(batch_deadline, target_timeout),
        )
    };
    let value = get(MissiveRequest::Conversation { id: id.into() })?;
    let object = missive_resource(&value, "conversations")
        .ok_or_else(|| RefreshError::Failed("Missive returned no conversation".into()))?;
    let mut conversation = parse_missive_conversation_for_user(object, current_user_id)
        .ok_or_else(|| RefreshError::Failed("Missive returned an invalid conversation".into()))?;
    let messages = get(MissiveRequest::ConversationMessages { id: id.into() })?;
    conversation.messages = parse_missive_entries(&messages, "messages");
    for message in &mut conversation.messages {
        if message.id.is_empty() {
            continue;
        }
        let detail = get(MissiveRequest::Message {
            id: message.id.clone(),
        })?;
        if let Some(hydrated) = missive_resource(&detail, "messages").and_then(missive_entry) {
            *message = hydrated;
        }
    }
    conversation.drafts = parse_missive_entries(
        &get(MissiveRequest::ConversationDrafts { id: id.into() })?,
        "drafts",
    );
    conversation.posts = parse_missive_entries(
        &get(MissiveRequest::ConversationPosts { id: id.into() })?,
        "posts",
    );
    conversation.notes = parse_missive_entries(
        &get(MissiveRequest::ConversationNotes { id: id.into() })?,
        "comments",
    );
    Ok(conversation)
}

fn fetch_missive_snapshot(
    config: &MissiveConfig,
    panes: &[AgentInfo],
    selected_conversation: Option<&str>,
    session_users: Option<&[MissiveUser]>,
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
) -> Result<(Vec<MissiveConversation>, Vec<MissiveUser>), RefreshError> {
    let pane_ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.missive_urls.iter())
        .filter_map(|url| missive_conversation_id(url).map(str::to_string))
        .collect::<HashSet<_>>();
    let configured = config
        .team
        .as_deref()
        .is_some_and(|team| !team.trim().is_empty());
    let token_available =
        std::env::var_os(&config.token_env).is_some_and(|value| !value.is_empty());
    if !configured && !pane_ids.is_empty() {
        return Err(RefreshError::Failed(
            "Missive team is not configured".into(),
        ));
    }
    if !token_available && !pane_ids.is_empty() {
        return Err(RefreshError::Failed(format!(
            "Missive token environment variable {} is not set",
            config.token_env
        )));
    }
    if !configured || !token_available {
        return Ok((Vec::new(), Vec::new()));
    }
    let team = config.team.clone().unwrap_or_default();
    let users = match session_users {
        Some(users) => users.to_vec(),
        None => {
            let users_value = run_missive_get(
                &MissiveRequest::Users {
                    organization: config.organization.clone(),
                },
                config,
                program,
                target_deadline(batch_deadline, target_timeout),
            )?;
            value_array(&users_value, "users")
                .iter()
                .filter_map(missive_user)
                .collect::<Vec<_>>()
        }
    };
    let current_user_id = users
        .iter()
        .find(|user| user.is_me)
        .map(|user| user.id.as_str());
    let conversations_value = run_missive_get(
        &MissiveRequest::Conversations { team },
        config,
        program,
        target_deadline(batch_deadline, target_timeout),
    )?;
    let mut conversations =
        parse_missive_conversations_for_user(&conversations_value, current_user_id);
    for conversation in &mut conversations {
        conversation.pane_bound = pane_ids.contains(&conversation.id);
    }
    let mut detail_ids = pane_ids.clone();
    if let Some(selected) = selected_conversation {
        detail_ids.insert(selected.to_string());
    } else if let Some(first) = conversations.first() {
        detail_ids.insert(first.id.clone());
    }
    for id in detail_ids {
        let detail = fetch_missive_conversation_detail(
            &id,
            current_user_id,
            config,
            program,
            batch_deadline,
            target_timeout,
        );
        let mut detail = match detail {
            Ok(detail) => detail,
            Err(error) if pane_ids.contains(&id) => return Err(error),
            Err(_) => continue,
        };
        detail.pane_bound = pane_ids.contains(&id);
        if let Some(index) = conversations
            .iter()
            .position(|conversation| conversation.id == detail.id)
        {
            conversations[index] = detail;
        } else {
            conversations.push(detail);
        }
    }
    conversations.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_activity_at));
    Ok((conversations, users))
}

fn work_index_repos(config: &WorkIndexConfig, panes: &[AgentInfo]) -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    let candidates = config
        .repos
        .iter()
        .filter_map(|repo| normalize_repo_slug(repo).ok())
        .chain(
            panes
                .iter()
                .filter_map(|pane| pane.work_context.repo.as_deref())
                .filter_map(|repo| normalize_repo_slug(repo).ok()),
        )
        .chain(
            panes
                .iter()
                .flat_map(|pane| pane.work_context.pr_urls.iter())
                .filter_map(|url| repo_slug_from_pr_url(url)),
        );
    for candidate in candidates {
        if !repos.iter().any(|repo| repo_slugs_match(repo, &candidate)) {
            repos.push(candidate);
        }
    }
    repos
}

fn pane_pr_urls(panes: &[AgentInfo]) -> Vec<String> {
    let mut urls = panes
        .iter()
        .flat_map(|pane| pane.work_context.pr_urls.iter().cloned())
        .collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    urls
}

fn pane_pr_target(url: &str) -> Option<(String, u64)> {
    let repo = repo_slug_from_pr_url(url)?;
    let number = url.trim_end_matches('/').rsplit('/').next()?.parse().ok()?;
    Some((repo, number))
}

fn upsert_github(pull_requests: &mut Vec<GithubPullRequest>, pull_request: GithubPullRequest) {
    if let Some(index) = pull_requests
        .iter()
        .position(|existing| existing.url == pull_request.url)
    {
        pull_requests[index] = pull_request;
    } else {
        pull_requests.push(pull_request);
    }
}

fn pane_ticket_ids(panes: &[AgentInfo]) -> Vec<String> {
    let mut ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.ticket_ids.iter())
        .filter_map(|id| normalize_ticket_id(id).ok())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn previous_github(previous: Option<&Snapshot>) -> Vec<GithubPullRequest> {
    previous
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .filter(|item| item.source.github)
        .filter_map(|item| {
            Some(GithubPullRequest {
                repo: item.repo.clone(),
                number: item.pr_number?,
                url: item.pr_url.clone()?,
                title: item.pr_title.clone()?,
                body: item.ticket_ids.join(" "),
                branch: item.branch.clone().unwrap_or_default(),
                draft: item.draft,
                state: item.pr_state.clone().unwrap_or_else(|| "open".into()),
                review_decision: item.review_decision.clone(),
                created_at: item.created_at,
                updated_at: item.updated_at,
                additions: item.additions,
                deletions: item.deletions,
                author: item.author.clone(),
                assignees: item.assignees.clone(),
                labels: item.labels.clone(),
                check_state: item.check_state,
                audience: item.audience,
            })
        })
        .collect()
}

fn previous_linear(previous: Option<&Snapshot>) -> Vec<LinearTicket> {
    let mut tickets = Vec::new();
    for ticket in previous
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .filter(|item| item.source.linear)
        .flat_map(|item| item.ticket_details.iter())
    {
        if !tickets
            .iter()
            .any(|existing: &LinearTicket| existing.identifier == ticket.identifier)
        {
            tickets.push(ticket.clone());
        }
    }
    tickets
}

fn previous_missive(previous: Option<&Snapshot>, panes: &[AgentInfo]) -> Vec<MissiveConversation> {
    let pane_ids = panes
        .iter()
        .flat_map(|pane| pane.work_context.missive_urls.iter())
        .filter_map(|url| missive_conversation_id(url))
        .collect::<HashSet<_>>();
    previous
        .map(|snapshot| {
            snapshot
                .conversations
                .iter()
                .cloned()
                .map(|mut conversation| {
                    conversation.pane_bound = pane_ids.contains(conversation.id.as_str());
                    conversation
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn refresh_work_index(
    config: &WorkIndexConfig,
    panes: &[AgentInfo],
    now: Instant,
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
) -> Snapshot {
    refresh_work_index_with_missive(
        config,
        &MissiveConfig::default(),
        panes,
        WorkIndexRefreshContext::default(),
        now,
        batch_deadline,
        target_timeout,
        gh_program,
        linearis_program,
        Path::new("curl"),
    )
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WorkIndexRefreshContext<'a> {
    pub(crate) selected_missive: Option<&'a str>,
    pub(crate) session_missive_users: Option<&'a [MissiveUser]>,
    pub(crate) previous: Option<&'a Snapshot>,
}

pub(crate) fn refresh_work_index_with_missive(
    config: &WorkIndexConfig,
    missive: &MissiveConfig,
    panes: &[AgentInfo],
    context: WorkIndexRefreshContext<'_>,
    now: Instant,
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
    curl_program: &Path,
) -> Snapshot {
    let WorkIndexRefreshContext {
        selected_missive,
        session_missive_users,
        previous,
    } = context;
    if !config.enabled {
        return Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        };
    }

    let repos = work_index_repos(config, panes);
    let mut degraded = WorkIndexUnavailable::default();
    let previous_github = previous_github(previous);
    let mut github = Vec::new();
    for url in pane_pr_urls(panes) {
        let Some((repo, number)) = pane_pr_target(&url) else {
            degraded.record(WorkIndexSource::Github, "pane pull request URL is invalid");
            continue;
        };
        match fetch_github_pull_request(
            &repo,
            number,
            gh_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(pull_request) => upsert_github(&mut github, pull_request),
            Err(RefreshError::TimedOut) => {
                degraded.record(
                    WorkIndexSource::Github,
                    "pull request observation timed out",
                );
                if let Some(pull_request) = previous_github
                    .iter()
                    .find(|pull_request| pull_request.url == url)
                {
                    upsert_github(&mut github, pull_request.clone());
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Github, message);
                if let Some(pull_request) = previous_github
                    .iter()
                    .find(|pull_request| pull_request.url == url)
                {
                    upsert_github(&mut github, pull_request.clone());
                }
            }
        }
    }
    for repo in &repos {
        match fetch_github_pull_requests(
            repo,
            gh_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(mut values) => {
                let authored = match fetch_github_pr_numbers(
                    repo,
                    gh_program,
                    &["--author", "@me"],
                    target_deadline(batch_deadline, target_timeout),
                ) {
                    Ok(numbers) => numbers,
                    Err(RefreshError::TimedOut) => {
                        degraded.record(WorkIndexSource::Github, "audience observation timed out");
                        HashSet::new()
                    }
                    Err(RefreshError::Failed(message)) => {
                        degraded.record(WorkIndexSource::Github, message);
                        HashSet::new()
                    }
                };
                let others = match fetch_github_pr_numbers(
                    repo,
                    gh_program,
                    &["--search", "review-requested:@me OR mentions:@me"],
                    target_deadline(batch_deadline, target_timeout),
                ) {
                    Ok(numbers) => numbers,
                    Err(RefreshError::TimedOut) => {
                        degraded.record(WorkIndexSource::Github, "audience observation timed out");
                        HashSet::new()
                    }
                    Err(RefreshError::Failed(message)) => {
                        degraded.record(WorkIndexSource::Github, message);
                        HashSet::new()
                    }
                };
                for value in &mut values {
                    value.audience = if authored.contains(&value.number) {
                        PrAudience::Authored
                    } else if others.contains(&value.number) {
                        PrAudience::Other
                    } else {
                        PrAudience::Unclassified
                    };
                }
                for value in values {
                    upsert_github(&mut github, value);
                }
            }
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Github, "list observation timed out");
                for pull_request in previous_github
                    .iter()
                    .filter(|pull_request| repo_slugs_match(&pull_request.repo, repo))
                    .cloned()
                {
                    upsert_github(&mut github, pull_request);
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Github, message);
                for pull_request in previous_github
                    .iter()
                    .filter(|pull_request| repo_slugs_match(&pull_request.repo, repo))
                    .cloned()
                {
                    upsert_github(&mut github, pull_request);
                }
            }
        }
    }

    let previous_tickets = previous_linear(previous);
    let mut listed_ticket_ids = HashSet::new();
    let mut tickets = match config.linear_team.as_deref() {
        Some(team) if !team.trim().is_empty() => match fetch_linear_tickets(
            team,
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(tickets) => {
                listed_ticket_ids.extend(
                    tickets
                        .iter()
                        .map(|ticket| ticket.identifier.to_ascii_uppercase()),
                );
                tickets
            }
            // Neither a timeout nor a failure may throw away a live GitHub
            // half: 74 pull requests with no ticket edge still beat an empty
            // index. Degrade and name the cause instead.
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Linear, "observation timed out");
                previous_tickets.clone()
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Linear, message);
                previous_tickets.clone()
            }
        },
        _ => Vec::new(),
    };
    for identifier in pane_ticket_ids(panes) {
        if listed_ticket_ids.contains(&identifier.to_ascii_uppercase()) {
            continue;
        }
        match fetch_linear_ticket(
            &identifier,
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(ticket) => {
                if let Some(index) = tickets.iter().position(|existing| {
                    existing.identifier.eq_ignore_ascii_case(&ticket.identifier)
                }) {
                    tickets[index] = ticket;
                } else {
                    tickets.push(ticket);
                }
            }
            Err(RefreshError::TimedOut) => {
                degraded.record(WorkIndexSource::Linear, "ticket observation timed out");
                if !tickets
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                {
                    if let Some(ticket) = previous_tickets
                        .iter()
                        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                    {
                        tickets.push(ticket.clone());
                    }
                }
            }
            Err(RefreshError::Failed(message)) => {
                degraded.record(WorkIndexSource::Linear, message);
                if !tickets
                    .iter()
                    .any(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                {
                    if let Some(ticket) = previous_tickets
                        .iter()
                        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&identifier))
                    {
                        tickets.push(ticket.clone());
                    }
                }
            }
        }
    }
    let attachments = fetch_attachments(&tickets, linearis_program, batch_deadline, target_timeout);
    let (conversations, missive_users) = match fetch_missive_snapshot(
        missive,
        panes,
        selected_missive,
        session_missive_users,
        curl_program,
        batch_deadline,
        target_timeout,
    ) {
        Ok(snapshot) => snapshot,
        Err(RefreshError::TimedOut) => {
            degraded.record(WorkIndexSource::Missive, "observation timed out");
            (previous_missive(previous, panes), Vec::new())
        }
        Err(RefreshError::Failed(message)) => {
            degraded.record(WorkIndexSource::Missive, message);
            (previous_missive(previous, panes), Vec::new())
        }
    };

    let mut items = github
        .into_iter()
        .map(|pr| {
            let searchable = format!("{}\n{}", pr.branch, pr.body).to_ascii_lowercase();
            let ticket_ids = tickets
                .iter()
                .filter(|ticket| searchable.contains(&ticket.identifier.to_ascii_lowercase()))
                .map(|ticket| ticket.identifier.clone())
                .collect();
            WorkItem {
                repo: pr.repo,
                pr_number: Some(pr.number),
                pr_url: Some(pr.url),
                pr_title: Some(pr.title),
                pr_state: Some(pr.state),
                draft: pr.draft,
                review_decision: pr.review_decision,
                created_at: pr.created_at,
                updated_at: pr.updated_at,
                additions: pr.additions,
                deletions: pr.deletions,
                author: pr.author,
                assignees: pr.assignees,
                labels: pr.labels,
                check_state: pr.check_state,
                audience: pr.audience,
                ticket_ids,
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: Some(pr.branch),
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: WorkItemSource {
                    github: true,
                    ..WorkItemSource::default()
                },
            }
        })
        .collect::<Vec<_>>();

    let mut ticket_by_id = tickets
        .iter()
        .map(|ticket| (ticket.identifier.clone(), ticket))
        .collect::<HashMap<_, _>>();
    for attachment in attachments {
        let ticket = ticket_by_id.remove(&attachment.ticket_id);
        let item_index = items.iter().position(|item| {
            item.pr_number == Some(attachment.number)
                && repo_slugs_match(&item.repo, &attachment.repo)
        });
        let index = if let Some(index) = item_index {
            index
        } else {
            items.push(WorkItem {
                repo: attachment.repo.clone(),
                pr_number: Some(attachment.number),
                pr_url: Some(attachment.url.clone()),
                pr_title: attachment.title.clone(),
                pr_state: attachment.state.clone(),
                draft: attachment.draft,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: attachment.branch.clone(),
                preview_urls: attachment.preview_urls.clone(),
                panes: Vec::new(),
                source: WorkItemSource::default(),
            });
            items.len() - 1
        };
        let item = &mut items[index];
        item.source.linear = true;
        push_unique(&mut item.ticket_ids, attachment.ticket_id);
        item.pr_state = item.pr_state.take().or(attachment.state);
        item.branch = item.branch.take().or(attachment.branch);
        item.preview_urls.extend(attachment.preview_urls);
        item.preview_urls.sort();
        item.preview_urls.dedup();
        if let Some(ticket) = ticket {
            item.ticket_title = ticket.title.clone();
            item.ticket_state = ticket.state.clone();
            item.ticket_details.push(ticket.clone());
        }
    }

    for ticket in ticket_by_id.into_values() {
        let Some(_ticket_url) = linear_ticket_url(&ticket.identifier) else {
            continue;
        };
        let repo = panes
            .iter()
            .find(|pane| {
                pane.work_context
                    .ticket_ids
                    .iter()
                    .filter_map(|id| normalize_ticket_id(id).ok())
                    .any(|id| id == ticket.identifier)
            })
            .and_then(|pane| pane.work_context.repo.as_deref())
            .and_then(|repo| normalize_repo_slug(repo).ok())
            .or_else(|| {
                (config.repos.len() == 1)
                    .then(|| normalize_repo_slug(&config.repos[0]).ok())
                    .flatten()
            })
            .unwrap_or_default();
        items.push(WorkItem {
            repo,
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: PrCheckState::Unknown,
            audience: PrAudience::Unclassified,
            ticket_ids: vec![ticket.identifier.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
            ticket_details: vec![ticket.clone()],
            branch: ticket.branch.clone(),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource {
                linear: true,
                ..WorkItemSource::default()
            },
        });
    }

    join_panes(&mut items, panes);
    items.sort_by(|left, right| {
        left.repo
            .cmp(&right.repo)
            .then_with(|| left.pr_number.cmp(&right.pr_number))
            .then_with(|| left.ticket_ids.cmp(&right.ticket_ids))
    });
    let _ = now;
    Snapshot {
        items,
        conversations,
        missive_users,
        unavailable: (!degraded.is_empty()).then_some(degraded),
        observed_at: SystemTime::now(),
    }
}

fn exit_detail(label: &str, output: &std::process::Output) -> String {
    // Keep the child's own words: a bare "exited unsuccessfully" is
    // undiagnosable in the field, which is exactly where these tools fail.
    let stderr = String::from_utf8_lossy(&output.stderr);
    // These tools report failures as pretty-printed JSON, whose first line is
    // a bare "{". Collapse to one line and keep it bounded.
    let collapsed = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail = (!collapsed.is_empty()).then(|| {
        if collapsed.chars().count() > 200 {
            collapsed.chars().take(200).collect::<String>()
        } else {
            collapsed.clone()
        }
    });
    let detail = detail.as_deref();
    match detail {
        Some(detail) => format!("{label} exited unsuccessfully: {detail}"),
        None => format!("{label} exited unsuccessfully"),
    }
}

fn target_deadline(batch_deadline: Instant, target_timeout: Duration) -> Instant {
    (Instant::now() + target_timeout).min(batch_deadline)
}

/// Resolve provider-local "me" identities and assignee directories once per
/// app session. Callers retain the returned value across index refreshes.
pub(crate) fn resolve_work_index_session(
    config: &WorkIndexConfig,
    mut session: WorkIndexSession,
    missive_users: &[MissiveUser],
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
) -> WorkIndexSession {
    if !session.linear.resolved {
        session.linear = fetch_linear_directory(
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
        );
        session.linear.resolved = true;
    }
    if !session.github.resolved {
        session.github =
            fetch_github_directory(&config.repos, gh_program, batch_deadline, target_timeout);
        session.github.resolved = true;
    }
    if !session.missive.resolved && !missive_users.is_empty() {
        session.missive = resolve_missive_assignees(missive_users);
        session.missive_users = missive_users.to_vec();
    }
    session
}

fn fetch_linear_directory(program: &Path, deadline: Instant) -> ProviderDirectory {
    let viewer = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["auth", "status", "--compact"]);
        crate::noninteractive_process::output_with_deadline(command, deadline)
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
            .and_then(|value| nested_text(value.get("user"), "name"))
    };
    let mut assignees = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["users", "list", "--active", "-l", "250", "--compact"]);
        crate::noninteractive_process::output_with_deadline(command, deadline)
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
            .and_then(|value| value.get("nodes").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|user| nested_text(Some(&user), "name"))
            .collect::<Vec<_>>()
    };
    include_viewer(&mut assignees, viewer.as_deref());
    ProviderDirectory {
        viewer,
        assignees,
        resolved: true,
    }
}

fn fetch_github_directory(
    repos: &[String],
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
) -> ProviderDirectory {
    let viewer = {
        let mut command = crate::noninteractive_process::command(program);
        command.args(["api", "user"]);
        crate::noninteractive_process::output_with_deadline(
            command,
            target_deadline(batch_deadline, target_timeout),
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|value| nested_text(Some(&value), "login"))
    };
    let mut assignees = Vec::new();
    for repo in repos
        .iter()
        .filter_map(|repo| normalize_repo_slug(repo).ok())
    {
        let mut command = crate::noninteractive_process::command(program);
        command.args([
            "api",
            &format!("repos/{repo}/assignees"),
            "--paginate",
            "--slurp",
        ]);
        let Some(value) = crate::noninteractive_process::output_with_deadline(
            command,
            target_deadline(batch_deadline, target_timeout),
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok()) else {
            continue;
        };
        collect_github_assignees(&value, &mut assignees);
    }
    include_viewer(&mut assignees, viewer.as_deref());
    ProviderDirectory {
        viewer,
        assignees,
        resolved: true,
    }
}

fn collect_github_assignees(value: &Value, assignees: &mut Vec<String>) {
    let Some(values) = value.as_array() else {
        return;
    };
    for value in values {
        if value.is_array() {
            collect_github_assignees(value, assignees);
        } else if let Some(login) = nested_text(Some(value), "login") {
            assignees.push(login);
        }
    }
}

fn include_viewer(assignees: &mut Vec<String>, viewer: Option<&str>) {
    if let Some(viewer) = viewer {
        assignees.push(viewer.to_string());
    }
    assignees.sort_by_key(|value| value.to_ascii_lowercase());
    assignees.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
}

pub(crate) fn resolve_missive_assignees(users: &[MissiveUser]) -> ProviderDirectory {
    let viewer = users
        .iter()
        .find(|user| user.is_me)
        .map(|user| user.name.clone());
    let mut assignees = users
        .iter()
        .map(|user| user.name.clone())
        .collect::<Vec<_>>();
    include_viewer(&mut assignees, viewer.as_deref());
    ProviderDirectory {
        viewer,
        assignees,
        resolved: true,
    }
}

const GITHUB_PULL_REQUEST_SUMMARY_FIELDS: &str = "number,title,author,assignees,state,updatedAt,createdAt,reviewDecision,labels,isDraft,headRefName,url";

fn parse_github_pull_request(value: Value, fallback_repo: &str) -> Option<GithubPullRequest> {
    let url = value.get("url")?.as_str()?.to_string();
    Some(GithubPullRequest {
        repo: repo_slug_from_pr_url(&url).unwrap_or_else(|| fallback_repo.to_string()),
        number: value.get("number")?.as_u64()?,
        url,
        title: value.get("title")?.as_str()?.to_string(),
        body: String::new(),
        branch: value.get("headRefName")?.as_str()?.to_string(),
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        state: value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("OPEN")
            .to_ascii_lowercase(),
        review_decision: value
            .get("reviewDecision")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        additions: value.get("additions").and_then(Value::as_u64).unwrap_or(0),
        deletions: value.get("deletions").and_then(Value::as_u64).unwrap_or(0),
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string),
        assignees: value
            .get("assignees")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|assignee| nested_text(Some(assignee), "login"))
            .collect(),
        labels: value
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| value_text(Some(label)))
            .collect(),
        check_state: pr_check_state(value.get("statusCheckRollup")),
        audience: PrAudience::Unclassified,
    })
}

fn fetch_github_pull_requests(
    repo: &str,
    program: &Path,
    deadline: Instant,
) -> Result<Vec<GithubPullRequest>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        GITHUB_PULL_REQUEST_SUMMARY_FIELDS,
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("GitHub observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout)
        .map_err(|_| RefreshError::Failed("GitHub observation returned invalid JSON".into()))?;
    Ok(values
        .into_iter()
        .filter_map(|value| parse_github_pull_request(value, repo))
        .collect())
}

fn fetch_github_pull_request(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
) -> Result<GithubPullRequest, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let number_arg = number.to_string();
    command.args([
        "pr",
        "view",
        &number_arg,
        "--repo",
        repo,
        "--json",
        GITHUB_PULL_REQUEST_SUMMARY_FIELDS,
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("GitHub pull request observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub pull request observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub pull request observation returned invalid JSON".into())
    })?;
    parse_github_pull_request(value, repo).ok_or_else(|| {
        RefreshError::Failed("GitHub pull request observation returned an invalid item".into())
    })
}

fn fetch_github_pr_numbers(
    repo: &str,
    program: &Path,
    filter: &[&str],
    deadline: Instant,
) -> Result<HashSet<u64>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args(["pr", "list", "--repo", repo, "--state", "open"]);
    command.args(filter);
    command.args(["--limit", "200", "--json", "number"]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("GitHub PR audience observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub PR audience observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub PR audience observation returned invalid JSON".into())
    })?;
    Ok(values
        .iter()
        .filter_map(|value| value.get("number").and_then(Value::as_u64))
        .collect())
}

fn pr_check_state(value: Option<&Value>) -> PrCheckState {
    let Some(checks) = value.and_then(Value::as_array) else {
        return PrCheckState::Unknown;
    };
    if checks.iter().any(|check| {
        github_check_state(check).is_some_and(|state| {
            matches!(
                state,
                "ERROR" | "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
            )
        })
    }) {
        PrCheckState::Failing
    } else if checks.is_empty()
        || checks.iter().any(|check| {
            !matches!(
                github_check_state(check),
                Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
            )
        })
    {
        PrCheckState::Pending
    } else {
        PrCheckState::Passing
    }
}

const GITHUB_PULL_REQUEST_DETAIL_FIELDS: &str =
    "number,title,body,author,baseRefName,headRefName,headRefOid,createdAt,updatedAt,labels,url,reviewDecision,isDraft,statusCheckRollup,reviews,comments,files,commits,mergeable,mergeStateStatus";

fn fetch_github_pull_request_detail(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
) -> Result<WorkItemDetail, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let number_arg = number.to_string();
    command.args([
        "pr",
        "view",
        &number_arg,
        "--repo",
        repo,
        "--json",
        GITHUB_PULL_REQUEST_DETAIL_FIELDS,
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("GitHub PR detail observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "GitHub PR detail observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("GitHub PR detail observation returned invalid JSON".into())
    })?;
    Ok(WorkItemDetail {
        number: value.get("number").and_then(Value::as_u64),
        title: value_text(value.get("title")),
        body: value_text(value.get("body")),
        author: value
            .get("author")
            .and_then(|author| author.get("login"))
            .and_then(Value::as_str)
            .map(str::to_string),
        base_ref_name: value_text(value.get("baseRefName")),
        head_ref_name: value_text(value.get("headRefName")),
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        labels: value
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| value_text(Some(label)))
            .collect(),
        url: value_text(value.get("url")),
        review_decision: value_text(value.get("reviewDecision")),
        is_draft: value.get("isDraft").and_then(Value::as_bool),
        reviewers: value
            .get("reviews")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|review| {
                review
                    .get("author")
                    .and_then(|author| author.get("login"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .fold(Vec::new(), |mut reviewers, reviewer| {
                if !reviewers.contains(&reviewer) {
                    reviewers.push(reviewer);
                }
                reviewers
            }),
        mergeable: value_text(value.get("mergeable")),
        merge_state_status: value_text(value.get("mergeStateStatus")),
        head_sha: value_text(value.get("headRefOid")),
        checks: status_check_summary(value.get("statusCheckRollup")),
        comments: github_comments(value.get("comments")),
        actions: github_actions(value.get("statusCheckRollup")),
        files: github_files(value.get("files")),
        commits: github_commits(value.get("commits")),
        unresolved_review_threads: fetch_unresolved_review_thread_count(
            repo, number, program, deadline,
        ),
        unavailable: None,
        observed_at: SystemTime::now(),
    })
}

/// GraphQL query for a PR's review-thread resolution state. `gh pr view --json`
/// does not expose this, so it takes a second call, kept small (thread
/// resolution only) and best-effort: a failure here must never fail or delay
/// the PR detail it's attached to.
const UNRESOLVED_REVIEW_THREADS_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100) { nodes { isResolved } } } } }";

/// Count a pull request's unresolved review threads via `gh api graphql`.
/// Degrades to `None` on any failure (bad repo slug, non-zero exit, timeout,
/// malformed JSON) so the caller's detail fetch is never blocked by it.
fn fetch_unresolved_review_thread_count(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
) -> Option<usize> {
    let (owner, name) = repo.split_once('/')?;
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "api",
        "graphql",
        "-f",
        &format!("query={UNRESOLVED_REVIEW_THREADS_QUERY}"),
        "-F",
        &format!("owner={owner}"),
        "-F",
        &format!("name={name}"),
        "-F",
        &format!("number={number}"),
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).ok()?;
    if !output.status.success() {
        return None;
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).ok()?;
    parse_unresolved_review_thread_count(&value)
}

/// Pure parse of the `unresolvedReviewThreadsQuery` GraphQL response. Kept
/// separate from the `gh` shellout so it can be exercised with a fixture
/// instead of a live network call.
fn parse_unresolved_review_thread_count(value: &Value) -> Option<usize> {
    let nodes = value
        .get("data")?
        .get("repository")?
        .get("pullRequest")?
        .get("reviewThreads")?
        .get("nodes")?
        .as_array()?;
    Some(
        nodes
            .iter()
            .filter(|node| node.get("isResolved").and_then(Value::as_bool) == Some(false))
            .count(),
    )
}

/// Read one Linear issue with its comment threads.
///
/// Reuses `WorkItemDetail` so the LRU cache, the loading set and the generation
/// guard that already serve pull requests apply to tickets unchanged.
fn fetch_linear_ticket_detail(
    identifier: &str,
    program: &Path,
    deadline: Instant,
) -> Result<WorkItemDetail, RefreshError> {
    let run = |args: &[&str]| {
        let mut command = crate::noninteractive_process::command(program);
        command.args(args);
        crate::noninteractive_process::output_with_deadline(command, deadline).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("linearis could not be run ({})", program.display()))
            }
        })
    };
    // Newer linearis builds expose `issues get`; keep the installed `read`
    // spelling as a compatibility fallback until every host has upgraded.
    let mut output = run(&["issues", "get", identifier])?;
    if !output.status.success() {
        output = run(&[
            "issues",
            "read",
            identifier,
            "--with-comment-threads",
            "--compact",
        ])?;
    }
    if !output.status.success() {
        return Err(RefreshError::Failed(format!(
            "linearis issues read failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| RefreshError::Failed("linearis returned invalid JSON".to_string()))?;

    let mut detail = WorkItemDetail::empty();
    detail.title = value_text(value.get("title"));
    detail.body = value_text(value.get("description"));
    detail.url = value_text(value.get("url")).or_else(|| linear_ticket_url(identifier));
    detail.created_at = value_time(value.get("createdAt"));
    detail.updated_at = value_time(value.get("updatedAt"));
    detail.comments = linear_comments(value.get("comments"));
    Ok(detail)
}

/// Flatten Linear's comment threads: a root comment followed by its replies, in
/// the order they were written, so a thread reads top to bottom.
fn linear_comments(value: Option<&Value>) -> Vec<WorkItemComment> {
    fn author(comment: &Value) -> Option<String> {
        let user = comment.get("user")?;
        user.get("displayName")
            .or_else(|| user.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }
    fn push(comment: &Value, out: &mut Vec<WorkItemComment>) {
        let Some(body) = value_text(comment.get("body")) else {
            return;
        };
        out.push(WorkItemComment {
            author: author(comment),
            body,
            created_at: value_time(comment.get("createdAt")),
        });
        let replies = comment
            .get("replies")
            .and_then(|replies| replies.get("nodes").or(Some(replies)))
            .and_then(Value::as_array);
        for reply in replies.into_iter().flatten() {
            push(reply, out);
        }
    }

    let nodes = value
        .and_then(|value| value.get("nodes").or(Some(value)))
        .and_then(Value::as_array);
    let mut out = Vec::new();
    for comment in nodes.into_iter().flatten() {
        push(comment, &mut out);
    }
    out
}

fn github_comments(value: Option<&Value>) -> Vec<WorkItemComment> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|comment| {
            let body = value_text(comment.get("body"))?;
            Some(WorkItemComment {
                author: comment
                    .get("author")
                    .and_then(|author| author.get("login"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                body,
                created_at: value_time(comment.get("createdAt")),
            })
        })
        .collect()
}

fn github_actions(value: Option<&Value>) -> Vec<WorkItemAction> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| {
            let name =
                value_text(action.get("name")).or_else(|| value_text(action.get("context")))?;
            let state = github_check_state(action)
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".to_string());
            Some(WorkItemAction { name, state })
        })
        .collect()
}

fn github_check_state(check: &Value) -> Option<&str> {
    ["conclusion", "state", "status"]
        .into_iter()
        .find_map(|field| {
            check
                .get(field)
                .and_then(Value::as_str)
                .filter(|state| !state.is_empty())
        })
}

fn github_files(value: Option<&Value>) -> Vec<WorkItemFile> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| {
            Some(WorkItemFile {
                path: value_text(file.get("path"))?,
                additions: file.get("additions").and_then(Value::as_u64).unwrap_or(0),
                deletions: file.get("deletions").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

fn github_commits(value: Option<&Value>) -> Vec<WorkItemCommit> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|commit| {
            let oid = value_text(commit.get("oid"))?;
            Some(WorkItemCommit {
                short_id: oid.chars().take(7).collect(),
                subject: value_text(commit.get("messageHeadline")).unwrap_or_else(|| "—".into()),
            })
        })
        .collect()
}

fn status_check_summary(value: Option<&Value>) -> Option<WorkItemCheckSummary> {
    let rollup = value?.as_array()?;
    if rollup.is_empty() {
        return None;
    }
    let failing = rollup
        .iter()
        .filter(|check| {
            github_check_state(check).is_some_and(|state| {
                matches!(
                    state,
                    "ERROR" | "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                )
            })
        })
        .count();
    Some(WorkItemCheckSummary {
        failing,
        total: rollup.len(),
    })
}

pub(crate) fn parse_rfc3339_system_time(value: &str) -> Option<SystemTime> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let zone_index = bytes[19..]
        .iter()
        .position(|byte| matches!(byte, b'Z' | b'+' | b'-'))?
        + 19;
    if zone_index > 19
        && (bytes[19] != b'.'
            || zone_index == 20
            || !bytes[20..zone_index].iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    let zone = bytes[zone_index];
    let offset_seconds = match zone {
        b'Z' if zone_index + 1 == bytes.len() => 0,
        b'+' | b'-'
            if zone_index + 6 == bytes.len() && bytes.get(zone_index + 3) == Some(&b':') =>
        {
            let hours = parse_decimal(bytes.get(zone_index + 1..zone_index + 3)?)?;
            let minutes = parse_decimal(bytes.get(zone_index + 4..zone_index + 6)?)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            u64::from(hours) * 60 * 60 + u64::from(minutes) * 60
        }
        _ => return None,
    };
    let year = parse_decimal(bytes.get(0..4)?)?;
    let month = parse_decimal(bytes.get(5..7)?)?;
    let day = parse_decimal(bytes.get(8..10)?)?;
    let hour = parse_decimal(bytes.get(11..13)?)?;
    let minute = parse_decimal(bytes.get(14..16)?)?;
    let second = parse_decimal(bytes.get(17..19)?)?;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };
    if year < 1970 || day == 0 || day > month_days || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = days_since_unix_epoch(year, month, day)
        .checked_mul(24 * 60 * 60)?
        .checked_add(u64::from(hour) * 60 * 60)?
        .checked_add(u64::from(minute) * 60)?
        .checked_add(u64::from(second))?;
    let utc_seconds = match zone {
        b'+' if offset_seconds > seconds => {
            return SystemTime::UNIX_EPOCH
                .checked_sub(Duration::from_secs(offset_seconds - seconds));
        }
        b'+' => seconds - offset_seconds,
        b'-' => seconds.checked_add(offset_seconds)?,
        _ => seconds,
    };
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(utc_seconds))
}

fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        if byte.is_ascii_digit() {
            Some(value * 10 + u32::from(*byte - b'0'))
        } else {
            None
        }
    })
}

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_since_unix_epoch(year: u32, month: u32, day: u32) -> u64 {
    let adjusted_year = i64::from(year) - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        (adjusted_year - 399) / 400
    };
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as u64
}

fn fetch_linear_tickets(
    team: &str,
    program: &Path,
    deadline: Instant,
) -> Result<Vec<LinearTicket>, RefreshError> {
    let mut tickets = Vec::new();
    tickets.extend(fetch_linear_ticket_group(
        team,
        program,
        deadline,
        TicketGroup::Assigned,
        &["--assignee", "me"],
    )?);
    tickets.extend(fetch_linear_ticket_group(
        team,
        program,
        deadline,
        TicketGroup::Triage,
        &["--status", "Triage"],
    )?);
    if let Some(cycle) = fetch_active_linear_cycle(team, program, deadline)? {
        tickets.extend(fetch_linear_ticket_group(
            team,
            program,
            deadline,
            TicketGroup::DoneThisCycle,
            &["--cycle", &cycle, "--status", "Done"],
        )?);
    }

    let mut deduplicated: Vec<LinearTicket> = Vec::new();
    for ticket in tickets {
        if let Some(existing) = deduplicated
            .iter_mut()
            .find(|existing| existing.identifier == ticket.identifier)
        {
            if ticket_group_rank(ticket.group) > ticket_group_rank(existing.group) {
                *existing = ticket;
            }
        } else {
            deduplicated.push(ticket);
        }
    }
    Ok(deduplicated)
}

fn parse_linear_ticket(value: &Value, group: TicketGroup) -> Option<LinearTicket> {
    let identifier = normalize_ticket_id(value.get("identifier")?.as_str()?).ok()?;
    Some(WorkTicket {
        url: value_text(value.get("url")).or_else(|| linear_ticket_url(&identifier)),
        identifier,
        title: value_text(value.get("title")),
        description: value_text(value.get("description")),
        state: nested_text(value.get("state"), "name"),
        assignee: nested_text(value.get("assignee"), "name"),
        priority: value
            .get("priority")
            .and_then(Value::as_u64)
            .and_then(|priority| u8::try_from(priority).ok()),
        cycle: nested_text(value.get("cycle"), "name"),
        group,
        created_at: value
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        updated_at: value
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_system_time),
        branch: value_text(value.get("branchName")),
        labels: value
            .get("labels")
            .and_then(|labels| labels.get("nodes"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| nested_text(Some(label), "name"))
            .collect(),
        parent: value.get("parent").and_then(format_linear_reference),
        relations: format_linear_relations(value),
    })
}

fn fetch_linear_ticket(
    identifier: &str,
    program: &Path,
    deadline: Instant,
) -> Result<LinearTicket, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args(["issues", "read", identifier]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("Linear ticket observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear ticket observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("Linear ticket observation returned invalid JSON".into())
    })?;
    parse_linear_ticket(&value, TicketGroup::Assigned).ok_or_else(|| {
        RefreshError::Failed("Linear ticket observation returned an invalid item".into())
    })
}

fn ticket_group_rank(group: TicketGroup) -> u8 {
    match group {
        TicketGroup::Assigned => 0,
        TicketGroup::Triage => 1,
        TicketGroup::DoneThisCycle => 2,
    }
}

fn fetch_active_linear_cycle(
    team: &str,
    program: &Path,
    deadline: Instant,
) -> Result<Option<String>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args(["cycles", "list", "--team", team, "--active", "--compact"]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("Linear cycle observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear cycle observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout).map_err(|_| {
        RefreshError::Failed("Linear cycle observation returned invalid JSON".into())
    })?;
    Ok(value
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| nodes.first())
        .and_then(|cycle| value_text(cycle.get("name"))))
}

fn fetch_linear_ticket_group(
    team: &str,
    program: &Path,
    deadline: Instant,
    group: TicketGroup,
    filters: &[&str],
) -> Result<Vec<LinearTicket>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args(["issues", "list", "--team", team]);
    command.args(filters);
    command.args(["-l", "100", "--compact"]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("Linear observation failed: {error}"))
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "Linear observation",
            &output,
        )));
    }
    let value = serde_json::from_slice::<Value>(&output.stdout)
        .map_err(|_| RefreshError::Failed("Linear observation returned invalid JSON".into()))?;
    let Some(nodes) = value.get("nodes").and_then(Value::as_array) else {
        return Err(RefreshError::Failed(
            "Linear observation returned no nodes".into(),
        ));
    };
    Ok(nodes
        .iter()
        .filter_map(|node| parse_linear_ticket(node, group))
        .collect())
}

fn nested_text(value: Option<&Value>, field: &str) -> Option<String> {
    value_text(value.and_then(|value| value.get(field))).or_else(|| value_text(value))
}

fn format_linear_reference(value: &Value) -> Option<String> {
    let identifier = nested_text(Some(value), "identifier");
    let title = nested_text(Some(value), "title");
    match (identifier, title) {
        (Some(identifier), Some(title)) => Some(format!("{identifier}  {title}")),
        (Some(identifier), None) => Some(identifier),
        (None, Some(title)) => Some(title),
        (None, None) => value_text(Some(value)),
    }
}

fn format_linear_relations(value: &Value) -> Vec<String> {
    let forward = value
        .get("relations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| format_linear_relation(relation, false));
    let inverse = value
        .get("inverseRelations")
        .and_then(|relations| relations.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| format_linear_relation(relation, true));
    forward.chain(inverse).collect()
}

fn format_linear_relation(value: &Value, inverse: bool) -> Option<String> {
    let kind = nested_text(Some(value), "type").map(|kind| match (inverse, kind.as_str()) {
        (true, "blocks") => "blocked by".to_string(),
        (true, "blocked by") => "blocks".to_string(),
        (true, "duplicate" | "duplicate of") => "duplicated by".to_string(),
        (false, "duplicate") => "duplicate of".to_string(),
        _ => kind,
    });
    let related = value
        .get("relatedIssue")
        .or_else(|| value.get("issue"))
        .and_then(format_linear_reference)
        .or_else(|| format_linear_reference(value));
    match (kind, related) {
        (Some(kind), Some(related)) => Some(format!("{kind}  {related}")),
        (None, Some(related)) => Some(related),
        _ => None,
    }
}

fn fetch_attachments(
    tickets: &[LinearTicket],
    program: &Path,
    batch_deadline: Instant,
    target_timeout: Duration,
) -> Vec<Attachment> {
    let mut results = Vec::new();
    for chunk in tickets.chunks(8) {
        std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|ticket| {
                    let program = program.to_path_buf();
                    scope.spawn(move || {
                        fetch_ticket_attachments(
                            ticket,
                            &program,
                            target_deadline(batch_deadline, target_timeout),
                        )
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                if let Ok(Ok(mut attachments)) = handle.join() {
                    results.append(&mut attachments);
                }
            }
        });
        if Instant::now() >= batch_deadline {
            break;
        }
    }
    results
}

fn fetch_ticket_attachments(
    ticket: &LinearTicket,
    program: &Path,
    deadline: Instant,
) -> Result<Vec<Attachment>, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "attachments",
        "list",
        &ticket.identifier,
        "--source-type",
        "github",
        "--compact",
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(error.to_string())
            }
        },
    )?;
    if !output.status.success() {
        return Err(RefreshError::Failed(exit_detail(
            "attachment observation",
            &output,
        )));
    }
    let values = serde_json::from_slice::<Vec<Value>>(&output.stdout)
        .map_err(|_| RefreshError::Failed("attachment observation returned invalid JSON".into()))?;
    Ok(values
        .into_iter()
        .filter_map(|value| {
            let metadata = value.get("metadata")?;
            let number = metadata.get("number")?.as_u64()?;
            let url = value.get("url")?.as_str()?.to_string();
            let repo = match (
                metadata.get("repoLogin").and_then(Value::as_str),
                metadata.get("repoName").and_then(Value::as_str),
            ) {
                (Some(owner), Some(name)) => {
                    normalize_repo_slug(&format!("{owner}/{name}")).ok()?
                }
                _ => repo_slug_from_pr_url(&url)?,
            };
            Some(Attachment {
                ticket_id: ticket.identifier.clone(),
                repo,
                number,
                url,
                title: value_text(metadata.get("title")),
                state: value_text(metadata.get("status")),
                draft: metadata
                    .get("draft")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                branch: value_text(metadata.get("branch")),
                preview_urls: metadata
                    .get("previewLinks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|link| link.get("url").and_then(Value::as_str).map(str::to_string))
                    .collect(),
            })
        })
        .collect())
}

fn value_time(value: Option<&Value>) -> Option<SystemTime> {
    value
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_system_time)
}

fn value_text(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| {
        value.as_str().map(str::to_string).or_else(|| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    })
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn join_panes(items: &mut Vec<WorkItem>, panes: &[AgentInfo]) {
    for pane in panes {
        let pane_ticket_ids = pane
            .work_context
            .ticket_ids
            .iter()
            .filter_map(|id| normalize_ticket_id(id).ok())
            .collect::<HashSet<_>>();
        let matched = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let pr_match = item.pr_url.as_deref().is_some_and(|url| {
                    pane.work_context
                        .pr_urls
                        .iter()
                        .any(|pane_url| pane_url == url)
                });
                let ticket_match = item
                    .ticket_ids
                    .iter()
                    .any(|ticket| pane_ticket_ids.contains(ticket));
                let repo_match =
                    pane.work_context.repo.as_deref().is_none_or(|repo| {
                        item.repo.is_empty() || repo_slugs_match(repo, &item.repo)
                    });
                (pr_match || ticket_match)
                    .then_some(index)
                    .filter(|_| repo_match)
            })
            .collect::<Vec<_>>();
        let mut matched = matched;
        if matched.is_empty()
            && (!pane.work_context.pr_urls.is_empty()
                || !pane_ticket_ids.is_empty()
                || pane.work_context.repo.is_some())
        {
            let repo = pane
                .work_context
                .repo
                .as_deref()
                .and_then(|repo| normalize_repo_slug(repo).ok())
                .or_else(|| {
                    pane.work_context
                        .pr_urls
                        .iter()
                        .find_map(|url| repo_slug_from_pr_url(url))
                })
                .unwrap_or_default();
            items.push(WorkItem {
                repo,
                pr_number: None,
                pr_url: pane.work_context.pr_urls.first().cloned(),
                pr_title: None,
                pr_state: None,
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                ticket_ids: {
                    // Sorted so the snapshot is byte-stable across refreshes:
                    // it is consumed as JSON by ghx and diffed by hand.
                    let mut ids = pane_ticket_ids.into_iter().collect::<Vec<_>>();
                    ids.sort();
                    ids
                },
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: pane.work_context.branch.clone(),
                preview_urls: pane.work_context.preview_urls.clone(),
                panes: Vec::new(),
                source: WorkItemSource {
                    pane: true,
                    ..WorkItemSource::default()
                },
            });
            matched.push(items.len() - 1);
        }
        for index in matched {
            let item = &mut items[index];
            item.source.pane = true;
            item.panes.push(WorkItemPane {
                pane_id: pane.pane_id.clone(),
                agent_label: pane
                    .display_agent
                    .clone()
                    .or_else(|| pane.agent.clone())
                    .or_else(|| pane.name.clone()),
                workspace_id: pane.workspace_id.clone(),
                tab_id: pane.tab_id.clone(),
                role: pane.work_context.role,
                active_owner: pane.work_context.active_owner,
                agent_status: pane.agent_status,
            });
        }
    }
}

fn work_index_snapshot_path_for(
    state_dir: &Path,
    session_name: Option<&str>,
) -> std::path::PathBuf {
    state_dir.join("work-index").join(format!(
        "{}.json",
        session_name.unwrap_or(crate::session::DEFAULT_SESSION_NAME)
    ))
}

pub(crate) fn work_index_snapshot_path() -> std::path::PathBuf {
    let session_name = crate::session::active_name();
    work_index_snapshot_path_for(&crate::config::state_dir(), session_name.as_deref())
}

pub(crate) fn remove_session_snapshot(session_name: &str) -> io::Result<()> {
    let path = work_index_snapshot_path_for(&crate::config::state_dir(), Some(session_name));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn write_snapshot(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(io::Error::other)?;
    std::fs::write(path, bytes)
}

/// Read back a snapshot persisted by [`write_snapshot`].
///
/// Cold start should show the last known work index rather than an empty
/// view until the first refresh completes, but the persisted file is best
/// effort: a missing or corrupt file (partial write, format change) must
/// fall back to `None` rather than panic or block startup.
pub(crate) fn load_snapshot(path: &Path) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Locate a CLI herdr shells out to.
///
/// The server does not always inherit an interactive shell's `PATH` — started
/// from launchd, a desktop launcher or a login-less session it gets a bare one
/// — so relying on the bare name means the tool silently "could not be run" on
/// exactly the machines where it is installed. Search `PATH` first, then the
/// usual install locations, and fall back to the bare name so the error still
/// names the program rather than a guessed path.
fn resolve_program(name: &str) -> std::path::PathBuf {
    let executable = |path: &Path| -> bool {
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
    };

    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if executable(&candidate) {
                return candidate;
            }
        }
    }

    // Ordered by how these tools are actually installed here: user bin first,
    // then Homebrew, then a global npm prefix.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(home) = home {
        for relative in ["bin", ".local/bin", ".npm-global/bin", ".bun/bin"] {
            candidates.push(home.join(relative).join(name));
        }
    }
    for absolute in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        candidates.push(Path::new(absolute).join(name));
    }
    for candidate in candidates {
        if executable(&candidate) {
            return candidate;
        }
    }
    Path::new(name).to_path_buf()
}

impl crate::app::App {
    pub(crate) fn work_index_gh_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_gh_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("gh")
    }

    pub(crate) fn work_index_linearis_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_linearis_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("linearis")
    }

    pub(crate) fn work_index_curl_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_curl_program_override.as_ref() {
            return program.clone();
        }
        resolve_program("curl")
    }

    pub(crate) fn work_index_refresh_deadline(&self) -> Option<Instant> {
        self.work_index_config.enabled.then(|| {
            self.work_index_refresh_in_flight
                .as_ref()
                .map_or(self.next_work_index_refresh, |refresh| refresh.deadline)
        })
    }

    pub(crate) fn work_item_detail_refresh_deadline(&self) -> Option<Instant> {
        self.work_item_detail_refresh_in_flight
            .as_ref()
            .map(|refresh| refresh.deadline)
    }

    pub(crate) fn start_work_index_refresh_if_due(&mut self, now: Instant) {
        if !self.work_index_config.enabled {
            return;
        }
        if self
            .work_index_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            self.work_index_refresh_in_flight = None;
        }
        if self.work_index_refresh_in_flight.is_some() || now < self.next_work_index_refresh {
            return;
        }
        self.next_work_index_refresh =
            now + Duration::from_secs(self.work_index_config.refresh_interval_seconds.max(1));
        self.last_work_index_refresh_generation =
            self.last_work_index_refresh_generation.wrapping_add(1);
        let generation = self.last_work_index_refresh_generation;
        let deadline = now + WORK_INDEX_BATCH_TIMEOUT;
        self.work_index_refresh_in_flight = Some(WorkIndexRefreshInFlight {
            generation,
            deadline,
        });
        if let Some(view) = self.state.work_view.as_mut() {
            view.refreshing = true;
        }
        let config = self.work_index_config.clone();
        let panes = self.collect_agent_infos();
        let mut session_config = config.clone();
        session_config.repos = work_index_repos(&config, &panes);
        let selected_missive = self
            .state
            .work_view
            .as_ref()
            .and_then(|view| view.selected_missive.clone());
        let event_tx = self.event_tx.clone();
        let gh_program = self.work_index_gh_program();
        let linearis_program = self.work_index_linearis_program();
        let session = self.work_index_session.clone();
        let curl_program = self.work_index_curl_program();
        let missive = self.missive_config.clone();
        let session_missive_users =
            (!session.missive_users.is_empty()).then(|| session.missive_users.clone());
        let previous_snapshot = self.work_index_snapshot.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-work-index".into())
            .spawn(move || {
                let snapshot = refresh_work_index_with_missive(
                    &config,
                    &missive,
                    &panes,
                    WorkIndexRefreshContext {
                        selected_missive: selected_missive.as_deref(),
                        session_missive_users: session_missive_users.as_deref(),
                        previous: previous_snapshot.as_ref(),
                    },
                    Instant::now(),
                    deadline,
                    WORK_INDEX_TARGET_TIMEOUT,
                    &gh_program,
                    &linearis_program,
                    &curl_program,
                );
                let session = resolve_work_index_session(
                    &session_config,
                    session,
                    &snapshot.missive_users,
                    deadline,
                    WORK_INDEX_TARGET_TIMEOUT,
                    &gh_program,
                    &linearis_program,
                );
                let _ = event_tx.blocking_send(crate::events::AppEvent::WorkIndexRefreshed {
                    generation,
                    snapshot: Box::new(snapshot),
                    session,
                });
            });
    }

    pub(crate) fn handle_work_index_refreshed(
        &mut self,
        generation: u64,
        snapshot: Snapshot,
        session: WorkIndexSession,
    ) -> bool {
        if generation <= self.last_applied_work_index_refresh_generation
            || generation != self.last_work_index_refresh_generation
        {
            return false;
        }
        self.work_index_refresh_in_flight = None;
        self.last_applied_work_index_refresh_generation = generation;
        if let Err(error) = write_snapshot(&work_index_snapshot_path(), &snapshot) {
            tracing::warn!(error = %error, "failed to persist work index snapshot");
        }
        if let Some(work_view) = self.state.work_view.as_mut() {
            work_view.replace_snapshot(snapshot.clone());
            work_view.refreshing = false;
        }
        self.work_index_session = session.clone();
        self.state.work_index_session = session;
        self.state.work_index_snapshot = Some(snapshot.clone());
        self.work_index_snapshot = Some(snapshot);
        self.refresh_pane_settlement_at(Instant::now());
        self.invalidate_work_item_details();
        true
    }

    fn invalidate_work_item_details(&mut self) {
        if let Some(refresh) = self.work_item_detail_refresh_in_flight.take() {
            for key in refresh.keys {
                self.state.work_item_detail_loading.remove(&key);
            }
        }
        self.last_work_item_detail_refresh_generation = self
            .last_work_item_detail_refresh_generation
            .wrapping_add(1);
        self.state.work_item_detail_cache.clear();
    }

    pub(crate) fn start_work_item_detail_refresh_if_due(
        &mut self,
        now: Instant,
        section: crate::app::state::DockHomeSection,
        selection: Option<crate::app::state::WorkItemKey>,
        detail_visible: bool,
    ) {
        if self
            .work_item_detail_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| now >= refresh.deadline)
        {
            if let Some(refresh) = self.work_item_detail_refresh_in_flight.take() {
                for key in refresh.keys {
                    self.state.work_item_detail_loading.remove(&key);
                }
            }
        }
        if !detail_visible {
            return;
        }
        if self.work_item_detail_refresh_in_flight.is_some() {
            return;
        }
        let mut keys = self.state.dock_home_keys_for_section(section);
        if let Some(selection) = selection.as_ref() {
            if let Some(index) = keys.iter().position(|key| key == selection) {
                let selection = keys.remove(index);
                keys.insert(0, selection);
            } else {
                keys.insert(0, selection.clone());
            }
        }
        keys.truncate(WORK_ITEM_DETAIL_CACHE_CAPACITY);
        let interval = Duration::from_secs(self.work_index_config.refresh_interval_seconds.max(1));
        keys.retain(|key| {
            // Pull requests are prefetched across the whole section: one `gh`
            // call each, and the tab strip gets walked often. A ticket costs a
            // separate `linearis` read, so only the one actually being looked
            // at is worth fetching - prefetching the section would mean a
            // process per ticket for detail nobody has asked to see.
            let fetchable = if key.pr_number.is_some() {
                !key.repo.is_empty()
            } else {
                key.ticket_id.is_some() && selection.as_ref() == Some(key)
            };
            fetchable
                && !self
                    .state
                    .work_item_detail_cache
                    .is_fresh(key, now, interval)
        });
        if keys.is_empty() {
            return;
        }

        self.last_work_item_detail_refresh_generation = self
            .last_work_item_detail_refresh_generation
            .wrapping_add(1);
        let generation = self.last_work_item_detail_refresh_generation;
        let deadline = now + WORK_INDEX_BATCH_TIMEOUT;
        self.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: keys.clone(),
            generation,
            deadline,
        });
        self.state
            .work_item_detail_loading
            .extend(keys.iter().cloned());
        let event_tx = self.event_tx.clone();
        let gh_program = self.work_index_gh_program();
        let linearis_program = self.work_index_linearis_program();
        let _ = std::thread::Builder::new()
            .name("herdr-work-item-details".into())
            .spawn(move || {
                let mut details = Vec::with_capacity(keys.len());
                for chunk in keys.chunks(8) {
                    std::thread::scope(|scope| {
                        let handles = chunk
                            .iter()
                            .map(|key| {
                                let key = key.clone();
                                let gh_program = gh_program.clone();
                                let linearis_program = linearis_program.clone();
                                scope.spawn(move || {
                                    let target =
                                        target_deadline(deadline, WORK_INDEX_TARGET_TIMEOUT);
                                    let (result, what) = match (key.pr_number, &key.ticket_id) {
                                        (Some(number), _) => (
                                            fetch_github_pull_request_detail(
                                                &key.repo,
                                                number,
                                                &gh_program,
                                                target,
                                            ),
                                            "GitHub PR detail",
                                        ),
                                        (None, Some(ticket)) => (
                                            fetch_linear_ticket_detail(
                                                ticket,
                                                &linearis_program,
                                                target,
                                            ),
                                            "Linear ticket detail",
                                        ),
                                        (None, None) => (
                                            Err(RefreshError::Failed(
                                                "work item has neither a pull request nor a ticket"
                                                    .to_string(),
                                            )),
                                            "work item detail",
                                        ),
                                    };
                                    let detail = match result {
                                        Ok(detail) => detail,
                                        Err(RefreshError::TimedOut) => WorkItemDetail::unavailable(
                                            format!("{what} observation timed out"),
                                        ),
                                        Err(RefreshError::Failed(message)) => {
                                            WorkItemDetail::unavailable(message)
                                        }
                                    };
                                    (key, detail)
                                })
                            })
                            .collect::<Vec<_>>();
                        for handle in handles {
                            if let Ok(detail) = handle.join() {
                                details.push(detail);
                            }
                        }
                    });
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                let _ = event_tx.blocking_send(crate::events::AppEvent::WorkItemDetailRefreshed {
                    generation,
                    details,
                });
            });
    }

    pub(crate) fn handle_work_item_detail_refreshed(
        &mut self,
        generation: u64,
        details: Vec<(crate::app::state::WorkItemKey, WorkItemDetail)>,
    ) -> bool {
        let matches_in_flight = self
            .work_item_detail_refresh_in_flight
            .as_ref()
            .is_some_and(|refresh| {
                refresh.generation == generation
                    && refresh.keys.len() == details.len()
                    && refresh
                        .keys
                        .iter()
                        .zip(&details)
                        .all(|(expected, (actual, _))| expected == actual)
            });
        if !matches_in_flight
            || generation <= self.last_applied_work_item_detail_refresh_generation
            || generation != self.last_work_item_detail_refresh_generation
        {
            return false;
        }
        let refresh = self.work_item_detail_refresh_in_flight.take();
        self.last_applied_work_item_detail_refresh_generation = generation;
        if let Some(refresh) = refresh {
            for key in refresh.keys {
                self.state.work_item_detail_loading.remove(&key);
            }
        }
        for (key, detail) in details {
            self.state.work_item_detail_cache.insert(key, detail);
        }
        true
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[test]
    fn resolve_program_finds_a_tool_on_path_and_names_it_when_missing() {
        let dir = std::env::temp_dir().join(format!("herdr-resolve-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let tool = dir.join("herdr-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").expect("write fake tool");

        let previous = std::env::var_os("PATH");
        // SAFETY: single-threaded test, restored before returning.
        unsafe { std::env::set_var("PATH", &dir) };
        let found = resolve_program("herdr-fake-tool");
        let missing = resolve_program("herdr-tool-that-does-not-exist");
        match previous {
            Some(path) => unsafe { std::env::set_var("PATH", path) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert_eq!(found, tool, "a tool on PATH resolves to its full path");
        assert_eq!(
            missing,
            Path::new("herdr-tool-that-does-not-exist"),
            "an unfound tool keeps its bare name so the error can name it"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unresolved_review_thread_count_counts_only_unresolved_nodes() {
        let value: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                 {"isResolved":false},
                 {"isResolved":true},
                 {"isResolved":false}
               ]}}}}}"#,
        )
        .expect("valid fixture");

        assert_eq!(super::parse_unresolved_review_thread_count(&value), Some(2));
    }

    #[test]
    fn unresolved_review_thread_count_is_zero_when_every_thread_is_resolved() {
        let value: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                 {"isResolved":true}
               ]}}}}}"#,
        )
        .expect("valid fixture");

        assert_eq!(super::parse_unresolved_review_thread_count(&value), Some(0));
    }

    #[test]
    fn unresolved_review_thread_count_is_none_on_a_malformed_response() {
        let value: Value = serde_json::from_str(r#"{"data":null,"errors":[{"message":"boom"}]}"#)
            .expect("valid fixture");

        assert_eq!(super::parse_unresolved_review_thread_count(&value), None);
    }

    #[test]
    fn linear_comment_threads_flatten_to_root_then_replies() {
        let value: Value = serde_json::from_str(
            r#"{"nodes":[
                 {"body":"root one","createdAt":"2026-09-01T10:00:00.000Z",
                  "user":{"displayName":"Ada"},
                  "replies":{"nodes":[{"body":"a reply","user":{"name":"Grace"}}]}},
                 {"body":"root two","user":{"displayName":"Alan"}}
               ]}"#,
        )
        .expect("fixture");

        let comments = linear_comments(Some(&value));
        let rendered: Vec<(Option<&str>, &str)> = comments
            .iter()
            .map(|comment| (comment.author.as_deref(), comment.body.as_str()))
            .collect();

        assert_eq!(
            rendered,
            vec![
                (Some("Ada"), "root one"),
                (Some("Grace"), "a reply"),
                (Some("Alan"), "root two"),
            ],
            "a thread reads root first, then its replies"
        );
        assert!(
            comments[0].created_at.is_some(),
            "a comment keeps the time it was written"
        );
        assert!(comments[1].created_at.is_none());
    }

    #[test]
    fn linear_comments_tolerate_an_absent_or_empty_field() {
        assert!(linear_comments(None).is_empty());
        let empty: Value = serde_json::from_str(r#"{"nodes":[]}"#).expect("fixture");
        assert!(linear_comments(Some(&empty)).is_empty());
    }

    #[test]
    fn missive_client_request_catalog_is_get_only() {
        let requests = [
            MissiveRequest::Conversations {
                team: "team".into(),
            },
            MissiveRequest::Conversation {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationMessages {
                id: "conversation".into(),
            },
            MissiveRequest::Message {
                id: "message".into(),
            },
            MissiveRequest::ConversationDrafts {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationPosts {
                id: "conversation".into(),
            },
            MissiveRequest::ConversationNotes {
                id: "conversation".into(),
            },
            MissiveRequest::Users {
                organization: Some("organization".into()),
            },
        ];
        assert!(requests.iter().all(|request| request.method() == "GET"));
        assert!(requests
            .iter()
            .all(|request| request.url().starts_with(MISSIVE_API_BASE)));
        assert_eq!(
            requests[0].url(),
            "https://public.missiveapp.com/v1/conversations?team_all=team"
        );
    }

    #[test]
    fn missive_fixture_parsers_cover_conversations_messages_drafts_and_posts() {
        let conversations: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/conversations.json"))
                .expect("conversation fixture");
        let messages: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/messages.json"))
                .expect("message fixture");
        let drafts: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/drafts.json"))
                .expect("draft fixture");
        let posts: Value =
            serde_json::from_str(include_str!("../tests/fixtures/missive/posts.json"))
                .expect("post fixture");

        let parsed = parse_missive_conversations_for_user(&conversations, Some("user-1"));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].subject, "Billing question from fixture");
        assert_eq!(parsed[0].assignees[0].name, "Ada Example");
        assert!(!parsed[0].assignees[0].is_me);
        assert_eq!(
            parsed[0].app_url,
            "missive://mail.missiveapp.com/#inbox/conversations/11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(
            parsed[0].web_url,
            "https://mail.missiveapp.com/#inbox/conversations/11111111-1111-4111-8111-111111111111"
        );
        assert!(!parsed[0].closed);
        assert!(parsed[1].closed);
        assert_eq!(parsed[1].subject, "Archived fixture conversation");
        assert_eq!(parse_missive_entries(&messages, "messages").len(), 2);
        assert_eq!(
            parse_missive_entries(&messages, "messages")[1].preview,
            "I am checking that now."
        );
        assert_eq!(parse_missive_entries(&drafts, "drafts").len(), 1);
        assert_eq!(
            parse_missive_entries(&posts, "posts")[0].preview,
            "Invoice lookup completed."
        );
        assert_eq!(parse_missive_entries(&posts, "comments").len(), 1);

        let conversation_response = serde_json::json!({
            "conversations": [conversations["conversations"][0].clone()]
        });
        assert_eq!(
            missive_resource(&conversation_response, "conversations")
                .and_then(|value| parse_missive_conversation_for_user(value, None))
                .map(|conversation| conversation.id),
            Some("11111111-1111-4111-8111-111111111111".into())
        );
        let message_response = serde_json::json!({
            "messages": messages["messages"][0].clone()
        });
        assert_eq!(
            missive_resource(&message_response, "messages")
                .and_then(missive_entry)
                .map(|message| message.preview),
            Some("Could you clarify the latest invoice?".into())
        );
    }

    #[test]
    fn missive_selected_conversation_hydrates_every_read_only_detail() {
        let dir = fixture_dir("missive-selected-detail");
        let curl = dir.join("curl");
        write_executable(
            &curl,
            r#"#!/bin/sh
case "$*" in
  *"--request GET"*) ;;
  *) exit 64 ;;
esac
for argument do url="$argument"; done
case "$url" in
  *"/users?organization=org") printf '%s' '{"users":[{"id":"me","name":"Ada","me":true}]}' ;;
  *"/conversations?team_all=team") printf '%s' '{"conversations":[{"id":"first","subject":"First","app_url":"missive://first","web_url":"https://mail.missiveapp.com/#inbox/conversations/first"},{"id":"selected","subject":"Selected","app_url":"missive://selected","web_url":"https://mail.missiveapp.com/#inbox/conversations/selected"}]}' ;;
  *"/conversations/selected/messages") printf '%s' '{"messages":[{"id":"message-1","preview":"list preview"}]}' ;;
  *"/messages/message-1") printf '%s' '{"messages":{"id":"message-1","preview":"hydrated message"}}' ;;
  *"/conversations/selected/drafts") printf '%s' '{"drafts":[{"id":"draft-1","preview":"draft"}]}' ;;
  *"/conversations/selected/posts") printf '%s' '{"posts":[{"id":"post-1","preview":"post"}]}' ;;
  *"/conversations/selected/comments") printf '%s' '{"comments":[{"id":"note-1","preview":"note"}]}' ;;
  *"/conversations/selected") printf '%s' '{"conversations":[{"id":"selected","subject":"Selected detail","app_url":"missive://selected","web_url":"https://mail.missiveapp.com/#inbox/conversations/selected"}]}' ;;
  *) exit 65 ;;
esac
"#,
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: Some("org".into()),
        };

        let (conversations, users) = fetch_missive_snapshot(
            &config,
            &[],
            Some("selected"),
            None,
            &curl,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
        )
        .expect("Missive snapshot");

        let selected = conversations
            .iter()
            .find(|conversation| conversation.id == "selected")
            .expect("selected conversation");
        assert_eq!(selected.subject, "Selected detail");
        assert_eq!(selected.messages[0].preview, "hydrated message");
        assert_eq!(selected.drafts[0].preview, "draft");
        assert_eq!(selected.posts[0].preview, "post");
        assert_eq!(selected.notes[0].preview, "note");
        assert!(users[0].is_me);
        assert!(conversations
            .iter()
            .find(|conversation| conversation.id == "first")
            .is_some_and(|conversation| conversation.messages.is_empty()));
    }

    #[test]
    fn pane_conversation_missing_from_list_is_fetched_and_marked_bound() {
        let dir = fixture_dir("missive-pane-fallback");
        let curl = dir.join("curl");
        let log = dir.join("curl-argv.log");
        write_executable(
            &curl,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
for argument do url="$argument"; done
case "$url" in
  *"/users") printf '%s' '{{"users":[{{"id":"me","name":"Ada","me":true}}]}}' ;;
  *"/conversations?team_all=team") printf '%s' '{{"conversations":[]}}' ;;
  *"/conversations/pane-only/messages") printf '%s' '{{"messages":[]}}' ;;
  *"/conversations/pane-only/drafts") printf '%s' '{{"drafts":[]}}' ;;
  *"/conversations/pane-only/posts") printf '%s' '{{"posts":[]}}' ;;
  *"/conversations/pane-only/comments") printf '%s' '{{"comments":[]}}' ;;
  *"/conversations/pane-only") printf '%s' '{{"conversations":[{{"id":"pane-only","subject":"Pane conversation","web_url":"https://mail.missiveapp.com/#inbox/conversations/pane-only"}}]}}' ;;
  *) exit 65 ;;
esac
"#,
                log.display()
            ),
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: None,
        };
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/pane-only".into()],
            ..Default::default()
        });

        let (conversations, _) = fetch_missive_snapshot(
            &config,
            &panes,
            None,
            None,
            &curl,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
        )
        .expect("Missive pane fallback");

        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].subject, "Pane conversation");
        assert!(conversations[0].pane_bound);
        let argv = std::fs::read_to_string(log).expect("curl argv");
        assert!(argv.contains("/conversations/pane-only"));
    }

    #[test]
    fn retained_missive_conversations_recompute_pane_binding() {
        let previous = Snapshot {
            items: Vec::new(),
            conversations: vec![MissiveConversation {
                id: "previous".into(),
                subject: "Previous".into(),
                app_url: "missive://previous".into(),
                web_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                pane_bound: true,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            }],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        assert!(!previous_missive(Some(&previous), &[])[0].pane_bound);
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/previous".into()],
            ..Default::default()
        });
        assert!(previous_missive(Some(&previous), &panes)[0].pane_bound);
    }

    #[test]
    fn missive_session_users_are_not_persisted() {
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: vec![MissiveUser {
                id: "user-1".into(),
                name: "Ada".into(),
                email: None,
                is_me: true,
            }],
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let persisted = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert!(persisted.get("missive_users").is_none());
    }

    #[test]
    fn missive_users_are_reused_after_the_session_identity_is_resolved() {
        let dir = fixture_dir("missive-session-users");
        let curl = dir.join("curl");
        write_executable(
            &curl,
            r#"#!/bin/sh
for argument do url="$argument"; done
case "$url" in
  *"/users"*) exit 70 ;;
  *"/conversations?team_all=team") printf '%s' '{"conversations":[]}' ;;
  *) exit 71 ;;
esac
"#,
        );
        let token_env = format!(
            "HERDR_TEST_MISSIVE_TOKEN_{}",
            crate::config::test_unique_suffix().replace('-', "_")
        );
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        env.set(&token_env, "test-token");
        let config = MissiveConfig {
            token_env,
            team: Some("team".into()),
            organization: Some("org".into()),
        };
        let users = vec![MissiveUser {
            id: "me".into(),
            name: "Ada".into(),
            email: None,
            is_me: true,
        }];

        let (_, reused) = fetch_missive_snapshot(
            &config,
            &[],
            None,
            Some(&users),
            &curl,
            Instant::now() + Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .expect("cached session identity should avoid /users");

        assert_eq!(reused, users);
    }

    #[test]
    fn missive_curl_program_can_be_overridden_without_changing_global_path() {
        let mut app = test_app_with_work_index();
        app.work_index_curl_program_override = Some(Path::new("/tmp/fake-missive-curl").into());
        assert_eq!(
            app.work_index_curl_program(),
            Path::new("/tmp/fake-missive-curl")
        );
    }
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn work_item_key(number: u64) -> crate::app::state::WorkItemKey {
        crate::app::state::WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/owner/repo/pull/{number}")),
            ticket_id: None,
        }
    }

    fn unavailable_detail(observed_at: SystemTime) -> WorkItemDetail {
        let mut detail = WorkItemDetail::unavailable("not available");
        detail.observed_at = observed_at;
        detail
    }

    fn test_app_with_work_index() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.work_index.enabled = true;
        config.work_index.repos = vec!["owner/repo".into()];
        let mut app =
            crate::app::App::new(&config, true, None, api_rx, crate::api::EventHub::default());
        app.state.work_index_enabled = true;
        app
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write fake executable");
        let mut permissions = std::fs::metadata(path)
            .expect("read fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake executable");
    }

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-work-index-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create fixture directory");
        path
    }

    fn config() -> WorkIndexConfig {
        WorkIndexConfig {
            enabled: true,
            refresh_interval_seconds: 300,
            linear_team: Some("SCA".into()),
            repos: vec!["owner/repo".into()],
        }
    }

    fn panes_with_context(context: crate::work_context::PaneWorkContext) -> Vec<AgentInfo> {
        vec![AgentInfo {
            terminal_id: "terminal".into(),
            work_context: context,
            name: Some("fixture".into()),
            agent: Some("codex".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: Some("cx".into()),
            agent_status: AgentStatus::Idle,
            wait: None,
            eta_s: None,
            reported_at: None,
            screen_detection_skipped: false,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            gates: Vec::new(),
            items: Vec::new(),
            decisions: Vec::new(),
            agent_session: None,
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            pane_id: "pane".into(),
            focused: true,
            launch_pending: false,
            interactive_ready: true,
            state_change_seq: 0,
            cwd: None,
            foreground_cwd: None,
            revision: 0,
        }]
    }

    fn fake_programs(
        dir: &Path,
        gh: &str,
        linearis: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let gh_path = dir.join("gh");
        let linearis_path = dir.join("linearis");
        write_executable(&gh_path, gh);
        write_executable(&linearis_path, linearis);
        (gh_path, linearis_path)
    }

    #[test]
    fn pane_declared_objects_are_fetched_with_empty_repo_config() {
        let dir = fixture_dir("pane-fallback");
        let gh_log = dir.join("gh-argv.log");
        let linear_log = dir.join("linear-argv.log");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "pr list --repo pane/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[]' ;;
  "pr list --repo pane/repo --state open"*) printf '%s' '[]' ;;
  "pr view 77 --repo pane/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":77,"title":"Merged pane PR","state":"MERGED","headRefName":"issue/out-9","url":"https://github.com/pane/repo/pull/77"}}' ;;
  *) exit 42 ;;
esac
"#,
                gh_log.display()
            ),
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "issues list"*) printf '%s' '{{"nodes":[]}}' ;;
  "cycles list"*) printf '%s' '{{"nodes":[]}}' ;;
  "issues read SCA-9999") printf '%s' '{{"identifier":"SCA-9999","title":"Outside filter","description":"full summary","state":{{"name":"Canceled"}},"assignee":{{"name":"other"}},"priority":1,"branchName":"issue/sca-9999"}}' ;;
  "attachments list SCA-9999"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#,
                linear_log.display()
            ),
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("pane/repo".into()),
            pr_urls: vec!["https://github.com/pane/repo/pull/77".into()],
            ticket_ids: vec!["SCA-9999".into()],
            ..Default::default()
        });
        let mut config = config();
        config.repos.clear();

        let snapshot = refresh_work_index(
            &config,
            &panes,
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        let pull_request = snapshot
            .items
            .iter()
            .find(|item| item.pr_url.as_deref() == Some("https://github.com/pane/repo/pull/77"))
            .expect("pane pull request");
        assert_eq!(pull_request.pr_title.as_deref(), Some("Merged pane PR"));
        assert_eq!(pull_request.pr_state.as_deref(), Some("merged"));
        assert!(pull_request.source.github && pull_request.source.pane);
        let ticket = snapshot
            .items
            .iter()
            .flat_map(|item| item.ticket_details.iter())
            .find(|ticket| ticket.identifier == "SCA-9999")
            .expect("pane ticket");
        assert_eq!(ticket.title.as_deref(), Some("Outside filter"));
        assert_eq!(ticket.description.as_deref(), Some("full summary"));
        assert!(snapshot.items.iter().any(|item| {
            item.source.linear && item.source.pane && item.ticket_ids == ["SCA-9999"]
        }));
        let gh_argv = std::fs::read_to_string(gh_log).expect("GitHub argv");
        assert!(gh_argv.contains("pr list --repo pane/repo --state open"));
        assert!(gh_argv.contains(&format!(
            "pr view 77 --repo pane/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}"
        )));
        let linear_argv = std::fs::read_to_string(linear_log).expect("Linear argv");
        assert!(linear_argv
            .lines()
            .any(|line| line == "issues read SCA-9999"));
    }

    #[test]
    fn observation_budgets_cover_real_provider_latency() {
        assert!(WORK_INDEX_TARGET_TIMEOUT >= Duration::from_secs(20));
        assert!(WORK_INDEX_BATCH_TIMEOUT >= Duration::from_secs(60));
    }

    #[test]
    fn pane_pull_request_finishes_before_slow_list_and_previous_list_items_survive() {
        let dir = fixture_dir("pane-before-slow-list");
        let argv_log = dir.join("gh-argv.log");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "pr view 159 --repo owner/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":159,"title":"Pane first","state":"MERGED","headRefName":"t3/f15","url":"https://github.com/owner/repo/pull/159"}}' ;;
  "pr list --repo owner/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") exec sleep 1 ;;
  *) exit 42 ;;
esac
"#,
                argv_log.display()
            ),
            "#!/bin/sh\ncase \"$*\" in *\"issues list\"*) printf '%s' '{\"nodes\":[]}' ;; *\"cycles list\"*) printf '%s' '{\"nodes\":[]}' ;; *) printf '%s' '[]' ;; esac\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("owner/repo".into()),
            pr_urls: vec!["https://github.com/owner/repo/pull/159".into()],
            ..Default::default()
        });
        let previous = Snapshot {
            items: vec![WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(7),
                pr_url: Some("https://github.com/owner/repo/pull/7".into()),
                pr_title: Some("Previous list item".into()),
                pr_state: Some("open".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: PrCheckState::Unknown,
                audience: PrAudience::Unclassified,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: Some("previous".into()),
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: WorkItemSource {
                    github: true,
                    ..WorkItemSource::default()
                },
            }],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        let snapshot = refresh_work_index_with_missive(
            &config(),
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                previous: Some(&previous),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + Duration::from_secs(5),
            Duration::from_millis(100),
            &gh,
            &linearis,
            Path::new("/usr/bin/false"),
        );

        assert!(snapshot.items.iter().any(|item| {
            item.pr_number == Some(159)
                && item.pr_title.as_deref() == Some("Pane first")
                && item.source.pane
        }));
        assert!(snapshot.items.iter().any(|item| item.pr_number == Some(7)));
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Github),
            Some("list observation timed out")
        );
        let argv = std::fs::read_to_string(argv_log).expect("GitHub argv");
        let mut calls = argv.lines();
        assert!(calls
            .next()
            .is_some_and(|call| call.starts_with("pr view 159 --repo owner/repo")));
        assert!(calls
            .next()
            .is_some_and(|call| call.starts_with("pr list --repo owner/repo")));
    }

    #[test]
    fn repo_union_deduplicates_config_pane_and_pull_request_repositories() {
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            repo: Some("owner/repo".into()),
            pr_urls: vec![
                "https://github.com/OWNER/repo/pull/1".into(),
                "https://github.com/pane/other/pull/2".into(),
            ],
            ..Default::default()
        });
        let mut config = config();
        config.repos = vec!["owner/repo".into(), "configured/only".into()];

        assert_eq!(
            work_index_repos(&config, &panes),
            ["owner/repo", "configured/only", "pane/other"]
        );
    }

    #[test]
    fn failed_repository_does_not_discard_a_successful_pane_pull_request() {
        let dir = fixture_dir("partial-repo-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr view 77 --repo good/repo --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '{{"number":77,"title":"Fresh pane PR","state":"MERGED","headRefName":"issue/77","url":"https://github.com/good/repo/pull/77"}}' ;;
  "pr list --repo good/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[]' ;;
  "pr list --repo bad/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' 'rate limited' >&2; exit 42 ;;
  *"--state open"*) printf '%s' '[]' ;;
  *) exit 43 ;;
esac
"#
            ),
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            pr_urls: vec!["https://github.com/good/repo/pull/77".into()],
            ..Default::default()
        });
        let mut config = config();
        config.repos = vec!["good/repo".into(), "bad/repo".into()];

        let snapshot = refresh_work_index(
            &config,
            &panes,
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        assert!(snapshot.items.iter().any(|item| {
            item.pr_number == Some(77)
                && item.pr_title.as_deref() == Some("Fresh pane PR")
                && item.source.github
                && item.source.pane
        }));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("rate limited")));
    }

    #[test]
    fn github_audience_failure_is_named_without_discarding_pull_requests() {
        let dir = fixture_dir("github-audience-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
case "$*" in
  "pr list --repo owner/repo --state open --limit 200 --json {GITHUB_PULL_REQUEST_SUMMARY_FIELDS}") printf '%s' '[{{"number":7,"title":"Visible PR","state":"OPEN","headRefName":"issue/7","url":"https://github.com/owner/repo/pull/7"}}]' ;;
  *"--state open"*) printf '%s' 'audience unavailable' >&2; exit 42 ;;
  *) exit 43 ;;
esac
"#
            ),
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );

        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        assert!(snapshot.items.iter().any(|item| item.pr_number == Some(7)));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("audience unavailable")));
    }

    #[test]
    fn parses_normal_rfc3339_timestamp() {
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T11:22:33Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_788_088_953))
        );
    }

    #[test]
    fn parses_leap_year_rfc3339_timestamp() {
        assert_eq!(
            parse_rfc3339_system_time("2024-02-29T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_164_800))
        );
    }

    #[test]
    fn parses_fractional_and_offset_rfc3339_timestamps() {
        let expected = parse_rfc3339_system_time("2026-08-30T11:22:33Z");
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T13:22:33.123+02:00"),
            expected
        );
        assert_eq!(
            parse_rfc3339_system_time("2026-08-30T09:22:33-02:00"),
            expected
        );
    }

    #[test]
    fn malformed_rfc3339_timestamp_is_unknown() {
        assert_eq!(parse_rfc3339_system_time("2023-02-29T00:00:00Z"), None);
    }

    #[test]
    fn older_work_item_json_defaults_created_at_to_unknown() {
        let item: WorkItem = serde_json::from_str(
            r#"{"repo":"owner/repo","pr_number":7,"pr_url":null,"pr_title":null,"pr_state":null,"draft":false,"review_decision":null,"ticket_ids":[],"ticket_title":null,"ticket_state":null,"branch":null,"preview_urls":[],"panes":[],"source":{"github":true,"linear":false,"pane":false}}"#,
        )
        .expect("older work item JSON");

        assert_eq!(item.created_at, None);
    }

    #[test]
    fn work_index_session_resolves_me_once_with_injected_programs() {
        let dir = fixture_dir("session-identities");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  "api user") printf '%s' '{"login":"matthias"}' ;;
  "api repos/owner/repo/assignees --paginate --slurp") printf '%s' '[[{"login":"grace"},{"login":"matthias"}]]' ;;
  *) exit 42 ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  "auth status --compact") printf '%s' '{"authenticated":true,"user":{"name":"Matthias"}}' ;;
  "users list --active -l 250 --compact") printf '%s' '{"nodes":[{"name":"Ada"},{"name":"Matthias"}]}' ;;
  *) exit 42 ;;
esac
"#,
        );
        let deadline = Instant::now() + WORK_INDEX_BATCH_TIMEOUT;
        let missive_users = [MissiveUser {
            id: "missive-1".into(),
            name: "Mina".into(),
            email: None,
            is_me: true,
        }];
        let session = resolve_work_index_session(
            &config(),
            WorkIndexSession::default(),
            &missive_users,
            deadline,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );

        assert_eq!(session.linear.viewer.as_deref(), Some("Matthias"));
        assert_eq!(session.linear.assignees, ["Ada", "Matthias"]);
        assert_eq!(session.github.viewer.as_deref(), Some("matthias"));
        assert_eq!(session.github.assignees, ["grace", "matthias"]);
        assert_eq!(session.missive.viewer.as_deref(), Some("Mina"));
        assert_eq!(session.missive.assignees, ["Mina"]);

        let unchanged = resolve_work_index_session(
            &config(),
            session.clone(),
            &[],
            deadline,
            WORK_INDEX_TARGET_TIMEOUT,
            Path::new("/usr/bin/false"),
            Path::new("/usr/bin/false"),
        );
        assert_eq!(
            unchanged, session,
            "resolved providers are not queried twice"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn github_pull_request_list_uses_lean_open_query() {
        let dir = fixture_dir("github-created-at");
        let (gh, _linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
test "$*" = "pr list --repo owner/repo --state open --limit 200 --json number,title,author,assignees,state,updatedAt,createdAt,reviewDecision,labels,isDraft,headRefName,url" || exit 42
printf '%s' '[{"number":7,"title":"PR","author":{"login":"ada"},"assignees":[{"login":"grace"}],"state":"OPEN","headRefName":"branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7","createdAt":"2026-08-30T11:22:33Z","updatedAt":"2026-08-31T11:22:33Z","labels":[{"name":"bug"}]}]'
"#,
            "#!/bin/sh\nprintf '%s' '[]'\n",
        );

        let pull_requests = fetch_github_pull_requests(
            "owner/repo",
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .unwrap_or_else(|_| panic!("GitHub pull request fetch failed"));

        assert_eq!(pull_requests.len(), 1);
        assert_eq!(
            pull_requests[0].created_at,
            parse_rfc3339_system_time("2026-08-30T11:22:33Z")
        );
        assert_eq!(pull_requests[0].author.as_deref(), Some("ada"));
        assert_eq!(pull_requests[0].assignees, vec!["grace"]);
        assert_eq!(pull_requests[0].state, "open");
        assert_eq!(
            (pull_requests[0].additions, pull_requests[0].deletions),
            (0, 0)
        );
        assert_eq!(pull_requests[0].labels, vec!["bug"]);
        assert_eq!(pull_requests[0].check_state, PrCheckState::Unknown);
    }

    #[test]
    fn github_pull_request_detail_fetch_requests_every_render_field() {
        let dir = fixture_dir("github-detail-fields");
        let (gh, _linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
test "$*" = "pr view 7 --repo owner/repo --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}" || exit 42
printf '%s' '{{"number":7,"title":"Detail","body":"Body","author":{{"login":"ms"}},"baseRefName":"main","headRefName":"feat/detail","headRefOid":"abcdef012345","createdAt":"2026-08-30T11:22:33Z","updatedAt":"2026-08-30T12:22:33Z","labels":[{{"name":"high-risk"}}],"url":"https://github.com/owner/repo/pull/7","reviewDecision":"REVIEW_REQUIRED","isDraft":false,"statusCheckRollup":[{{"name":"test","conclusion":"FAILURE"}},{{"name":"lint","conclusion":"SUCCESS"}}],"reviews":[{{"author":{{"login":"grace"}}}}],"comments":[{{"author":{{"login":"reviewer"}},"body":"Looks good"}}],"files":[{{"path":"src/lib.rs","additions":4,"deletions":2}}],"commits":[{{"oid":"abcdef012345","messageHeadline":"fix detail"}}],"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN"}}'
"#
            ),
            "#!/bin/sh\nprintf '%s' '[]'\n",
        );

        let detail = fetch_github_pull_request_detail(
            "owner/repo",
            7,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .unwrap_or_else(|_| panic!("GitHub pull request detail fetch failed"));

        assert_eq!(detail.title.as_deref(), Some("Detail"));
        assert_eq!(detail.author.as_deref(), Some("ms"));
        assert_eq!(detail.reviewers, vec!["grace"]);
        assert_eq!(detail.head_sha.as_deref(), Some("abcdef012345"));
        assert_eq!(detail.merge_state_status.as_deref(), Some("CLEAN"));
        assert_eq!(detail.labels, vec!["high-risk"]);
        assert_eq!(detail.review_decision.as_deref(), Some("REVIEW_REQUIRED"));
        assert_eq!(
            detail.checks,
            Some(WorkItemCheckSummary {
                failing: 1,
                total: 2
            })
        );
        assert_eq!(detail.comments[0].author.as_deref(), Some("reviewer"));
        assert_eq!(detail.comments[0].body, "Looks good");
        assert_eq!(detail.actions[0].name, "test");
        assert_eq!(detail.actions[0].state, "FAILURE");
        assert_eq!(detail.files[0].path, "src/lib.rs");
        assert_eq!(
            (detail.files[0].additions, detail.files[0].deletions),
            (4, 2)
        );
        assert_eq!(detail.commits[0].short_id, "abcdef0");
        assert_eq!(detail.commits[0].subject, "fix detail");
    }

    #[test]
    fn github_pull_request_detail_maps_the_real_cli_shape() {
        let dir = fixture_dir("github-detail-real-shape");
        let fixture = include_str!("../tests/fixtures/work-index/github-pr-view.json");
        let script = format!(
            "#!/bin/sh\ntest \"$*\" = \"pr view 125 --repo example/project --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}\" || exit 42\nprintf '%s' '{fixture}'\n"
        );
        let (gh, _linearis) = fake_programs(&dir, &script, "#!/bin/sh\nprintf '%s' '[]'\n");

        let detail = fetch_github_pull_request_detail(
            "example/project",
            125,
            &gh,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .expect("GitHub pull request detail from captured CLI fixture");

        assert_eq!(detail.number, Some(125));
        assert_eq!(
            detail.title.as_deref(),
            Some("Render pull request details from the CLI response")
        );
        assert_eq!(detail.author.as_deref(), Some("example-author"));
        assert_eq!(detail.base_ref_name.as_deref(), Some("main"));
        assert_eq!(detail.head_ref_name.as_deref(), Some("fix/pr-detail"));
        assert_eq!(detail.reviewers, vec!["example-reviewer"]);
        assert_eq!(detail.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(detail.actions.len(), 2);
        assert!(detail
            .actions
            .iter()
            .all(|action| action.state == "SUCCESS"));
        assert_eq!(
            detail.checks,
            Some(WorkItemCheckSummary {
                failing: 0,
                total: 2,
            })
        );
        assert_eq!(detail.comments.len(), 1);
        assert_eq!(detail.files.len(), 1);
        assert_eq!(detail.commits.len(), 1);

        let value: Value = serde_json::from_str(fixture).expect("captured GitHub fixture JSON");
        assert_eq!(
            pr_check_state(value.get("statusCheckRollup")),
            PrCheckState::Passing
        );
    }

    #[test]
    fn linear_ticket_fetch_includes_inverse_relations() {
        let dir = fixture_dir("linear-inverse-relations");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
printf '%s' '{"nodes":[{"identifier":"SCA-7","relations":{"nodes":[{"type":"blocks","relatedIssue":{"identifier":"SCA-8","title":"outbound"}}]},"inverseRelations":{"nodes":[{"type":"blocks","issue":{"identifier":"SCA-6","title":"inbound"}},{"type":"duplicate","issue":{"identifier":"SCA-5","title":"original"}}]}}]}'
"#,
        );

        let tickets =
            fetch_linear_tickets("SCA", &linearis, Instant::now() + WORK_INDEX_TARGET_TIMEOUT)
                .expect("Linear ticket fetch");

        assert_eq!(
            tickets[0].relations,
            vec![
                "blocks  SCA-8  outbound",
                "blocked by  SCA-6  inbound",
                "duplicated by  SCA-5  original"
            ]
        );
    }

    #[test]
    fn linear_ticket_sets_use_assignee_triage_and_active_cycle_queries() {
        let dir = fixture_dir("linear-ticket-sets");
        let log = dir.join("argv.log");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  "issues list --team SCA --assignee me -l 100 --compact") printf '%s' '{{"nodes":[{{"identifier":"SCA-1","title":"assigned","state":{{"name":"In Progress"}},"priority":2}}]}}' ;;
  "issues list --team SCA --status Triage -l 100 --compact") printf '%s' '{{"nodes":[{{"identifier":"SCA-2","title":"triage","state":{{"name":"Triage"}},"priority":3}}]}}' ;;
  "cycles list --team SCA --active --compact") printf '%s' '{{"nodes":[{{"name":"cycle 34"}}]}}' ;;
  "issues list --team SCA --cycle cycle 34 --status Done -l 100 --compact") printf '%s' '{{"nodes":[{{"identifier":"SCA-3","title":"done","state":{{"name":"Done"}},"priority":4,"cycle":{{"name":"cycle 34"}}}}]}}' ;;
  *) exit 42 ;;
esac
"#,
                log.display()
            ),
        );

        let tickets =
            fetch_linear_tickets("SCA", &linearis, Instant::now() + WORK_INDEX_TARGET_TIMEOUT)
                .expect("Linear ticket sets");

        assert_eq!(tickets.len(), 3);
        assert_eq!(tickets[0].group, TicketGroup::Assigned);
        assert_eq!(tickets[1].group, TicketGroup::Triage);
        assert_eq!(tickets[2].group, TicketGroup::DoneThisCycle);
        assert_eq!(tickets[0].priority, Some(2));
        assert_eq!(tickets[2].cycle.as_deref(), Some("cycle 34"));
        let argv = std::fs::read_to_string(log).expect("read argv log");
        assert!(argv.contains("--assignee me"));
        assert!(argv.contains("--status Triage"));
        assert!(argv.contains("--cycle cycle 34 --status Done"));
    }

    #[test]
    fn linear_detail_prefers_get_and_projects_comments() {
        let dir = fixture_dir("linear-detail-get");
        let (_gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
test "$*" = "issues get SCA-7" || exit 42
printf '%s' '{"title":"ticket","description":"- [ ] ship","url":"https://linear.app/acme/issue/SCA-7","comments":{"nodes":[{"body":"newest","createdAt":"2026-09-01T10:00:00Z","user":{"name":"Ada"}}]}}'
"#,
        );

        let detail = fetch_linear_ticket_detail(
            "SCA-7",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .expect("Linear ticket detail");
        assert_eq!(detail.title.as_deref(), Some("ticket"));
        assert_eq!(detail.body.as_deref(), Some("- [ ] ship"));
        assert_eq!(detail.comments[0].author.as_deref(), Some("Ada"));
    }

    #[test]
    fn linear_detail_maps_the_real_cli_shape() {
        let dir = fixture_dir("linear-detail-real-shape");
        let fixture = include_str!("../tests/fixtures/work-index/linear-issue-read.json");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  \"issues get SCA-3165\") exit 42 ;;\n  \"issues read SCA-3165 --with-comment-threads --compact\") printf '%s' '{fixture}' ;;\n  *) exit 43 ;;\nesac\n"
        );
        let (_gh, linearis) = fake_programs(&dir, "#!/bin/sh\nprintf '%s' '[]'\n", &script);

        let detail = fetch_linear_ticket_detail(
            "SCA-3165",
            &linearis,
            Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        )
        .expect("Linear ticket detail from captured CLI fixture");

        assert_eq!(
            detail.title.as_deref(),
            Some("Render ticket details from the CLI response")
        );
        assert!(detail
            .body
            .as_deref()
            .is_some_and(|body| body.contains("Shows the full ticket body")));
        assert_eq!(
            detail.url.as_deref(),
            Some("https://linear.app/scalable/issue/SCA-3165")
        );
        assert!(detail.created_at.is_some());
        assert!(detail.updated_at.is_some());
        assert_eq!(detail.comments.len(), 1);
    }

    #[test]
    fn lean_pull_request_summaries_join_tickets_from_branch_only() {
        let dir = fixture_dir("ticket-pr-join");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  *"--author @me"*|*"review-requested:@me"*) printf '%s' '[]' ;;
  *) printf '%s' '[{"number":7,"title":"branch match","headRefName":"issue/sca-7-fix","url":"https://github.com/owner/repo/pull/7"},{"number":8,"title":"body unavailable in list","headRefName":"plain","url":"https://github.com/owner/repo/pull/8"}]' ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-7","title":"ticket","state":{"name":"In Progress"}}]}' ;;
  *"cycles list"*) printf '%s' '{"nodes":[]}' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let linked = snapshot
            .items
            .iter()
            .filter(|item| item.pr_number.is_some())
            .collect::<Vec<_>>();
        assert_eq!(linked.len(), 2);
        assert_eq!(linked[0].ticket_ids, ["SCA-7"]);
        assert!(linked[1].ticket_ids.is_empty());
    }

    #[test]
    fn status_rollup_distinguishes_absent_empty_and_all_passing() {
        assert_eq!(status_check_summary(None), None);
        assert_eq!(status_check_summary(Some(&serde_json::json!([]))), None);
        assert_eq!(
            status_check_summary(Some(&serde_json::json!([
                {"conclusion": "SUCCESS"},
                {"conclusion": "NEUTRAL"},
                {"conclusion": null}
            ]))),
            Some(WorkItemCheckSummary {
                failing: 0,
                total: 3
            })
        );
        assert_eq!(
            status_check_summary(Some(&serde_json::json!([
                {"conclusion": "FAILURE"},
                {"conclusion": "TIMED_OUT"},
                {"conclusion": "CANCELLED"},
                {"conclusion": "ACTION_REQUIRED"},
                {"conclusion": "SUCCESS"}
            ]))),
            Some(WorkItemCheckSummary {
                failing: 4,
                total: 5
            })
        );
    }

    #[test]
    fn work_item_detail_cache_is_keyed_and_bounded() {
        let mut cache = WorkItemDetailCache::default();
        for number in 1..=17 {
            cache.insert(
                work_item_key(number),
                unavailable_detail(SystemTime::UNIX_EPOCH),
            );
        }

        assert_eq!(cache.len(), WORK_ITEM_DETAIL_CACHE_CAPACITY);
        assert!(cache.get(&work_item_key(1)).is_none());
        assert!(cache.get(&work_item_key(2)).is_some());
        assert!(cache.get(&work_item_key(17)).is_some());
    }

    #[test]
    fn work_item_detail_cache_freshness_uses_monotonic_time() {
        let mut cache = WorkItemDetailCache::default();
        let key = work_item_key(7);
        let refreshed_at = Instant::now();
        let wall_clock_in_future = SystemTime::now() + Duration::from_secs(3_600);
        cache.insert_at(
            key.clone(),
            unavailable_detail(wall_clock_in_future),
            refreshed_at,
        );

        assert!(cache.is_fresh(
            &key,
            refreshed_at + Duration::from_secs(59),
            Duration::from_secs(60),
        ));
        assert!(!cache.is_fresh(
            &key,
            refreshed_at + Duration::from_secs(60),
            Duration::from_secs(60),
        ));
    }

    #[test]
    fn stale_work_item_detail_generation_is_rejected() {
        let mut app = test_app_with_work_index();
        let current_key = work_item_key(8);
        app.last_work_item_detail_refresh_generation = 2;
        app.last_applied_work_item_detail_refresh_generation = 0;
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![current_key.clone()],
            generation: 2,
            deadline: Instant::now() + WORK_INDEX_TARGET_TIMEOUT,
        });
        app.state
            .work_item_detail_loading
            .insert(current_key.clone());

        assert!(!app.handle_work_item_detail_refreshed(
            1,
            vec![(work_item_key(7), unavailable_detail(SystemTime::UNIX_EPOCH),)],
        ));
        assert!(app
            .state
            .work_item_detail_cache
            .get(&work_item_key(7))
            .is_none());
        assert!(app.state.work_item_detail_loading.contains(&current_key));
    }

    #[test]
    fn work_index_refresh_invalidates_detail_cache_and_in_flight_generation() {
        let mut app = test_app_with_work_index();
        let key = work_item_key(8);
        app.state
            .work_item_detail_cache
            .insert(key.clone(), unavailable_detail(SystemTime::UNIX_EPOCH));
        app.state.work_item_detail_loading.insert(key.clone());
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![key.clone()],
            generation: 4,
            deadline: Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
        });
        app.last_work_item_detail_refresh_generation = 4;
        app.invalidate_work_item_details();
        assert!(app.work_item_detail_refresh_in_flight.is_none());
        assert!(!app.state.work_item_detail_loading.contains(&key));
        assert!(app.state.work_item_detail_cache.get(&key).is_none());
        assert_eq!(app.last_work_item_detail_refresh_generation, 5);
        assert!(!app.handle_work_item_detail_refreshed(
            4,
            vec![(key, unavailable_detail(SystemTime::UNIX_EPOCH))],
        ));
    }

    #[test]
    fn one_bounded_batch_prefetches_the_active_pr_section_selected_first() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("first"),
            crate::workspace::Workspace::test_new("second"),
        ];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in [7_u64, 8].into_iter().enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }
        let first_key = app.state.dock_home_selected_row().expect("first row").key;
        app.state.dock_home_selection = Some(first_key);
        app.state.move_dock_home_selection(1);
        let key = app
            .state
            .dock_home_selected_row()
            .expect("newly selected row")
            .key;
        assert_eq!(key.pr_number, Some(8));
        let now = Instant::now();

        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );
        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            true,
        );

        assert_eq!(app.last_work_item_detail_refresh_generation, 1);
        let refresh = app
            .work_item_detail_refresh_in_flight
            .as_ref()
            .expect("detail batch");
        assert_eq!(refresh.generation, 1, "second call must not start a batch");
        assert_eq!(refresh.keys.len(), 2);
        assert_eq!(refresh.keys.first(), Some(&key));
        assert!(refresh
            .keys
            .iter()
            .all(|key| app.state.work_item_detail_loading.contains(key)));
    }

    #[test]
    fn detail_prefetch_batch_is_capped_at_cache_capacity() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = (1_u64..=20)
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in (1_u64..=20).enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Prs,
            None,
            true,
        );

        assert_eq!(
            app.work_item_detail_refresh_in_flight
                .as_ref()
                .expect("detail batch")
                .keys
                .len(),
            WORK_ITEM_DETAIL_CACHE_CAPACITY
        );
    }

    #[test]
    fn selected_ticket_outside_dock_projection_still_refreshes_detail() {
        let mut app = test_app_with_work_index();
        app.work_index_linearis_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        let key = crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-3165".into()),
        };

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Tickets,
            Some(key.clone()),
            true,
        );

        let refresh = app
            .work_item_detail_refresh_in_flight
            .as_ref()
            .expect("selected ticket detail batch");
        assert_eq!(refresh.keys, [key]);
    }

    #[test]
    fn detail_prefetch_does_not_rotate_beyond_the_cache_capacity() {
        let mut app = test_app_with_work_index();
        app.work_index_gh_program_override = Some(Path::new("/usr/bin/false").to_path_buf());
        app.state.workspaces = (1_u64..=20)
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for (workspace_index, number) in (1_u64..=20).enumerate() {
            let pane_id = app.state.workspaces[workspace_index].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[workspace_index]
                .terminal_id(pane_id)
                .expect("terminal")
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("work context");
        }
        let keys = app
            .state
            .dock_home_keys_for_section(crate::app::state::DockHomeSection::Prs);
        for key in keys.iter().take(WORK_ITEM_DETAIL_CACHE_CAPACITY) {
            app.state
                .work_item_detail_cache
                .insert(key.clone(), unavailable_detail(SystemTime::now()));
        }
        let now = Instant::now();

        app.start_work_item_detail_refresh_if_due(
            now,
            crate::app::state::DockHomeSection::Prs,
            None,
            true,
        );

        assert!(app.work_item_detail_refresh_in_flight.is_none());
    }

    #[test]
    fn expired_detail_batch_is_cleared_while_detail_is_hidden() {
        let mut app = test_app_with_work_index();
        let key = work_item_key(8);
        app.state.work_item_detail_loading.insert(key.clone());
        app.work_item_detail_refresh_in_flight = Some(WorkItemDetailRefreshInFlight {
            keys: vec![key.clone()],
            generation: 1,
            deadline: Instant::now() - Duration::from_secs(1),
        });

        app.start_work_item_detail_refresh_if_due(
            Instant::now(),
            crate::app::state::DockHomeSection::Prs,
            Some(key.clone()),
            false,
        );

        assert!(app.work_item_detail_refresh_in_flight.is_none());
        assert!(!app.state.work_item_detail_loading.contains(&key));
    }

    #[test]
    fn attachment_join_keeps_orphan_buckets_and_ignores_branch_names() {
        let dir = fixture_dir("join");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
printf '%s' '[{"number":7,"title":"Attached PR","headRefName":"plain-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7","createdAt":"2026-08-30T11:22:33Z"},{"number":8,"title":"No ticket PR","headRefName":"other-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/8"}]'
"#,
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Attached ticket","state":"In Progress","branchName":"plain-branch"},{"identifier":"SCA-3","title":"No PR ticket","state":"In Review","branchName":"SCA-3"}]}' ;;
  *"attachments list SCA-2"*) printf '%s' '[{"url":"https://github.com/owner/repo/pull/7","metadata":{"number":7,"repoName":"repo","repoLogin":"owner","status":"open","draft":false,"branch":"plain-branch","previewLinks":[]}}]' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let joined = snapshot
            .items
            .iter()
            .find(|item| item.pr_number == Some(7))
            .expect("joined PR");
        assert_eq!(joined.ticket_ids, vec!["SCA-2"]);
        assert_eq!(joined.branch.as_deref(), Some("plain-branch"));
        assert_eq!(
            joined.created_at,
            parse_rfc3339_system_time("2026-08-30T11:22:33Z")
        );
        assert!(snapshot
            .items
            .iter()
            .any(|item| item.ticket_ids == ["SCA-3"]));
        assert!(snapshot
            .items
            .iter()
            .any(|item| item.pr_number == Some(8) && item.ticket_ids.is_empty()));
    }

    #[test]
    fn attachment_only_pull_request_keeps_its_title() {
        // A PR outside the configured repo allowlist is never enumerated by
        // `gh`, so its title can only come from the attachment payload.
        let dir = fixture_dir("attachment-title");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            r#"#!/bin/sh
case "$*" in
  *"issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-9","title":"Cross repo ticket","state":"In Progress","branchName":"sca-9"}]}' ;;
  *"attachments list SCA-9"*) printf '%s' '[{"url":"https://github.com/owner/other/pull/42","metadata":{"number":42,"title":"Titled from attachment","repoName":"other","repoLogin":"owner","status":"merged","draft":false,"branch":"sca-9","previewLinks":[]}}]' ;;
  *) printf '%s' '[]' ;;
esac
"#,
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        let item = snapshot
            .items
            .iter()
            .find(|item| item.pr_number == Some(42))
            .expect("attachment-only PR");
        assert_eq!(item.pr_title.as_deref(), Some("Titled from attachment"));
        assert_eq!(item.pr_state.as_deref(), Some("merged"));
        assert_eq!(item.ticket_ids, vec!["SCA-9"]);
    }

    #[test]
    fn linear_failure_keeps_the_github_half() {
        // A dead Linear half must degrade, not erase: the pull requests are
        // still real work and still worth showing.
        let dir = fixture_dir("linear-down");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
printf '%s' '[{"number":7,"title":"Live PR","headRefName":"b","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7"}]'
"#,
            "#!/bin/sh\nprintf '%s' '{\n  \"error\": \"No API token found.\"\n}' >&2\nexit 1\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].pr_number, Some(7));
        let unavailable = snapshot.unavailable.expect("degraded message");
        let unavailable = unavailable.summary();
        // Collapsed to one line, so the cause is legible rather than "{".
        assert!(unavailable.contains("No API token found"), "{unavailable}");
        assert!(!unavailable.contains('\n'));
    }

    #[test]
    fn provider_failures_keep_previous_source_items_and_name_every_reason() {
        let dir = fixture_dir("all-sources-degraded");
        let (good_gh, good_linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
case "$*" in
  *"--author @me"*|*"review-requested:@me"*) printf '%s' '[]' ;;
  *) printf '%s' '[{"number":7,"title":"Previous PR","body":"","state":"OPEN","headRefName":"old","url":"https://github.com/owner/repo/pull/7"}]' ;;
esac
"#,
            r#"#!/bin/sh
case "$*" in
  "issues list"*) printf '%s' '{"nodes":[{"identifier":"SCA-2","title":"Previous ticket","state":{"name":"In Progress"}}]}' ;;
  "cycles list"*) printf '%s' '{"nodes":[]}' ;;
  "attachments list"*) printf '%s' '[]' ;;
  *) exit 42 ;;
esac
"#,
        );
        let mut previous = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &good_gh,
            &good_linearis,
        );
        previous.conversations.push(MissiveConversation {
            id: "previous".into(),
            subject: "Previous conversation".into(),
            app_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/previous".into(),
            assignees: Vec::new(),
            last_activity_at: None,
            closed: false,
            pane_bound: true,
            messages: Vec::new(),
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        });
        let (failed_gh, failed_linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' 'github rate limited' >&2\nexit 1\n",
            "#!/bin/sh\nprintf '%s' 'linear rate limited' >&2\nexit 1\n",
        );
        let panes = panes_with_context(crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/previous".into()],
            ..Default::default()
        });

        let snapshot = refresh_work_index_with_missive(
            &config(),
            &MissiveConfig::default(),
            &panes,
            WorkIndexRefreshContext {
                previous: Some(&previous),
                ..WorkIndexRefreshContext::default()
            },
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &failed_gh,
            &failed_linearis,
            Path::new("/usr/bin/false"),
        );

        assert!(snapshot
            .items
            .iter()
            .any(|item| item.pr_title.as_deref() == Some("Previous PR")));
        assert!(snapshot.items.iter().any(|item| {
            item.ticket_details
                .iter()
                .any(|ticket| ticket.title.as_deref() == Some("Previous ticket"))
        }));
        assert_eq!(snapshot.conversations[0].subject, "Previous conversation");
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Github)
            .is_some_and(|reason| reason.contains("github rate limited")));
        assert!(snapshot
            .unavailable_reason(WorkIndexSource::Linear)
            .is_some_and(|reason| reason.contains("linear rate limited")));
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Missive),
            Some("team is not configured")
        );
    }

    #[test]
    fn nonzero_github_exit_sets_unavailable() {
        let dir = fixture_dir("gh-failure");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nexit 1\n",
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + WORK_INDEX_BATCH_TIMEOUT,
            WORK_INDEX_TARGET_TIMEOUT,
            &gh,
            &linearis,
        );
        assert!(snapshot.items.is_empty());
        assert!(snapshot.unavailable.is_some());
    }

    #[test]
    fn timeout_is_no_observation() {
        let dir = fixture_dir("timeout");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nsleep 2\n",
            "#!/bin/sh\nprintf '%s' '{\"nodes\":[]}'\n",
        );
        let snapshot = refresh_work_index(
            &config(),
            &[],
            Instant::now(),
            Instant::now() + Duration::from_secs(1),
            Duration::from_millis(50),
            &gh,
            &linearis,
        );
        assert!(snapshot.items.is_empty());
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Github),
            Some("observation timed out")
        );
    }

    #[test]
    fn linear_timeout_is_no_observation() {
        let dir = fixture_dir("linear-timeout");
        let (gh, linearis) = fake_programs(
            &dir,
            "#!/bin/sh\nprintf '%s' '[]'\n",
            "#!/bin/sh\nsleep 2\n",
        );
        let mut linear_only_config = config();
        linear_only_config.repos.clear();
        let snapshot = refresh_work_index(
            &linear_only_config,
            &[],
            Instant::now(),
            Instant::now() + Duration::from_secs(1),
            Duration::from_millis(50),
            &gh,
            &linearis,
        );

        assert!(snapshot.items.is_empty());
        assert_eq!(
            snapshot.unavailable_reason(WorkIndexSource::Linear),
            Some("observation timed out")
        );
    }

    #[test]
    fn snapshot_write_is_valid_json_at_explicit_path() {
        let dir = fixture_dir("write");
        let path = dir.join("nested/work-index.json");
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        };
        write_snapshot(&path, &snapshot).expect("write snapshot");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read snapshot"))
                .expect("valid JSON");
        assert!(value.get("items").is_some());
    }

    #[test]
    fn snapshot_path_is_scoped_to_session_name_including_default() {
        let state_dir = Path::new("/tmp/herdr-state");
        assert_eq!(
            work_index_snapshot_path_for(state_dir, None),
            state_dir.join("work-index/default.json")
        );
        assert_eq!(
            work_index_snapshot_path_for(state_dir, Some("customer-support")),
            state_dir.join("work-index/customer-support.json")
        );
    }

    #[test]
    fn cold_load_reads_only_its_session_snapshot_and_ignores_legacy_file() {
        let dir = fixture_dir("session-snapshot-isolation");
        let alpha_path = work_index_snapshot_path_for(&dir, Some("alpha"));
        let beta_path = work_index_snapshot_path_for(&dir, Some("beta"));
        let legacy_path = dir.join("work-index.json");
        let snapshot = |reason| Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(WorkIndexSource::Github, reason)),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        write_snapshot(&alpha_path, &snapshot("alpha")).expect("alpha snapshot");
        write_snapshot(&beta_path, &snapshot("beta")).expect("beta snapshot");
        write_snapshot(&legacy_path, &snapshot("legacy")).expect("legacy snapshot");

        assert_eq!(
            load_snapshot(&alpha_path)
                .and_then(|snapshot| snapshot.unavailable_summary())
                .as_deref(),
            Some("GitHub: alpha")
        );
        assert_eq!(
            load_snapshot(&beta_path)
                .and_then(|snapshot| snapshot.unavailable_summary())
                .as_deref(),
            Some("GitHub: beta")
        );
        assert!(load_snapshot(&work_index_snapshot_path_for(&dir, None)).is_none());
    }

    #[test]
    fn load_snapshot_round_trips_a_written_snapshot() {
        let dir = fixture_dir("load-round-trip");
        let path = dir.join("work-index.json");
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(
                WorkIndexSource::Linear,
                "observation timed out",
            )),
            observed_at: SystemTime::now(),
        };
        write_snapshot(&path, &snapshot).expect("write snapshot");

        let loaded = load_snapshot(&path).expect("snapshot loads back");

        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn load_snapshot_returns_none_for_missing_file() {
        let dir = fixture_dir("load-missing");
        let path = dir.join("does-not-exist.json");

        assert!(load_snapshot(&path).is_none());
    }

    #[test]
    fn load_snapshot_returns_none_for_corrupt_file() {
        let dir = fixture_dir("load-corrupt");
        let path = dir.join("work-index.json");
        std::fs::write(&path, b"not valid json").expect("write corrupt fixture");

        assert!(load_snapshot(&path).is_none());
    }
}

/// A write a human asked for against a work item.
///
/// Each variant maps to exactly one CLI invocation. They are kept together so
/// the confirm-then-run flow has a single vocabulary, and so it is obvious at a
/// glance which of them change something outside herdr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkItemWrite {
    CommentOnPullRequest {
        repo: String,
        number: u64,
        body: String,
    },
    CommentOnTicket {
        identifier: String,
        body: String,
    },
    TransitionTicket {
        identifier: String,
        state: String,
    },
    LinkTicketPullRequest {
        identifier: String,
        title: String,
        url: String,
    },
    ApprovePullRequest {
        repo: String,
        number: u64,
    },
    MergePullRequest {
        repo: String,
        number: u64,
    },
    ClosePullRequest {
        repo: String,
        number: u64,
    },
    MarkPullRequestDraft {
        repo: String,
        number: u64,
    },
    MarkPullRequestReady {
        repo: String,
        number: u64,
    },
}

impl WorkItemWrite {
    /// What the user is about to do, for the confirmation line. Phrased as the
    /// action and its target so a mis-aimed write is visible before it runs.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::CommentOnPullRequest { repo, number, .. } => {
                format!("comment on {repo}#{number}")
            }
            Self::CommentOnTicket { identifier, .. } => format!("comment on {identifier}"),
            Self::TransitionTicket { identifier, state } => {
                format!("move {identifier} to {state}")
            }
            Self::LinkTicketPullRequest { identifier, .. } => {
                format!("link pull request to {identifier}")
            }
            Self::ApprovePullRequest { repo, number } => format!("approve {repo}#{number}"),
            Self::MergePullRequest { repo, number } => {
                format!("squash-merge {repo}#{number}")
            }
            Self::ClosePullRequest { repo, number } => format!("close {repo}#{number}"),
            Self::MarkPullRequestDraft { repo, number } => {
                format!("mark {repo}#{number} draft")
            }
            Self::MarkPullRequestReady { repo, number } => {
                format!("mark {repo}#{number} ready")
            }
        }
    }

    /// The work item this write targets, so its cached detail can be dropped
    /// once the write lands and the next refresh shows the result.
    pub(crate) fn target(&self) -> Option<crate::app::state::WorkItemKey> {
        match self {
            Self::CommentOnPullRequest { repo, number, .. }
            | Self::ApprovePullRequest { repo, number }
            | Self::MergePullRequest { repo, number }
            | Self::ClosePullRequest { repo, number }
            | Self::MarkPullRequestDraft { repo, number }
            | Self::MarkPullRequestReady { repo, number } => Some(crate::app::state::WorkItemKey {
                repo: repo.clone(),
                pr_number: Some(*number),
                pr_url: None,
                ticket_id: None,
            }),
            Self::CommentOnTicket { identifier, .. }
            | Self::TransitionTicket { identifier, .. }
            | Self::LinkTicketPullRequest { identifier, .. } => {
                Some(crate::app::state::WorkItemKey {
                    repo: String::new(),
                    pr_number: None,
                    pr_url: None,
                    ticket_id: Some(identifier.clone()),
                })
            }
        }
    }
}

/// Run one authorized write. Returns the message to show the user either way.
pub(crate) fn run_work_item_write(
    write: &WorkItemWrite,
    gh_program: &Path,
    linearis_program: &Path,
    deadline: Instant,
) -> Result<String, String> {
    let (command, stdin) = match write {
        WorkItemWrite::CommentOnPullRequest { repo, number, body } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            // `--body-file -` rather than `--body`: a multi-line markdown comment
            // has no business going through argv quoting or its length limit.
            command.args([
                "pr",
                "comment",
                &number.to_string(),
                "-R",
                repo,
                "--body-file",
                "-",
            ]);
            (command, Some(body.clone().into_bytes()))
        }
        WorkItemWrite::CommentOnTicket { identifier, body } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args(["issues", "discuss", identifier, "--body", body]);
            (command, None)
        }
        WorkItemWrite::TransitionTicket { identifier, state } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args(["issues", "update", identifier, "--status", state]);
            (command, None)
        }
        WorkItemWrite::LinkTicketPullRequest {
            identifier,
            title,
            url,
        } => {
            let mut command = crate::noninteractive_process::command(linearis_program);
            command.args([
                "attachments",
                "create",
                identifier,
                "--title",
                title,
                "--url",
                url,
            ]);
            (command, None)
        }
        WorkItemWrite::ApprovePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "review", &number.to_string(), "-R", repo, "--approve"]);
            (command, None)
        }
        WorkItemWrite::MergePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "merge", &number.to_string(), "-R", repo, "--squash"]);
            (command, None)
        }
        WorkItemWrite::ClosePullRequest { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "close", &number.to_string(), "-R", repo]);
            (command, None)
        }
        WorkItemWrite::MarkPullRequestDraft { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "ready", "--undo", &number.to_string(), "-R", repo]);
            (command, None)
        }
        WorkItemWrite::MarkPullRequestReady { repo, number } => {
            let mut command = crate::noninteractive_process::command(gh_program);
            command.args(["pr", "ready", &number.to_string(), "-R", repo]);
            (command, None)
        }
    };

    let output = match stdin {
        Some(stdin) => {
            crate::noninteractive_process::output_with_stdin_and_deadline(command, stdin, deadline)
        }
        None => crate::noninteractive_process::output_with_deadline(command, deadline),
    }
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            format!("{} timed out", write.describe())
        } else {
            format!("{} could not be run", write.describe())
        }
    })?;

    if output.status.success() {
        return Ok(format!("{} done", write.describe()));
    }
    // The CLI's own message says why far better than a generic failure would.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no reason given")
        .trim()
        .to_string();
    Err(format!("{} failed: {reason}", write.describe()))
}

#[cfg(all(test, unix))]
mod work_item_write_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn recorder() -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-linear-write-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("create recorder directory");
        let program = root.join("linearis");
        let log = root.join("argv.log");
        std::fs::write(
            &program,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write recorder");
        let mut permissions = std::fs::metadata(&program)
            .expect("recorder metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).expect("make recorder executable");
        (program, log)
    }

    #[test]
    fn ticket_writes_map_to_linearis_argv() {
        let (linearis, log) = recorder();
        let deadline = Instant::now() + Duration::from_secs(2);
        run_work_item_write(
            &WorkItemWrite::TransitionTicket {
                identifier: "SCA-7".into(),
                state: "In Review".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("transition command");
        run_work_item_write(
            &WorkItemWrite::LinkTicketPullRequest {
                identifier: "SCA-7".into(),
                title: "owner/repo#42".into(),
                url: "https://github.com/owner/repo/pull/42".into(),
            },
            Path::new("/usr/bin/false"),
            &linearis,
            deadline,
        )
        .expect("link command");

        let argv = std::fs::read_to_string(log).expect("read recorder log");
        assert!(argv.contains("issues update SCA-7 --status In Review"));
        assert!(argv.contains(
            "attachments create SCA-7 --title owner/repo#42 --url https://github.com/owner/repo/pull/42"
        ));
    }

    #[test]
    fn pr_state_writes_map_to_gh_argv() {
        let (gh, log) = recorder();
        let deadline = Instant::now() + Duration::from_secs(2);
        for write in [
            WorkItemWrite::ClosePullRequest {
                repo: "owner/repo".into(),
                number: 42,
            },
            WorkItemWrite::MarkPullRequestDraft {
                repo: "owner/repo".into(),
                number: 42,
            },
            WorkItemWrite::MarkPullRequestReady {
                repo: "owner/repo".into(),
                number: 42,
            },
        ] {
            run_work_item_write(&write, &gh, Path::new("/usr/bin/false"), deadline)
                .expect("pull request state command");
        }

        assert_eq!(
            std::fs::read_to_string(log).expect("read recorder log"),
            [
                "pr close 42 -R owner/repo",
                "pr ready --undo 42 -R owner/repo",
                "pr ready 42 -R owner/repo",
                "",
            ]
            .join("\n")
        );
    }
}
