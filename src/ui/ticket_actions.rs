//! Shared Linear ticket actions for the full view, dock, and sidebar menu.

use ratatui::{layout::Rect, Frame};

use crate::{
    app::state::{Palette, TicketTransitionChoice},
    work_index::{WorkItemDetail, WorkTicket},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TicketAction {
    Refresh,
    AskQuestion,
    Explain,
    WorkInThread,
    TransitionMenu,
    Transition(TicketTransitionChoice),
    AssignToMe,
    PriorityMenu,
    SetPriority(u8),
    LinkPr,
    Comment,
    OpenInLinear,
    CopyLink,
    CopyIdentifier,
    Cancel,
}

impl TicketAction {
    pub(crate) fn label(self) -> String {
        match self {
            Self::Refresh => "Refresh".into(),
            Self::AskQuestion => "Ask a question".into(),
            Self::Explain => "Explain this ticket".into(),
            Self::WorkInThread => "Work on it in a thread".into(),
            Self::TransitionMenu => "Transition ▸".into(),
            Self::Transition(choice) => choice.label().into(),
            Self::AssignToMe => "Assign to me".into(),
            Self::PriorityMenu => "Priority ▸".into(),
            Self::SetPriority(priority) => format!("P{priority}"),
            Self::LinkPr => "Link PR".into(),
            Self::Comment => "Comment".into(),
            Self::OpenInLinear => "Open in Linear".into(),
            Self::CopyLink => "Copy link".into(),
            Self::CopyIdentifier => "Copy identifier".into(),
            Self::Cancel => "Cancel ticket".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TicketActionMenuPage {
    #[default]
    Actions,
    Transitions,
    Priorities,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TicketActionMenuState {
    pub(crate) page: TicketActionMenuPage,
    pub(crate) selected: usize,
}

impl TicketActionMenuState {
    pub(crate) fn move_by(&mut self, delta: i8, count: usize) {
        if count == 0 {
            self.selected = 0;
        } else if delta.is_negative() {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.selected = self
                .selected
                .saturating_add(delta as usize)
                .min(count.saturating_sub(1));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketActionContext {
    pub(crate) identifier: String,
    pub(crate) title: String,
    pub(crate) state: Option<String>,
    pub(crate) priority: Option<u8>,
    pub(crate) assignee: Option<String>,
    pub(crate) viewer_name: Option<String>,
    pub(crate) viewer_id: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) has_context_pr: bool,
}

impl TicketActionContext {
    pub(crate) fn from_ticket(
        ticket: &WorkTicket,
        detail: Option<&WorkItemDetail>,
        viewer_name: Option<&str>,
        viewer_id: Option<&str>,
        has_context_pr: bool,
    ) -> Self {
        Self {
            identifier: ticket.identifier.clone(),
            title: ticket
                .title
                .clone()
                .or_else(|| detail.and_then(|detail| detail.title.clone()))
                .unwrap_or_else(|| "(untitled ticket)".into()),
            state: ticket.state.clone(),
            priority: ticket.priority,
            assignee: ticket.assignee.clone(),
            viewer_name: viewer_name.map(str::to_string),
            viewer_id: viewer_id.map(str::to_string),
            url: detail
                .and_then(|detail| detail.url.clone())
                .or_else(|| ticket.url.clone())
                .or_else(|| crate::work_context::linear_ticket_url(&ticket.identifier)),
            has_context_pr,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketActionEntry {
    pub(crate) action: TicketAction,
    pub(crate) label: String,
    pub(crate) disabled_reason: Option<&'static str>,
}

impl TicketActionEntry {
    pub(crate) fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }

    pub(crate) fn display_label(&self) -> String {
        self.disabled_reason.map_or_else(
            || self.label.clone(),
            |reason| format!("{} · {reason}", self.label),
        )
    }
}

fn entry(action: TicketAction, label: impl Into<String>) -> TicketActionEntry {
    TicketActionEntry {
        action,
        label: label.into(),
        disabled_reason: None,
    }
}

fn disabled(
    action: TicketAction,
    label: impl Into<String>,
    reason: &'static str,
) -> TicketActionEntry {
    TicketActionEntry {
        action,
        label: label.into(),
        disabled_reason: Some(reason),
    }
}

/// The one ticket-action table consumed by every ticket menu.
pub(crate) fn ticket_action_table(
    context: &TicketActionContext,
    page: TicketActionMenuPage,
) -> Vec<TicketActionEntry> {
    match page {
        TicketActionMenuPage::Actions => {
            let assign = match context.viewer_id.as_deref() {
                None => disabled(
                    TicketAction::AssignToMe,
                    "Assign to me",
                    "viewer unavailable",
                ),
                Some(_)
                    if context
                        .assignee
                        .as_deref()
                        .zip(context.viewer_name.as_deref())
                        .is_some_and(|(assignee, viewer)| {
                            assignee.eq_ignore_ascii_case(viewer)
                        }) =>
                {
                    disabled(TicketAction::AssignToMe, "Assign to me", "already assigned")
                }
                Some(_) => entry(TicketAction::AssignToMe, "Assign to me"),
            };
            let link_pr = if context.has_context_pr {
                entry(TicketAction::LinkPr, "Link PR")
            } else {
                disabled(TicketAction::LinkPr, "Link PR", "no PR in pane")
            };
            let open = if context.url.is_some() {
                entry(TicketAction::OpenInLinear, "Open in Linear")
            } else {
                disabled(
                    TicketAction::OpenInLinear,
                    "Open in Linear",
                    "link unavailable",
                )
            };
            let copy_link = if context.url.is_some() {
                entry(TicketAction::CopyLink, "Copy link")
            } else {
                disabled(TicketAction::CopyLink, "Copy link", "link unavailable")
            };
            let cancel = if context.state.as_deref().is_some_and(|state| {
                state.eq_ignore_ascii_case("canceled") || state.eq_ignore_ascii_case("cancelled")
            }) {
                disabled(TicketAction::Cancel, "Cancel ticket", "already canceled")
            } else {
                entry(TicketAction::Cancel, "Cancel ticket")
            };
            vec![
                entry(TicketAction::Refresh, "Refresh"),
                entry(TicketAction::AskQuestion, "Ask a question"),
                entry(TicketAction::Explain, "Explain this ticket"),
                entry(TicketAction::WorkInThread, "Work on it in a thread"),
                entry(TicketAction::TransitionMenu, "Transition ▸"),
                assign,
                entry(TicketAction::PriorityMenu, "Priority ▸"),
                link_pr,
                entry(TicketAction::Comment, "Comment"),
                open,
                copy_link,
                entry(TicketAction::CopyIdentifier, "Copy identifier"),
                cancel,
            ]
        }
        TicketActionMenuPage::Transitions => TicketTransitionChoice::ALL
            .into_iter()
            .map(|choice| {
                if context
                    .state
                    .as_deref()
                    .is_some_and(|state| state.eq_ignore_ascii_case(choice.label()))
                {
                    disabled(
                        TicketAction::Transition(choice),
                        choice.label(),
                        "current state",
                    )
                } else {
                    entry(TicketAction::Transition(choice), choice.label())
                }
            })
            .collect(),
        TicketActionMenuPage::Priorities => (0_u8..=4)
            .map(|priority| {
                let label = format!("P{priority}");
                if context.priority == Some(priority) {
                    disabled(
                        TicketAction::SetPriority(priority),
                        label,
                        "current priority",
                    )
                } else {
                    entry(TicketAction::SetPriority(priority), label)
                }
            })
            .collect(),
    }
}

pub(crate) fn ticket_action_menu_layout(
    anchor: Rect,
    area: Rect,
    context: &TicketActionContext,
    state: TicketActionMenuState,
) -> Option<crate::ui::dropdown::DropdownLayout> {
    let entries = ticket_action_table(context, state.page);
    let width = entries
        .iter()
        .map(TicketActionEntry::display_label)
        .map(|label| label.chars().count().saturating_add(2))
        .max()
        .unwrap_or(1);
    crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count: entries.len(),
            selected: state.selected,
            has_filter: false,
            max_rows: entries.len(),
            min_width: u16::try_from(width).unwrap_or(u16::MAX),
        },
        area,
    )
}

pub(crate) fn render_ticket_action_menu(
    palette: &Palette,
    frame: &mut Frame,
    area: Rect,
    anchor: Rect,
    context: &TicketActionContext,
    state: TicketActionMenuState,
) {
    let entries = ticket_action_table(context, state.page);
    let Some(layout) = ticket_action_menu_layout(anchor, area, context, state) else {
        return;
    };
    let rows = entries
        .iter()
        .map(|item| crate::ui::dropdown::DropdownMenuRow::Item {
            label: item.display_label(),
            enabled: item.enabled(),
        })
        .collect::<Vec<_>>();
    crate::ui::dropdown::render_menu(palette, frame, &layout, &rows, state.selected);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> TicketActionContext {
        TicketActionContext {
            identifier: "SCA-7".into(),
            title: "repair parser".into(),
            state: Some("In Progress".into()),
            priority: Some(2),
            assignee: Some("ada".into()),
            viewer_name: None,
            viewer_id: None,
            url: None,
            has_context_pr: false,
        }
    }

    #[test]
    fn ticket_action_table_orders_actions_and_names_dim_reasons() {
        let entries = ticket_action_table(&context(), TicketActionMenuPage::Actions);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Refresh",
                "Ask a question",
                "Explain this ticket",
                "Work on it in a thread",
                "Transition ▸",
                "Assign to me",
                "Priority ▸",
                "Link PR",
                "Comment",
                "Open in Linear",
                "Copy link",
                "Copy identifier",
                "Cancel ticket",
            ]
        );
        assert_eq!(entries[5].disabled_reason, Some("viewer unavailable"));
        assert_eq!(entries[7].disabled_reason, Some("no PR in pane"));
        assert_eq!(entries[9].disabled_reason, Some("link unavailable"));
    }

    #[test]
    fn assign_to_me_uses_identity_but_dims_against_viewer_name() {
        let mut context = context();
        context.viewer_name = Some("ada".into());
        context.viewer_id = Some("linear-user-id".into());
        let entries = ticket_action_table(&context, TicketActionMenuPage::Actions);
        assert_eq!(entries[5].disabled_reason, Some("already assigned"));

        context.assignee = Some("grace".into());
        let entries = ticket_action_table(&context, TicketActionMenuPage::Actions);
        assert!(entries[5].enabled());
        assert_eq!(context.viewer_id.as_deref(), Some("linear-user-id"));
    }

    #[test]
    fn ticket_action_submenus_dim_current_values() {
        let context = context();
        let transitions = ticket_action_table(&context, TicketActionMenuPage::Transitions);
        assert_eq!(transitions[1].disabled_reason, Some("current state"));
        let priorities = ticket_action_table(&context, TicketActionMenuPage::Priorities);
        assert_eq!(priorities[2].disabled_reason, Some("current priority"));
    }

    #[test]
    fn ticket_action_menu_opens_downward_and_scrolls_when_short() {
        let context = context();
        let state = TicketActionMenuState {
            selected: 11,
            ..Default::default()
        };
        let anchor = Rect::new(20, 5, 3, 1);
        let layout = ticket_action_menu_layout(anchor, Rect::new(0, 0, 50, 10), &context, state)
            .expect("space below the anchor");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.rect.bottom(), 10);
        assert!(layout.first_visible <= 11);
        assert!(11 < layout.first_visible + layout.visible_rows);
    }
}
