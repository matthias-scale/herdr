use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppState, DockTab};
use crate::terminal::TerminalRuntimeRegistry;

mod editor;

pub(crate) use editor::{dock_editor_cursor, dock_editor_has_focus};

pub(super) fn render_dock(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
    let area = app.view.dock_rect;
    if area.width == 0 || area.height == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(" ")).style(Style::default().bg(app.palette.panel_bg)),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if app.dock_collapsed { "«" } else { "»" },
            Style::default().fg(app.palette.overlay1),
        )))
        .style(Style::default().bg(app.palette.panel_bg)),
        app.view.dock_handle_rect,
    );

    if app.dock_collapsed {
        return;
    }

    frame.render_widget(
        Paragraph::new("│").style(Style::default().fg(app.palette.surface_dim)),
        app.view.dock_divider_rect,
    );
    if app.view.dock_tab_bar_rect.width > 0 {
        for (tab, area) in DockTab::ALL
            .into_iter()
            .zip(app.view.dock_tab_hit_areas.iter().copied())
        {
            let style = if app.dock_tab == tab {
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.palette.overlay0)
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(tab.label(), style))),
                area,
            );
        }
    }
    match app.dock_tab {
        DockTab::Editor => editor::render_editor_body(app, terminal_runtimes, frame),
        DockTab::Shortcuts | DockTab::Context => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "not implemented yet",
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ))),
            app.view.dock_body_rect,
        ),
    }
}
