//! Compact Linear ticket surface for the focused pane's primary ticket.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, WorkItemKey};
use crate::ui::work_list_detail::{TicketItem, WorkItem as _};

pub(crate) fn focused_ticket_key(app: &AppState) -> Option<WorkItemKey> {
    let (context, _) = super::chooser::focused_availability(app);
    Some(WorkItemKey {
        repo: String::new(),
        pr_number: None,
        pr_url: None,
        ticket_id: Some(context.primary_ticket()?.to_string()),
    })
}

fn focused_ticket_item(app: &AppState) -> Option<TicketItem<'_>> {
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
        has_context_pr: false,
    })
}

pub(crate) fn render_linear(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(item) = focused_ticket_item(app) else {
        frame.render_widget(
            Paragraph::new(" ticket not indexed yet")
                .style(Style::default().fg(app.palette.overlay1)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    render_ticket_item(app, frame, area, &item);
}

pub(crate) fn render_ticket_item(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    item: &TicketItem<'_>,
) {
    let detail = item.detail();
    let mut lines = vec![
        Line::from(Span::styled(
            format!(" {}  {}", detail.heading, detail.title),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!(" {}", detail.byline),
            Style::default().fg(app.palette.subtext0),
        )),
        Line::from(Span::styled(
            format!(" Linked PRs  {}", detail.linked_prs.len()),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    for pr in &detail.linked_prs {
        let glyph = match pr.check_state {
            crate::work_index::PrCheckState::Passing => "✓",
            crate::work_index::PrCheckState::Failing => "✗",
            crate::work_index::PrCheckState::Pending => "◌",
            crate::work_index::PrCheckState::Unknown => "—",
        };
        lines.push(Line::from(Span::styled(
            format!("  ⑂ #{} {}  {glyph}", pr.number, pr.title),
            Style::default().fg(app.palette.subtext0),
        )));
    }
    if !detail.checks.is_empty() {
        lines.push(Line::from(Span::styled(
            " Acceptance criteria",
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        )));
        for (text, state) in &detail.checks {
            lines.push(Line::from(Span::styled(
                format!("  {} {text}", if state == "done" { "✓" } else { "✗" }),
                Style::default().fg(app.palette.subtext0),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        format!(" Comments  {}  newest first", detail.comments.len()),
        Style::default()
            .fg(app.palette.text)
            .add_modifier(Modifier::BOLD),
    )));
    for comment in &detail.comments {
        lines.push(Line::from(Span::styled(
            format!("  {}", comment.author.as_deref().unwrap_or("unknown")),
            Style::default().fg(app.palette.subtext0),
        )));
        lines.extend(crate::ui::markdown::body_lines(
            &app.palette,
            Some(&comment.body),
            usize::from(area.width.saturating_sub(4)),
            "    ",
        ));
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_index::{
        PrAudience, PrCheckState, TicketGroup, WorkItem as IndexedWorkItem, WorkItemComment,
        WorkItemDetail, WorkItemSource, WorkTicket,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::SystemTime;

    fn ticket() -> WorkTicket {
        WorkTicket {
            identifier: "SCA-3165".into(),
            title: Some("image edit reference".into()),
            description: Some("- [x] doc exists\n- [ ] registry updated".into()),
            state: Some("In Progress".into()),
            assignee: Some("matthias".into()),
            priority: Some(2),
            cycle: Some("cycle 34".into()),
            group: TicketGroup::Assigned,
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: Some("https://linear.app/scalable/issue/SCA-3165".into()),
            parent: None,
            relations: Vec::new(),
        }
    }

    fn pr() -> IndexedWorkItem {
        IndexedWorkItem {
            repo: "owner/repo".into(),
            pr_number: Some(166),
            pr_url: Some("https://github.com/owner/repo/pull/166".into()),
            pr_title: Some("skill data".into()),
            pr_state: Some("open".into()),
            draft: false,
            review_decision: None,
            created_at: None,
            updated_at: None,
            additions: 0,
            deletions: 0,
            author: None,
            labels: Vec::new(),
            check_state: PrCheckState::Passing,
            audience: PrAudience::Other,
            ticket_ids: vec!["SCA-3165".into()],
            ticket_title: None,
            ticket_state: None,
            ticket_details: Vec::new(),
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: WorkItemSource::default(),
        }
    }

    #[test]
    fn compact_linear_surface_renders_fixture() {
        let app = AppState::test_new();
        let ticket = ticket();
        let pr = pr();
        let mut cached = WorkItemDetail::empty();
        cached.comments.push(WorkItemComment {
            author: Some("ada".into()),
            body: "ready for review".into(),
            created_at: Some(SystemTime::UNIX_EPOCH),
        });
        let item = TicketItem {
            summary: &ticket,
            cached_detail: Some(&cached),
            linked_prs: vec![&pr],
            observed_at: SystemTime::UNIX_EPOCH,
            has_context_pr: false,
        };
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_ticket_item(&app, frame, frame.area(), &item))
            .expect("render compact Linear surface");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("SCA-3165"));
        assert!(text.contains("In Progress · P2 · matthias · cycle 34"));
        assert!(text.contains("#166 skill data  ✓"));
        assert!(text.contains("✓ doc exists"));
        assert!(text.contains("✗ registry updated"));
        assert!(text.contains("ready for review"));
    }

    #[test]
    fn compact_linear_surface_is_unavailable_without_ticket() {
        let app = AppState::test_new();
        assert!(focused_ticket_key(&app).is_none());
        assert!(focused_ticket_item(&app).is_none());
        assert!(!super::super::chooser::surface_available(
            crate::app::DockSurface::Linear,
            &crate::work_context::PaneWorkContext::default(),
            true,
        ));
    }
}
