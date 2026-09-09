use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{AppState, DockSurface};
use crate::terminal::TerminalRuntimeRegistry;

pub(crate) mod agents;
pub(crate) mod chooser;
pub(crate) mod diff;
pub(crate) mod editor;
pub(crate) mod files;
mod home;
pub(crate) mod hosts;
pub(crate) mod linear;
pub(crate) mod missive;
pub(crate) mod pr;
pub(crate) mod symphony;

pub(crate) use chooser::{
    card_hit_areas_for_count as chooser_card_hit_areas_for_count,
    menu_layout as chooser_menu_layout,
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
pub(crate) fn tab_width_label(label: &str, active: bool) -> u16 {
    let label = crate::ui::text::display_width_u16(label);
    label
        .saturating_add(1)
        .saturating_add(if active { 2 } else { 0 })
}

/// Columns the `+` occupies. The space in front of it belongs to the last tab.
pub(crate) const PLUS_WIDTH: u16 = 1;

/// Where the strip draws each of its parts. One rect per open surface, in the
/// order of `dock_open_surfaces`, so a hit test can map an index back to a
/// surface; a tab scrolled out of the strip gets a zero-width rect no click can
/// land in.
pub(crate) struct StripLayout {
    pub tabs: Vec<Rect>,
    pub close: Rect,
    pub plus: Rect,
}

/// Lay the tab strip out exactly as `render_tab_strip` draws it: every tab keeps
/// its label plus one separating space, the active tab keeps its close glyph,
/// and the `+` takes the cell after the last tab that fits. Squeezing the tabs
/// into equal shares instead would run the labels together and put the `+`
/// somewhere the strip never draws it.
pub(crate) fn strip_layout_labels(
    strip: Rect,
    labels: &[String],
    active_index: Option<usize>,
) -> StripLayout {
    let mut tabs = vec![Rect::default(); labels.len()];
    if strip.width == 0 || strip.height == 0 {
        return StripLayout {
            tabs,
            close: Rect::default(),
            plus: Rect::default(),
        };
    }

    let widths: Vec<u16> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| tab_width_label(label, active_index == Some(index)))
        .collect();
    let available = strip.width.saturating_sub(PLUS_WIDTH);
    let first = first_visible_tab(&widths, available, active_index);

    let mut x = strip.x;
    let tabs_right = strip.x.saturating_add(available);
    for index in first..widths.len() {
        let remaining = tabs_right.saturating_sub(x);
        if remaining == 0 {
            break;
        }
        let width = widths[index].min(remaining);
        tabs[index] = Rect::new(x, strip.y, width, 1);
        x = x.saturating_add(width);
        if width < widths[index] {
            // A clipped tab ends the strip: whatever follows has no room left.
            break;
        }
    }

    let close = active_index
        .and_then(|index| tabs.get(index).copied())
        .zip(active_index.and_then(|index| labels.get(index)))
        .map(|(tab, label)| close_rect_label(tab, label))
        .unwrap_or_default();
    let plus = Rect::new(
        x.min(strip.right().saturating_sub(PLUS_WIDTH)),
        strip.y,
        PLUS_WIDTH,
        1,
    );

    StripLayout { tabs, close, plus }
}

/// First tab the strip can draw while still showing the active one. Scrolling
/// starts at the left and only advances until the active tab fits.
fn first_visible_tab(widths: &[u16], available: u16, active: Option<usize>) -> usize {
    let Some(active) = active else {
        return 0;
    };
    let mut first = 0;
    while first < active {
        let used = widths[first..=active]
            .iter()
            .copied()
            .fold(0u16, u16::saturating_add);
        if used <= available {
            break;
        }
        first += 1;
    }
    first
}

/// Column of the close glyph inside an active tab's rect.
pub(crate) fn close_rect_label(tab: Rect, label: &str) -> Rect {
    let offset = crate::ui::text::display_width_u16(label).saturating_add(1);
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
        Some(DockSurface::Files) => files::render_files(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Pr) => pr::render_pr(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Linear) => linear::render_linear(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Missive) => missive::render_missive(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Agents) => agents::render_agents(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Hosts) => hosts::render_hosts(app, frame, app.view.dock_body_rect),
        Some(DockSurface::Symphony) => {
            symphony::render_symphony(app, frame, app.view.dock_body_rect)
        }
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

/// Render a sidebar-selected provider object in the pane area while the dock
/// stays collapsed. These are the same renderers used by object dock tabs.
pub(crate) fn render_object_preview(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(object) = app.dock_object_preview.as_ref() else {
        return;
    };
    match object.surface {
        DockSurface::Pr => pr::render_pr(app, frame, area),
        DockSurface::Linear => linear::render_linear(app, frame, area),
        DockSurface::Missive => missive::render_missive(app, frame, area),
        _ => {}
    }
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
    for (index, (_surface, area)) in app
        .dock_open_surfaces
        .iter()
        .copied()
        .zip(app.view.dock_tab_hit_areas.iter().copied())
        .enumerate()
    {
        if area.width == 0 {
            continue;
        }
        let active = app.active_dock_tab_index() == Some(index);
        let style = if active {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        // The last column of a tab is its separating space, so a clipped label
        // still never touches its neighbour.
        let full_label = app.dock_tab_label(index);
        let label: String = full_label
            .chars()
            .take(usize::from(area.width.saturating_sub(1)))
            .collect();
        frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), area);
        if active {
            let close = close_rect_label(area, &full_label);
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
        let super_p = if cfg!(target_os = "macos") {
            "cmd+p"
        } else {
            "super+p"
        };
        let palette_chord = format!("ctrl+alt+p / {super_p}");

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
                " global                      \u{2590}".to_string(),
                " ctrl+b                      \u{2595}".to_string(),
                " prefix mode                 \u{2595}".to_string(),
                " prefix+?                    \u{2595}".to_string(),
                " keybinds                    \u{2595}".to_string(),
                " prefix+s                    \u{2595}".to_string(),
                " settings                    \u{2595}".to_string(),
                format!(" {palette_chord:<28}\u{2595}"),
                " command palette             \u{2595}".to_string(),
                " prefix+q                    \u{2595}".to_string(),
                " detach                      \u{2595}".to_string(),
                " prefix+shift+r              \u{2595}".to_string(),
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

    #[test]
    fn f27_collapsed_dock_stays_collapsed_when_unassigned_ticket_opens_home() {
        let mut app = crate::ui::sidebar_work_item_fixture();
        app.sidebar_group_mode = crate::app::state::SidebarGroupMode::LinearTeam;
        app.dock_collapsed = true;
        app.dock_open_surfaces = vec![DockSurface::Linear];
        app.dock_tab_bindings = vec![Some(crate::app::state::DockTabBinding {
            object: crate::app::state::DockObjectRef {
                surface: DockSurface::Linear,
                key: "SCA-3165".into(),
            },
            origin: crate::app::state::DockTabOrigin::User,
        })];
        app.dock_tab = Some(DockSurface::Linear);
        app.dock_active_tab_index = Some(0);
        assert!(app.open_sidebar_unassigned_object("linear:OPS-12"));

        assert!(app.dock_collapsed);
        assert_eq!(app.dock_tab_label(0), "SCA-3165");
        assert!(app.dock_object_preview.is_none());
        assert_eq!(
            app.home
                .as_ref()
                .and_then(|home| home.ticket.as_ref())
                .map(|ticket| (ticket.identifier.as_str(), ticket.title.as_str())),
            Some(("OPS-12", "pixel EMQ drop"))
        );
    }
}
