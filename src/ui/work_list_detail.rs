use std::time::{Duration, SystemTime};

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::app::state::Palette;
use crate::ui::text::{display_width, truncate_end};
use crate::work_index::{
    MissiveConversation, MissiveEntry, PrAudience, PrCheckState, WorkItem as IndexedWorkItem,
    WorkItemComment, WorkItemDetail as IndexedWorkItemDetail, WorkTicket,
};

pub(crate) fn section_separator(
    palette: &Palette,
    title: impl AsRef<str>,
    width: u16,
) -> [Line<'static>; 2] {
    let width = usize::from(width);
    let label = truncate_end(&format!("─ {} ", title.as_ref()), width);
    let rule = format!(
        "{label}{}",
        "─".repeat(width.saturating_sub(display_width(&label)))
    );
    let style = Style::default()
        .fg(palette.subtext0)
        .add_modifier(Modifier::DIM);
    [Line::default(), Line::from(Span::styled(rule, style))]
}

pub(crate) fn comment_header(comment: &WorkItemComment, observed_at: SystemTime) -> String {
    let author = comment.author.as_deref().unwrap_or("unknown");
    let age = comment
        .created_at
        .map(|created_at| age(Some(created_at), observed_at))
        .unwrap_or_else(|| "—".into());
    format!("{author} · {age}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkRow {
    pub(crate) group: &'static str,
    pub(crate) glyph: &'static str,
    pub(crate) title: String,
    pub(crate) metadata: String,
    pub(crate) changes: String,
    pub(crate) age: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkDetail {
    pub(crate) heading: String,
    pub(crate) title: String,
    pub(crate) byline: String,
    pub(crate) branches: String,
    pub(crate) checks_summary: String,
    pub(crate) reviewers: String,
    pub(crate) description: Option<String>,
    pub(crate) checks: Vec<(String, String)>,
    pub(crate) comments: Vec<WorkItemComment>,
    pub(crate) linked_prs: Vec<LinkedPr>,
    pub(crate) sections: Vec<WorkDetailSection>,
    pub(crate) open_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkDetailSection {
    pub(crate) label: &'static str,
    pub(crate) entries: Vec<WorkItemComment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkedPr {
    pub(crate) repo: String,
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) check_state: PrCheckState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkActionKind {
    CheckOut,
    Land,
    FixInThread,
    StartThread,
    Transition,
    LinkPr,
    More,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkAction {
    pub(crate) kind: WorkActionKind,
    pub(crate) label: &'static str,
    pub(crate) enabled: bool,
}

pub(crate) trait WorkItem {
    fn key(&self) -> String;
    fn row(&self) -> WorkRow;
    fn detail(&self) -> WorkDetail;
    fn actions(&self) -> Vec<WorkAction>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PrSort {
    #[default]
    Updated,
    Created,
    Number,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TicketSort {
    #[default]
    Updated,
    Priority,
    Identifier,
}

impl TicketSort {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Updated => Self::Priority,
            Self::Priority => Self::Identifier,
            Self::Identifier => Self::Updated,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Priority => "priority",
            Self::Identifier => "identifier",
        }
    }
}

impl PrSort {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Updated => Self::Created,
            Self::Created => Self::Number,
            Self::Number => Self::Updated,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Created => "created",
            Self::Number => "number",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PrItem<'a> {
    pub(crate) summary: &'a IndexedWorkItem,
    pub(crate) cached_detail: Option<&'a IndexedWorkItemDetail>,
    pub(crate) observed_at: SystemTime,
    pub(crate) approval_label: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrApprovalSignal {
    ApprovedReview,
    Label(String),
}

impl PrApprovalSignal {
    pub(crate) fn confirmation_label(&self) -> String {
        match self {
            Self::ApprovedReview => "approved review".into(),
            Self::Label(label) => format!("label {label}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PrLandStatus {
    Enabled(PrApprovalSignal),
    AwaitingApproval,
    Blocked,
}

pub(crate) fn pr_land_status(detail: &IndexedWorkItemDetail, approval_label: &str) -> PrLandStatus {
    let checks_and_merge_ready = !detail.actions.is_empty()
        && detail.actions.iter().all(|check| check.state == "SUCCESS")
        && detail.merge_state_status.as_deref() == Some("CLEAN");
    if !checks_and_merge_ready {
        return PrLandStatus::Blocked;
    }
    if detail
        .review_decision
        .as_deref()
        .is_some_and(|decision| decision.eq_ignore_ascii_case("APPROVED"))
    {
        return PrLandStatus::Enabled(PrApprovalSignal::ApprovedReview);
    }
    if detail.labels.iter().any(|label| label == approval_label) {
        return PrLandStatus::Enabled(PrApprovalSignal::Label(approval_label.into()));
    }
    PrLandStatus::AwaitingApproval
}

impl PrItem<'_> {
    pub(crate) fn is_open(&self) -> bool {
        self.summary
            .pr_state
            .as_deref()
            .is_none_or(|state| state.eq_ignore_ascii_case("open"))
    }

    pub(crate) fn matches(&self, query: &str) -> bool {
        let mut text_terms = Vec::new();
        for token in query.split_whitespace() {
            if let Some(label) = token.strip_prefix("label:") {
                if !self
                    .summary
                    .labels
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(label))
                {
                    return false;
                }
            } else {
                text_terms.push(token.to_ascii_lowercase());
            }
        }
        let number = self.summary.pr_number.map(|number| format!("#{number}"));
        let haystack = format!(
            "{} {}",
            self.summary.pr_title.as_deref().unwrap_or_default(),
            number.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        text_terms.iter().all(|term| haystack.contains(term))
    }

    pub(crate) fn land_enabled(&self) -> bool {
        let Some(detail) = self.cached_detail else {
            return false;
        };
        matches!(
            pr_land_status(detail, self.approval_label),
            PrLandStatus::Enabled(_)
        )
    }

    pub(crate) fn land_status(&self) -> PrLandStatus {
        self.cached_detail.map_or(PrLandStatus::Blocked, |detail| {
            pr_land_status(detail, self.approval_label)
        })
    }
}

impl WorkItem for PrItem<'_> {
    fn key(&self) -> String {
        format!(
            "{}#{}",
            self.summary.repo,
            self.summary.pr_number.unwrap_or_default()
        )
    }

    fn row(&self) -> WorkRow {
        let state = self.summary.pr_state.as_deref().unwrap_or("open");
        let glyph = if self.summary.check_state == PrCheckState::Failing {
            "⚠"
        } else if state.eq_ignore_ascii_case("merged") {
            "✓"
        } else if state.eq_ignore_ascii_case("closed") {
            "✗"
        } else {
            "⑂"
        };
        let check = match self.summary.check_state {
            PrCheckState::Passing => "✓",
            PrCheckState::Failing => "✗",
            PrCheckState::Pending => "◌",
            PrCheckState::Unknown => "—",
        };
        WorkRow {
            group: if self.summary.audience == PrAudience::Authored {
                "Authored"
            } else {
                "Others"
            },
            glyph,
            title: self
                .summary
                .pr_title
                .clone()
                .unwrap_or_else(|| "(untitled PR)".into()),
            metadata: format!(
                "#{} · {} · {check}",
                self.summary.pr_number.unwrap_or_default(),
                self.summary
                    .repo
                    .rsplit('/')
                    .next()
                    .unwrap_or(&self.summary.repo)
            ),
            changes: format!("+{} −{}", self.summary.additions, self.summary.deletions),
            age: age(
                self.summary.updated_at.or(self.summary.created_at),
                self.observed_at,
            ),
        }
    }

    fn detail(&self) -> WorkDetail {
        let Some(detail) = self.cached_detail else {
            return WorkDetail {
                heading: format!(
                    "{} #{} ↗",
                    self.summary.repo,
                    self.summary.pr_number.unwrap_or_default()
                ),
                title: self
                    .summary
                    .pr_title
                    .clone()
                    .unwrap_or_else(|| "(untitled PR)".into()),
                ..WorkDetail::default()
            };
        };
        let passing = detail
            .actions
            .iter()
            .filter(|check| check.state == "SUCCESS")
            .count();
        let total = detail.actions.len();
        WorkDetail {
            heading: format!(
                "{} #{} ↗",
                self.summary.repo,
                detail.number.or(self.summary.pr_number).unwrap_or_default()
            ),
            title: detail
                .title
                .clone()
                .or_else(|| self.summary.pr_title.clone())
                .unwrap_or_else(|| "(untitled PR)".into()),
            byline: format!(
                "{} · updated {}",
                detail.author.as_deref().unwrap_or("unknown"),
                age(detail.updated_at, self.observed_at)
            ),
            branches: format!(
                "{}{} ← {}",
                detail.base_ref_name.as_deref().unwrap_or("base"),
                if detail.merge_state_status.as_deref() == Some("BEHIND") {
                    " ⚠"
                } else {
                    ""
                },
                detail.head_ref_name.as_deref().unwrap_or("head")
            ),
            checks_summary: format!("✓ {passing} of {total} passing"),
            reviewers: if detail.reviewers.is_empty() {
                "—".into()
            } else {
                detail.reviewers.join(", ")
            },
            description: detail.body.clone(),
            checks: detail
                .actions
                .iter()
                .map(|check| (check.name.clone(), check.state.clone()))
                .collect(),
            linked_prs: Vec::new(),
            sections: Vec::new(),
            open_url: detail.url.clone().or_else(|| self.summary.pr_url.clone()),
            comments: {
                let mut comments = detail.comments.clone();
                comments.sort_by_key(|comment| std::cmp::Reverse(comment.created_at));
                comments
            },
        }
    }

    fn actions(&self) -> Vec<WorkAction> {
        vec![
            WorkAction {
                kind: WorkActionKind::CheckOut,
                label: "Check out ▾",
                enabled: true,
            },
            WorkAction {
                kind: WorkActionKind::Land,
                label: "Land",
                enabled: self.land_enabled(),
            },
            WorkAction {
                kind: WorkActionKind::FixInThread,
                label: "Fix in a thread",
                enabled: self
                    .cached_detail
                    .is_some_and(|detail| !detail.comments.is_empty()),
            },
        ]
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TicketItem<'a> {
    pub(crate) summary: &'a WorkTicket,
    pub(crate) cached_detail: Option<&'a IndexedWorkItemDetail>,
    pub(crate) linked_prs: Vec<&'a IndexedWorkItem>,
    pub(crate) observed_at: SystemTime,
    pub(crate) has_context_pr: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ConversationItem<'a> {
    pub(crate) summary: &'a MissiveConversation,
    pub(crate) observed_at: SystemTime,
}

impl ConversationItem<'_> {
    pub(crate) fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        query.is_empty()
            || self.summary.subject.to_ascii_lowercase().contains(&query)
            || self
                .summary
                .assignees
                .iter()
                .any(|user| user.name.to_ascii_lowercase().contains(&query))
    }
}

fn missive_age(then: Option<SystemTime>, now: SystemTime) -> String {
    then.map_or_else(|| "unknown".into(), |then| age(Some(then), now))
}

fn missive_comments(entries: &[MissiveEntry]) -> Vec<WorkItemComment> {
    entries
        .iter()
        .map(|entry| WorkItemComment {
            author: entry.author.clone(),
            body: entry.preview.clone(),
            created_at: entry.created_at,
        })
        .collect()
}

impl WorkItem for ConversationItem<'_> {
    fn key(&self) -> String {
        self.summary.id.clone()
    }

    fn row(&self) -> WorkRow {
        let (group, glyph) = if self.summary.closed {
            ("Closed", "●")
        } else if self.summary.assignees.is_empty() {
            ("Unassigned", "◌")
        } else if self.summary.assignees.iter().any(|user| user.is_me) {
            ("Assigned to me", "○")
        } else {
            ("Assigned", "○")
        };
        WorkRow {
            group,
            glyph,
            title: self.summary.subject.clone(),
            metadata: if self.summary.assignees.is_empty() {
                "unassigned".into()
            } else {
                self.summary
                    .assignees
                    .iter()
                    .map(|user| user.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            changes: String::new(),
            age: missive_age(self.summary.last_activity_at, self.observed_at),
        }
    }

    fn detail(&self) -> WorkDetail {
        WorkDetail {
            heading: "Missive ↗".into(),
            title: self.summary.subject.clone(),
            byline: format!(
                "{} · {}",
                if self.summary.closed {
                    "closed"
                } else {
                    "open"
                },
                if self.summary.assignees.is_empty() {
                    "unassigned".into()
                } else {
                    self.summary
                        .assignees
                        .iter()
                        .map(|user| user.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
            sections: vec![
                WorkDetailSection {
                    label: "Messages",
                    entries: missive_comments(&self.summary.messages),
                },
                WorkDetailSection {
                    label: "Internal notes",
                    entries: missive_comments(&self.summary.notes),
                },
                WorkDetailSection {
                    label: "Drafts",
                    entries: missive_comments(&self.summary.drafts),
                },
                WorkDetailSection {
                    label: "Posts",
                    entries: missive_comments(&self.summary.posts),
                },
            ],
            open_url: Some(self.summary.app_url.clone()),
            ..WorkDetail::default()
        }
    }

    fn actions(&self) -> Vec<WorkAction> {
        vec![
            WorkAction {
                kind: WorkActionKind::StartThread,
                label: "Start thread ▾",
                enabled: true,
            },
            WorkAction {
                kind: WorkActionKind::More,
                label: "Open in Missive",
                enabled: true,
            },
        ]
    }
}

pub(crate) fn sorted_filtered_conversations<'a>(
    conversations: &'a [MissiveConversation],
    query: &str,
    show_closed: bool,
    observed_at: SystemTime,
) -> Vec<ConversationItem<'a>> {
    let mut rows = conversations
        .iter()
        .map(|summary| ConversationItem {
            summary,
            observed_at,
        })
        .filter(|item| (show_closed || !item.summary.closed) && item.matches(query))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let rank = |item: &ConversationItem<'_>| {
            if item.summary.closed {
                2
            } else if item.summary.assignees.is_empty() {
                1
            } else {
                0
            }
        };
        rank(left).cmp(&rank(right)).then_with(|| {
            right
                .summary
                .last_activity_at
                .cmp(&left.summary.last_activity_at)
        })
    });
    rows
}

impl TicketItem<'_> {
    pub(crate) fn is_open(&self) -> bool {
        !self
            .summary
            .state
            .as_deref()
            .is_some_and(|state| matches_ci(state, &["done", "completed", "cancelled", "canceled"]))
    }

    pub(crate) fn matches(&self, query: &str) -> bool {
        let mut text_terms = Vec::new();
        for token in query.split_whitespace() {
            if let Some(label) = token.strip_prefix("label:") {
                if !self
                    .summary
                    .labels
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(label))
                {
                    return false;
                }
            } else {
                text_terms.push(token.to_ascii_lowercase());
            }
        }
        let haystack = format!(
            "{} {}",
            self.summary.identifier,
            self.summary.title.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        text_terms.iter().all(|term| haystack.contains(term))
    }

    pub(crate) fn transition_enabled(&self, target: &str) -> bool {
        !self
            .summary
            .state
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case(target))
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.cached_detail
            .and_then(|detail| detail.url.as_deref())
            .or(self.summary.url.as_deref())
    }
}

impl WorkItem for TicketItem<'_> {
    fn key(&self) -> String {
        self.summary.identifier.clone()
    }

    fn row(&self) -> WorkRow {
        let state = self.summary.state.as_deref().unwrap_or("Todo");
        let glyph = if matches_ci(state, &["done", "completed", "cancelled", "canceled"]) {
            "✓"
        } else if state.to_ascii_lowercase().contains("block") || self.summary.priority == Some(1) {
            "●"
        } else if matches_ci(state, &["in progress", "started", "in review"]) {
            "◐"
        } else {
            "○"
        };
        WorkRow {
            group: match self.summary.group {
                crate::work_index::TicketGroup::Assigned => "Assigned to me",
                crate::work_index::TicketGroup::Triage => "Triage",
                crate::work_index::TicketGroup::DoneThisCycle => "Done (this cycle)",
            },
            glyph,
            title: self
                .summary
                .title
                .clone()
                .unwrap_or_else(|| "(untitled ticket)".into()),
            metadata: self.summary.identifier.clone(),
            changes: self
                .summary
                .priority
                .map(|priority| format!("P{priority}"))
                .unwrap_or_else(|| "—".into()),
            age: age(
                self.summary.updated_at.or(self.summary.created_at),
                self.observed_at,
            ),
        }
    }

    fn detail(&self) -> WorkDetail {
        let description = self
            .cached_detail
            .and_then(|detail| detail.body.clone())
            .or_else(|| self.summary.description.clone());
        let comments = self.cached_detail.map_or_else(Vec::new, |detail| {
            let mut comments = detail.comments.clone();
            comments.sort_by_key(|comment| std::cmp::Reverse(comment.created_at));
            comments
        });
        let linked_prs = self
            .linked_prs
            .iter()
            .filter_map(|item| {
                Some(LinkedPr {
                    repo: item.repo.clone(),
                    number: item.pr_number?,
                    title: item
                        .pr_title
                        .clone()
                        .unwrap_or_else(|| "(untitled PR)".into()),
                    url: item.pr_url.clone()?,
                    check_state: item.check_state,
                })
            })
            .collect();
        WorkDetail {
            heading: format!("{} ↗", self.summary.identifier),
            title: self
                .summary
                .title
                .clone()
                .or_else(|| self.cached_detail.and_then(|detail| detail.title.clone()))
                .unwrap_or_else(|| "(untitled ticket)".into()),
            byline: format!(
                "{} · {} · {} · {}",
                self.summary.state.as_deref().unwrap_or("unknown"),
                self.summary
                    .priority
                    .map(|priority| format!("P{priority}"))
                    .unwrap_or_else(|| "—".into()),
                self.summary.assignee.as_deref().unwrap_or("unassigned"),
                self.summary.cycle.as_deref().unwrap_or("no cycle")
            ),
            description: description.clone(),
            checks: checklist_items(description.as_deref())
                .into_iter()
                .map(|(checked, text)| {
                    (
                        text,
                        if checked {
                            "done".into()
                        } else {
                            "todo".into()
                        },
                    )
                })
                .collect(),
            comments,
            linked_prs,
            ..WorkDetail::default()
        }
    }

    fn actions(&self) -> Vec<WorkAction> {
        vec![
            WorkAction {
                kind: WorkActionKind::StartThread,
                label: "Start thread ▾",
                enabled: self.url().is_some(),
            },
            WorkAction {
                kind: WorkActionKind::Transition,
                label: "Transition ▾",
                enabled: true,
            },
            WorkAction {
                kind: WorkActionKind::LinkPr,
                label: "Link PR",
                enabled: self.has_context_pr,
            },
            WorkAction {
                kind: WorkActionKind::More,
                label: "⋯",
                enabled: true,
            },
        ]
    }
}

pub(crate) fn sorted_filtered_tickets<'a>(
    items: &'a [IndexedWorkItem],
    details: &'a crate::work_index::WorkItemDetailCache,
    query: &str,
    sort: TicketSort,
    open_only: bool,
    observed_at: SystemTime,
    has_context_pr: bool,
    sidebar_filter: Option<(
        &crate::app::state::SidebarWorkFilter,
        &crate::work_index::WorkIndexSession,
    )>,
) -> Vec<TicketItem<'a>> {
    let mut seen = std::collections::HashSet::new();
    let tickets = items
        .iter()
        .flat_map(|item| item.ticket_details.iter())
        .filter(|ticket| seen.insert(ticket.identifier.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    let mut rows = tickets
        .into_iter()
        .map(|summary| {
            let key = crate::app::state::WorkItemKey {
                repo: String::new(),
                pr_number: None,
                pr_url: None,
                ticket_id: Some(summary.identifier.clone()),
            };
            TicketItem {
                summary,
                cached_detail: details.get(&key),
                linked_prs: items
                    .iter()
                    .filter(|item| {
                        item.pr_number.is_some()
                            && item
                                .ticket_ids
                                .iter()
                                .any(|ticket| ticket.eq_ignore_ascii_case(&summary.identifier))
                    })
                    .collect(),
                observed_at,
                has_context_pr,
            }
        })
        .filter(|item| {
            (!open_only || item.is_open())
                && item.matches(query)
                && sidebar_filter
                    .is_none_or(|(filter, session)| filter.matches_linear(item.summary, session))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        ticket_group_rank(left.summary.group)
            .cmp(&ticket_group_rank(right.summary.group))
            .then_with(|| match sort {
                TicketSort::Updated => right.summary.updated_at.cmp(&left.summary.updated_at),
                TicketSort::Priority => left
                    .summary
                    .priority
                    .unwrap_or(u8::MAX)
                    .cmp(&right.summary.priority.unwrap_or(u8::MAX)),
                TicketSort::Identifier => left.summary.identifier.cmp(&right.summary.identifier),
            })
    });
    rows
}

fn ticket_group_rank(group: crate::work_index::TicketGroup) -> u8 {
    match group {
        crate::work_index::TicketGroup::Assigned => 0,
        crate::work_index::TicketGroup::Triage => 1,
        crate::work_index::TicketGroup::DoneThisCycle => 2,
    }
}

fn matches_ci(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

pub(crate) fn checklist_items(description: Option<&str>) -> Vec<(bool, String)> {
    description
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| {
            let line = line.trim_start();
            let checked = if line.starts_with("- [x] ") || line.starts_with("- [X] ") {
                true
            } else if line.starts_with("- [ ] ") {
                false
            } else {
                return None;
            };
            Some((checked, line[6..].to_string()))
        })
        .collect()
}

pub(crate) fn description_without_checklist(description: Option<&str>) -> Option<String> {
    let description = description?;
    let body = description
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !(line.starts_with("- [ ] ")
                || line.starts_with("- [x] ")
                || line.starts_with("- [X] "))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_string())
}

/// The branch Herdr derives from a ticket. `prefix` is
/// `source_control.branch_prefix`; an empty prefix leaves the bare slug.
pub(crate) fn ticket_worktree_branch(prefix: &str, identifier: &str, title: &str) -> String {
    let mut slug = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let prefix = format!("{prefix}{}-", identifier.to_ascii_lowercase());
    let available = 40usize.saturating_sub(prefix.chars().count());
    slug = slug.trim_matches('-').chars().take(available).collect();
    slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        prefix.trim_end_matches('-').to_string()
    } else {
        format!("{prefix}{slug}")
    }
}

pub(crate) fn sorted_filtered_prs<'a>(
    items: &'a [IndexedWorkItem],
    details: &'a crate::work_index::WorkItemDetailCache,
    query: &str,
    sort: PrSort,
    open_only: bool,
    observed_at: SystemTime,
    approval_label: &'a str,
    sidebar_filter: Option<(
        &crate::app::state::SidebarWorkFilter,
        &crate::work_index::WorkIndexSession,
    )>,
) -> Vec<PrItem<'a>> {
    let mut rows = items
        .iter()
        .filter(|item| item.pr_number.is_some() && item.audience != PrAudience::Unclassified)
        .map(|summary| {
            let key = crate::app::state::WorkItemKey {
                repo: summary.repo.clone(),
                pr_number: summary.pr_number,
                pr_url: summary.pr_url.clone(),
                ticket_id: None,
            };
            PrItem {
                summary,
                cached_detail: details.get(&key),
                observed_at,
                approval_label,
            }
        })
        .filter(|item| {
            (!open_only || item.is_open())
                && item.matches(query)
                && sidebar_filter
                    .is_none_or(|(filter, session)| filter.matches_github(item.summary, session))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.row()
            .group
            .cmp(right.row().group)
            .then_with(|| match sort {
                PrSort::Updated => right.summary.updated_at.cmp(&left.summary.updated_at),
                PrSort::Created => right.summary.created_at.cmp(&left.summary.created_at),
                PrSort::Number => right.summary.pr_number.cmp(&left.summary.pr_number),
            })
    });
    rows
}

fn age(then: Option<SystemTime>, now: SystemTime) -> String {
    let elapsed = then
        .and_then(|then| now.duration_since(then).ok())
        .unwrap_or(Duration::ZERO);
    if elapsed.as_secs() >= 86_400 {
        format!("{}d", elapsed.as_secs() / 86_400)
    } else if elapsed.as_secs() >= 3_600 {
        format!("{}h", elapsed.as_secs() / 3_600)
    } else {
        format!("{}m", elapsed.as_secs() / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_index::{
        TicketGroup, WorkItemAction, WorkItemCheckSummary, WorkItemSource, WorkTicket,
    };

    fn item(number: u64, audience: PrAudience, updated: u64) -> IndexedWorkItem {
        IndexedWorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(number),
            pr_url: Some(format!("https://github.com/owner/repo/pull/{number}")),
            pr_title: Some(format!("PR {number}")),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(number)),
            updated_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(updated)),
            additions: number,
            deletions: 1,
            author: Some("ada".into()),
            assignees: vec!["ada".into()],
            labels: vec!["bug".into()],
            check_state: PrCheckState::Passing,
            audience,
            ticket_ids: Vec::new(),
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: Some(format!("pr-{number}")),
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn detail(states: &[&str], merge: &str, with_comment: bool) -> IndexedWorkItemDetail {
        let mut detail = IndexedWorkItemDetail::empty();
        detail.actions = states
            .iter()
            .enumerate()
            .map(|(index, state)| WorkItemAction {
                name: format!("check-{index}"),
                state: (*state).into(),
            })
            .collect();
        detail.checks = Some(WorkItemCheckSummary {
            failing: states.iter().filter(|state| **state != "SUCCESS").count(),
            total: states.len(),
        });
        detail.merge_state_status = Some(merge.into());
        detail.head_sha = Some("abc123".into());
        if with_comment {
            detail.comments.push(WorkItemComment {
                author: Some("grace".into()),
                body: "fix this".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            });
        }
        detail
    }

    fn ticket(identifier: &str, group: TicketGroup, priority: u8, updated: u64) -> WorkTicket {
        WorkTicket {
            identifier: identifier.into(),
            title: Some(format!("Ticket {identifier}")),
            description: Some("- [x] first\n- [ ] second".into()),
            state: Some(if group == TicketGroup::DoneThisCycle {
                "Done".into()
            } else {
                "In Progress".into()
            }),
            assignee: Some("ada".into()),
            priority: Some(priority),
            cycle: Some("cycle 34".into()),
            group,
            created_at: Some(SystemTime::UNIX_EPOCH),
            updated_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(updated)),
            branch: None,
            labels: vec!["bug".into()],
            url: Some(format!("https://linear.app/acme/issue/{identifier}")),
            parent: None,
            relations: Vec::new(),
        }
    }

    fn ticket_item(ticket: WorkTicket) -> IndexedWorkItem {
        IndexedWorkItem {
            repo: "owner/repo".into(),
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
            ticket_details: vec![ticket],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    fn conversation(id: &str, closed: bool, assigned: bool) -> MissiveConversation {
        MissiveConversation {
            id: id.into(),
            subject: format!("Conversation {id}"),
            app_url: format!("https://mail.missiveapp.com/#inbox/conversations/{id}"),
            web_url: format!("https://mail.missiveapp.com/#inbox/conversations/{id}"),
            assignees: assigned
                .then(|| crate::work_index::MissiveUser {
                    id: "ada".into(),
                    name: "Ada".into(),
                    email: None,
                    is_me: true,
                })
                .into_iter()
                .collect(),
            last_activity_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(20)),
            closed,
            messages: vec![crate::work_index::MissiveEntry {
                id: "message".into(),
                author: Some("Customer".into()),
                preview: "message preview".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            notes: vec![crate::work_index::MissiveEntry {
                id: "note".into(),
                author: Some("Ada".into()),
                preview: "internal note".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            drafts: Vec::new(),
            posts: Vec::new(),
        }
    }

    #[test]
    fn conversation_item_projects_list_detail_and_actions() {
        let conversation = conversation("open", false, true);
        let item = ConversationItem {
            summary: &conversation,
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
        };
        let row = item.row();
        assert_eq!(row.group, "Assigned to me");
        assert_eq!(row.glyph, "○");
        assert_eq!(row.metadata, "Ada");
        let detail = item.detail();
        assert_eq!(detail.heading, "Missive ↗");
        assert_eq!(detail.sections[0].label, "Messages");
        assert_eq!(detail.sections[0].entries[0].body, "message preview");
        assert_eq!(detail.sections[1].label, "Internal notes");
        assert_eq!(
            detail.open_url.as_deref(),
            Some(conversation.app_url.as_str())
        );
        assert_eq!(
            item.actions()
                .iter()
                .map(|action| action.label)
                .collect::<Vec<_>>(),
            ["Start thread ▾", "Open in Missive"]
        );
    }

    #[test]
    fn conversations_filter_closed_and_group_unassigned_after_assigned() {
        let mut assigned_to_other = conversation("other", false, true);
        assigned_to_other.assignees[0].is_me = false;
        assigned_to_other.last_activity_at = None;
        let conversations = vec![
            conversation("unassigned", false, false),
            conversation("closed", true, true),
            conversation("assigned", false, true),
            assigned_to_other,
        ];
        let open = sorted_filtered_conversations(
            &conversations,
            "conversation",
            false,
            SystemTime::UNIX_EPOCH + Duration::from_secs(60),
        );
        assert_eq!(
            open.iter().map(|item| item.row().group).collect::<Vec<_>>(),
            ["Assigned to me", "Assigned", "Unassigned"]
        );
        assert_eq!(open[1].row().age, "unknown");
        assert_eq!(
            sorted_filtered_conversations(
                &conversations,
                "closed",
                true,
                SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            )[0]
            .row()
            .glyph,
            "●"
        );
    }

    #[test]
    fn section_separator_is_blank_then_a_dim_named_rule() {
        let palette = Palette::catppuccin();
        let [blank, rule] = section_separator(&palette, "Description", 24);

        assert!(blank.spans.is_empty());
        assert_eq!(rule.width(), 24);
        assert!(rule
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("Description"));
        assert!(rule.spans.iter().all(|span| {
            span.style.fg == Some(palette.subtext0)
                && span.style.add_modifier.contains(Modifier::DIM)
        }));
    }

    #[test]
    fn comment_header_joins_author_and_age() {
        let comment = WorkItemComment {
            author: Some("Ada".into()),
            body: String::new(),
            created_at: Some(SystemTime::UNIX_EPOCH),
        };

        assert_eq!(
            comment_header(
                &comment,
                SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 3_600)
            ),
            "Ada · 2h"
        );
    }

    #[test]
    fn pr_fixture_groups_filters_and_sorts() {
        let items = vec![
            item(2, PrAudience::Other, 20),
            item(1, PrAudience::Authored, 10),
            item(3, PrAudience::Authored, 30),
        ];
        let cache = crate::work_index::WorkItemDetailCache::default();
        let rows = sorted_filtered_prs(
            &items,
            &cache,
            "label:bug PR",
            PrSort::Updated,
            true,
            SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            "approved",
            None,
        );
        assert_eq!(
            rows.iter().map(|item| item.row().group).collect::<Vec<_>>(),
            ["Authored", "Authored", "Others"]
        );
        assert_eq!(
            rows.iter()
                .map(|item| item.summary.pr_number)
                .collect::<Vec<_>>(),
            [Some(3), Some(1), Some(2)]
        );
        assert!(rows[0].matches("#3"));
    }

    #[test]
    fn pr_projection_applies_github_assignee_draft_and_state_filters() {
        let mut visible = item(1, PrAudience::Authored, 10);
        visible.source.github = true;
        visible.assignees = vec!["Matthias".into()];
        let mut draft = item(2, PrAudience::Authored, 20);
        draft.source.github = true;
        draft.assignees = vec!["Matthias".into()];
        draft.draft = true;
        let mut merged = item(3, PrAudience::Authored, 30);
        merged.source.github = true;
        merged.assignees = vec!["Matthias".into()];
        merged.pr_state = Some("merged".into());
        let mut other = item(4, PrAudience::Authored, 40);
        other.source.github = true;
        other.assignees = vec!["Ada".into()];
        let items = vec![visible, draft, merged, other];
        let filters = crate::app::state::SidebarWorkFilter::default();
        let mut session = crate::work_index::WorkIndexSession::default();
        session.github.viewer = Some("Matthias".into());
        let cache = crate::work_index::WorkItemDetailCache::default();

        let rows = sorted_filtered_prs(
            &items,
            &cache,
            "",
            PrSort::Number,
            false,
            SystemTime::UNIX_EPOCH,
            "approved",
            Some((&filters, &session)),
        );

        assert_eq!(
            rows.iter()
                .map(|row| row.summary.pr_number)
                .collect::<Vec<_>>(),
            [Some(1)]
        );
    }

    #[test]
    fn pr_detail_fixture_projects_required_content() {
        let summary = item(7, PrAudience::Authored, 20);
        let mut cached = detail(&["SUCCESS", "FAILURE"], "BEHIND", true);
        cached.number = Some(7);
        cached.title = Some("repair parser".into());
        cached.author = Some("ada".into());
        cached.base_ref_name = Some("main".into());
        cached.head_ref_name = Some("fix/parser".into());
        cached.reviewers = vec!["grace".into()];
        cached.body = Some("## Why\n- regression".into());
        let item = PrItem {
            summary: &summary,
            cached_detail: Some(&cached),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            approval_label: "approved",
        };
        let projected = item.detail();
        assert_eq!(projected.heading, "owner/repo #7 ↗");
        assert_eq!(projected.branches, "main ⚠ ← fix/parser");
        assert_eq!(projected.reviewers, "grace");
        assert_eq!(projected.comments[0].body, "fix this");
    }

    #[test]
    fn pr_action_enablement_matrix() {
        assert_eq!(
            PrApprovalSignal::Label("approved".into()).confirmation_label(),
            "label approved"
        );
        let summary = item(7, PrAudience::Authored, 20);
        let uncached = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
            approval_label: "approved",
        };
        let uncached_actions = uncached.actions();
        assert!(uncached_actions
            .iter()
            .find(|action| action.kind == WorkActionKind::CheckOut)
            .is_some_and(|action| action.enabled));
        assert!(!uncached_actions
            .iter()
            .any(|action| action.kind != WorkActionKind::CheckOut && action.enabled));
        for (states, merge, review, labels, comment, land, status, fix) in [
            (
                vec!["SUCCESS"],
                "CLEAN",
                Some("APPROVED"),
                Vec::new(),
                true,
                true,
                PrLandStatus::Enabled(PrApprovalSignal::ApprovedReview),
                true,
            ),
            (
                vec!["SUCCESS"],
                "CLEAN",
                None,
                Vec::new(),
                false,
                false,
                PrLandStatus::AwaitingApproval,
                false,
            ),
            (
                vec!["SUCCESS"],
                "CLEAN",
                None,
                vec!["approved"],
                false,
                true,
                PrLandStatus::Enabled(PrApprovalSignal::Label("approved".into())),
                false,
            ),
            (
                vec!["FAILURE"],
                "CLEAN",
                Some("APPROVED"),
                Vec::new(),
                true,
                false,
                PrLandStatus::Blocked,
                true,
            ),
            (
                vec!["SUCCESS"],
                "BEHIND",
                Some("APPROVED"),
                Vec::new(),
                false,
                false,
                PrLandStatus::Blocked,
                false,
            ),
            (
                Vec::new(),
                "CLEAN",
                Some("APPROVED"),
                Vec::new(),
                false,
                false,
                PrLandStatus::Blocked,
                false,
            ),
        ] {
            let mut cached = detail(&states, merge, comment);
            cached.review_decision = review.map(str::to_string);
            cached.labels = labels.into_iter().map(str::to_string).collect();
            let item = PrItem {
                summary: &summary,
                cached_detail: Some(&cached),
                observed_at: SystemTime::UNIX_EPOCH,
                approval_label: "approved",
            };
            let actions = item.actions();
            assert!(actions
                .iter()
                .find(|action| action.kind == WorkActionKind::CheckOut)
                .is_some_and(|action| action.enabled));
            assert_eq!(
                actions
                    .iter()
                    .find(|action| action.kind == WorkActionKind::Land)
                    .map(|action| action.enabled),
                Some(land)
            );
            assert_eq!(item.land_status(), status);
            assert_eq!(
                actions
                    .iter()
                    .find(|action| action.kind == WorkActionKind::FixInThread)
                    .map(|action| action.enabled),
                Some(fix)
            );
        }
    }

    #[test]
    fn ticket_fixture_groups_filters_and_sorts() {
        let items = vec![
            ticket_item(ticket("SCA-3", TicketGroup::Triage, 3, 30)),
            ticket_item(ticket("SCA-2", TicketGroup::Assigned, 1, 20)),
            ticket_item(ticket("SCA-1", TicketGroup::Assigned, 2, 10)),
            ticket_item(ticket("SCA-4", TicketGroup::DoneThisCycle, 4, 40)),
        ];
        let cache = crate::work_index::WorkItemDetailCache::default();
        let rows = sorted_filtered_tickets(
            &items,
            &cache,
            "label:bug ticket",
            TicketSort::Priority,
            false,
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            false,
            None,
        );
        assert_eq!(
            rows.iter().map(|item| item.row().group).collect::<Vec<_>>(),
            [
                "Assigned to me",
                "Assigned to me",
                "Triage",
                "Done (this cycle)"
            ]
        );
        assert_eq!(
            rows.iter()
                .map(|item| item.summary.identifier.as_str())
                .collect::<Vec<_>>(),
            ["SCA-2", "SCA-1", "SCA-3", "SCA-4"]
        );
        assert_eq!(rows[0].row().glyph, "●");
        assert_eq!(rows[3].row().glyph, "✓");

        let open = sorted_filtered_tickets(
            &items,
            &cache,
            "sca-4",
            TicketSort::Identifier,
            true,
            SystemTime::UNIX_EPOCH,
            false,
            None,
        );
        assert!(open.is_empty());
    }

    #[test]
    fn ticket_projection_applies_linear_team_assignee_and_status_filters() {
        let mut assigned = ticket("SCA-1", TicketGroup::Assigned, 1, 10);
        assigned.assignee = Some("Matthias".into());
        assigned.state = Some("In Progress".into());
        let mut canceled = ticket("SCA-2", TicketGroup::Assigned, 2, 20);
        canceled.assignee = Some("Matthias".into());
        canceled.state = Some("Canceled".into());
        let mut other = ticket("OPS-3", TicketGroup::Assigned, 3, 30);
        other.assignee = Some("Matthias".into());
        let items = vec![
            ticket_item(assigned),
            ticket_item(canceled),
            ticket_item(other),
        ];
        let filters = crate::app::state::SidebarWorkFilter::default();
        let mut session = crate::work_index::WorkIndexSession::default();
        session.linear.viewer = Some("Matthias".into());
        let cache = crate::work_index::WorkItemDetailCache::default();

        let rows = sorted_filtered_tickets(
            &items,
            &cache,
            "",
            TicketSort::Identifier,
            false,
            SystemTime::UNIX_EPOCH,
            false,
            Some((&filters, &session)),
        );

        assert_eq!(
            rows.iter()
                .map(|row| row.summary.identifier.as_str())
                .collect::<Vec<_>>(),
            ["SCA-1"]
        );
    }

    #[test]
    fn ticket_detail_projects_checklist_comments_and_linked_prs() {
        let ticket_item = ticket_item(ticket("SCA-7", TicketGroup::Assigned, 2, 10));
        let mut linked_pr = item(7, PrAudience::Other, 20);
        linked_pr.ticket_ids = vec!["sca-7".into()];
        let mut cached = IndexedWorkItemDetail::empty();
        cached.body = Some("Intro\n- [x] parsed\n- [ ] rendered".into());
        cached.comments = vec![
            WorkItemComment {
                author: Some("old".into()),
                body: "first".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            },
            WorkItemComment {
                author: Some("new".into()),
                body: "second".into(),
                created_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            },
        ];
        let item = TicketItem {
            summary: &ticket_item.ticket_details[0],
            cached_detail: Some(&cached),
            linked_prs: vec![&linked_pr],
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
            has_context_pr: true,
        };
        let detail = item.detail();
        assert_eq!(detail.heading, "SCA-7 ↗");
        assert_eq!(detail.byline, "In Progress · P2 · ada · cycle 34");
        assert_eq!(
            detail.checks,
            [
                ("parsed".into(), "done".into()),
                ("rendered".into(), "todo".into())
            ]
        );
        assert_eq!(detail.comments[0].author.as_deref(), Some("new"));
        assert_eq!(detail.linked_prs[0].number, 7);
        assert_eq!(
            description_without_checklist(detail.description.as_deref()).as_deref(),
            Some("Intro")
        );
    }

    #[test]
    fn ticket_action_enablement_and_worktree_name_contract() {
        let summary = ticket("SCA-3165", TicketGroup::Assigned, 2, 10);
        let item = TicketItem {
            summary: &summary,
            cached_detail: None,
            linked_prs: Vec::new(),
            observed_at: SystemTime::UNIX_EPOCH,
            has_context_pr: false,
        };
        assert!(!item.transition_enabled("In Progress"));
        assert!(item.transition_enabled("Done"));
        assert!(!item
            .actions()
            .iter()
            .find(|action| action.kind == WorkActionKind::LinkPr)
            .is_some_and(|action| action.enabled));

        let branch = ticket_worktree_branch(
            crate::config::DEFAULT_BRANCH_PREFIX,
            "SCA-3165",
            "Image edit simple v3 reference addendum with a very long suffix",
        );
        assert!(branch.starts_with("issue/sca-3165-"));
        assert!(branch.len() <= 40, "{branch}");
    }
}
