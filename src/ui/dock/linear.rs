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

/// Tickets the operator can attach when detection found none.
///
/// Capped at nine because the picker selects by digit, the idiom the work-link
/// picker already uses. Sorted by identifier so the list does not reshuffle
/// under the cursor between index refreshes. Identifiers the manual work-context
/// tier would reject are dropped rather than offered and then refused; the index
/// can hold tickets from teams outside the recognised prefixes.
pub(crate) fn attachable_tickets(app: &AppState) -> Vec<(String, String)> {
    let Some(snapshot) = app.work_index_snapshot.as_ref() else {
        return Vec::new();
    };
    let mut tickets: Vec<(String, String)> = snapshot
        .items
        .iter()
        .flat_map(|item| item.ticket_details.iter())
        .map(|ticket| {
            (
                ticket.identifier.clone(),
                ticket.title.clone().unwrap_or_default(),
            )
        })
        .collect();
    tickets.retain(|(identifier, _)| crate::work_context::normalize_ticket_id(identifier).is_ok());
    // Order case-insensitively so the dedup below, which is case-insensitive,
    // always sees its duplicates adjacent.
    tickets.sort_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
    });
    tickets.dedup_by(|left, right| left.0.eq_ignore_ascii_case(&right.0));
    tickets.truncate(9);
    tickets
}

/// Ticket the picker attaches for a digit, if that row exists.
pub(crate) fn attachable_ticket_for_digit(app: &AppState, digit: char) -> Option<String> {
    let index = digit.to_digit(10)?.checked_sub(1)? as usize;
    attachable_tickets(app)
        .into_iter()
        .nth(index)
        .map(|(identifier, _)| identifier)
}

/// The picker shown in place of a ticket detail when nothing is attached.
fn render_ticket_picker(app: &AppState, frame: &mut Frame, area: Rect) {
    let tickets = attachable_tickets(app);
    if tickets.is_empty() {
        let message = match app.work_index_snapshot.as_ref() {
            Some(snapshot) => snapshot
                .short_unavailable_reason(
                    crate::work_index::WorkIndexSource::Linear,
                    std::time::SystemTime::now(),
                )
                .map(|reason| format!(" Linear: {reason}"))
                .unwrap_or_else(|| {
                    if snapshot
                        .items
                        .iter()
                        .any(|item| !item.ticket_details.is_empty())
                    {
                        " no ticket in the index can be attached".into()
                    } else {
                        " no tickets in the latest index".into()
                    }
                }),
            None => " ticket not indexed yet".into(),
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(app.palette.overlay1)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    }

    let mut lines = vec![ratatui::text::Line::from(ratatui::text::Span::styled(
        " No ticket for this pane. Attach one:",
        Style::default().fg(app.palette.overlay1),
    ))];
    for (index, (identifier, title)) in tickets.iter().enumerate() {
        lines.push(ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(
                format!(" {} ", index + 1),
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            ratatui::text::Span::styled(
                format!("{identifier} "),
                Style::default().fg(app.palette.text),
            ),
            ratatui::text::Span::styled(title.clone(), Style::default().fg(app.palette.overlay1)),
        ]));
    }
    // The dock can be shorter than the list. Rows past the fit are clipped by
    // the paragraph but their digits still attach, so say so rather than let
    // the count look complete.
    if lines.len() > area.height as usize {
        lines.truncate(area.height.saturating_sub(1) as usize);
        // One header line plus the rows kept above the hint.
        let shown = lines.len().saturating_sub(1);
        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
            format!(
                " … {} more, their digits still work",
                tickets.len().saturating_sub(shown)
            ),
            Style::default().fg(app.palette.overlay1),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// The detail layout the dock is drawing, so the input layer can number the
/// section digits and hit-test the headers against the same geometry.
pub(crate) fn focused_ticket_layout(
    app: &AppState,
    area: Rect,
) -> Option<crate::ui::work_view::DetailLayout> {
    let item = focused_ticket_item(app)?;
    let view = focused_ticket_key(app)
        .and_then(|key| app.dock_object_views.get(&key).cloned())
        .unwrap_or_default();
    Some(crate::ui::work_view::ticket_detail_layout(
        app,
        &item,
        &view,
        &crate::ui::work_view::TicketDetailControls {
            start_menu: app.dock_ticket_start_menu,
            action_menu: app.dock_ticket_action_menu,
            transition_menu: None,
            comment_draft: app.dock_ticket_comment_draft.as_deref(),
            pending_write: app.dock_pending_write.as_ref(),
            notice: app.dock_write_notice.as_deref(),
        },
        area,
    ))
}

pub(crate) fn render_linear(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(item) = focused_ticket_item(app) else {
        render_ticket_picker(app, frame, area);
        return;
    };
    // Key the view exactly as the input layer does. `stable_key()` uses the
    // index's canonical identifier while focus uses the work context's, and
    // the two differ by case often enough that scroll and expansion state
    // would land in an entry this render never reads.
    let key = focused_ticket_key(app).unwrap_or_else(|| item.stable_key());
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

    fn app_with_indexed_tickets(identifiers: &[&str]) -> AppState {
        let mut app = AppState::test_new();
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: identifiers
                .iter()
                .map(|identifier| {
                    let ticket = crate::work_index::WorkTicket {
                        identifier: (*identifier).into(),
                        title: Some(format!("{identifier} title")),
                        description: None,
                        state: None,
                        assignee: None,
                        creator: None,
                        priority: None,
                        cycle: None,
                        group: crate::work_index::TicketGroup::Assigned,
                        created_at: None,
                        updated_at: None,
                        branch: None,
                        labels: Vec::new(),
                        url: None,
                        parent: None,
                        relations: Vec::new(),
                    };
                    crate::work_index::WorkItem {
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
                        check_state: crate::work_index::PrCheckState::Unknown,
                        audience: crate::work_index::PrAudience::Unclassified,
                        cached_pr_detail: None,
                        ticket_ids: vec![ticket.identifier.clone()],
                        ticket_title: None,
                        ticket_state: None,
                        ticket_details: vec![ticket],
                        branch: None,
                        preview_urls: Vec::new(),
                        panes: Vec::new(),
                        source: crate::work_index::WorkItemSource::default(),
                    }
                })
                .collect(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        });
        app
    }

    #[test]
    fn attachable_tickets_are_sorted_deduped_and_capped_at_nine() {
        let identifiers: Vec<String> = (1..=12).map(|n| format!("MAT-{n:02}")).collect();
        let mut with_duplicate: Vec<&str> = identifiers.iter().map(String::as_str).collect();
        with_duplicate.reverse();
        with_duplicate.push("mat-01");
        let app = app_with_indexed_tickets(&with_duplicate);

        let tickets = attachable_tickets(&app);
        assert_eq!(tickets.len(), 9);
        let listed: Vec<&str> = tickets.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            listed,
            vec![
                "MAT-01", "MAT-02", "MAT-03", "MAT-04", "MAT-05", "MAT-06", "MAT-07", "MAT-08",
                "MAT-09",
            ]
        );
    }

    #[test]
    fn a_digit_selects_the_row_at_that_position() {
        let app = app_with_indexed_tickets(&["MAT-02", "MAT-01", "MAT-03"]);
        assert_eq!(
            attachable_ticket_for_digit(&app, '2').as_deref(),
            Some("MAT-02")
        );
        assert_eq!(attachable_ticket_for_digit(&app, '4'), None);
        assert_eq!(attachable_ticket_for_digit(&app, '0'), None);
    }

    #[test]
    fn case_variants_collapse_to_one_row() {
        let app = app_with_indexed_tickets(&["MAT-1", "mat-1", "SCA-2"]);
        let listed: Vec<String> = attachable_tickets(&app)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(listed, vec!["MAT-1".to_string(), "SCA-2".to_string()]);
    }

    #[test]
    fn identifiers_the_manual_tier_would_reject_are_not_offered() {
        let app = app_with_indexed_tickets(&["OPS-9", "MAT-1"]);
        let listed: Vec<String> = attachable_tickets(&app)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(listed, vec!["MAT-1".to_string()]);
    }

    #[test]
    fn a_short_dock_says_how_many_rows_it_hid() {
        let identifiers: Vec<String> = (1..=9).map(|n| format!("MAT-{n}")).collect();
        let refs: Vec<&str> = identifiers.iter().map(String::as_str).collect();
        let app = app_with_indexed_tickets(&refs);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 5))
            .expect("test terminal");
        terminal
            .draw(|frame| render_ticket_picker(&app, frame, Rect::new(0, 0, 60, 5)))
            .expect("draw the picker");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        // Header plus three rows fit; the hint replaces the fifth line.
        assert!(rendered.contains("MAT-3"), "{rendered}");
        assert!(rendered.contains("6 more"), "{rendered}");
    }

    #[test]
    fn an_empty_index_offers_nothing_to_attach() {
        let app = AppState::test_new();
        assert!(attachable_tickets(&app).is_empty());
        assert_eq!(attachable_ticket_for_digit(&app, '1'), None);
    }
}
