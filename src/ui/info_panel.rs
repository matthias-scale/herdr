use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::widgets::render_panel_shell;
use crate::{
    app::{state::InfoPanelLinkRow, AppState},
    terminal::{TerminalId, TerminalState},
    work_context::{work_link_candidates, WorkLinkKind},
};

pub(crate) const INFO_PANEL_MIN_WIDTH: u16 = 26;
const INFO_PANEL_WIDTH: u16 = 36;
const INFO_PANEL_MIN_MAIN_WIDTH: u16 = 44;

pub(crate) fn panel_width_for_main(main_width: u16) -> Option<u16> {
    let available = main_width.saturating_sub(INFO_PANEL_MIN_MAIN_WIDTH);
    (available >= INFO_PANEL_MIN_WIDTH).then_some(INFO_PANEL_WIDTH.min(available))
}

fn focused_terminal(app: &AppState) -> Option<&TerminalState> {
    let workspace = app.active.and_then(|ws_idx| app.workspaces.get(ws_idx))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id: &TerminalId = workspace.terminal_id(pane_id)?;
    app.terminals.get(terminal_id)
}

pub(super) fn visible_candidates(terminal: &TerminalState) -> Vec<crate::work_context::WorkLinkCandidate> {
    let mut preview_seen = false;
    work_link_candidates(terminal.effective_work_context())
        .into_iter()
        .filter(|candidate| {
            if candidate.kind != WorkLinkKind::Preview {
                return true;
            }
            if preview_seen {
                false
            } else {
                preview_seen = true;
                true
            }
        })
        .take(9)
        .collect()
}

fn info_panel_layout_line(inner: Rect, index: usize) -> Option<Rect> {
    let offset = u16::try_from(index).ok()?;
    let y = inner.y.checked_add(offset)?;
    (y < inner.y.saturating_add(inner.height)).then_some(Rect::new(inner.x, y, inner.width, 1))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InfoPanelLayout {
    inner: Rect,
    link_rows: Vec<Rect>,
}

impl InfoPanelLayout {
    fn line(&self, index: usize) -> Option<Rect> {
        info_panel_layout_line(self.inner, index)
    }
}

fn info_panel_layout(area: Rect, link_count: usize) -> Option<InfoPanelLayout> {
    if area.width < 2 || area.height < 3 {
        return None;
    }
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(area);
    let link_rows = (0..link_count)
        .filter_map(|index| info_panel_layout_line(inner, 2usize.saturating_add(index)))
        .collect();
    Some(InfoPanelLayout { inner, link_rows })
}

pub(crate) fn compute_link_rows(app: &AppState, area: Rect) -> Vec<InfoPanelLinkRow> {
    let Some(terminal) = focused_terminal(app) else {
        return Vec::new();
    };
    let candidates = visible_candidates(terminal);
    let Some(layout) = info_panel_layout(area, candidates.len()) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .zip(layout.link_rows)
        .map(|(candidate, rect)| InfoPanelLinkRow {
            rect,
            copy_value: candidate.copy_value,
        })
        .collect()
}

fn state_label(terminal: &TerminalState) -> String {
    let agent = terminal
        .effective_display_agent()
        .or_else(|| terminal.effective_agent_label().map(str::to_string))
        .unwrap_or_else(|| "terminal".to_string());
    format!(
        "{agent} · {}",
        super::status::state_label(terminal.state, true)
    )
}

fn field_line(label: &str, value: &str, p: &crate::app::state::Palette) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(p.text)),
    ])
}

pub(super) fn link_prefix(kind: WorkLinkKind) -> &'static str {
    match kind {
        WorkLinkKind::Ticket => "ticket",
        WorkLinkKind::PullRequest => "pr",
        WorkLinkKind::Preview => "preview",
    }
}

pub(super) fn render_info_panel(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_panel_shell(frame, area, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let Some(terminal) = focused_terminal(app) else {
        frame.render_widget(Paragraph::new("no focused pane"), inner);
        return;
    };

    let context = terminal.effective_work_context();
    let candidates = visible_candidates(terminal);
    let Some(layout) = info_panel_layout(area, candidates.len()) else {
        return;
    };
    debug_assert_eq!(inner, layout.inner);
    let title = context.work_title.as_deref().unwrap_or("untitled");
    let branch = context.branch.as_deref().unwrap_or("—");

    if let Some(row) = layout.line(0) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "WORK CONTEXT",
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            ))),
            row,
        );
    }
    if let Some(row) = layout.line(1) {
        frame.render_widget(
            Paragraph::new(field_line("title", title, &app.palette)),
            row,
        );
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(row) = layout.link_rows.get(index).copied() else {
            break;
        };
        let number = if index < 9 {
            format!("{} ", index + 1)
        } else {
            "  ".to_string()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(number, Style::default().fg(app.palette.accent)),
                Span::styled(
                    format!("{}: ", link_prefix(candidate.kind)),
                    Style::default().fg(app.palette.overlay0),
                ),
                Span::styled(
                    candidate.label.clone(),
                    Style::default().fg(app.palette.text),
                ),
            ])),
            row,
        );
    }
    if candidates.is_empty() {
        if let Some(row) = layout.line(2) {
            frame.render_widget(Paragraph::new(field_line("links", "—", &app.palette)), row);
        }
    }
    let footer_start = 2usize.saturating_add(candidates.len().max(1));
    if let Some(row) = layout.line(footer_start) {
        frame.render_widget(
            Paragraph::new(field_line("branch", branch, &app.palette)),
            row,
        );
    }
    if let Some(row) = layout.line(footer_start.saturating_add(1)) {
        frame.render_widget(
            Paragraph::new(field_line("agent", &state_label(terminal), &app.palette)),
            row,
        );
    }

    // Keep each link as one screen row so its hit target matches the rendered
    // numbering even when a URL is longer than the desktop panel.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::state::AppState, work_context::PaneWorkContextPatch};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn info_panel_link_rows_follow_shared_candidate_order() {
        let mut app = AppState::test_new();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("one"));
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane_id).cloned().unwrap();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                pr_urls: Some(vec!["https://github.com/o/r/pull/2".into()]),
                ..Default::default()
            })
            .unwrap();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                preview_urls: vec!["https://preview.vercel.app".into()],
                ..Default::default()
            })
            .unwrap();

        let area = Rect::new(0, 0, 50, 12);
        let rows = compute_link_rows(&app, area);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].copy_value, "MAT-1");
        assert_eq!(rows[1].copy_value, "https://github.com/o/r/pull/2");
        assert_eq!(rows[2].copy_value, "https://preview.vercel.app");

        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_info_panel(&app, frame, area))
            .unwrap();
        for (row, label) in rows.iter().zip([
            "MAT-1",
            "https://github.com/o/r/pull/2",
            "https://preview.vercel.app",
        ]) {
            let rendered = (area.x..area.x + area.width)
                .map(|x| terminal.backend().buffer()[(x, row.rect.y)].symbol())
                .collect::<String>();
            assert!(rendered.contains(label), "row {}: {rendered}", row.rect.y);
        }
    }
}
