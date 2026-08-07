use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use crate::{
    app::{state::InfoPanelLinkRow, AppState},
    terminal::{state::AgentMetadata, TerminalState},
};

fn field_line(app: &AppState, label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default()
                .fg(app.palette.overlay0)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.into(), Style::default().fg(app.palette.text)),
    ])
}

fn focused_terminal<'a>(
    app: &'a AppState,
) -> Option<(&'a crate::workspace::Workspace, &'a TerminalState)> {
    let workspace = app.active.and_then(|index| app.workspaces.get(index))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id = workspace.terminal_id(pane_id)?;
    let terminal = app.terminals.get(terminal_id)?;
    Some((workspace, terminal))
}

fn pane_title(terminal: &TerminalState) -> String {
    terminal
        .manual_label
        .clone()
        .or_else(|| terminal.effective_title())
        .or_else(|| terminal.terminal_title_stripped())
        .unwrap_or_else(|| "untitled".to_string())
}

fn worktree_label(workspace: &crate::workspace::Workspace) -> String {
    if let Some(worktree) = workspace.worktree_space() {
        return format!("{} ({})", worktree.label, worktree.checkout_path.display());
    }
    workspace
        .git_space()
        .map(|space| format!("{} ({})", space.repo_name, space.repo_root.display()))
        .unwrap_or_else(|| "—".to_string())
}

fn session_value(terminal: &TerminalState) -> String {
    terminal
        .hook_authority
        .as_ref()
        .and_then(|authority| authority.session_ref.as_ref())
        .or_else(|| {
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref)
        })
        .map(|session| session.value.clone())
        .unwrap_or_else(|| "—".to_string())
}

fn latest_update(terminal: &TerminalState) -> Option<std::time::Instant> {
    terminal
        .agent_metadata
        .values()
        .map(|metadata| metadata.reported_at)
        .chain(
            terminal
                .hook_authority
                .as_ref()
                .map(|authority| authority.reported_at),
        )
        .max()
}

fn metadata_lines(app: &AppState, terminal: &TerminalState) -> Vec<Line<'static>> {
    let mut metadata = terminal.agent_metadata.values().collect::<Vec<_>>();
    // Sorted by source so the panel keeps the same shape between frames: the map's
    // iteration order is arbitrary, and a block that reshuffles under the reader is
    // worse than one whose order nobody chose.
    metadata.sort_by(|left, right| left.source.cmp(&right.source));
    metadata
        .into_iter()
        .flat_map(|metadata| metadata_lines_for_report(app, metadata))
        .collect()
}

fn metadata_lines_for_report(app: &AppState, metadata: &AgentMetadata) -> Vec<Line<'static>> {
    let age = crate::activity_age::compact_label(Some(metadata.reported_at), app.view_observed_at);
    let mut lines = vec![field_line(
        app,
        "report",
        format!("{} · {age} ago", metadata.source),
    )];
    if let Some(agent) = &metadata.agent_label {
        lines.push(field_line(app, "reported agent", agent.clone()));
    }
    if let Some(display_agent) = &metadata.display_agent {
        lines.push(field_line(app, "reported model", display_agent.clone()));
    }
    if let Some(title) = &metadata.title {
        lines.push(field_line(app, "reported title", title.clone()));
    }
    if !metadata.state_labels.is_empty() {
        let mut labels = metadata
            .state_labels
            .iter()
            .map(|(state, label)| format!("{state}={label}"))
            .collect::<Vec<_>>();
        labels.sort();
        lines.push(field_line(app, "reported states", labels.join(", ")));
    }
    lines
}

fn context_lines(app: &AppState) -> Option<Vec<Line<'static>>> {
    let (workspace, terminal) = focused_terminal(app)?;
    let pane_id = workspace.focused_pane_id()?;
    let tab = workspace
        .active_tab_display_name()
        .unwrap_or_else(|| "—".to_string());
    let provider = terminal
        .effective_agent_label()
        .map(str::to_string)
        .unwrap_or_else(|| "terminal".to_string());
    let model = terminal
        .effective_display_agent()
        .unwrap_or_else(|| "—".to_string());
    let state = super::status::state_label(terminal.state, true);
    let last_update = latest_update(terminal)
        .map(|at| {
            format!(
                "{} ago",
                crate::activity_age::compact_label(Some(at), app.view_observed_at)
            )
        })
        .unwrap_or_else(|| "—".to_string());

    let mut lines = vec![
        Line::from(Span::styled(
            " CONTEXT",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        field_line(app, "title", pane_title(terminal)),
        field_line(
            app,
            "space",
            format!(
                "{} / tab {tab}",
                workspace.display_name_from_terminals(&app.terminals)
            ),
        ),
        field_line(app, "pane", pane_id.raw().to_string()),
        field_line(app, "provider", provider),
        field_line(app, "model", model),
        field_line(app, "state", state),
        field_line(app, "cwd", terminal.cwd.display().to_string()),
        field_line(app, "worktree", worktree_label(workspace)),
        field_line(app, "session", session_value(terminal)),
        field_line(app, "last update", last_update),
    ];
    lines.extend(metadata_lines(app, terminal));
    Some(lines)
}

/// The work links sit in their own block at the foot of the tab rather than in the
/// scrolling field list above: a click has to land on a known row to copy the URL,
/// and rows the paragraph wrapper reflowed cannot be located again from outside.
fn split_off_links_area(area: Rect, link_count: usize) -> (Rect, Option<Rect>) {
    if link_count == 0 || area.height < 4 {
        return (area, None);
    }
    let wanted = u16::try_from(link_count.saturating_add(1))
        .unwrap_or(u16::MAX)
        .min(area.height / 2);
    let fields_height = area.height.saturating_sub(wanted);
    (
        Rect::new(area.x, area.y, area.width, fields_height),
        Some(Rect::new(
            area.x,
            area.y.saturating_add(fields_height),
            area.width,
            wanted,
        )),
    )
}

fn link_row_rects(links_area: Rect, link_count: usize) -> Vec<Rect> {
    (0..link_count)
        .map_while(|index| {
            let y = links_area.y.saturating_add(1).checked_add(index as u16)?;
            (y < links_area.y.saturating_add(links_area.height))
                .then(|| Rect::new(links_area.x, y, links_area.width, 1))
        })
        .collect()
}

/// Registered with the same click-to-copy path the work-context panel uses, so a
/// link in the dock copies exactly what the panel's copy would.
pub(crate) fn context_link_rows(app: &AppState, area: Rect) -> Vec<InfoPanelLinkRow> {
    let Some((_, terminal)) = focused_terminal(app) else {
        return Vec::new();
    };
    let candidates = super::info_panel::visible_candidates(terminal);
    let (_, Some(links_area)) = split_off_links_area(area, candidates.len()) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .zip(link_row_rects(links_area, usize::from(links_area.height)))
        .map(|(candidate, rect)| InfoPanelLinkRow {
            rect,
            copy_value: candidate.copy_value,
        })
        .collect()
}

fn render_links(app: &AppState, frame: &mut Frame, links_area: Rect) {
    let Some((_, terminal)) = focused_terminal(app) else {
        return;
    };
    let candidates = super::info_panel::visible_candidates(terminal);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " WORK LINKS",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(links_area.x, links_area.y, links_area.width, 1),
    );
    for (candidate, rect) in candidates
        .iter()
        .zip(link_row_rects(links_area, usize::from(links_area.height)))
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {}: ", super::info_panel::link_prefix(candidate.kind)),
                    Style::default().fg(app.palette.overlay0),
                ),
                Span::styled(
                    candidate.label.clone(),
                    Style::default().fg(app.palette.text),
                ),
            ])),
            rect,
        );
    }
}

pub(super) fn render_context(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(lines) = context_lines(app) else {
        frame.render_widget(Paragraph::new(" no focused pane"), area);
        return;
    };
    let link_count = focused_terminal(app)
        .map(|(_, terminal)| super::info_panel::visible_candidates(terminal).len())
        .unwrap_or(0);
    let (area, links_area) = split_off_links_area(area, link_count);
    if let Some(links_area) = links_area {
        render_links(app, frame, links_area);
    }

    let viewport = area.height as usize;
    let initial_rows = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(area.width.max(1));
    let needs_scrollbar = initial_rows > viewport && area.width > 1;
    let text_width = if needs_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let total_rows = Paragraph::new(lines.clone())
        .wrap(Wrap { trim: false })
        .line_count(text_width.max(1));
    let max_scroll = total_rows.saturating_sub(viewport);
    let scroll = usize::from(app.dock_scroll).min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows: viewport.max(1),
    };
    let track = release_notes_scrollbar_rect(area, metrics);
    let text_area = track
        .map(|_| Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height))
        .unwrap_or(area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        text_area,
    );
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn rendered_text(terminal: &Terminal<TestBackend>, area: Rect) -> String {
        (area.y..area.y + area.height)
            .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
            .map(|(x, y)| terminal.backend().buffer()[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn the_context_tab_renders_focused_pane_report_metadata_and_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("review-space")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = app.workspaces[0].terminal_id(pane_id).cloned().unwrap();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.state = crate::detect::AgentState::Working;
        terminal.manual_label = Some("focused worker".into());
        terminal.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("session-128").unwrap(),
        });
        terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
            source: "provider:codex".into(),
            agent_label: Some("codex".into()),
            applies_to_source: None,
            title: Some("review task".into()),
            display_agent: Some("gpt-test".into()),
            state_labels: std::collections::HashMap::from([(
                String::from("working"),
                String::from("thinking"),
            )]),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });
        let area = Rect::new(0, 0, 42, 20);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| render_context(&app, frame, area))
            .unwrap();

        let rendered = rendered_text(&terminal, area);
        for expected in [
            "focused worker",
            "review-space",
            "provider:codex",
            "gpt-test",
            "working",
            "session-128",
            "cwd:",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected} in {rendered:?}"
            );
        }
    }

    #[test]
    fn the_context_tab_says_when_no_pane_is_focused() {
        let app = AppState::test_new();
        let area = Rect::new(0, 0, 24, 4);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| render_context(&app, frame, area))
            .unwrap();

        assert!(rendered_text(&terminal, area).contains("no focused pane"));
    }
}
