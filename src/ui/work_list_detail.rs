use std::time::{Duration, SystemTime};

use crate::work_index::{
    PrAudience, PrCheckState, WorkItem as IndexedWorkItem, WorkItemComment,
    WorkItemDetail as IndexedWorkItemDetail,
};

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkActionKind {
    CheckOut,
    Land,
    FixInThread,
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
        !detail.actions.is_empty()
            && detail.actions.iter().all(|check| check.state == "SUCCESS")
            && detail.merge_state_status.as_deref() == Some("CLEAN")
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

pub(crate) fn sorted_filtered_prs<'a>(
    items: &'a [IndexedWorkItem],
    details: &'a crate::work_index::WorkItemDetailCache,
    query: &str,
    sort: PrSort,
    open_only: bool,
    observed_at: SystemTime,
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
        .filter(|item| (!open_only || item.is_open()) && item.matches(query))
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
    use crate::work_index::{WorkItemAction, WorkItemCheckSummary, WorkItemSource};

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
    fn pr_action_enablement_matrix() {
        let summary = item(7, PrAudience::Authored, 20);
        let uncached = PrItem {
            summary: &summary,
            cached_detail: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let uncached_actions = uncached.actions();
        assert!(uncached_actions
            .iter()
            .find(|action| action.kind == WorkActionKind::CheckOut)
            .is_some_and(|action| action.enabled));
        assert!(!uncached_actions
            .iter()
            .any(|action| action.kind != WorkActionKind::CheckOut && action.enabled));
        for (states, merge, comment, land, fix) in [
            (vec!["SUCCESS"], "CLEAN", true, true, true),
            (vec!["FAILURE"], "CLEAN", true, false, true),
            (vec!["SUCCESS"], "BEHIND", false, false, false),
            (Vec::new(), "CLEAN", false, false, false),
        ] {
            let cached = detail(&states, merge, comment);
            let item = PrItem {
                summary: &summary,
                cached_detail: Some(&cached),
                observed_at: SystemTime::UNIX_EPOCH,
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
            assert_eq!(
                actions
                    .iter()
                    .find(|action| action.kind == WorkActionKind::FixInThread)
                    .map(|action| action.enabled),
                Some(fix)
            );
        }
    }
}
