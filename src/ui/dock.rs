use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppState, DockTab};
use crate::terminal::TerminalRuntimeRegistry;

mod editor;
mod home;

pub(crate) use home::detail_tab_layouts as home_detail_tab_layouts;
pub(crate) use home::poll_tab_layouts as home_poll_tab_layouts;
pub(crate) use home::section_layouts as home_section_layouts;
pub(crate) use home::tab_layouts as home_tab_layouts;
pub(crate) use home::ticket_tab_layouts as home_ticket_tab_layouts;

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
        DockTab::Home => home::render_home(app, frame, app.view.dock_body_rect),
        DockTab::Editor => editor::render_editor_body(app, terminal_runtimes, frame),
        DockTab::Shortcuts => {
            super::dock_shortcuts::render_shortcuts(app, frame, app.view.dock_body_rect)
        }
        DockTab::Context => {
            super::dock_context::render_context(app, frame, app.view.dock_body_rect)
        }
        DockTab::Scratchpad => {
            super::dock_scratchpad::render_scratchpad(app, frame, app.view.dock_body_rect)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::{backend::TestBackend, Terminal};

    /// Body renderings frozen before the `DockTab` → `DockSurface` rename so the
    /// rename stays behaviour-neutral for the surfaces that already existed.
    fn body_text(app: &AppState, tab: DockTab) -> String {
        let area = app.view.dock_body_rect;
        let runtimes = TerminalRuntimeRegistry::new();
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| match tab {
                DockTab::Editor => editor::render_editor_body(app, &runtimes, frame),
                DockTab::Shortcuts => {
                    super::super::dock_shortcuts::render_shortcuts(app, frame, area)
                }
                DockTab::Context => super::super::dock_context::render_context(app, frame, area),
                DockTab::Scratchpad => {
                    super::super::dock_scratchpad::render_scratchpad(app, frame, area)
                }
                DockTab::Home => home::render_home(app, frame, area),
            })
            .expect("render dock body");
        let buffer = terminal.backend().buffer().clone();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn characterization_app() -> AppState {
        let mut app = AppState::test_new();
        app.dock_collapsed = false;
        app.view.dock_body_rect = Rect::new(0, 0, 30, 12);
        app
    }

    #[test]
    fn existing_dock_surfaces_render_unchanged() {
        let app = characterization_app();

        assert_eq!(
            body_text(&app, DockTab::Editor),
            [
                "focus an agent first          ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
                "                              ",
            ]
            .join("\n")
        );
        assert_eq!(
            body_text(&app, DockTab::Shortcuts),
            [
                " global                      \u{2590}",
                " ctrl+b                      \u{2595}",
                " prefix mode                 \u{2595}",
                " prefix+?                    \u{2595}",
                " keybinds                    \u{2595}",
                " prefix+s                    \u{2595}",
                " settings                    \u{2595}",
                " prefix+q                    \u{2595}",
                " detach                      \u{2595}",
                " prefix+shift+r              \u{2595}",
                " reload config               \u{2595}",
                " prefix+o                    \u{2595}",
            ]
            .join("\n")
        );
        assert_eq!(
            body_text(&app, DockTab::Context)
                .lines()
                .next()
                .unwrap_or_default(),
            " no focused pane              "
        );
        assert_eq!(
            body_text(&app, DockTab::Scratchpad)
                .lines()
                .next()
                .unwrap_or_default(),
            " no repository for this pane  "
        );
    }
}
