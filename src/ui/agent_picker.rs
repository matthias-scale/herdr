use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::widgets::{modal_stack_areas, render_modal_header, render_modal_shell};
use crate::app::AppState;

/// The second step of the pane menu's "Send to agent...": one numbered row per
/// running agent, named the way the sidebar names it.
pub(super) fn render_agent_picker(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(picker) = app.agent_picker.as_ref() else {
        return;
    };
    let popup_h = (picker.candidates.len() as u16).saturating_add(6);
    let Some(inner) = render_modal_shell(frame, area, 78, popup_h, &app.palette) else {
        return;
    };
    let areas = modal_stack_areas(inner, 1, 1, 0, 1);
    render_modal_header(frame, areas.header, "SEND TO AGENT", &app.palette);

    let mut lines = vec![Line::from(Span::styled(
        crate::ui::text::truncate_end(&picker.text.replace('\n', " "), 74),
        Style::default().fg(app.palette.overlay0),
    ))];
    lines.extend(
        picker
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                Line::from(vec![
                    Span::styled(
                        format!("{} ", index + 1),
                        Style::default()
                            .fg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        candidate.label.clone(),
                        Style::default().fg(app.palette.text),
                    ),
                ])
            }),
    );
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        areas.content,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("1-9", Style::default().fg(app.palette.accent)),
            Span::styled(" select  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc", Style::default().fg(app.palette.accent)),
            Span::styled(" cancel", Style::default().fg(app.palette.overlay0)),
        ])),
        areas.footer.expect("picker footer is configured"),
    );
}
