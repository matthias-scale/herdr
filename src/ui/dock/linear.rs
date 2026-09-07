//! Compact Linear ticket surface for the focused pane's primary ticket.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use crate::app::state::{AppState, WorkItemKey};
use crate::ui::work_list_detail::{comment_header, section_separator, TicketItem, WorkItem as _};

pub(crate) fn focused_ticket_key(app: &AppState) -> Option<WorkItemKey> {
    let (context, _) = super::chooser::focused_availability(app);
    let ticket_id = app
        .active_dock_object(crate::app::DockSurface::Linear)
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
            " [Start thread ▾] [⋯]",
            Style::default().fg(app.palette.accent),
        )),
    ];
    lines.extend(section_separator(
        &app.palette,
        format!("Linked PRs  {}", detail.linked_prs.len()),
        area.width,
    ));
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
    lines.extend(section_separator(&app.palette, "Description", area.width));
    lines.extend(crate::ui::markdown::body_lines(
        &app.palette,
        crate::ui::work_list_detail::description_without_checklist(detail.description.as_deref())
            .as_deref(),
        usize::from(area.width.saturating_sub(2)),
        " ",
    ));
    if !detail.checks.is_empty() {
        lines.extend(section_separator(
            &app.palette,
            "Acceptance criteria",
            area.width,
        ));
        for (text, state) in &detail.checks {
            lines.push(Line::from(Span::styled(
                format!("  {} {text}", if state == "done" { "✓" } else { "✗" }),
                Style::default().fg(app.palette.subtext0),
            )));
        }
    }
    lines.extend(section_separator(
        &app.palette,
        format!("Comments  {}  newest first", detail.comments.len()),
        area.width,
    ));
    for (index, comment) in detail.comments.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", comment_header(comment, item.observed_at)),
            Style::default()
                .fg(app.palette.subtext0)
                .add_modifier(Modifier::DIM),
        )));
        lines.extend(crate::ui::markdown::body_lines(
            &app.palette,
            Some(&comment.body),
            usize::from(area.width.saturating_sub(4)),
            "    ",
        ));
    }
    if let Some(draft) = app.dock_ticket_comment_draft.as_deref() {
        lines.push(Line::from(Span::styled(
            format!(" Comment: {draft}▏  Enter to stage · Esc cancel"),
            Style::default().fg(app.palette.yellow),
        )));
    }
    if let Some(write) = app.dock_pending_write.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(" Confirm {}? [y/N]", write.describe()),
            Style::default()
                .fg(app.palette.yellow)
                .add_modifier(Modifier::BOLD),
        )));
    } else if let Some(notice) = app.dock_write_notice.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(" {notice}"),
            Style::default().fg(app.palette.subtext0),
        )));
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);

    if let Some(choice) = app.dock_ticket_start_menu {
        render_start_menu(app, frame, area, choice);
    } else if let Some(menu) = app.dock_ticket_action_menu {
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            item.summary,
            item.cached_detail,
            app.work_index_session.linear.viewer.as_deref(),
            app.work_index_session.linear_viewer_identity(),
            item.has_context_pr,
        );
        crate::ui::ticket_actions::render_ticket_action_menu(
            &app.palette,
            frame,
            area,
            ticket_action_menu_anchor(area),
            &context,
            menu,
        );
    }
}

fn ticket_action_menu_anchor(area: Rect) -> Rect {
    Rect::new(
        area.right().saturating_sub(3),
        area.y.saturating_add(2),
        3.min(area.width),
        1,
    )
}

fn render_start_menu(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    choice: crate::app::state::PrCheckoutChoice,
) {
    let anchor = Rect::new(area.x.saturating_add(1), area.y.saturating_add(2), 16, 1);
    let Some(layout) = crate::ui::dropdown::layout_dropdown(
        &crate::ui::dropdown::DropdownSpec {
            anchor,
            item_count: 2,
            selected: usize::from(choice == crate::app::state::PrCheckoutChoice::NewWorktree),
            has_filter: false,
            max_rows: 2,
            min_width: 20,
        },
        area,
    ) else {
        return;
    };
    let lines = [
        (
            crate::app::state::PrCheckoutChoice::CurrentCheckout,
            "Current checkout",
        ),
        (
            crate::app::state::PrCheckoutChoice::NewWorktree,
            "New worktree",
        ),
    ]
    .into_iter()
    .map(|(option, label)| {
        Line::from(Span::styled(
            format!("{} {label}", if option == choice { "▸" } else { " " }),
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.panel_bg),
        ))
    })
    .collect::<Vec<_>>();
    frame.render_widget(Clear, layout.rect);
    frame.render_widget(Paragraph::new(lines), layout.rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_index::{
        PrAudience, PrCheckState, TicketGroup, WorkIndexSource, WorkIndexUnavailable,
        WorkItem as IndexedWorkItem, WorkItemComment, WorkItemDetail, WorkItemSource, WorkTicket,
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
            creator: None,
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
            assignees: Vec::new(),
            labels: Vec::new(),
            check_state: PrCheckState::Passing,
            audience: PrAudience::Other,
            cached_pr_detail: None,
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
        cached.body = Some(
            "Ticket body with https://example.invalid/a/very/long/path/that/must/wrap.\n- [x] doc exists\n- [ ] registry updated"
                .into(),
        );
        cached.comments.push(WorkItemComment {
            author: Some("ada".into()),
            body: "ready for review".into(),
            created_at: Some(SystemTime::UNIX_EPOCH),
        });
        cached.comments.push(WorkItemComment {
            author: Some("grace".into()),
            body: "newer comment".into(),
            created_at: Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(60)),
        });
        let item = TicketItem {
            summary: &ticket,
            cached_detail: Some(&cached),
            linked_prs: vec![&pr],
            observed_at: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(120),
            has_context_pr: false,
        };
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_ticket_item(&app, frame, frame.area(), &item))
            .expect("render compact Linear surface");
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let rows = (0..24)
            .map(|row| {
                (0..60)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(text.contains("SCA-3165"));
        assert!(text.contains("[Start thread ▾] [⋯]"));
        assert!(text.contains("In Progress · P2 · matthias · cycle 34"));
        assert!(text.contains("#166 skill data  ✓"));
        assert!(text.contains("Description"));
        assert!(text.contains("Ticket body with"));
        assert!(text.contains("https://example.invalid"));
        assert!(text.contains("✓ doc exists"));
        assert!(text.contains("✗ registry updated"));
        assert!(text.contains("grace · 1m"));
        assert!(text.contains("ada · 2m"));
        assert!(text.contains("ready for review"));
        let newer_body = rows
            .iter()
            .position(|row| row == "    newer comment")
            .expect("newer comment row");
        assert!(rows[newer_body + 1].is_empty());
        assert_eq!(rows[newer_body + 2], "  ada · 2m");
    }

    #[test]
    fn compact_linear_menu_uses_shared_ticket_actions() {
        let mut app = AppState::test_new();
        app.dock_ticket_action_menu = Some(Default::default());
        let ticket = ticket();
        let item = TicketItem {
            summary: &ticket,
            cached_detail: None,
            linked_prs: Vec::new(),
            observed_at: SystemTime::UNIX_EPOCH,
            has_context_pr: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).expect("test terminal");
        terminal
            .draw(|frame| render_ticket_item(&app, frame, frame.area(), &item))
            .expect("render compact ticket menu");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in ["Refresh", "Ask a question", "Priority ▸", "Cancel ticket"] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    }

    #[test]
    fn compact_linear_menu_opens_downward_and_clamps() {
        let ticket = ticket();
        let context = crate::ui::ticket_actions::TicketActionContext::from_ticket(
            &ticket, None, None, None, false,
        );
        let area = Rect::new(40, 3, 50, 7);
        let anchor = ticket_action_menu_anchor(area);
        let layout = crate::ui::ticket_actions::ticket_action_menu_layout(
            anchor,
            area,
            &context,
            crate::ui::ticket_actions::TicketActionMenuState {
                selected: 12,
                ..Default::default()
            },
        )
        .expect("rows below the dock action row");
        assert_eq!(layout.rect.y, anchor.bottom());
        assert_eq!(layout.rect.bottom(), area.bottom());
        assert!(layout.first_visible <= 12);
        assert!(12 < layout.first_visible + layout.visible_rows);
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
            false,
        ));
    }

    #[test]
    fn unresolved_pane_ticket_shows_linear_failure_reason() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("linear")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("focused terminal")
            .replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-9999".into()],
                ..Default::default()
            });
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(
                WorkIndexSource::Linear,
                "rate limited",
            )),
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let mut terminal = Terminal::new(TestBackend::new(48, 4)).expect("test terminal");
        terminal
            .draw(|frame| render_linear(&app, frame, frame.area()))
            .expect("render Linear failure");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Linear: rate limited"), "{text:?}");
        assert!(!text.contains("not indexed yet"), "{text:?}");

        app.work_index_snapshot
            .as_mut()
            .expect("snapshot")
            .unavailable = None;
        terminal
            .draw(|frame| render_linear(&app, frame, frame.area()))
            .expect("render successful empty Linear observation");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("ticket absent from latest index"), "{text:?}");
        assert!(!text.contains("not indexed yet"), "{text:?}");
    }
}
