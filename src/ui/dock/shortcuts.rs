use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::text::display_width;
use crate::app::AppState;

fn shortcut_lines(app: &AppState) -> Vec<Line<'static>> {
    let groups = super::keybind_help::keybind_help_groups(app);
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| display_width(key)))
        .max()
        .unwrap_or(8);
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);

    groups
        .into_iter()
        .flat_map(|(group, entries)| {
            let mut lines = vec![Line::from(Span::styled(format!(" {group}"), heading_style))];
            for (key, label) in entries {
                let padding = key_width.saturating_sub(display_width(&key)) + 1;
                lines.push(Line::from(vec![
                    Span::styled(format!(" {key}{} ", " ".repeat(padding)), key_style),
                    Span::styled(label.into_owned(), label_style),
                ]));
            }
            lines.push(Line::raw(""));
            lines
        })
        .collect()
}

fn wrapped_line_count(lines: &[Line<'static>], width: u16) -> usize {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
}

pub(super) fn render_shortcuts(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = shortcut_lines(app);
    let viewport = area.height as usize;
    let initial_rows = wrapped_line_count(&lines, area.width);
    let needs_scrollbar = initial_rows > viewport && area.width > 1;
    let text_width = if needs_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    let total_rows = wrapped_line_count(&lines, text_width);
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
    fn the_shortcuts_tab_lists_the_users_configured_binds_not_the_defaults() {
        let mut app = AppState::test_new();
        app.keybinds.help = crate::config::ActionKeybinds::direct("ctrl+alt+h");
        let area = Rect::new(0, 0, 24, 8);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();

        terminal
            .draw(|frame| render_shortcuts(&app, frame, area))
            .unwrap();

        let rendered = rendered_text(&terminal, area);
        assert!(rendered.contains("ctrl+alt+h"), "rendered: {rendered:?}");
        assert!(rendered.contains("keybinds"), "rendered: {rendered:?}");
        assert!(rendered.contains('▐'), "rendered: {rendered:?}");
        assert!(!rendered.contains("not implemented yet"));
    }
}
