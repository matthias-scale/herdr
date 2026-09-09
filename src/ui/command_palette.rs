use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::{
    scrollbar::{render_scrollbar, should_show_scrollbar},
    text::{display_width_u16, truncate_end},
    widgets::{panel_contrast_fg, render_panel_shell},
};
use crate::app::state::AppState;

pub(super) fn render_command_palette(app: &AppState, frame: &mut Frame) {
    let Some(popup) = app.command_palette_popup_rect() else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    let Some(input) = app.command_palette_input_rect() else {
        return;
    };
    let Some(body) = app.command_palette_body_rect() else {
        return;
    };
    let Some(footer) = app.command_palette_footer_rect() else {
        return;
    };

    render_input(app, frame, input);
    render_separator(frame, Rect::new(inner.x, input.y + 1, inner.width, 1), app);

    let entries = app.command_palette_entries();
    render_rows(app, frame, body, &entries);
    render_command_palette_scrollbar(app, entries.len(), frame, body);
    frame.render_widget(
        Paragraph::new(truncate_end(
            "enter run · esc close · ctrl+u clear",
            footer.width.saturating_sub(1) as usize,
        ))
        .style(
            Style::default()
                .fg(app.palette.overlay0)
                .bg(app.palette.panel_bg),
        ),
        footer,
    );
}

fn render_input(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let query_budget = area.width.saturating_sub(3) as usize;
    let query = truncate_end(&app.command_palette.query, query_budget);
    let base = Style::default().fg(p.text).bg(p.panel_bg);
    let prompt = Span::styled(
        "> ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    );
    let cursor = Span::styled("█", Style::default().fg(p.accent));
    frame.render_widget(
        Paragraph::new(Line::from(vec![prompt, Span::styled(query, base), cursor])).style(base),
        area,
    );
}

fn render_separator(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new("─".repeat(area.width as usize)).style(
            Style::default()
                .fg(app.palette.surface1)
                .bg(app.palette.panel_bg),
        ),
        area,
    );
}

fn render_rows(
    app: &AppState,
    frame: &mut Frame,
    body: Rect,
    entries: &[crate::app::PaletteEntry],
) {
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new("no matching command").style(
                Style::default()
                    .fg(app.palette.overlay0)
                    .bg(app.palette.panel_bg),
            ),
            body,
        );
        return;
    }

    let key_width = entries
        .iter()
        .map(|entry| display_width_u16(&entry.key))
        .max()
        .unwrap_or(0)
        .min(24);
    let start = app.command_palette.scroll.min(entries.len());
    let end = entries
        .len()
        .min(start.saturating_add(body.height as usize));
    for (visible_index, entry) in entries[start..end].iter().enumerate() {
        let index = start + visible_index;
        let row = Rect::new(body.x, body.y + visible_index as u16, body.width, 1);
        let selected = index == app.command_palette.selected;
        let base = if selected {
            Style::default()
                .bg(app.palette.accent)
                .fg(panel_contrast_fg(&app.palette))
        } else {
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.text)
        };
        frame.render_widget(Clear, row);
        frame.render_widget(Paragraph::new(" ").style(base), row);

        let key_area = Rect::new(
            row.x + row.width.saturating_sub(key_width),
            row.y,
            key_width,
            1,
        );
        let left_width = row.width.saturating_sub(key_width);
        let left_area = Rect::new(row.x, row.y, left_width, 1);
        let group = format!("  {}", entry.group);
        let group_style = if selected {
            base
        } else {
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.overlay0)
        };
        let label_budget = left_width
            .saturating_sub(1)
            .saturating_sub(display_width_u16(&group)) as usize;
        let label = truncate_end(&entry.label, label_budget);
        let label_style = if selected {
            base.add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.text)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(label, label_style),
                Span::styled(group, group_style),
            ]))
            .style(base),
            left_area,
        );

        if key_width > 0 {
            let key = truncate_end(&entry.key, key_width as usize);
            let key_style = if selected {
                base
            } else {
                Style::default()
                    .bg(app.palette.panel_bg)
                    .fg(app.palette.overlay0)
            };
            let padding = key_width.saturating_sub(display_width_u16(&key)) as usize;
            frame.render_widget(
                Paragraph::new(format!("{}{}", " ".repeat(padding), key)).style(key_style),
                key_area,
            );
        }
    }
}

fn render_command_palette_scrollbar(
    app: &AppState,
    entry_count: usize,
    frame: &mut Frame,
    body: Rect,
) {
    if body.width <= 1 || body.height == 0 {
        return;
    }
    let viewport = body.height as usize;
    if entry_count <= viewport {
        return;
    }
    let metrics = crate::pane::ScrollMetrics {
        viewport_rows: viewport,
        offset_from_bottom: entry_count
            .saturating_sub(viewport)
            .saturating_sub(app.command_palette.scroll),
        max_offset_from_bottom: entry_count.saturating_sub(viewport),
    };
    if !should_show_scrollbar(metrics) {
        return;
    }
    render_scrollbar(
        frame,
        metrics,
        Rect::new(body.x + body.width - 1, body.y, 1, body.height),
        app.palette.surface_dim,
        app.palette.overlay0,
        "▕",
    );
}
