use std::collections::{HashMap, HashSet};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkIndexRefreshInFlight {
    pub(crate) generation: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkItemPane {
    pub pane_id: String,
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
    pub ticket_ids: Vec<String>,
    pub ticket_title: Option<String>,
    pub ticket_state: Option<String>,
    pub branch: Option<String>,
    pub preview_urls: Vec<String>,
    pub panes: Vec<WorkItemPane>,
    pub source: WorkItemSource,
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
}

#[derive(Debug, Clone)]
struct LinearTicket {
    id: String,
    title: Option<String>,
    state: Option<String>,
    branch: Option<String>,
}

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
            Err(RefreshError::TimedOut) => Vec::new(),
            Err(RefreshError::Failed(message)) => return unavailable_snapshot(message),
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
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
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
        .map(|ticket| (ticket.id.clone(), ticket))
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
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
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
        }
    }

    for ticket in ticket_by_id.into_values() {
        let Some(_ticket_url) = linear_ticket_url(&ticket.id) else {
            continue;
        };
        let repo = panes
            .iter()
            .find(|pane| {
                pane.work_context
                    .ticket_ids
                    .iter()
                    .filter_map(|id| normalize_ticket_id(id).ok())
                    .any(|id| id == ticket.id)
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
            ticket_ids: vec![ticket.id.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
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
        unavailable: None,
        observed_at: SystemTime::now(),
    }
}

fn exit_detail(label: &str, output: &std::process::Output) -> String {
    // Keep the child's own words: a bare "exited unsuccessfully" is
    // undiagnosable in the field, which is exactly where these tools fail.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.lines().find(|line| !line.trim().is_empty());
    match detail {
        Some(detail) => format!("{label} exited unsuccessfully: {}", detail.trim()),
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
        "number,title,headRefName,isDraft,reviewDecision,url",
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
            })
        })
        .collect())
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
            let id = normalize_ticket_id(node.get("identifier")?.as_str()?).ok()?;
            Some(LinearTicket {
                id,
                title: value_text(node.get("title")),
                state: value_text(node.get("state")),
                branch: value_text(node.get("branchName")),
            })
        })
        .collect())
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
        &ticket.id,
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
                ticket_id: ticket.id.clone(),
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
                ticket_ids: {
                    // Sorted so the snapshot is byte-stable across refreshes:
                    // it is consumed as JSON by ghx and diffed by hand.
                    let mut ids = pane_ticket_ids.into_iter().collect::<Vec<_>>();
                    ids.sort();
                    ids
                },
                ticket_title: None,
                ticket_state: None,
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

impl crate::app::App {
    fn work_index_gh_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_gh_program_override.as_ref() {
            return program.clone();
        }
        Path::new("gh").to_path_buf()
    }

    fn work_index_linearis_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = self.work_index_linearis_program_override.as_ref() {
            return program.clone();
        }
        Path::new("linearis").to_path_buf()
    }

    pub(crate) fn work_index_refresh_deadline(&self) -> Option<Instant> {
        self.work_index_config.enabled.then(|| {
            self.work_index_refresh_in_flight
                .as_ref()
                .map_or(self.next_work_index_refresh, |refresh| refresh.deadline)
        })
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
        self.work_index_snapshot = Some(snapshot);
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
    fn attachment_join_keeps_orphan_buckets_and_ignores_branch_names() {
        let dir = fixture_dir("join");
        let (gh, linearis) = fake_programs(
            &dir,
            r#"#!/bin/sh
printf '%s' '[{"number":7,"title":"Attached PR","headRefName":"plain-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/7"},{"number":8,"title":"No ticket PR","headRefName":"other-branch","isDraft":false,"reviewDecision":"","url":"https://github.com/owner/repo/pull/8"}]'
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
            Instant::now() + Duration::from_millis(20),
            Duration::from_millis(1),
            &gh,
            &linearis,
        );
        assert!(snapshot.items.is_empty());
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
