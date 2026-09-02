//! Pull-request projection of the repo-wide work index.
//!
//! The projection is pure data: snapshot in, rows and groups out. Selection,
//! rotation, and repo filtering live here so the renderer stays dumb and the
//! rules stay testable without a PTY. Options B/C/D (tickets, agents, review
//! queue) slot in as additional render arms over the same `WorkViewState`.

use crate::app::state::{WorkItemKey, WorkProjection, WorkViewState};
use crate::work_context::repo_slugs_match;
use crate::work_index::{Snapshot, WorkItem};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkPrRow {
    pub(crate) key: WorkItemKey,
    pub(crate) repo: String,
    /// PR number, or "—" when the item only carries a URL.
    pub(crate) number: String,
    pub(crate) title: String,
    /// Pane id of the pane with `active_owner == true`; `None` renders
    /// un-owned. Never invented: participation without ownership is not
    /// ownership.
    pub(crate) owner: Option<String>,
    /// Additional attached panes beyond the active owner (or all attached
    /// panes when nothing owns the item).
    pub(crate) extra_panes: usize,
    /// Exactly one ticket renders its id, several render "N tickets", none
    /// render "no ticket".
    pub(crate) ticket: String,
    /// "D" draft, "RR" review required, "✓" approved, "✗" changes requested,
    /// "—" otherwise.
    pub(crate) review: String,
    pub(crate) owner_pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkPrGroup {
    pub(crate) header: String,
    /// The trailing `no ticket (N)` group. Required: on real data it is about
    /// 30% of open PRs, so it must never be hidden or merged away.
    pub(crate) no_ticket: bool,
    pub(crate) rows: Vec<WorkPrRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkPrProjection {
    pub(crate) groups: Vec<WorkPrGroup>,
    pub(crate) row_count: usize,
}

impl WorkPrProjection {
    pub(crate) fn flat_rows<'a>(&'a self) -> impl Iterator<Item = &'a WorkPrRow> + 'a {
        self.groups.iter().flat_map(|group| group.rows.iter())
    }

    fn row_index(&self, key: Option<&WorkItemKey>) -> Option<usize> {
        if self.row_count == 0 {
            return None;
        }
        key.and_then(|key| self.flat_rows().position(|row| &row.key == key))
            .or(Some(0))
    }
}

fn is_pull_request(item: &WorkItem) -> bool {
    item.pr_number.is_some() || item.pr_url.is_some()
}

/// Review cell shared by both projections. Draft wins over the review
/// decision: GitHub marks drafts REVIEW_REQUIRED at open time, and the draft
/// state is the more actionable signal.
fn review_cell(draft: bool, review_decision: Option<&str>) -> &'static str {
    if draft {
        return "D";
    }
    match review_decision {
        Some("REVIEW_REQUIRED") => "RR",
        Some("APPROVED") => "✓",
        Some("CHANGES_REQUESTED") => "✗",
        _ => "—",
    }
}

fn parse_pr_number(pr_url: &str) -> Option<u64> {
    pr_url.rsplit('/').next()?.parse().ok()
}

fn item_key(item: &WorkItem) -> WorkItemKey {
    WorkItemKey {
        repo: item.repo.clone(),
        pr_number: item.pr_number,
        pr_url: item.pr_url.clone(),
        ticket_id: item
            .pr_number
            .is_none()
            .then(|| item.ticket_ids.first().cloned())
            .flatten(),
    }
}

fn project_row(item: &WorkItem) -> WorkPrRow {
    let owner = item.panes.iter().find(|pane| pane.active_owner);
    let extra_panes = item.panes.len() - usize::from(owner.is_some());
    let ticket = match item.ticket_ids.as_slice() {
        [] => "no ticket".to_string(),
        [ticket] => ticket.clone(),
        tickets => format!("{} tickets", tickets.len()),
    };
    WorkPrRow {
        key: item_key(item),
        repo: item.repo.clone(),
        number: item
            .pr_number
            .map(|number| number.to_string())
            .unwrap_or_else(|| "—".to_string()),
        title: item
            .pr_title
            .clone()
            .unwrap_or_else(|| "(untitled PR)".to_string()),
        owner: owner.map(|pane| {
            pane.agent_label
                .clone()
                .unwrap_or_else(|| pane.pane_id.clone())
        }),
        extra_panes,
        ticket,
        review: review_cell(item.draft, item.review_decision.as_deref()).to_string(),
        owner_pane_id: owner.map(|pane| pane.pane_id.clone()),
    }
}

/// Project the snapshot into the flat, repo-grouped PR list with the trailing
/// `no ticket (N)` group. Rows sort by PR number descending inside a group.
pub(crate) fn project_pull_requests(
    snapshot: &Snapshot,
    repo_filter: Option<&str>,
) -> WorkPrProjection {
    let mut ticketed: std::collections::BTreeMap<String, Vec<WorkPrRow>> =
        std::collections::BTreeMap::new();
    let mut unticketed = Vec::new();
    for item in &snapshot.items {
        if !is_pull_request(item) {
            continue;
        }
        if repo_filter.is_some_and(|repo| !repo_slugs_match(repo, &item.repo)) {
            continue;
        }
        let row = project_row(item);
        if item.ticket_ids.is_empty() {
            unticketed.push(row);
        } else {
            ticketed.entry(item.repo.clone()).or_default().push(row);
        }
    }
    let sort_rows = |rows: &mut Vec<WorkPrRow>| {
        rows.sort_by(|left, right| {
            right
                .key
                .pr_number
                .unwrap_or(0)
                .cmp(&left.key.pr_number.unwrap_or(0))
                .then_with(|| left.title.cmp(&right.title))
        });
    };
    let mut groups: Vec<WorkPrGroup> = ticketed
        .into_iter()
        .map(|(repo, mut rows)| {
            sort_rows(&mut rows);
            WorkPrGroup {
                header: format!("{repo} ({})", rows.len()),
                no_ticket: false,
                rows,
            }
        })
        .collect();
    if !unticketed.is_empty() {
        sort_rows(&mut unticketed);
        groups.push(WorkPrGroup {
            header: format!("no ticket ({})", unticketed.len()),
            no_ticket: true,
            rows: unticketed,
        });
    }
    let row_count = groups.iter().map(|group| group.rows.len()).sum();
    WorkPrProjection { groups, row_count }
}

impl WorkViewState {
    /// The PR projection of the current snapshot, or `None` when another
    /// projection is active (placeholder) or no snapshot has been collected.
    pub(crate) fn projection(&self) -> Option<WorkPrProjection> {
        if self.projection != WorkProjection::PullRequests {
            return None;
        }
        self.snapshot
            .as_ref()
            .map(|snapshot| project_pull_requests(snapshot, self.repo_filter.as_deref()))
    }

    /// Index of the selected row within the current projection. Falls back to
    /// the first row when no explicit selection exists; `None` when empty.
    pub(crate) fn selected_index(&self, projection: &WorkPrProjection) -> Option<usize> {
        projection.row_index(self.selected.as_ref())
    }

    /// Move the selection within the current projection. Placeholder
    /// projections have no rows, so movement is a no-op there and the PR
    /// selection survives a rotation round-trip untouched.
    pub(crate) fn move_selection(&mut self, delta: i64) {
        let Some(projection) = self.projection() else {
            return;
        };
        let Some(index) = self.selected_index(&projection) else {
            return;
        };
        let last = projection.row_count.saturating_sub(1) as i64;
        let next = (index as i64 + delta).clamp(0, last) as usize;
        self.selected = projection.flat_rows().nth(next).map(|row| row.key.clone());
    }

    pub(crate) fn rotate(&mut self, forward: bool) {
        self.projection = if forward {
            self.projection.rotate_right()
        } else {
            self.projection.rotate_left()
        };
    }

    /// Cycle the repo filter across the repos present in the snapshot:
    /// none → first repo → … → last repo → none. Returns the new filter so
    /// the caller can surface it as a hint. Filters get their own key, never
    /// an arrow: arrows are rotation and movement.
    pub(crate) fn cycle_repo_filter(&mut self) -> Option<String> {
        let repos: Vec<String> = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .items
                    .iter()
                    .filter(|item| is_pull_request(item))
                    .map(|item| item.repo.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        if repos.is_empty() {
            self.repo_filter = None;
            return None;
        }
        let next = match self.repo_filter.as_deref() {
            None => repos.first().cloned(),
            Some(current) => repos
                .iter()
                .position(|repo| repo_slugs_match(repo, current))
                .and_then(|index| repos.get(index + 1))
                .cloned(),
        };
        self.repo_filter = next.clone();
        next
    }

    /// The selected PR row, when the PR projection is active and has rows.
    pub(crate) fn selected_row(&self) -> Option<WorkPrRow> {
        let projection = self.projection()?;
        let index = self.selected_index(&projection)?;
        let selected = projection.flat_rows().nth(index).cloned();
        selected
    }

    /// Swap in a refreshed snapshot, keeping the selection on the same work
    /// item. A vanished item falls back to the first row; an orphaned
    /// selection never becomes a dead pointer.
    pub(crate) fn replace_snapshot(&mut self, snapshot: Snapshot) {
        let key = self.selected.take();
        self.snapshot = Some(snapshot);
        if let Some(key) = key {
            let still_there = self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.items.iter().any(|item| item_key(item) == key));
            if still_there {
                self.selected = Some(key);
            }
        }
    }
}

/// One pane's declared PR binding, gathered in canonical workspace/tab/layout
/// order so a lifecycle change never moves a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHomeBinding {
    pub(crate) ws_idx: usize,
    pub(crate) pane_id: crate::layout::PaneId,
    pub(crate) agent_label: Option<String>,
    pub(crate) agent_state: crate::detect::AgentState,
    pub(crate) pr_url: String,
    pub(crate) role: Option<crate::work_context::PaneWorkRole>,
    pub(crate) active_owner: bool,
    pub(crate) work_title: Option<String>,
    pub(crate) ticket_ids: Vec<String>,
    pub(crate) preview_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHomeTicketBinding {
    pub(crate) ws_idx: usize,
    pub(crate) pane_id: crate::layout::PaneId,
    pub(crate) ticket_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHomeRow {
    pub(crate) key: WorkItemKey,
    pub(crate) number: String,
    pub(crate) title: String,
    /// Owning agent's label; `None` when no pane actively owns the PR.
    pub(crate) owner: Option<String>,
    /// Sidebar glyph vocabulary: ● working, ○ blocked/idle, · no agent.
    pub(crate) glyph: &'static str,
    /// Review cell from the snapshot, or "?" when the row is a declared
    /// binding that no fetch has ever confirmed.
    pub(crate) review: String,
    pub(crate) ticket: String,
    pub(crate) ticket_ids: Vec<String>,
    pub(crate) preview_urls: Vec<String>,
    /// Time since the PR was opened, or "—" when GitHub did not provide a
    /// trustworthy creation time.
    pub(crate) age: String,
    pub(crate) fetched: bool,
    pub(crate) extra_panes: usize,
    /// Jump target: the owning pane, else the first registered pane.
    pub(crate) ws_idx: usize,
    pub(crate) pane_id: crate::layout::PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHomeTicketRow {
    pub(crate) key: WorkItemKey,
    pub(crate) ticket: crate::work_index::WorkTicket,
    pub(crate) linked_pr_url: Option<String>,
    pub(crate) jump_target: Option<(usize, crate::layout::PaneId)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHomeProjection {
    pub(crate) rows: Vec<DockHomeRow>,
    pub(crate) ticket_rows: Vec<DockHomeTicketRow>,
    /// Unbound open PRs / ticket-only items; `None` when no snapshot exists
    /// and the honest answer is "unknown", not zero.
    pub(crate) unbound_prs: Option<usize>,
    pub(crate) unbound_tickets: Option<usize>,
    pub(crate) observed_at: Option<std::time::SystemTime>,
    pub(crate) unavailable: Option<String>,
    /// Whether the work index is configured on. Without it, "switched off" and
    /// "on but nothing observed yet" render as the same `unknown`.
    pub(crate) index_enabled: bool,
}

/// Dock home rows: one per bound PR, ordered by first canonical binding,
/// enriched from the work-index snapshot when one was collected.
pub(crate) fn project_dock_home(
    bindings: &[DockHomeBinding],
    ticket_bindings: &[DockHomeTicketBinding],
    snapshot: Option<&Snapshot>,
    index_enabled: bool,
) -> DockHomeProjection {
    project_dock_home_at(
        bindings,
        ticket_bindings,
        snapshot,
        index_enabled,
        SystemTime::now(),
    )
}

fn project_dock_home_at(
    bindings: &[DockHomeBinding],
    ticket_bindings: &[DockHomeTicketBinding],
    snapshot: Option<&Snapshot>,
    index_enabled: bool,
    now: SystemTime,
) -> DockHomeProjection {
    let mut rows: Vec<DockHomeRow> = Vec::new();
    for binding in bindings {
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.key.pr_url.as_deref() == Some(binding.pr_url.as_str()))
        {
            row.extra_panes += 1;
            if row.ticket == "no ticket" && !binding.ticket_ids.is_empty() {
                row.ticket = ticket_cell(&binding.ticket_ids);
            }
            for ticket_id in &binding.ticket_ids {
                if !row.ticket_ids.contains(ticket_id) {
                    row.ticket_ids.push(ticket_id.clone());
                }
            }
            for preview_url in &binding.preview_urls {
                if !row.preview_urls.contains(preview_url) {
                    row.preview_urls.push(preview_url.clone());
                }
            }
            if binding.active_owner && row.owner.is_none() {
                row.owner = binding.agent_label.clone();
                row.glyph = dock_home_glyph(binding.agent_state);
                row.ws_idx = binding.ws_idx;
                row.pane_id = binding.pane_id;
            }
            continue;
        }
        let item = snapshot.and_then(|snapshot| {
            snapshot
                .items
                .iter()
                .find(|item| item.pr_url.as_deref() == Some(binding.pr_url.as_str()))
        });
        let fetched = item.is_some();
        let review = item
            .map(|item| review_cell(item.draft, item.review_decision.as_deref()).to_string())
            .unwrap_or_else(|| "?".to_string());
        let title = item
            .and_then(|item| item.pr_title.clone())
            .or_else(|| binding.work_title.clone())
            .unwrap_or_else(|| "(untitled PR)".to_string());
        let repo = item
            .map(|item| item.repo.clone())
            .or_else(|| binding.repo_slug())
            .unwrap_or_default();
        let ticket_ids = item
            .map(|item| item.ticket_ids.as_slice())
            .filter(|ticket_ids| !ticket_ids.is_empty())
            .unwrap_or(binding.ticket_ids.as_slice());
        let mut preview_urls = item
            .map(|item| item.preview_urls.clone())
            .unwrap_or_default();
        for preview_url in &binding.preview_urls {
            if !preview_urls.contains(preview_url) {
                preview_urls.push(preview_url.clone());
            }
        }
        let age = item
            .and_then(|item| item.created_at)
            .and_then(|created_at| now.duration_since(created_at).ok())
            .map(compact_elapsed)
            .unwrap_or_else(|| "—".to_string());
        rows.push(DockHomeRow {
            key: WorkItemKey {
                repo,
                pr_number: item
                    .and_then(|item| item.pr_number)
                    .or_else(|| parse_pr_number(&binding.pr_url)),
                pr_url: Some(binding.pr_url.clone()),
                ticket_id: None,
            },
            number: item
                .and_then(|item| item.pr_number)
                .or_else(|| parse_pr_number(&binding.pr_url))
                .map(|number| number.to_string())
                .unwrap_or_else(|| "—".to_string()),
            title,
            owner: binding
                .active_owner
                .then(|| binding.agent_label.clone())
                .flatten(),
            glyph: dock_home_glyph(binding.agent_state),
            review,
            ticket: ticket_cell(ticket_ids),
            ticket_ids: ticket_ids.to_vec(),
            preview_urls,
            age,
            fetched,
            extra_panes: 0,
            ws_idx: binding.ws_idx,
            pane_id: binding.pane_id,
        });
    }
    let unbound = snapshot.map(|snapshot| {
        let prs = snapshot
            .items
            .iter()
            .filter(|item| is_pull_request(item) && item.panes.is_empty())
            .count();
        let tickets = snapshot
            .items
            .iter()
            .filter(|item| !is_pull_request(item) && item.panes.is_empty())
            .count();
        (prs, tickets)
    });
    let mut ticket_rows = Vec::new();
    if let Some(snapshot) = snapshot {
        for item in snapshot.items.iter().filter(|item| !is_pull_request(item)) {
            for ticket_id in &item.ticket_ids {
                push_ticket_row(&mut ticket_rows, Some(snapshot), ticket_id, None);
            }
        }
    }
    for binding in ticket_bindings {
        push_ticket_row(
            &mut ticket_rows,
            snapshot,
            &binding.ticket_id,
            Some((binding.ws_idx, binding.pane_id)),
        );
    }
    ticket_rows.sort_by(|left, right| left.ticket.identifier.cmp(&right.ticket.identifier));
    DockHomeProjection {
        rows,
        ticket_rows,
        unbound_prs: unbound.map(|(prs, _)| prs),
        unbound_tickets: unbound.map(|(_, tickets)| tickets),
        observed_at: snapshot.map(|snapshot| snapshot.observed_at),
        unavailable: snapshot.and_then(|snapshot| snapshot.unavailable.clone()),
        index_enabled,
    }
}

fn push_ticket_row(
    rows: &mut Vec<DockHomeTicketRow>,
    snapshot: Option<&Snapshot>,
    ticket_id: &str,
    jump_target: Option<(usize, crate::layout::PaneId)>,
) {
    if let Some(existing) = rows
        .iter_mut()
        .find(|row| row.ticket.identifier == ticket_id)
    {
        if existing.jump_target.is_none() {
            existing.jump_target = jump_target;
        }
        return;
    }
    let item = snapshot
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .find(|item| item.ticket_ids.iter().any(|id| id == ticket_id));
    let ticket = item
        .and_then(|item| {
            item.ticket_details
                .iter()
                .find(|ticket| ticket.identifier == ticket_id)
        })
        .cloned()
        .unwrap_or_else(|| crate::work_index::WorkTicket {
            identifier: ticket_id.to_string(),
            title: None,
            description: None,
            state: None,
            assignee: None,
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: crate::work_context::linear_ticket_url(ticket_id),
            parent: None,
            relations: Vec::new(),
        });
    let linked_pr_url = snapshot
        .into_iter()
        .flat_map(|snapshot| snapshot.items.iter())
        .find(|item| item.pr_url.is_some() && item.ticket_ids.iter().any(|id| id == ticket_id))
        .and_then(|item| item.pr_url.clone());
    rows.push(DockHomeTicketRow {
        key: WorkItemKey {
            // Repository enrichment may arrive on a later work-index refresh.
            // Ticket selection identity must remain stable across that refresh.
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some(ticket_id.to_string()),
        },
        linked_pr_url,
        ticket,
        jump_target,
    });
}

fn ticket_cell(ticket_ids: &[String]) -> String {
    match ticket_ids {
        [] => "no ticket".to_string(),
        [ticket] => ticket.clone(),
        tickets => format!("{} tickets", tickets.len()),
    }
}

pub(crate) fn compact_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", (seconds / (24 * 60 * 60)).min(999))
    }
}

fn dock_home_glyph(state: crate::detect::AgentState) -> &'static str {
    match state {
        crate::detect::AgentState::Working => "●",
        crate::detect::AgentState::Blocked | crate::detect::AgentState::Idle => "○",
        _ => "·",
    }
}

impl DockHomeBinding {
    fn repo_slug(&self) -> Option<String> {
        // https://github.com/<owner>/<repo>/pull/<n>
        let mut parts = self.pr_url.split('/');
        match (parts.next_back(), parts.next_back(), parts.next_back()) {
            (Some(_number), Some("pull"), Some(repo)) => {
                let owner = parts.next_back()?;
                Some(format!("{owner}/{repo}"))
            }
            _ => None,
        }
    }
}

impl crate::app::state::AppState {
    /// Bound PR registrations in canonical workspace/tab/layout order.
    pub(crate) fn dock_home_bindings(&self) -> Vec<DockHomeBinding> {
        let mut bindings = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for detail in workspace.pane_details(&self.terminals) {
                let pane = workspace
                    .tabs
                    .get(detail.tab_idx)
                    .and_then(|tab| tab.panes.get(&detail.pane_id));
                let Some(terminal) = pane
                    .map(|pane| pane.attached_terminal_id.clone())
                    .and_then(|id| self.terminals.get(&id))
                else {
                    continue;
                };
                let context = terminal.effective_work_context();
                let Some(pr_url) = context.primary_pr() else {
                    continue;
                };
                bindings.push(DockHomeBinding {
                    ws_idx,
                    pane_id: detail.pane_id,
                    // `agent_label` falls back to the `>_` shell glyph for an
                    // agentless pane. That is not an owner, so only a detected
                    // agent fills the owner cell; everything else stays `—`.
                    agent_label: detail
                        .agent
                        .is_some()
                        .then(|| detail.agent_label.clone())
                        .filter(|label| !label.is_empty()),
                    agent_state: detail.state,
                    pr_url: pr_url.to_string(),
                    role: context.role,
                    active_owner: context.active_owner,
                    work_title: context.work_title.clone(),
                    ticket_ids: context.ticket_ids.clone(),
                    preview_urls: context.preview_urls.clone(),
                });
            }
        }
        bindings
    }

    pub(crate) fn dock_home_ticket_bindings(&self) -> Vec<DockHomeTicketBinding> {
        let mut bindings = Vec::new();
        for (ws_idx, workspace) in self.workspaces.iter().enumerate() {
            for detail in workspace.pane_details(&self.terminals) {
                let terminal = workspace
                    .tabs
                    .get(detail.tab_idx)
                    .and_then(|tab| tab.panes.get(&detail.pane_id))
                    .map(|pane| pane.attached_terminal_id.clone())
                    .and_then(|id| self.terminals.get(&id));
                let Some(terminal) = terminal else {
                    continue;
                };
                for ticket_id in &terminal.effective_work_context().ticket_ids {
                    if let Ok(ticket_id) = crate::work_context::normalize_ticket_id(ticket_id) {
                        bindings.push(DockHomeTicketBinding {
                            ws_idx,
                            pane_id: detail.pane_id,
                            ticket_id,
                        });
                    }
                }
            }
        }
        bindings
    }

    pub(crate) fn dock_home_projection(&self) -> DockHomeProjection {
        project_dock_home(
            &self.dock_home_bindings(),
            &self.dock_home_ticket_bindings(),
            self.work_index_snapshot.as_ref(),
            self.work_index_enabled,
        )
    }

    /// Follow a changed pane focus to the work item it is bound to without
    /// repeatedly overwriting an attach-local explicit selection for the same
    /// pane. A pull request wins when the pane has one; otherwise the pane's
    /// ticket selects the tickets section, so opening a ticket-only tab still
    /// moves the dock to the work being looked at.
    pub(crate) fn reconcile_dock_home_with_focused_pane(&mut self) {
        let focused = self.current_pane_focus_target();
        if self.dock_home_followed_pane == focused {
            return;
        }
        self.dock_home_followed_pane = focused.clone();

        let Some(focused) = focused else {
            return;
        };
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == focused.workspace_id)
        else {
            return;
        };
        let Some(context) = self
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(focused.pane_id))
            .and_then(|terminal_id| self.terminals.get(terminal_id))
            .map(|terminal| terminal.effective_work_context().clone())
        else {
            return;
        };

        let projection = self.dock_home_projection();
        if let Some(pr_url) = context.primary_pr() {
            if let Some(key) = projection
                .rows
                .iter()
                .find(|row| {
                    row.key
                        .pr_url
                        .as_deref()
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(pr_url))
                })
                .map(|row| row.key.clone())
            {
                self.dock_home_selection = Some(key);
                self.dock_home_section = crate::app::state::DockHomeSection::Prs;
                self.dock_scroll = 0;
                return;
            }
        }
        let Some(ticket_id) = context.primary_ticket() else {
            return;
        };
        let Some(key) = projection
            .ticket_rows
            .iter()
            .find(|row| {
                row.key
                    .ticket_id
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(ticket_id))
            })
            .map(|row| row.key.clone())
        else {
            return;
        };

        self.dock_home_ticket_selection = Some(key);
        self.dock_home_section = crate::app::state::DockHomeSection::Tickets;
        self.dock_scroll = 0;
    }

    /// Index of the selected home row, defaulting to the first row; `None`
    /// when nothing is bound.
    pub(crate) fn dock_home_selected_index(
        &self,
        projection: &DockHomeProjection,
    ) -> Option<usize> {
        if projection.rows.is_empty() {
            return None;
        }
        self.dock_home_selection
            .as_ref()
            .and_then(|key| projection.rows.iter().position(|row| &row.key == key))
            .or(Some(0))
    }

    pub(crate) fn move_dock_home_selection(&mut self, delta: i64) {
        let projection = self.dock_home_projection();
        match self.dock_home_section {
            crate::app::state::DockHomeSection::Prs => {
                let Some(index) = self.dock_home_selected_index(&projection) else {
                    return;
                };
                let last = projection.rows.len().saturating_sub(1) as i64;
                let next = (index as i64 + delta).clamp(0, last) as usize;
                self.dock_home_selection = projection.rows.get(next).map(|row| row.key.clone());
            }
            crate::app::state::DockHomeSection::Tickets => {
                let Some(index) = self.dock_home_selected_ticket_index(&projection) else {
                    return;
                };
                let last = projection.ticket_rows.len().saturating_sub(1) as i64;
                let next = (index as i64 + delta).clamp(0, last) as usize;
                self.dock_home_ticket_selection =
                    projection.ticket_rows.get(next).map(|row| row.key.clone());
            }
        }
        self.dock_scroll = 0;
    }

    pub(crate) fn dock_home_selected_ticket_index(
        &self,
        projection: &DockHomeProjection,
    ) -> Option<usize> {
        if projection.ticket_rows.is_empty() {
            return None;
        }
        self.dock_home_ticket_selection
            .as_ref()
            .and_then(|key| {
                projection
                    .ticket_rows
                    .iter()
                    .position(|row| &row.key == key)
            })
            .or(Some(0))
    }

    pub(crate) fn set_dock_home_section(&mut self, section: crate::app::state::DockHomeSection) {
        self.dock_home_section = section;
        self.dock_scroll = 0;
    }

    pub(crate) fn set_dock_home_detail_tab(&mut self, tab: crate::app::state::DockHomeDetailTab) {
        self.dock_home_detail_tab = tab;
        self.dock_scroll = 0;
    }

    pub(crate) fn dock_home_keys_for_section(
        &self,
        section: crate::app::state::DockHomeSection,
    ) -> Vec<WorkItemKey> {
        let projection = self.dock_home_projection();
        match section {
            crate::app::state::DockHomeSection::Prs => {
                projection.rows.into_iter().map(|row| row.key).collect()
            }
            crate::app::state::DockHomeSection::Tickets => projection
                .ticket_rows
                .into_iter()
                .map(|row| row.key)
                .collect(),
        }
    }

    pub(crate) fn dock_home_active_selection(&self) -> Option<WorkItemKey> {
        match self.dock_home_section {
            crate::app::state::DockHomeSection::Prs => self.dock_home_selection.clone(),
            crate::app::state::DockHomeSection::Tickets => self.dock_home_ticket_selection.clone(),
        }
    }

    pub(crate) fn dock_home_selected_row(&self) -> Option<DockHomeRow> {
        let projection = self.dock_home_projection();
        let index = self.dock_home_selected_index(&projection)?;
        projection.rows.get(index).cloned()
    }

    /// Focus the pane behind the selected home row. Returns false when the
    /// pane vanished between projection and jump — never a dead pointer.
    pub(crate) fn jump_to_dock_home_selection(&mut self) -> bool {
        let target = match self.dock_home_section {
            crate::app::state::DockHomeSection::Prs => self
                .dock_home_selected_row()
                .map(|row| (row.ws_idx, row.pane_id)),
            crate::app::state::DockHomeSection::Tickets => {
                let projection = self.dock_home_projection();
                self.dock_home_selected_ticket_index(&projection)
                    .and_then(|index| projection.ticket_rows.get(index))
                    .and_then(|row| row.jump_target)
            }
        };
        let Some((ws_idx, pane_id)) = target else {
            return false;
        };
        let exists = self
            .workspaces
            .get(ws_idx)
            .is_some_and(|ws| ws.tabs.iter().any(|tab| tab.panes.contains_key(&pane_id)));
        if !exists {
            return false;
        }
        self.focus_pane_in_workspace(ws_idx, pane_id);
        self.dock_home_focused = false;
        self.dock_scroll = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::work_index::{WorkItem, WorkItemPane, WorkItemSource};

    fn item(
        repo: &str,
        number: u64,
        title: &str,
        tickets: &[&str],
        panes: &[(&str, bool)],
    ) -> WorkItem {
        WorkItem {
            repo: repo.to_string(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/{repo}/pull/{number}")),
            pr_title: Some(title.to_string()),
            pr_state: Some("open".to_string()),
            draft: false,
            review_decision: None,
            created_at: None,
            ticket_ids: tickets.iter().map(|ticket| ticket.to_string()).collect(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: panes
                .iter()
                .map(|(pane_id, active_owner)| WorkItemPane {
                    pane_id: pane_id.to_string(),
                    agent_label: None,
                    workspace_id: "ws".to_string(),
                    tab_id: "tab".to_string(),
                    role: None,
                    active_owner: *active_owner,
                    agent_status: AgentStatus::Working,
                })
                .collect(),
            source: WorkItemSource::default(),
        }
    }

    fn snapshot(items: Vec<WorkItem>) -> Snapshot {
        Snapshot {
            items,
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn view(items: Vec<WorkItem>) -> WorkViewState {
        WorkViewState::new(true, Some(snapshot(items)))
    }

    fn home_binding(
        ws_idx: usize,
        pr_number: u64,
        agent_state: crate::detect::AgentState,
    ) -> DockHomeBinding {
        DockHomeBinding {
            ws_idx,
            pane_id: crate::layout::PaneId::alloc(),
            agent_label: Some(format!("agent-{ws_idx}")),
            agent_state,
            pr_url: format!("https://github.com/owner/repo/pull/{pr_number}"),
            role: None,
            active_owner: true,
            work_title: Some(format!("binding {pr_number}")),
            ticket_ids: Vec::new(),
            preview_urls: Vec::new(),
        }
    }

    fn state_with_bound_prs(pr_numbers: &[u64]) -> crate::app::state::AppState {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = pr_numbers
            .iter()
            .map(|number| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        state.active = (!state.workspaces.is_empty()).then_some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        for (ws_idx, number) in pr_numbers.iter().enumerate() {
            let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("root terminal")
                .clone();
            state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!("https://github.com/owner/repo/pull/{number}")]),
                    ..Default::default()
                })
                .expect("valid work context");
        }
        state
    }

    #[test]
    fn dock_home_row_order_follows_bindings_across_lifecycle_changes() {
        let mut bindings = vec![
            home_binding(0, 10, crate::detect::AgentState::Working),
            home_binding(1, 20, crate::detect::AgentState::Blocked),
        ];
        let before = project_dock_home(&bindings, &[], None, true);
        bindings[0].agent_state = crate::detect::AgentState::Idle;
        bindings[1].agent_state = crate::detect::AgentState::Working;
        let after = project_dock_home(&bindings, &[], None, true);

        let keys = |projection: &DockHomeProjection| {
            projection
                .rows
                .iter()
                .map(|row| row.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&before), keys(&after));
        assert_eq!(before.rows[0].glyph, "●");
        assert_eq!(after.rows[0].glyph, "○");
    }

    #[test]
    fn dock_home_rows_distinguish_missing_and_fetched_pull_requests() {
        let missing = home_binding(0, 10, crate::detect::AgentState::Idle);
        let fetched = home_binding(1, 20, crate::detect::AgentState::Working);
        let mut fetched_item = item("owner/repo", 20, "snapshot title", &[], &[]);
        fetched_item.review_decision = Some("APPROVED".to_string());

        let projection = project_dock_home(
            &[missing, fetched],
            &[],
            Some(&snapshot(vec![fetched_item])),
            true,
        );

        assert_eq!(projection.rows[0].key.repo, "owner/repo");
        assert_eq!(projection.rows[0].number, "10");
        assert_eq!(projection.rows[0].review, "?");
        assert!(!projection.rows[0].fetched);
        assert_eq!(projection.rows[1].number, "20");
        assert_eq!(projection.rows[1].title, "snapshot title");
        assert_eq!(projection.rows[1].review, "✓");
        assert!(projection.rows[1].fetched);
    }

    #[test]
    fn dock_home_ticket_precedence_and_missing_age_are_explicit() {
        let mut indexed_binding = home_binding(0, 10, crate::detect::AgentState::Working);
        indexed_binding.ticket_ids = vec!["MAT-10".to_string()];
        let mut fallback_binding = home_binding(1, 20, crate::detect::AgentState::Working);
        fallback_binding.ticket_ids = vec!["MAT-20".to_string(), "SCA-20".to_string()];
        fallback_binding.preview_urls = vec!["https://preview.example/pr-20".to_string()];
        let empty_binding = home_binding(2, 30, crate::detect::AgentState::Idle);
        let indexed_item = item("owner/repo", 10, "indexed", &["SCA-10"], &[]);
        let fallback_item = item("owner/repo", 20, "fallback", &[], &[]);

        let projection = project_dock_home_at(
            &[indexed_binding, fallback_binding, empty_binding],
            &[],
            Some(&snapshot(vec![indexed_item, fallback_item])),
            true,
            SystemTime::UNIX_EPOCH + Duration::from_secs(300),
        );

        assert_eq!(projection.rows[0].ticket, "SCA-10");
        assert_eq!(projection.rows[1].ticket, "2 tickets");
        assert_eq!(
            projection.rows[1].preview_urls,
            vec!["https://preview.example/pr-20"]
        );
        assert_eq!(projection.rows[2].ticket, "no ticket");
        assert!(projection.rows.iter().all(|row| row.age == "—"));
    }

    #[test]
    fn dock_home_age_is_time_since_pull_request_opened() {
        let binding = home_binding(0, 10, crate::detect::AgentState::Working);
        let mut indexed_item = item("owner/repo", 10, "indexed", &[], &[]);
        indexed_item.created_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(120));

        let projection = project_dock_home_at(
            &[binding],
            &[],
            Some(&snapshot(vec![indexed_item])),
            true,
            SystemTime::UNIX_EPOCH + Duration::from_secs(300),
        );

        assert_eq!(projection.rows[0].age, "3m");
    }

    #[test]
    fn dock_home_review_cell_prefers_draft_to_review_required() {
        let binding = home_binding(0, 10, crate::detect::AgentState::Idle);
        let mut draft = item("owner/repo", 10, "draft", &[], &[]);
        draft.draft = true;
        draft.review_decision = Some("REVIEW_REQUIRED".to_string());

        let projection = project_dock_home(&[binding], &[], Some(&snapshot(vec![draft])), true);

        assert_eq!(projection.rows[0].review, "D");
    }

    #[test]
    fn dock_home_unbound_counts_ignore_items_with_panes_and_none_without_snapshot() {
        let bound_pr = item("owner/repo", 1, "bound pr", &[], &[("pane-a", true)]);
        let unbound_pr = item("owner/repo", 2, "unbound pr", &[], &[]);
        let mut bound_ticket = item(
            "owner/repo",
            3,
            "bound ticket",
            &["SCA-3"],
            &[("pane-b", true)],
        );
        bound_ticket.pr_number = None;
        bound_ticket.pr_url = None;
        bound_ticket.pr_title = None;
        let mut unbound_ticket = item("owner/repo", 4, "unbound ticket", &["SCA-4"], &[]);
        unbound_ticket.pr_number = None;
        unbound_ticket.pr_url = None;
        unbound_ticket.pr_title = None;

        let projection = project_dock_home(
            &[],
            &[],
            Some(&snapshot(vec![
                bound_pr,
                unbound_pr,
                bound_ticket,
                unbound_ticket,
            ])),
            true,
        );
        assert_eq!(projection.unbound_prs, Some(1));
        assert_eq!(projection.unbound_tickets, Some(1));

        let unknown = project_dock_home(&[], &[], None, true);
        assert_eq!(unknown.unbound_prs, None);
        assert_eq!(unknown.unbound_tickets, None);
    }

    #[test]
    fn dock_home_selection_tracks_keys_and_clamps_at_both_ends() {
        let mut state = state_with_bound_prs(&[10, 20, 30]);
        state.work_index_snapshot = Some(snapshot(vec![
            item("owner/repo", 30, "thirty", &[], &[]),
            item("owner/repo", 20, "twenty", &[], &[]),
            item("owner/repo", 10, "ten", &[], &[]),
        ]));
        let initial = state.dock_home_projection();
        state.dock_home_selection = Some(initial.rows[1].key.clone());
        assert_eq!(state.dock_home_selected_index(&initial), Some(1));

        state.work_index_snapshot = Some(snapshot(vec![
            item("owner/repo", 10, "ten refreshed", &[], &[]),
            item("owner/repo", 30, "thirty refreshed", &[], &[]),
            item("owner/repo", 20, "twenty refreshed", &[], &[]),
        ]));
        let refreshed = state.dock_home_projection();
        assert_eq!(
            state
                .dock_home_selected_row()
                .expect("refreshed tab")
                .number,
            "20"
        );
        assert_eq!(state.dock_home_selected_index(&refreshed), Some(1));

        state.workspaces.swap(0, 1);
        let reordered = state.dock_home_projection();
        assert_eq!(state.dock_home_selected_index(&reordered), Some(0));

        state.workspaces.remove(0);
        let selected_gone = state.dock_home_projection();
        assert_eq!(state.dock_home_selected_index(&selected_gone), Some(0));

        let mut clamped = state_with_bound_prs(&[40, 50]);
        clamped.move_dock_home_selection(-1);
        assert_eq!(
            clamped.dock_home_selected_row().expect("top row").number,
            "40"
        );
        clamped.move_dock_home_selection(10);
        assert_eq!(
            clamped.dock_home_selected_row().expect("bottom row").number,
            "50"
        );
        clamped.move_dock_home_selection(1);
        assert_eq!(
            clamped
                .dock_home_selected_row()
                .expect("clamped bottom")
                .number,
            "50"
        );
    }

    #[test]
    fn selecting_detail_tab_preserves_pane_focus_and_typing_route() {
        let mut state = state_with_bound_prs(&[10]);
        state.dock_home_focused = false;
        let focused = state.current_pane_focus_target();

        state.set_dock_home_detail_tab(crate::app::state::DockHomeDetailTab::Files);

        assert_eq!(state.current_pane_focus_target(), focused);
        assert!(!state.dock_home_focused);
        assert_eq!(
            state.dock_home_detail_tab,
            crate::app::state::DockHomeDetailTab::Files
        );
    }

    #[test]
    fn dock_home_follows_changed_bound_pane_without_fighting_same_pane_selection() {
        let mut state = state_with_bound_prs(&[10, 20, 30]);
        let projection = state.dock_home_projection();
        let key_for = |number: &str| {
            projection
                .rows
                .iter()
                .find(|row| row.number == number)
                .expect("bound PR row")
                .key
                .clone()
        };
        state.dock_home_selection = Some(key_for("10"));
        state.dock_home_section = crate::app::state::DockHomeSection::Tickets;

        let second = state.workspaces[1].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(1, second));
        assert_eq!(state.dock_home_selection, Some(key_for("20")));
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Prs
        );

        state.dock_home_selection = Some(key_for("10"));
        assert!(!state.focus_pane_in_workspace(1, second));
        assert_eq!(state.dock_home_selection, Some(key_for("10")));
        state.switch_workspace(1);
        assert_eq!(state.dock_home_selection, Some(key_for("10")));

        let third = state.workspaces[2].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(2, third));
        assert_eq!(state.dock_home_selection, Some(key_for("30")));
    }

    fn state_with_bound_tickets(ticket_ids: &[&str]) -> crate::app::state::AppState {
        let mut state = crate::app::state::AppState::test_new();
        state.workspaces = ticket_ids
            .iter()
            .map(|ticket| crate::workspace::Workspace::test_new(ticket))
            .collect();
        state.active = (!state.workspaces.is_empty()).then_some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        for (ws_idx, ticket) in ticket_ids.iter().enumerate() {
            let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = state.workspaces[ws_idx]
                .terminal_id(pane_id)
                .expect("root terminal")
                .clone();
            state
                .terminals
                .get_mut(&terminal_id)
                .expect("terminal state")
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    ticket_ids: Some(vec![(*ticket).to_string()]),
                    ..Default::default()
                })
                .expect("valid work context");
        }
        state
    }

    #[test]
    fn dock_home_follows_a_focused_pane_to_its_ticket_when_it_has_no_pull_request() {
        let mut state = state_with_bound_tickets(&["SCA-100", "SCA-200"]);
        let projection = state.dock_home_projection();
        let key_for = |identifier: &str| {
            projection
                .ticket_rows
                .iter()
                .find(|row| row.ticket.identifier == identifier)
                .expect("bound ticket row")
                .key
                .clone()
        };
        state.dock_home_section = crate::app::state::DockHomeSection::Prs;

        let second = state.workspaces[1].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(1, second));

        assert_eq!(state.dock_home_ticket_selection, Some(key_for("SCA-200")));
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Tickets
        );

        let first = state.workspaces[0].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(0, first));
        assert_eq!(state.dock_home_ticket_selection, Some(key_for("SCA-100")));
    }

    #[test]
    fn dock_home_focus_on_unbound_pane_preserves_selection_and_section() {
        let mut state = state_with_bound_prs(&[10, 20]);
        state
            .workspaces
            .push(crate::workspace::Workspace::test_new("unbound"));
        state.ensure_test_terminals();
        let selected = state.dock_home_projection().rows[1].key.clone();
        state.dock_home_selection = Some(selected.clone());
        state.dock_home_section = crate::app::state::DockHomeSection::Tickets;

        let unbound = state.workspaces[2].tabs[0].root_pane;
        assert!(state.focus_pane_in_workspace(2, unbound));

        assert_eq!(state.dock_home_selection, Some(selected));
        assert_eq!(
            state.dock_home_section,
            crate::app::state::DockHomeSection::Tickets
        );
    }

    #[test]
    fn dock_home_ticket_rows_merge_snapshot_items_and_pane_ticket_ids() {
        let mut state = state_with_bound_prs(&[10]);
        let pane_id = state.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = state.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-200".into()]),
                ..Default::default()
            })
            .expect("work context");
        let mut ticket_only = item("owner/repo", 0, "unused", &["SCA-100"], &[]);
        ticket_only.pr_number = None;
        ticket_only.pr_url = None;
        ticket_only.pr_title = None;
        state.work_index_snapshot = Some(snapshot(vec![ticket_only]));

        let projection = state.dock_home_projection();
        assert_eq!(
            projection
                .ticket_rows
                .iter()
                .map(|row| row.ticket.identifier.as_str())
                .collect::<Vec<_>>(),
            vec!["SCA-100", "SCA-200"]
        );
        assert!(projection.ticket_rows[0].jump_target.is_none());
        assert_eq!(projection.ticket_rows[1].jump_target, Some((0, pane_id)));
    }

    #[test]
    fn each_home_section_keeps_its_key_selection_across_refresh_reordering() {
        let mut state = state_with_bound_prs(&[10, 20]);
        let pr_projection = state.dock_home_projection();
        state.dock_home_selection = Some(pr_projection.rows[1].key.clone());

        let mut first = item("owner/repo", 0, "unused", &["SCA-100"], &[]);
        first.pr_number = None;
        first.pr_url = None;
        first.pr_title = None;
        let mut second = item("owner/repo", 0, "unused", &["SCA-200"], &[]);
        second.pr_number = None;
        second.pr_url = None;
        second.pr_title = None;
        state.work_index_snapshot = Some(snapshot(vec![first.clone(), second.clone()]));
        let ticket_projection = state.dock_home_projection();
        state.dock_home_ticket_selection = Some(ticket_projection.ticket_rows[1].key.clone());

        state.work_index_snapshot = Some(snapshot(vec![second, first]));
        let refreshed = state.dock_home_projection();
        assert_eq!(
            state.dock_home_selected_row().expect("selected pr").number,
            "20"
        );
        let ticket = state
            .dock_home_selected_ticket_index(&refreshed)
            .and_then(|index| refreshed.ticket_rows.get(index))
            .expect("selected ticket");
        assert_eq!(ticket.ticket.identifier, "SCA-200");
    }

    #[test]
    fn ticket_selection_key_survives_repository_enrichment() {
        let mut before = item("", 0, "unused", &["SCA-200"], &[]);
        before.pr_number = None;
        before.pr_url = None;
        before.pr_title = None;
        let initial = project_dock_home(&[], &[], Some(&snapshot(vec![before])), true);
        let selected = initial.ticket_rows[0].key.clone();

        let mut after = item("owner/repo", 0, "unused", &["SCA-200"], &[]);
        after.pr_number = None;
        after.pr_url = None;
        after.pr_title = None;
        let refreshed = project_dock_home(&[], &[], Some(&snapshot(vec![after])), true);

        assert_eq!(refreshed.ticket_rows[0].key, selected);
    }

    #[test]
    fn dock_home_jump_is_a_noop_after_the_target_pane_disappears() {
        let mut state = state_with_bound_prs(&[10]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        state.dock_home_selection = state.dock_home_selected_row().map(|row| row.key);
        let active_before = state.active;
        let focused_before = state.workspaces[0].focused_pane_id();
        state.workspaces[0].tabs[0].panes.remove(&pane_id);

        assert!(!state.jump_to_dock_home_selection());
        assert_eq!(state.active, active_before);
        assert_eq!(state.workspaces[0].focused_pane_id(), focused_before);
    }

    #[test]
    fn pr_without_ticket_lands_in_counted_no_ticket_group() {
        let projection = project_pull_requests(
            &snapshot(vec![
                item("owner/repo", 1, "ticketed", &["SCA-1"], &[]),
                item("owner/repo", 2, "unticketed a", &[], &[]),
                item("owner/other", 3, "unticketed b", &[], &[]),
            ]),
            None,
        );
        let no_ticket = projection.groups.last().expect("trailing no ticket group");
        assert!(no_ticket.no_ticket);
        assert_eq!(no_ticket.header, "no ticket (2)");
        assert_eq!(no_ticket.rows.len(), 2);
        assert_eq!(no_ticket.rows[0].ticket, "no ticket");
    }

    #[test]
    fn ticket_cell_renders_id_count_or_no_ticket() {
        let projection = project_pull_requests(
            &snapshot(vec![
                item("owner/repo", 1, "one", &["SCA-1"], &[]),
                item("owner/repo", 2, "many", &["SCA-1", "SCA-2", "SCA-3"], &[]),
                item("owner/repo", 3, "none", &[], &[]),
            ]),
            None,
        );
        let rows: Vec<&WorkPrRow> = projection.flat_rows().collect();
        assert_eq!(rows[0].ticket, "3 tickets");
        assert_eq!(rows[1].ticket, "SCA-1");
        assert_eq!(rows[2].ticket, "no ticket");
    }

    #[test]
    fn active_owner_wins_and_unowned_renders_distinctly() {
        let projection = project_pull_requests(
            &snapshot(vec![
                item(
                    "owner/repo",
                    1,
                    "owned",
                    &["SCA-1"],
                    &[("pane-a", false), ("pane-b", true)],
                ),
                item(
                    "owner/repo",
                    2,
                    "participated",
                    &["SCA-2"],
                    &[("pane-c", false)],
                ),
                item("owner/repo", 3, "orphaned", &["SCA-3"], &[]),
            ]),
            None,
        );
        let row = |number| {
            projection
                .flat_rows()
                .find(|row| row.key.pr_number == Some(number))
                .expect("projected row")
        };
        assert_eq!(row(1).owner.as_deref(), Some("pane-b"));
        assert_eq!(row(1).extra_panes, 1);
        assert_eq!(row(1).owner_pane_id.as_deref(), Some("pane-b"));
        assert_eq!(row(2).owner, None);
        assert_eq!(row(2).extra_panes, 1);
        assert_eq!(row(2).owner_pane_id, None);
        assert_eq!(row(3).owner, None);
        assert_eq!(row(3).extra_panes, 0);
    }

    #[test]
    fn owner_cell_prefers_the_agent_label_over_the_pane_id() {
        // The approved layout shows who owns the work (`cc·opus·high`);
        // a pane id names nobody, so it is only the fallback.
        let mut labelled = item("owner/repo", 1, "owned", &["SCA-1"], &[("pane-a", true)]);
        labelled.panes[0].agent_label = Some("cc·opus·high".to_string());
        let unlabelled = item("owner/repo", 2, "owned", &["SCA-2"], &[("pane-b", true)]);
        let projection = project_pull_requests(&snapshot(vec![labelled, unlabelled]), None);
        let row = |number| {
            projection
                .flat_rows()
                .find(|row| row.key.pr_number == Some(number))
                .expect("projected row")
        };
        assert_eq!(row(1).owner.as_deref(), Some("cc·opus·high"));
        assert_eq!(row(1).owner_pane_id.as_deref(), Some("pane-a"));
        assert_eq!(row(2).owner.as_deref(), Some("pane-b"));
    }

    #[test]
    fn groups_by_repo_and_filter_narrows_to_one_repo() {
        let snapshot = snapshot(vec![
            item("owner/b", 1, "b one", &["SCA-1"], &[]),
            item("owner/a", 2, "a one", &["SCA-2"], &[]),
            item("owner/b", 3, "b two", &["SCA-3"], &[]),
        ]);
        let projection = project_pull_requests(&snapshot, None);
        let headers: Vec<&str> = projection
            .groups
            .iter()
            .map(|group| group.header.as_str())
            .collect();
        assert_eq!(headers, ["owner/a (1)", "owner/b (2)"]);

        let filtered = project_pull_requests(&snapshot, Some("owner/b"));
        assert_eq!(filtered.row_count, 2);
        assert!(filtered.flat_rows().all(|row| row.repo == "owner/b"));

        let mut view = view(vec![
            item("owner/b", 1, "b one", &["SCA-1"], &[]),
            item("owner/a", 2, "a one", &["SCA-2"], &[]),
        ]);
        assert_eq!(view.cycle_repo_filter().as_deref(), Some("owner/a"));
        assert_eq!(view.cycle_repo_filter().as_deref(), Some("owner/b"));
        assert_eq!(view.cycle_repo_filter(), None);
    }

    #[test]
    fn rotation_round_trip_preserves_the_selected_pr() {
        let mut view = view(vec![
            item("owner/repo", 1, "one", &["SCA-1"], &[]),
            item("owner/repo", 2, "two", &["SCA-2"], &[]),
            item("owner/repo", 3, "three", &["SCA-3"], &[]),
        ]);
        view.move_selection(1);
        let selected = view.selected_row().expect("selected row");
        assert_eq!(selected.title, "two");
        for _ in 0..4 {
            view.rotate(true);
        }
        assert_eq!(view.projection, WorkProjection::PullRequests);
        let after = view.selected_row().expect("selected row after rotation");
        assert_eq!(after.key, selected.key);
        for _ in 0..4 {
            view.rotate(false);
        }
        assert_eq!(view.projection, WorkProjection::PullRequests);
        assert_eq!(view.selected_row().expect("selected row").key, selected.key);
    }

    #[test]
    fn replace_snapshot_keeps_selection_on_the_same_work_item() {
        let mut view = view(vec![
            item("owner/repo", 1, "one", &["SCA-1"], &[]),
            item("owner/repo", 2, "two", &["SCA-2"], &[]),
            item("owner/repo", 3, "three", &["SCA-3"], &[]),
        ]);
        view.move_selection(1);
        let key = view.selected_row().expect("selected row").key;
        // A refresh adds a newer PR on top; the selection must follow PR 2.
        view.replace_snapshot(snapshot(vec![
            item("owner/repo", 1, "one", &["SCA-1"], &[]),
            item("owner/repo", 2, "two", &["SCA-2"], &[]),
            item("owner/repo", 3, "three", &["SCA-3"], &[]),
            item("owner/repo", 4, "four", &["SCA-4"], &[]),
        ]));
        assert_eq!(view.selected_row().expect("selected row").key, key);
    }

    #[test]
    fn replace_snapshot_falls_back_when_the_selected_pr_vanishes() {
        let mut view = view(vec![
            item("owner/repo", 1, "one", &["SCA-1"], &[]),
            item("owner/repo", 2, "two", &["SCA-2"], &[]),
        ]);
        view.move_selection(1);
        view.replace_snapshot(snapshot(vec![item(
            "owner/repo",
            1,
            "one",
            &["SCA-1"],
            &[],
        )]));
        // PR 2 merged away: fall back to the first row, never a dead pointer.
        let row = view.selected_row().expect("fallback row");
        assert_eq!(row.key.pr_number, Some(1));

        view.replace_snapshot(snapshot(Vec::new()));
        assert!(view.selected_row().is_none());
    }

    #[test]
    fn review_cell_maps_draft_and_decisions() {
        let mut draft = item("owner/repo", 1, "draft", &["SCA-1"], &[]);
        draft.draft = true;
        draft.review_decision = Some("REVIEW_REQUIRED".to_string());
        let mut approved = item("owner/repo", 2, "approved", &["SCA-1"], &[]);
        approved.review_decision = Some("APPROVED".to_string());
        let mut changes = item("owner/repo", 3, "changes", &["SCA-1"], &[]);
        changes.review_decision = Some("CHANGES_REQUESTED".to_string());
        let mut required = item("owner/repo", 4, "required", &["SCA-1"], &[]);
        required.review_decision = Some("REVIEW_REQUIRED".to_string());
        let projection =
            project_pull_requests(&snapshot(vec![draft, approved, changes, required]), None);
        let reviews: Vec<&str> = projection
            .flat_rows()
            .map(|row| row.review.as_str())
            .collect();
        assert_eq!(reviews, ["RR", "✗", "✓", "D"]);
    }

    #[test]
    fn selection_defaults_to_the_first_row_and_clamps_at_the_edges() {
        let mut view = view(vec![
            item("owner/repo", 1, "one", &["SCA-1"], &[]),
            item("owner/repo", 2, "two", &["SCA-2"], &[]),
        ]);
        // Rows sort by PR number descending.
        assert_eq!(
            view.selected_row().expect("first row").key.pr_number,
            Some(2)
        );
        view.move_selection(-1);
        assert_eq!(
            view.selected_row().expect("clamped top").key.pr_number,
            Some(2)
        );
        view.move_selection(5);
        assert_eq!(
            view.selected_row().expect("clamped bottom").key.pr_number,
            Some(1)
        );
    }
}
