use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::schema::{AgentInfo, AgentStatus};
use crate::config::WorkIndexConfig;
use crate::work_context::{
    linear_ticket_url, normalize_repo_slug, normalize_ticket_id, repo_slug_from_pr_url,
    repo_slugs_match, PaneWorkRole,
};

pub(crate) const WORK_INDEX_BATCH_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const WORK_INDEX_TARGET_TIMEOUT: Duration = Duration::from_secs(5);
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkTicket {
    pub(crate) identifier: String,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) created_at: Option<SystemTime>,
    pub(crate) updated_at: Option<SystemTime>,
    pub(crate) branch: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) url: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) relations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub items: Vec<WorkItem>,
    pub unavailable: Option<String>,
    pub observed_at: SystemTime,
}

#[derive(Debug, Clone)]
struct GithubPullRequest {
    repo: String,
    number: u64,
    url: String,
    title: String,
    branch: String,
    draft: bool,
    review_decision: Option<String>,
    created_at: Option<SystemTime>,
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

pub(crate) fn refresh_work_index(
    config: &WorkIndexConfig,
    panes: &[AgentInfo],
    now: Instant,
    batch_deadline: Instant,
    target_timeout: Duration,
    gh_program: &Path,
    linearis_program: &Path,
) -> Snapshot {
    if !config.enabled {
        return unavailable_snapshot("work index disabled");
    }

    let repos = config
        .repos
        .iter()
        .filter_map(|repo| normalize_repo_slug(repo).ok())
        .collect::<Vec<_>>();
    let mut degraded: Option<String> = None;
    let mut github = Vec::new();
    for repo in repos {
        match fetch_github_pull_requests(
            &repo,
            gh_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(mut values) => github.append(&mut values),
            Err(RefreshError::TimedOut) => {
                return unavailable_snapshot("GitHub observation timed out")
            }
            Err(RefreshError::Failed(message)) => return unavailable_snapshot(message),
        }
    }

    let tickets = match config.linear_team.as_deref() {
        Some(team) if !team.trim().is_empty() => match fetch_linear_tickets(
            team,
            linearis_program,
            target_deadline(batch_deadline, target_timeout),
        ) {
            Ok(tickets) => tickets,
            // Neither a timeout nor a failure may throw away a live GitHub
            // half: 74 pull requests with no ticket edge still beat an empty
            // index. Degrade and name the cause instead.
            Err(RefreshError::TimedOut) => {
                degraded = Some("Linear observation timed out".to_string());
                Vec::new()
            }
            Err(RefreshError::Failed(message)) => {
                degraded = Some(message);
                Vec::new()
            }
        },
        _ => Vec::new(),
    };
    let attachments = fetch_attachments(&tickets, linearis_program, batch_deadline, target_timeout);

    let mut items = github
        .into_iter()
        .map(|pr| WorkItem {
            repo: pr.repo,
            pr_number: Some(pr.number),
            pr_url: Some(pr.url),
            pr_title: Some(pr.title),
            pr_state: Some("open".into()),
            draft: pr.draft,
            review_decision: pr.review_decision,
            created_at: pr.created_at,
            ticket_ids: Vec::new(),
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
        unavailable: degraded,
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

fn unavailable_snapshot(message: impl Into<String>) -> Snapshot {
    Snapshot {
        items: Vec::new(),
        unavailable: Some(message.into()),
        observed_at: SystemTime::now(),
    }
}

fn target_deadline(batch_deadline: Instant, target_timeout: Duration) -> Instant {
    (Instant::now() + target_timeout).min(batch_deadline)
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
        "number,title,headRefName,isDraft,reviewDecision,url,createdAt",
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
        .filter_map(|value| {
            let url = value.get("url")?.as_str()?.to_string();
            Some(GithubPullRequest {
                repo: repo_slug_from_pr_url(&url).unwrap_or_else(|| repo.to_string()),
                number: value.get("number")?.as_u64()?,
                url,
                title: value.get("title")?.as_str()?.to_string(),
                branch: value.get("headRefName")?.as_str()?.to_string(),
                draft: value
                    .get("isDraft")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                review_decision: value
                    .get("reviewDecision")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                created_at: value
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339_system_time),
            })
        })
        .collect())
}

const GITHUB_PULL_REQUEST_DETAIL_FIELDS: &str =
    "number,title,body,author,baseRefName,headRefName,createdAt,updatedAt,labels,url,reviewDecision,isDraft,statusCheckRollup,comments,files,commits";

fn fetch_github_pull_request_detail(
    repo: &str,
    number: u64,
    program: &Path,
    deadline: Instant,
) -> Result<WorkItemDetail, RefreshError> {
    let mut command = crate::noninteractive_process::command(program);
    let number = number.to_string();
    command.args([
        "pr",
        "view",
        &number,
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
        checks: status_check_summary(value.get("statusCheckRollup")),
        comments: github_comments(value.get("comments")),
        actions: github_actions(value.get("statusCheckRollup")),
        files: github_files(value.get("files")),
        commits: github_commits(value.get("commits")),
        unresolved_review_threads: None,
        unavailable: None,
        observed_at: SystemTime::now(),
    })
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
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "issues",
        "read",
        identifier,
        "--with-comment-threads",
        "--compact",
    ]);
    let output = crate::noninteractive_process::output_with_deadline(command, deadline).map_err(
        |error| {
            if error.kind() == std::io::ErrorKind::TimedOut {
                RefreshError::TimedOut
            } else {
                RefreshError::Failed(format!("linearis could not be run ({})", program.display()))
            }
        },
    )?;
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
    detail.url = value_text(value.get("url"));
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
            let state = value_text(action.get("conclusion"))
                .filter(|state| !state.is_empty())
                .or_else(|| value_text(action.get("status")))
                .unwrap_or_else(|| "unknown".to_string());
            Some(WorkItemAction { name, state })
        })
        .collect()
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
            check
                .get("conclusion")
                .and_then(Value::as_str)
                .is_some_and(|conclusion| {
                    matches!(
                        conclusion,
                        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                    )
                })
        })
        .count();
    Some(WorkItemCheckSummary {
        failing,
        total: rollup.len(),
    })
}

fn parse_rfc3339_system_time(value: &str) -> Option<SystemTime> {
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
    let mut command = crate::noninteractive_process::command(program);
    command.args([
        "issues",
        "list",
        "--team",
        team,
        "--status",
        "In Progress,In Review",
        "-l",
        "100",
        "--compact",
    ]);
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
        .filter_map(|node| {
            let identifier = normalize_ticket_id(node.get("identifier")?.as_str()?).ok()?;
            Some(WorkTicket {
                url: linear_ticket_url(&identifier),
                identifier,
                title: value_text(node.get("title")),
                description: value_text(node.get("description")),
                state: nested_text(node.get("state"), "name"),
                assignee: nested_text(node.get("assignee"), "name"),
                created_at: node
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339_system_time),
                updated_at: node
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .and_then(parse_rfc3339_system_time),
                branch: value_text(node.get("branchName")),
                labels: node
                    .get("labels")
                    .and_then(|labels| labels.get("nodes"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|label| nested_text(Some(label), "name"))
                    .collect(),
                parent: node.get("parent").and_then(format_linear_reference),
                relations: format_linear_relations(node),
            })
        })
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

pub(crate) fn write_snapshot(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(io::Error::other)?;
    std::fs::write(path, bytes)
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
        let config = self.work_index_config.clone();
        let panes = self.collect_agent_infos();
        let event_tx = self.event_tx.clone();
        let gh_program = self.work_index_gh_program();
        let linearis_program = self.work_index_linearis_program();
        let _ = std::thread::Builder::new()
            .name("herdr-work-index".into())
            .spawn(move || {
                let snapshot = refresh_work_index(
                    &config,
                    &panes,
                    Instant::now(),
                    deadline,
                    WORK_INDEX_TARGET_TIMEOUT,
                    &gh_program,
                    &linearis_program,
                );
                let _ = event_tx.blocking_send(crate::events::AppEvent::WorkIndexRefreshed {
                    generation,
                    snapshot,
                });
            });
    }

    pub(crate) fn handle_work_index_refreshed(
        &mut self,
        generation: u64,
        snapshot: Snapshot,
    ) -> bool {
        if generation <= self.last_applied_work_index_refresh_generation
            || generation != self.last_work_index_refresh_generation
        {
            return false;
        }
        self.work_index_refresh_in_flight = None;
        self.last_applied_work_index_refresh_generation = generation;
        if let Err(error) = write_snapshot(
            &crate::config::state_dir().join("work-index.json"),
            &snapshot,
        ) {
            tracing::warn!(error = %error, "failed to persist work index snapshot");
        }
        if let Some(work_view) = self.state.work_view.as_mut() {
            work_view.replace_snapshot(snapshot.clone());
        }
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
    fn github_pull_request_fetch_requests_created_at() {
        let dir = fixture_dir("github-created-at");
        let (gh, _linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
test "$*" = "pr list --repo owner/repo --state open --limit 200 --json number,title,headRefName,isDraft,reviewDecision,url,createdAt" || exit 42
printf '%s' '[{"number":7,"title":"PR","headRefName":"branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7","createdAt":"2026-08-30T11:22:33Z"}]'
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
    }

    #[test]
    fn github_pull_request_detail_fetch_requests_every_render_field() {
        let dir = fixture_dir("github-detail-fields");
        let (gh, _linearis) = fake_programs(
            &dir,
            &format!(
                r#"#!/bin/sh
test "$*" = "pr view 7 --repo owner/repo --json {GITHUB_PULL_REQUEST_DETAIL_FIELDS}" || exit 42
printf '%s' '{{"number":7,"title":"Detail","body":"Body","author":{{"login":"ms"}},"baseRefName":"main","headRefName":"feat/detail","createdAt":"2026-08-30T11:22:33Z","updatedAt":"2026-08-30T12:22:33Z","labels":[{{"name":"high-risk"}}],"url":"https://github.com/owner/repo/pull/7","reviewDecision":"REVIEW_REQUIRED","isDraft":false,"statusCheckRollup":[{{"name":"test","conclusion":"FAILURE"}},{{"name":"lint","conclusion":"SUCCESS"}}],"comments":[{{"author":{{"login":"reviewer"}},"body":"Looks good"}}],"files":[{{"path":"src/lib.rs","additions":4,"deletions":2}}],"commits":[{{"oid":"abcdef012345","messageHeadline":"fix detail"}}]}}'
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
        assert_eq!(detail.labels, vec!["high-risk"]);
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
        // Collapsed to one line, so the cause is legible rather than "{".
        assert!(unavailable.contains("No API token found"), "{unavailable}");
        assert!(!unavailable.contains('\n'));
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
            snapshot.unavailable.as_deref(),
            Some("GitHub observation timed out")
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
            snapshot.unavailable.as_deref(),
            Some("Linear observation timed out")
        );
    }

    #[test]
    fn snapshot_write_is_valid_json_at_explicit_path() {
        let dir = fixture_dir("write");
        let path = dir.join("nested/work-index.json");
        let snapshot = Snapshot {
            items: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::now(),
        };
        write_snapshot(&path, &snapshot).expect("write snapshot");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read snapshot"))
                .expect("valid JSON");
        assert!(value.get("items").is_some());
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
            Self::ApprovePullRequest { repo, number } => format!("approve {repo}#{number}"),
            Self::MergePullRequest { repo, number } => {
                format!("squash-merge {repo}#{number}")
            }
            Self::ClosePullRequest { repo, number } => format!("close {repo}#{number}"),
        }
    }

    /// The work item this write targets, so its cached detail can be dropped
    /// once the write lands and the next refresh shows the result.
    pub(crate) fn target(&self) -> Option<crate::app::state::WorkItemKey> {
        match self {
            Self::CommentOnPullRequest { repo, number, .. }
            | Self::ApprovePullRequest { repo, number }
            | Self::MergePullRequest { repo, number }
            | Self::ClosePullRequest { repo, number } => Some(crate::app::state::WorkItemKey {
                repo: repo.clone(),
                pr_number: Some(*number),
                pr_url: None,
                ticket_id: None,
            }),
            Self::CommentOnTicket { .. } => None,
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
