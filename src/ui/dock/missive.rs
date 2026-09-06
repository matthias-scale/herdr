//! Compact read-only Missive conversation for the focused pane.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
    Frame,
};

use crate::app::state::AppState;
use crate::ui::work_list_detail::{ConversationItem, WorkItem as _};

pub(crate) fn focused_conversation_url(app: &AppState) -> Option<String> {
    let (context, _) = super::chooser::focused_availability(app);
    context.missive_urls.first().cloned()
}

fn focused_conversation(app: &AppState) -> Option<ConversationItem<'_>> {
    let url = focused_conversation_url(app)?;
    let snapshot = app.work_index_snapshot.as_ref()?;
    let summary = snapshot.conversations.iter().find(|conversation| {
        conversation.app_url == url
            || url
                .split("/conversations/")
                .nth(1)
                .is_some_and(|id| id == conversation.id)
    })?;
    Some(ConversationItem {
        summary,
        observed_at: snapshot.observed_at,
    })
}

pub(crate) fn render_missive(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(item) = focused_conversation(app) else {
        let message = match app.work_index_snapshot.as_ref() {
            Some(snapshot) => snapshot
                .unavailable_reason(crate::work_index::WorkIndexSource::Missive)
                .map(|reason| format!(" Missive: {reason}"))
                .unwrap_or_else(|| " conversation absent from latest index".into()),
            None => " conversation not indexed yet".into(),
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(app.palette.overlay1)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    let detail = item.detail();
    let mut lines = vec![
        Line::styled(
            format!(" {}  {}", detail.heading, detail.title),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            format!(" {}", detail.byline),
            Style::default().fg(app.palette.subtext0),
        ),
    ];
    for section in &detail.sections {
        if section.entries.is_empty() {
            continue;
        }
        lines.push(Line::styled(
            format!(" {}  {}", section.label, section.entries.len()),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ));
        for entry in &section.entries {
            lines.push(Line::styled(
                format!(
                    "  {} · {} · {}",
                    entry.author.as_deref().unwrap_or("unknown"),
                    relative_time(entry.created_at, item.observed_at),
                    entry.body
                ),
                Style::default().fg(app.palette.subtext0),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);
}

fn relative_time(then: Option<std::time::SystemTime>, now: std::time::SystemTime) -> String {
    let Some(then) = then else {
        return "unknown".into();
    };
    let seconds = now
        .duration_since(then)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    if seconds >= 86_400 {
        format!("{}d", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}m", seconds / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_index::{
        MissiveConversation, MissiveEntry, MissiveUser, Snapshot, WorkIndexSource,
        WorkIndexUnavailable,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::SystemTime;

    fn conversation() -> MissiveConversation {
        MissiveConversation {
            id: "sample".into(),
            subject: "Billing question".into(),
            app_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            web_url: "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
            assignees: vec![MissiveUser {
                id: "ada".into(),
                name: "Ada".into(),
                email: None,
                is_me: true,
            }],
            last_activity_at: Some(SystemTime::UNIX_EPOCH),
            closed: false,
            pane_bound: false,
            messages: vec![MissiveEntry {
                id: "message".into(),
                author: Some("Customer".into()),
                preview: "Could you clarify the invoice?".into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
            }],
            notes: Vec::new(),
            drafts: Vec::new(),
            posts: Vec::new(),
        }
    }

    #[test]
    fn compact_missive_surface_renders_conversation_fixture() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("missive")];
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
                missive_urls: vec![
                    "https://mail.missiveapp.com/#inbox/conversations/sample".into(),
                ],
                ..Default::default()
            });
        app.work_index_snapshot = Some(Snapshot {
            items: Vec::new(),
            conversations: vec![conversation()],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_missive(&app, frame, frame.area()))
            .expect("render compact Missive surface");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Billing question"));
        assert!(text.contains("clarify the invoice"));
    }

    #[test]
    fn compact_missive_surface_is_available_only_with_context() {
        let empty = crate::work_context::PaneWorkContext::default();
        let linked = crate::work_context::PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/sample".into()],
            ..Default::default()
        };
        assert!(!super::super::chooser::surface_available(
            crate::app::DockSurface::Missive,
            &empty,
            true,
        ));
        assert!(super::super::chooser::surface_available(
            crate::app::DockSurface::Missive,
            &linked,
            true,
        ));
        let snapshot = Snapshot {
            items: Vec::new(),
            conversations: vec![conversation()],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: SystemTime::UNIX_EPOCH,
        };
        assert_eq!(snapshot.conversations.len(), 1);
    }

    #[test]
    fn unresolved_pane_conversation_shows_missive_failure_reason() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("missive")];
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
                missive_urls: vec![
                    "https://mail.missiveapp.com/#inbox/conversations/missing".into()
                ],
                ..Default::default()
            });
        app.work_index_snapshot = Some(Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: Some(WorkIndexUnavailable::only(
                WorkIndexSource::Missive,
                "token unavailable",
            )),
            observed_at: SystemTime::UNIX_EPOCH,
        });
        let mut terminal = Terminal::new(TestBackend::new(48, 4)).expect("test terminal");
        terminal
            .draw(|frame| render_missive(&app, frame, frame.area()))
            .expect("render Missive failure");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Missive: token unavailable"), "{text:?}");
        assert!(!text.contains("not indexed yet"), "{text:?}");

        app.work_index_snapshot
            .as_mut()
            .expect("snapshot")
            .unavailable = None;
        terminal
            .draw(|frame| render_missive(&app, frame, frame.area()))
            .expect("render successful empty Missive observation");
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("conversation absent from latest index"),
            "{text:?}"
        );
        assert!(!text.contains("not indexed yet"), "{text:?}");
    }
}
