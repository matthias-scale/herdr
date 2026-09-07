//! The Symphony surface: what a single headless workflow is doing, bound by
//! identity so the panel tracks the job rather than a snapshot of it.
//!
//! Pure presentation over `AppState::symphony_snapshot`, with a link out to the
//! job's details in the runner dashboard.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::AppState;
use crate::symphony::Workflow;
use crate::ui::text::display_width;

/// Label of the dashboard link, drawn in the surface's top-right corner.
pub(crate) const DASHBOARD_LINK: &str = "dashboard ↗";

/// Where the dashboard link sits, or `None` when the surface is too narrow to
/// draw it or there is no job to link to. Render and hit test share this so a
/// click can never land on a link that was not drawn.
pub(crate) fn dashboard_link_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let url = dashboard_url(app)?;
    let _ = url;
    let width = u16::try_from(display_width(DASHBOARD_LINK)).ok()?;
    // The title needs at least a few columns of its own; below that the link is
    // dropped rather than drawn over the name.
    if area.width < width.saturating_add(8) || area.height == 0 {
        return None;
    }
    Some(Rect::new(
        area.right().saturating_sub(width).saturating_sub(1),
        area.y,
        width,
        1,
    ))
}

/// Dashboard URL of the bound job, when there is one.
pub(crate) fn dashboard_url(app: &AppState) -> Option<String> {
    crate::symphony::dashboard_url(app.dock_symphony_workflow()?)
}

fn field<'a>(palette: &crate::app::state::Palette, label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!(" {label:<9}"),
            Style::default().fg(palette.overlay0),
        ),
        Span::styled(value.to_string(), Style::default().fg(palette.subtext0)),
    ])
}

pub(crate) fn render_symphony(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = &app.palette;
    let Some(selection) = app.dock_symphony.as_ref() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " No Symphony job selected",
                Style::default().fg(palette.overlay1),
            ))),
            Rect::new(area.x, area.y, area.width, 1),
        );
        return;
    };
    let Some(workflow) = app.dock_symphony_workflow() else {
        // The job left the snapshot: it finished, or the runtime went away. Say
        // which job is gone rather than blanking the surface.
        let mut lines = vec![Line::from(Span::styled(
            format!(" {} is no longer open", selection.workflow_id),
            Style::default().fg(palette.overlay1),
        ))];
        if let Some(message) = app.symphony_snapshot.unavailable.as_deref() {
            lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(palette.red),
            )));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };

    let mut lines = vec![Line::from(Span::styled(
        format!(" {}", workflow.name),
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.push(status_line(app, workflow));
    for (label, value) in [
        ("ticket", workflow.ticket.as_deref()),
        ("repo", workflow.repo.as_deref()),
        ("pr", workflow.pr.as_deref()),
        ("phase", Some(workflow.phase.as_str())),
        ("wait", workflow.wait.as_deref()),
        ("started", workflow.started_at.as_deref()),
        ("receipts", workflow.receipts.as_deref()),
        ("workflow", Some(workflow.workflow_id.as_str())),
        ("run", Some(workflow.run_id.as_str())),
    ] {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            continue;
        };
        lines.push(field(palette, label, value));
    }
    frame.render_widget(Paragraph::new(lines).scroll((app.dock_scroll, 0)), area);

    if let Some(rect) = dashboard_link_rect(app, area) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                DASHBOARD_LINK,
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::UNDERLINED),
            ))),
            rect,
        );
    }
}

/// The one line that says what the job is doing right now, in the sidebar's own
/// vocabulary: a named wait owes a human an answer, anything else is running.
fn status_line(app: &AppState, workflow: &Workflow) -> Line<'static> {
    let palette = &app.palette;
    let age = crate::ui::symphony::age_label_since(
        workflow.started_at.as_deref(),
        std::time::SystemTime::now(),
    );
    let (dot, color, status) = match workflow.wait.as_deref() {
        Some(wait) if !wait.trim().is_empty() => ("○", palette.red, format!("waiting on {wait}")),
        _ => ("●", palette.blue, format!("running {}", workflow.phase)),
    };
    Line::from(vec![
        Span::styled(format!(" {dot} "), Style::default().fg(color)),
        Span::styled(status, Style::default().fg(palette.subtext0)),
        Span::styled(
            format!("  {age}"),
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::SymphonyDockSelection;
    use crate::app::AppState;
    use ratatui::{backend::TestBackend, Terminal};

    fn workflow() -> crate::symphony::Workflow {
        crate::symphony::Workflow {
            workflow_id: "symphony-MAT-138".to_string(),
            run_id: "019a".to_string(),
            name: "Temporal blocker dashboard".to_string(),
            phase: "runFlowStep".to_string(),
            wait: Some("plan-sign-off".to_string()),
            started_at: Some("2026-08-11T08:00:00Z".to_string()),
            ticket: Some("MAT-138".to_string()),
            repo: Some("matthias-scale/herdr".to_string()),
            pr: None,
            receipts: None,
        }
    }

    fn bound_app() -> AppState {
        let mut app = AppState::test_new();
        app.symphony_snapshot = crate::symphony::Snapshot {
            workflows: vec![workflow()],
            unavailable: None,
        };
        app.dock_symphony = Some(SymphonyDockSelection {
            workflow_id: "symphony-MAT-138".to_string(),
            run_id: "019a".to_string(),
        });
        app
    }

    fn render(app: &AppState, area: Rect) -> Vec<String> {
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| render_symphony(app, frame, area))
            .expect("render symphony surface");
        let buffer = terminal.backend().buffer().clone();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn surface_shows_the_bound_job_with_its_status_and_context() {
        let app = bound_app();
        let area = Rect::new(0, 0, 44, 12);
        let text = render(&app, area);
        assert!(text[0].contains("Temporal blocker dashboard"), "{text:?}");
        assert!(text[1].contains("waiting on plan-sign-off"), "{text:?}");
        // A named wait uses the sidebar's blocked dot, not a second vocabulary.
        assert!(text[1].contains('\u{25cb}'), "{text:?}");
        let body = text.join("\n");
        for expected in ["MAT-138", "matthias-scale/herdr", "runFlowStep", "019a"] {
            assert!(body.contains(expected), "{expected}: {body}");
        }
    }

    #[test]
    fn dashboard_link_sits_top_right_and_maps_back_to_the_job_url() {
        let app = bound_app();
        let area = Rect::new(0, 0, 44, 12);
        let rect = dashboard_link_rect(&app, area).expect("link rect");
        assert_eq!(rect.y, area.y, "the link belongs on the title row");
        assert_eq!(rect.right(), area.right().saturating_sub(1));
        assert!(render(&app, area)[0].contains(DASHBOARD_LINK));
        assert_eq!(
            dashboard_url(&app).as_deref(),
            Some(
                "http://localhost:8233/namespaces/default/workflows/symphony-MAT-138/019a/history"
            )
        );

        // Too narrow to hold both the name and the link: the link is dropped
        // rather than drawn over the title.
        assert_eq!(dashboard_link_rect(&app, Rect::new(0, 0, 14, 12)), None);
    }

    #[test]
    fn surface_says_which_job_closed_instead_of_going_blank() {
        let mut app = bound_app();
        app.symphony_snapshot.workflows.clear();
        let text = render(&app, Rect::new(0, 0, 44, 4));
        assert!(text[0].contains("symphony-MAT-138"), "{text:?}");
        assert!(text[0].contains("no longer open"), "{text:?}");
        assert_eq!(dashboard_link_rect(&app, Rect::new(0, 0, 44, 4)), None);
    }

    #[test]
    fn unbound_surface_says_so() {
        let mut app = bound_app();
        app.dock_symphony = None;
        assert!(render(&app, Rect::new(0, 0, 44, 4))[0].contains("No Symphony job selected"));
    }
}
