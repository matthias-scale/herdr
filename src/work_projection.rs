//! Pull-request projection of the repo-wide work index.
//!
//! The projection is pure data: snapshot in, rows and groups out. Selection,
//! rotation, and repo filtering live here so the renderer stays dumb and the
//! rules stay testable without a PTY. Options B/C/D (tickets, agents, review
//! queue) slot in as additional render arms over the same `WorkViewState`.

use crate::app::state::{WorkItemKey, WorkProjection, WorkViewState};
use crate::work_context::repo_slugs_match;
use crate::work_index::{Snapshot, WorkItem};

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

fn item_key(item: &WorkItem) -> WorkItemKey {
    WorkItemKey {
        repo: item.repo.clone(),
        pr_number: item.pr_number,
        pr_url: item.pr_url.clone(),
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
    // Draft wins over the review decision: GitHub marks drafts REVIEW_REQUIRED
    // at open time, and the draft state is the more actionable signal.
    let review = if item.draft {
        "D"
    } else {
        match item.review_decision.as_deref() {
            Some("REVIEW_REQUIRED") => "RR",
            Some("APPROVED") => "✓",
            Some("CHANGES_REQUESTED") => "✗",
            _ => "—",
        }
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
        review: review.to_string(),
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
            ticket_ids: tickets.iter().map(|ticket| ticket.to_string()).collect(),
            ticket_title: None,
            ticket_state: None,
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
