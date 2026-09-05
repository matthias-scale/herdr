use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppState, DockSurface};
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) mod chooser;
pub(crate) mod diff;
mod editor;
mod home;

pub(crate) use chooser::{
    card_hit_areas as chooser_card_hit_areas, menu_layout as chooser_menu_layout,
};
pub(crate) use home::detail_tab_layouts as home_detail_tab_layouts;
pub(crate) use home::poll_tab_layouts as home_poll_tab_layouts;
pub(crate) use home::section_layouts as home_section_layouts;
pub(crate) use home::tab_layouts as home_tab_layouts;
pub(crate) use home::ticket_tab_layouts as home_ticket_tab_layouts;

/// Glyph that closes the active tab, and the one that maximises the dock.
pub(crate) const CLOSE_GLYPH: &str = "×";
pub(crate) const MAXIMIZE_GLYPH: &str = "⤢";
pub(crate) const PLUS_GLYPH: &str = "+";

/// Columns a tab occupies: its label, a separating space, and — while it is the
/// active tab — the close glyph with its own space.
pub(crate) fn tab_width(surface: DockSurface, active: bool) -> u16 {
    let label = u16::try_from(surface.label().chars().count()).unwrap_or(u16::MAX);
    label
        .saturating_add(1)
        .saturating_add(if active { 2 } else { 0 })
}

/// Column of the close glyph inside an active tab's rect.
pub(crate) fn close_rect(tab: Rect, surface: DockSurface) -> Rect {
    let offset = u16::try_from(surface.label().chars().count())
        .unwrap_or(0)
        .saturating_add(1);
    if offset.saturating_add(1) > tab.width {
        return Rect::default();
    }
    Rect::new(tab.x.saturating_add(offset), tab.y, 1, 1)
}

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
        render_tab_strip(app, frame);
    }
    match app.dock_tab {
        None => chooser::render_chooser(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Home) => home::render_home(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Editor) => editor::render_editor_body(app, terminal_runtimes, frame),
        Some(DockSurface::Diff) => diff::render_diff(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Shortcuts) => {
            super::dock_shortcuts::render_shortcuts(app, frame, app.view.dock_body_rect)
        }
        Some(DockSurface::Context) => {
            super::dock_context::render_context(app, frame, app.view.dock_body_rect)
        }
        Some(DockSurface::Scratchpad) => {
            super::dock_scratchpad::render_scratchpad(app, frame, app.view.dock_body_rect)
        }
        Some(surface) => render_placeholder(app, frame, app.view.dock_body_rect, surface),
    }
    chooser::render_menu(app, frame);
}

/// Surfaces whose body arrives in a later slice announce themselves rather than
/// rendering an empty rectangle the user cannot tell from a bug.
fn render_placeholder(app: &AppState, frame: &mut Frame, area: Rect, surface: DockSurface) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(text) = surface.placeholder() else {
        return;
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {text}"),
            Style::default().fg(app.palette.overlay1),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_tab_strip(app: &AppState, frame: &mut Frame) {
    for (surface, area) in app
        .dock_open_surfaces
        .iter()
        .copied()
        .zip(app.view.dock_tab_hit_areas.iter().copied())
    {
        let active = app.dock_tab == Some(surface);
        let style = if active {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(surface.label(), style))),
            area,
        );
        if active {
            let close = close_rect(area, surface);
            if close.width > 0 {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        CLOSE_GLYPH,
                        Style::default().fg(app.palette.overlay1),
                    ))),
                    close,
                );
            }
        }
    }

    if app.view.dock_plus_rect.width > 0 {
        let style = if app.dock_surface_menu.is_some() {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay1)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(PLUS_GLYPH, style))),
            app.view.dock_plus_rect,
        );
    }
    if app.view.dock_maximize_rect.width > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                MAXIMIZE_GLYPH,
                Style::default().fg(if app.dock_maximized {
                    app.palette.accent
                } else {
                    app.palette.overlay1
                }),
            ))),
            app.view.dock_maximize_rect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::{backend::TestBackend, Terminal};

    /// Body renderings frozen before the `DockSurface` → `DockSurface` rename so the
    /// rename stays behaviour-neutral for the surfaces that already existed.
    fn body_text(app: &AppState, tab: DockSurface) -> String {
        let area = app.view.dock_body_rect;
        let runtimes = TerminalRuntimeRegistry::new();
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| match tab {
                DockSurface::Editor => editor::render_editor_body(app, &runtimes, frame),
                DockSurface::Shortcuts => {
                    super::super::dock_shortcuts::render_shortcuts(app, frame, area)
                }
                DockSurface::Context => {
                    super::super::dock_context::render_context(app, frame, area)
                }
                DockSurface::Scratchpad => {
                    super::super::dock_scratchpad::render_scratchpad(app, frame, area)
                }
                DockSurface::Home => home::render_home(app, frame, area),
                other => render_placeholder(app, frame, area, other),
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
            body_text(&app, DockSurface::Editor),
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
            body_text(&app, DockSurface::Shortcuts),
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
            body_text(&app, DockSurface::Context)
                .lines()
                .next()
                .unwrap_or_default(),
            " no focused pane              "
        );
        assert_eq!(
            body_text(&app, DockSurface::Scratchpad)
                .lines()
                .next()
                .unwrap_or_default(),
            " no repository for this pane  "
        );
    }
}
