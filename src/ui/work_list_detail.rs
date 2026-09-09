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

/// A section header that can be folded. `state` carries whether the section is
/// collapsed and which digit reopens it; `None` renders the plain rule used by
/// headers that hold nothing to fold.
///
/// The reopen hint sits on the collapsed header only. An expanded section shows
/// its content, so the reader has no question to answer there.
pub(crate) fn collapsible_section_separator(
    palette: &Palette,
    title: impl AsRef<str>,
    width: u16,
    collapsed: bool,
    digit: Option<char>,
) -> [Line<'static>; 2] {
    section_rule(palette, title, width, collapsed, digit)
}

fn section_rule(
    palette: &Palette,
    title: impl AsRef<str>,
    width: u16,
    collapsed: bool,
    digit: Option<char>,
) -> [Line<'static>; 2] {
    let width = usize::from(width);
    let glyph = if collapsed { "▸" } else { "▾" };
    let label = match (collapsed, digit) {
        (true, Some(digit)) => format!("─ {glyph} {} · alt+{digit} to expand ", title.as_ref()),
        _ => format!("─ {glyph} {} ", title.as_ref()),
    };
    let label = truncate_end(&label, width);
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

/// A collapsed comment shows this many body lines before its expand hint.
pub(crate) const COLLAPSED_COMMENT_BODY_LINES: usize = 3;

/// Stable identity for a comment across index refreshes. The comment list is
/// rebuilt on every observation and a new comment shifts the index of every
/// older one, so an expanded comment has to be remembered by content.
pub(crate) fn comment_identity(comment: &WorkItemComment) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    comment.author.hash(&mut hasher);
    comment.created_at.hash(&mut hasher);
    comment.body.hash(&mut hasher);
    hasher.finish()
}

/// Digit key that expands or collapses the comment at `index`. Only the first
/// nine get one; the rest are reached through expand-all.
pub(crate) fn comment_expand_digit(index: usize) -> Option<char> {
    if index >= 9 {
        return None;
    }
    char::from_digit(index as u32 + 1, 10)
}

/// Body lines for one comment, cut to a readable head unless it is expanded.
///
/// A long review comment used to push every comment under it off the pane, so
/// the list only read as a list once the reader had scrolled past all of it.
pub(crate) fn comment_body_lines(
    palette: &Palette,
    comment: &WorkItemComment,
    index: usize,
    width: usize,
    indent: &str,
    expanded: bool,
) -> Vec<Line<'static>> {
    let hint = |text: String| {
        Line::from(Span::styled(
            format!("{indent}{text}"),
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        ))
    };
    let body = crate::ui::markdown::body_lines(palette, Some(&comment.body), width, indent);
    if expanded {
        let mut lines = body;
        match comment_expand_digit(index) {
            Some(digit) => lines.push(hint(format!("{digit} to collapse"))),
            None => lines.push(hint("a to collapse all".to_string())),
        }
        return lines;
    }
    // Hiding a single line behind a hint line saves nothing and costs the
    // reader the content, so only cut when there is more than one line to gain.
    if body.len() <= COLLAPSED_COMMENT_BODY_LINES + 1 {
        return body;
    }
    let hidden = body.len() - COLLAPSED_COMMENT_BODY_LINES;
    let mut lines = body
        .into_iter()
        .take(COLLAPSED_COMMENT_BODY_LINES)
        .collect::<Vec<_>>();
    lines.push(match comment_expand_digit(index) {
        Some(digit) => hint(format!("… {hidden} more lines · {digit} to expand")),
        None => hint(format!("… {hidden} more lines · a to expand all")),
    });
    lines
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

pub(crate) trait WorkItem {
    fn key(&self) -> String;
    fn row(&self) -> WorkRow;
    fn detail(&self) -> WorkDetail;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrActionKind {
    CheckOut,
    Refresh,
    AskQuestion,
    Explain,
    FixFindings,
    ConvertToDraft,
    MarkReady,
    EnableAutoMerge(crate::config::MergeMethodConfig),
    DisableAutoMerge,
    Merge(crate::config::MergeMethodConfig),
    OpenOnGithub,
    CopyLink,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrActionPlacement {
    Header,
    Menu { group: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrAction {
    pub(crate) kind: PrActionKind,
    pub(crate) label: String,
    pub(crate) placement: PrActionPlacement,
    pub(crate) disabled_reason: Option<&'static str>,
}

impl PrAction {
    pub(crate) fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

fn merge_disabled_reason(
    summary: &IndexedWorkItem,
    detail: Option<&IndexedWorkItemDetail>,
) -> Option<&'static str> {
    if !summary
        .pr_state
        .as_deref()
        .is_none_or(|state| state.eq_ignore_ascii_case("open"))
    {
        return Some("pull request is closed");
    }
    let Some(detail) = detail else {
        return Some("details not loaded");
    };
    if detail.is_draft.unwrap_or(summary.draft) {
        return Some("pull request is a draft");
    }
    if detail.actions.iter().any(|check| {
        matches!(
            check.state.to_ascii_uppercase().as_str(),
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT"
        )
    }) {
        return Some("checks failing");
    }
    match detail.mergeable.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("MERGEABLE") => None,
        Some(value) if value.eq_ignore_ascii_case("UNKNOWN") => Some("mergeability pending"),
        Some(_) => Some("not mergeable"),
        None => Some("mergeability not loaded"),
    }
}

fn auto_merge_disabled_reason(
    summary: &IndexedWorkItem,
    detail: Option<&IndexedWorkItemDetail>,
) -> Option<&'static str> {
    if !summary
        .pr_state
        .as_deref()
        .is_none_or(|state| state.eq_ignore_ascii_case("open"))
    {
        return Some("pull request is closed");
    }
    let Some(detail) = detail else {
        return Some("details not loaded");
    };
    if detail.is_draft.unwrap_or(summary.draft) {
        return Some("pull request is a draft");
    }
    detail
        .mergeable
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("CONFLICTING"))
        .then_some("not mergeable")
}

impl PrItem<'_> {
    pub(crate) fn stable_key(&self) -> crate::app::state::WorkItemKey {
        crate::app::state::WorkItemKey {
            repo: self.summary.repo.clone(),
            pr_number: self.summary.pr_number,
            pr_url: self.summary.pr_url.clone(),
            ticket_id: None,
        }
    }

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
            } else if let Some(author) = token.strip_prefix("author:") {
                if !self
                    .summary
                    .author
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(author))
                {
                    return false;
                }
            } else if token.eq_ignore_ascii_case("is:draft") {
                if !self.summary.draft {
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

    pub(crate) fn merge_disabled_reason(&self) -> Option<&'static str> {
        merge_disabled_reason(self.summary, self.cached_detail)
    }

    pub(crate) fn action_table(
        &self,
        default_method: crate::config::MergeMethodConfig,
        checkout_available: bool,
    ) -> Vec<PrAction> {
        let open = self.is_open();
        let draft = self
            .cached_detail
            .and_then(|detail| detail.is_draft)
            .unwrap_or(self.summary.draft);
        let url_available = self
            .cached_detail
            .and_then(|detail| detail.url.as_ref())
            .or(self.summary.pr_url.as_ref())
            .is_some();
        let merge_reason = self.merge_disabled_reason();
        let auto_enabled = self
            .cached_detail
            .is_some_and(|detail| detail.auto_merge_enabled);
        let fix_reason = if !checkout_available {
            Some("Check out the PR first")
        } else if self
            .cached_detail
            .is_some_and(|detail| detail.unresolved_review_threads == Some(0))
        {
            Some("No unresolved review threads")
        } else {
            None
        };
        let mut actions = vec![
            PrAction {
                kind: PrActionKind::CheckOut,
                label: "Check out ▾".into(),
                placement: PrActionPlacement::Header,
                disabled_reason: (!checkout_available).then_some("checkout unavailable"),
            },
            PrAction {
                kind: PrActionKind::Merge(default_method),
                label: "Merge".into(),
                placement: PrActionPlacement::Header,
                disabled_reason: merge_reason,
            },
            PrAction {
                kind: PrActionKind::Refresh,
                label: "Refresh".into(),
                placement: PrActionPlacement::Menu { group: 0 },
                disabled_reason: None,
            },
            PrAction {
                kind: PrActionKind::AskQuestion,
                label: "Ask a question".into(),
                placement: PrActionPlacement::Menu { group: 1 },
                disabled_reason: (!checkout_available).then_some("checkout unavailable"),
            },
            PrAction {
                kind: PrActionKind::Explain,
                label: "Explain this PR".into(),
                placement: PrActionPlacement::Menu { group: 1 },
                disabled_reason: (!checkout_available).then_some("checkout unavailable"),
            },
            PrAction {
                kind: PrActionKind::FixFindings,
                label: "Fix findings in a thread".into(),
                placement: PrActionPlacement::Menu { group: 1 },
                disabled_reason: fix_reason,
            },
            PrAction {
                kind: if draft {
                    PrActionKind::MarkReady
                } else {
                    PrActionKind::ConvertToDraft
                },
                label: if draft {
                    "Mark ready"
                } else {
                    "Convert to draft"
                }
                .into(),
                placement: PrActionPlacement::Menu { group: 2 },
                disabled_reason: (!open).then_some("pull request is closed"),
            },
            PrAction {
                kind: if auto_enabled {
                    PrActionKind::DisableAutoMerge
                } else {
                    PrActionKind::EnableAutoMerge(default_method)
                },
                label: if auto_enabled {
                    "Disable auto-merge"
                } else {
                    "Enable auto-merge"
                }
                .into(),
                placement: PrActionPlacement::Menu { group: 2 },
                disabled_reason: auto_merge_disabled_reason(self.summary, self.cached_detail),
            },
        ];
        actions.extend(crate::config::MergeMethodConfig::ALL.map(|method| {
            PrAction {
                kind: PrActionKind::Merge(method),
                label: match method {
                    crate::config::MergeMethodConfig::Merge => "Merge",
                    crate::config::MergeMethodConfig::Squash => "Squash",
                    crate::config::MergeMethodConfig::Rebase => "Rebase",
                }
                .into(),
                placement: PrActionPlacement::Menu { group: 3 },
                disabled_reason: merge_reason,
            }
        }));
        actions.extend([
            PrAction {
                kind: PrActionKind::OpenOnGithub,
                label: "Open on GitHub".into(),
                placement: PrActionPlacement::Menu { group: 4 },
                disabled_reason: (!url_available).then_some("link unavailable"),
            },
            PrAction {
                kind: PrActionKind::CopyLink,
                label: "Copy link".into(),
                placement: PrActionPlacement::Menu { group: 4 },
                disabled_reason: (!url_available).then_some("link unavailable"),
            },
            PrAction {
                kind: PrActionKind::Close,
                label: "Close pull request".into(),
                placement: PrActionPlacement::Menu { group: 5 },
                disabled_reason: (!open).then_some("pull request is closed"),
            },
        ]);
        actions
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
            // Already ordered by the provider parsers; re-sorting here would
            // give the render a different order from the cached list the key
            // handler indexes, so a digit would open a different comment than
            // the one it numbers.
            comments: detail.comments.clone(),
        }
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
    pub(crate) fn stable_key(&self) -> crate::app::state::WorkItemKey {
        crate::app::state::WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some(self.summary.identifier.clone()),
        }
    }

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
        // Same as the pull-request projection: keep the parsers' order. A flat
        // re-sort here would also split Linear's threads, which are ordered by
        // whole thread so a reply stays under the root it answers.
        let comments = self
            .cached_detail
            .map_or_else(Vec::new, |detail| detail.comments.clone());
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
            cached_pr_detail: None,
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
        detail.mergeable = Some(
            if merge == "CLEAN" {
                "MERGEABLE"
            } else {
                "CONFLICTING"
            }
            .into(),
        );
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
            creator: None,
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
            cached_pr_detail: None,
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
            labels: Vec::new(),
            pane_bound: false,
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
    fn conversation_item_projects_list_and_detail() {
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
        let [blank, rule] = collapsible_section_separator(&palette, "Description", 24, false, None);

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
    fn a_collapsed_section_header_names_the_key_that_reopens_it() {
        let palette = Palette::catppuccin();
        let text = |line: &Line<'static>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        let [_, expanded] = collapsible_section_separator(&palette, "Checks", 48, false, Some('2'));
        assert!(
            text(&expanded).contains("\u{25be} Checks"),
            "{}",
            text(&expanded)
        );
        assert!(
            !text(&expanded).contains("alt+"),
            "an expanded section shows its content, so it has no question to answer"
        );

        let [_, collapsed] = collapsible_section_separator(&palette, "Checks", 48, true, Some('2'));
        assert!(
            text(&collapsed).contains("\u{25b8} Checks \u{b7} alt+2 to expand"),
            "{}",
            text(&collapsed)
        );

        // Past the ninth section there is no digit left to name.
        let [_, undigited] = collapsible_section_separator(&palette, "Checks", 48, true, None);
        assert!(
            text(&undigited).starts_with("\u{2500} \u{25b8} Checks "),
            "{}",
            text(&undigited)
        );
        assert!(!text(&undigited).contains("alt+"));
    }

    fn comment(body: &str) -> WorkItemComment {
        WorkItemComment {
            author: Some("ada".into()),
            body: body.into(),
            created_at: None,
        }
    }

    fn rendered(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_collapsed_comment_keeps_a_readable_head_and_names_its_digit() {
        let palette = Palette::catppuccin();
        let long = comment("one\ntwo\nthree\nfour\nfive\nsix");

        let collapsed = rendered(&comment_body_lines(&palette, &long, 2, 40, "  ", false));

        assert_eq!(collapsed.len(), COLLAPSED_COMMENT_BODY_LINES + 1);
        assert!(collapsed[0].contains("one"), "{collapsed:?}");
        assert_eq!(
            collapsed.last().map(String::as_str),
            Some("  … 3 more lines · 3 to expand"),
            "{collapsed:?}"
        );

        let expanded = rendered(&comment_body_lines(&palette, &long, 2, 40, "  ", true));

        assert!(expanded.iter().any(|line| line.contains("six")));
        assert_eq!(
            expanded.last().map(String::as_str),
            Some("  3 to collapse"),
            "{expanded:?}"
        );
    }

    #[test]
    fn a_short_comment_is_never_traded_for_a_hint_line() {
        let palette = Palette::catppuccin();
        // Four lines against a three-line head: cutting would hide one line
        // and spend one line saying so, which costs the reader and saves
        // nothing.
        let short = comment("one\ntwo\nthree\nfour");

        let collapsed = rendered(&comment_body_lines(&palette, &short, 0, 40, "  ", false));

        assert!(
            collapsed.iter().any(|line| line.contains("four")),
            "{collapsed:?}"
        );
        assert!(
            !collapsed.iter().any(|line| line.contains("to expand")),
            "{collapsed:?}"
        );
    }

    #[test]
    fn comments_past_the_ninth_point_at_expand_all_instead_of_a_digit() {
        let palette = Palette::catppuccin();
        let long = comment("one\ntwo\nthree\nfour\nfive");

        assert_eq!(comment_expand_digit(8), Some('9'));
        assert_eq!(comment_expand_digit(9), None);

        let collapsed = rendered(&comment_body_lines(&palette, &long, 9, 40, "  ", false));

        assert_eq!(
            collapsed.last().map(String::as_str),
            Some("  … 2 more lines · a to expand all"),
            "{collapsed:?}"
        );
    }

    #[test]
    fn comment_identity_follows_content_not_position() {
        let first = comment("first");
        let second = comment("second");

        assert_eq!(
            comment_identity(&first),
            comment_identity(&comment("first"))
        );
        assert_ne!(comment_identity(&first), comment_identity(&second));
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
    fn pr_search_combines_label_author_draft_and_free_text_tokens() {
        let mut draft = item(42, PrAudience::Authored, 30);
        draft.pr_title = Some("Fix parser edge case".into());
        draft.labels = vec!["bug".into(), "parser".into()];
        draft.author = Some("Ada".into());
        draft.draft = true;
        let pr = PrItem {
            summary: &draft,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        assert!(pr.matches("label:BUG author:ada is:draft parser #42"));
        assert!(!pr.matches("label:docs author:ada is:draft parser"));
        assert!(!pr.matches("label:bug author:grace is:draft parser"));
        draft.draft = false;
        let ready = PrItem {
            summary: &draft,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        assert!(!ready.matches("is:draft parser"));
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
        };
        let projected = item.detail();
        assert_eq!(projected.heading, "owner/repo #7 ↗");
        assert_eq!(projected.branches, "main ⚠ ← fix/parser");
        assert_eq!(projected.reviewers, "grace");
        assert_eq!(projected.comments[0].body, "fix this");
    }

    #[test]
    fn pr_action_table_has_exact_order_dynamic_labels_and_merge_gates() {
        let summary = item(7, PrAudience::Authored, 20);
        let uncached = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let uncached_actions = uncached.action_table(crate::config::MergeMethodConfig::Merge, true);
        assert_eq!(uncached.merge_disabled_reason(), Some("details not loaded"));
        assert!(uncached_actions
            .iter()
            .find(|action| action.kind == PrActionKind::CheckOut)
            .is_some_and(PrAction::enabled));

        let mut cached = detail(&["SUCCESS"], "CLEAN", true);
        let item = PrItem {
            summary: &summary,
            cached_detail: Some(&cached),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let labels = item
            .action_table(crate::config::MergeMethodConfig::Squash, true)
            .into_iter()
            .filter(|action| matches!(action.placement, PrActionPlacement::Menu { .. }))
            .map(|action| action.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "Refresh",
                "Ask a question",
                "Explain this PR",
                "Fix findings in a thread",
                "Convert to draft",
                "Enable auto-merge",
                "Merge",
                "Squash",
                "Rebase",
                "Open on GitHub",
                "Copy link",
                "Close pull request",
            ]
        );

        cached.auto_merge_enabled = true;
        let item = PrItem {
            summary: &summary,
            cached_detail: Some(&cached),
            observed_at: SystemTime::UNIX_EPOCH,
        };
        assert!(item
            .action_table(crate::config::MergeMethodConfig::Merge, true)
            .iter()
            .any(|action| {
                action.kind == PrActionKind::DisableAutoMerge
                    && action.label == "Disable auto-merge"
            }));

        for (states, mergeable, draft, expected) in [
            (vec!["SUCCESS"], "MERGEABLE", false, None),
            (vec!["FAILURE"], "MERGEABLE", false, Some("checks failing")),
            (vec!["SUCCESS"], "CONFLICTING", false, Some("not mergeable")),
            (
                vec!["SUCCESS"],
                "MERGEABLE",
                true,
                Some("pull request is a draft"),
            ),
        ] {
            let mut cached = detail(&states, "CLEAN", false);
            cached.mergeable = Some(mergeable.into());
            cached.is_draft = Some(draft);
            let item = PrItem {
                summary: &summary,
                cached_detail: Some(&cached),
                observed_at: SystemTime::UNIX_EPOCH,
            };
            assert_eq!(item.merge_disabled_reason(), expected);
        }
    }

    #[test]
    fn pr_action_table_disables_ask_question_without_pr_context() {
        let mut summary = item(7, PrAudience::Authored, 20);
        summary.branch = None;
        let item = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        let ask_question = item
            .action_table(crate::config::MergeMethodConfig::Merge, false)
            .into_iter()
            .find(|action| action.kind == PrActionKind::AskQuestion)
            .expect("ask question action");

        assert_eq!(ask_question.disabled_reason, Some("checkout unavailable"));
        assert!(!ask_question.enabled());
    }

    #[test]
    fn pr_action_table_disables_fix_findings_without_pr_context() {
        let mut summary = item(7, PrAudience::Authored, 20);
        summary.branch = None;
        let item = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };

        let fix_findings = item
            .action_table(crate::config::MergeMethodConfig::Merge, false)
            .into_iter()
            .find(|action| action.kind == PrActionKind::FixFindings)
            .expect("fix findings action");

        assert_eq!(fix_findings.disabled_reason, Some("Check out the PR first"));
        assert!(!fix_findings.enabled());
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
        // Provider parsers hand the cache a newest-first list; the projection
        // passes it through so the render and the key handler agree on which
        // comment is which.
        cached.comments = vec![
            WorkItemComment {
                author: Some("new".into()),
                body: "second".into(),
                created_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            },
            WorkItemComment {
                author: Some("old".into()),
                body: "first".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
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
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &summary, None, None, None, false,
        );
        let actions = crate::ui::ticket_actions::ticket_action_table(
            &context,
            crate::ui::ticket_actions::TicketActionMenuPage::Actions,
        );
        assert!(!actions
            .iter()
            .find(|action| action.action == crate::ui::ticket_actions::TicketAction::LinkPr)
            .is_some_and(crate::ui::ticket_actions::TicketActionEntry::enabled));

        let branch = ticket_worktree_branch(
            crate::config::DEFAULT_BRANCH_PREFIX,
            "SCA-3165",
            "Image edit simple v3 reference addendum with a very long suffix",
        );
        assert!(branch.starts_with("issue/sca-3165-"));
        assert!(branch.len() <= 40, "{branch}");
    }
}
