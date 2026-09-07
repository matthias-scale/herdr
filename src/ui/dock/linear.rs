//! Linear object resolution for the dock.
//!
//! Rendering lives in `ui::work_view`; the dock hosts the same full ticket
//! detail at its current width.

use ratatui::{layout::Rect, style::Style, widgets::Paragraph, Frame};

use crate::app::state::{AppState, ObjectViewState, WorkItemKey};
use crate::ui::work_list_detail::TicketItem;

pub(crate) fn focused_ticket_key(app: &AppState) -> Option<WorkItemKey> {
    let (context, _) = super::chooser::focused_availability(app);
    let ticket_id = app
        .presented_dock_object(crate::app::DockSurface::Linear)
        .map(|object| object.key.as_str())
        .or_else(|| context.primary_ticket())?;
    Some(WorkItemKey {
        repo: String::new(),
        pr_number: None,
        pr_url: None,
        ticket_id: Some(ticket_id.to_string()),
    })
}

pub(crate) fn focused_ticket_item(app: &AppState) -> Option<TicketItem<'_>> {
    let key = focused_ticket_key(app)?;
    let ticket_id = key.ticket_id.as_deref()?;
    let snapshot = app.work_index_snapshot.as_ref()?;
    let summary = snapshot
        .items
        .iter()
        .flat_map(|item| item.ticket_details.iter())
        .find(|ticket| ticket.identifier.eq_ignore_ascii_case(ticket_id))?;
    Some(TicketItem {
        summary,
        cached_detail: app.work_item_detail_cache.get(&key),
        linked_prs: snapshot
            .items
            .iter()
            .filter(|item| {
                item.pr_number.is_some()
                    && item
                        .ticket_ids
                        .iter()
                        .any(|ticket| ticket.eq_ignore_ascii_case(ticket_id))
            })
            .collect(),
        observed_at: snapshot.observed_at,
        has_context_pr: super::pr::focused_pr_key(app).is_some(),
    })
}

pub(crate) fn render_linear(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(item) = focused_ticket_item(app) else {
        let message = match app.work_index_snapshot.as_ref() {
            Some(snapshot) => snapshot
                .short_unavailable_reason(
                    crate::work_index::WorkIndexSource::Linear,
                    std::time::SystemTime::now(),
                )
                .map(|reason| format!(" Linear: {reason}"))
                .unwrap_or_else(|| " ticket absent from latest index".into()),
            None => " ticket not indexed yet".into(),
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(app.palette.overlay1)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    let key = item.stable_key();
    let view = app
        .dock_object_views
        .get(&key)
        .cloned()
        .unwrap_or_else(ObjectViewState::default);
    crate::ui::work_view::render_ticket_detail(
        app,
        &item,
        &view,
        crate::ui::work_view::TicketDetailControls {
            start_menu: app.dock_ticket_start_menu,
            action_menu: app.dock_ticket_action_menu,
            comment_draft: app.dock_ticket_comment_draft.as_deref(),
            pending_write: app.dock_pending_write.as_ref(),
            notice: app.dock_write_notice.as_deref(),
            ..Default::default()
        },
        area,
        frame,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_linear_is_unavailable_without_an_object() {
        let app = AppState::test_new();
        assert!(focused_ticket_key(&app).is_none());
        assert!(focused_ticket_item(&app).is_none());
    }
}
