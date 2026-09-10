//! The sidebar's idle star field and its pause button, pinned to the bottom of
//! the sidebar under the notepad and above the footer icons.
//!
//! Geometry is pure so the rows can be reserved before anything draws;
//! [`super::sidebar::workspace_list_rect_for_app`] subtracts them the same way
//! it subtracts the notepad's. The field itself comes from [`starfield`], which
//! knows nothing about terminals, palettes, or state.

mod starfield;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::text::display_width_u16;
use crate::app::state::Palette;
use crate::app::AppState;
use starfield::Level;

/// Rows the panel takes when it fits: a separator and four rows of stars.
const PANEL_ROWS: u16 = 5;
/// Rows the workspace list keeps for itself before the animation may claim any.
const MIN_LIST_ROWS_BESIDE_ANIMATION: u16 = 6;
/// Separator plus two rows of field. Below this the animation reads as noise.
const MIN_ANIMATION_ROWS: u16 = 3;

/// Shown while the field is moving: clicking it stops the animation.
const PAUSE_LABEL: &str = "‖ warp";
/// Shown while it is frozen: clicking it starts the animation again.
const RESUME_LABEL: &str = "▸ warp";

fn button_label(app: &AppState) -> &'static str {
    if app.hyperspace.paused() {
        RESUME_LABEL
    } else {
        PAUSE_LABEL
    }
}

/// How many rows of `content` the animation occupies. Zero whenever it is off,
/// the sidebar is collapsed, or the list would be squeezed below
/// [`MIN_LIST_ROWS_BESIDE_ANIMATION`].
pub(crate) fn animation_height(app: &AppState, content: Rect) -> u16 {
    if !app.hyperspace.enabled || app.sidebar_collapsed || content.width == 0 {
        return 0;
    }
    let spare = content
        .height
        .saturating_sub(MIN_LIST_ROWS_BESIDE_ANIMATION);
    if spare < MIN_ANIMATION_ROWS {
        return 0;
    }
    PANEL_ROWS.min(spare)
}

/// The panel itself, at the bottom of whatever content area it is given.
pub(crate) fn animation_panel_rect(app: &AppState, content: Rect) -> Rect {
    let height = animation_height(app, content);
    if height == 0 {
        return Rect::default();
    }
    Rect::new(
        content.x,
        content.bottom().saturating_sub(height),
        content.width,
        height,
    )
}

/// The star field's rows inside the panel, separator excluded.
fn field_rect(panel: Rect) -> Rect {
    if panel.height <= 1 {
        return Rect::default();
    }
    Rect::new(
        panel.x,
        panel.y.saturating_add(1),
        panel.width,
        panel.height.saturating_sub(1),
    )
}

/// The pause button, in the panel's bottom-left corner. Empty when the panel is
/// too narrow to hold the label, so a click can never resolve to a control the
/// operator cannot see.
pub(crate) fn pause_hit_area(app: &AppState, panel: Rect) -> Rect {
    if panel.width == 0 || panel.height == 0 {
        return Rect::default();
    }
    let width = display_width_u16(button_label(app));
    if width == 0 || width > panel.width {
        return Rect::default();
    }
    Rect::new(panel.x, panel.bottom().saturating_sub(1), width, 1)
}

fn level_color(level: Level, palette: &Palette) -> ratatui::style::Color {
    // Every one of these is checked by the chrome contrast floor in
    // `ui::tests::themed_chrome_keeps_a_contrast_floor`; `surface_dim` reads as
    // black-on-black there, so the faint end starts at `overlay0`.
    match level {
        Level::Faint => palette.overlay0,
        Level::Dim => palette.subtext0,
        Level::Bright => palette.accent,
    }
}

pub(crate) fn render_animation(app: &AppState, frame: &mut Frame, panel: Rect) {
    if panel.width == 0 || panel.height == 0 || !app.hyperspace.enabled {
        return;
    }
    let palette = &app.palette;
    {
        let separator = Style::default().fg(palette.overlay0);
        let buf = frame.buffer_mut();
        for x in panel.x..panel.right() {
            buf[(x, panel.y)].set_symbol("─");
            buf[(x, panel.y)].set_style(separator);
        }
    }

    let field = field_rect(panel);
    if field.height == 0 {
        return;
    }
    let stars = starfield::field(field.width, field.height, app.hyperspace.step());
    for (index, row) in stars.rows().enumerate() {
        // One span per run of equal brightness, so a row of scattered stars
        // costs a handful of spans rather than one per cell.
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_level = None;
        for cell in row {
            if Some(cell.level) != run_level {
                if let Some(level) = run_level.take() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        Style::default().fg(level_color(level, palette)),
                    ));
                }
                run_level = Some(cell.level);
            }
            run.push(cell.ch);
        }
        if let Some(level) = run_level {
            spans.push(Span::styled(
                run,
                Style::default().fg(level_color(level, palette)),
            ));
        }
        let y = field.y.saturating_add(index as u16);
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(field.x, y, field.width, 1),
        );
    }

    // The button goes on last: it sits over the field, so it has to win.
    let button = pause_hit_area(app, panel);
    if button.width == 0 {
        return;
    }
    let hovered = matches!(
        app.hovered_control,
        Some(crate::app::state::ControlId::SidebarAnimationPause)
    );
    let style = if hovered {
        Style::default().fg(palette.accent)
    } else if app.hyperspace.paused() {
        Style::default().fg(palette.subtext0)
    } else {
        Style::default().fg(palette.overlay1)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(button_label(app), style))
            .style(Style::default().bg(palette.sidebar_background())),
        button,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.hyperspace.enabled = true;
        app
    }

    #[test]
    fn a_disabled_animation_reserves_no_rows() {
        let mut app = state();
        app.hyperspace.enabled = false;
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), 0);
        assert_eq!(
            animation_panel_rect(&app, Rect::new(0, 0, 26, 40)),
            Rect::default()
        );
    }

    #[test]
    fn a_collapsed_sidebar_reserves_no_rows() {
        let mut app = state();
        app.sidebar_collapsed = true;
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), 0);
    }

    #[test]
    fn the_workspace_list_keeps_its_floor_before_the_animation_gets_anything() {
        let app = state();
        // Six rows is exactly the list's floor, so there is nothing to give.
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 6)), 0);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 8)), 0);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 9)), 3);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), PANEL_ROWS);
    }

    #[test]
    fn the_panel_sits_at_the_bottom_of_the_content_area() {
        let app = state();
        let content = Rect::new(3, 2, 26, 40);
        let panel = animation_panel_rect(&app, content);
        assert_eq!(panel.bottom(), content.bottom());
        assert_eq!(panel.x, content.x);
        assert_eq!(panel.width, content.width);
    }

    #[test]
    fn the_button_is_in_the_panels_bottom_left_corner() {
        let app = state();
        let panel = animation_panel_rect(&app, Rect::new(3, 2, 26, 40));
        let button = pause_hit_area(&app, panel);
        assert_eq!(button.x, panel.x);
        assert_eq!(button.y, panel.bottom() - 1);
        assert_eq!(button.height, 1);
        assert_eq!(button.width, display_width_u16(PAUSE_LABEL));
    }

    #[test]
    fn the_button_label_follows_the_pause_state() {
        let mut app = state();
        assert_eq!(button_label(&app), PAUSE_LABEL);
        app.hyperspace.toggle_paused(std::time::Instant::now());
        assert_eq!(button_label(&app), RESUME_LABEL);
    }

    #[test]
    fn a_sidebar_too_narrow_for_the_label_offers_no_button() {
        let app = state();
        let panel = Rect::new(0, 0, 3, 5);
        assert_eq!(pause_hit_area(&app, panel), Rect::default());
    }

    #[test]
    fn an_empty_panel_offers_no_button() {
        let app = state();
        assert_eq!(pause_hit_area(&app, Rect::default()), Rect::default());
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn enabled() -> AppState {
        let mut app = AppState::test_new();
        app.hyperspace.enabled = true;
        app
    }

    fn panel_text(app: &AppState, panel: Rect) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("test terminal");
        terminal
            .draw(|frame| render_animation(app, frame, panel))
            .expect("render");
        let buffer = terminal.backend().buffer().clone();
        (panel.y..panel.bottom())
            .map(|y| {
                (panel.x..panel.right())
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_field_and_the_button_both_reach_the_frame() {
        let app = enabled();
        let panel = Rect::new(0, 0, 26, 5);
        let text = panel_text(&app, panel);
        assert!(text.starts_with("──"), "separator row: {text:?}");
        assert!(text.contains("warp"), "button label: {text:?}");
        assert!(
            text.chars().filter(|c| *c == '·' || *c == '*').count() > 4,
            "star field: {text:?}"
        );
    }

    #[test]
    fn a_paused_field_is_the_same_frame_every_time() {
        let mut app = enabled();
        app.hyperspace.toggle_paused(std::time::Instant::now());
        let panel = Rect::new(0, 0, 26, 5);
        let first = panel_text(&app, panel);
        app.hyperspace
            .tick(std::time::Instant::now() + std::time::Duration::from_secs(5));
        assert_eq!(first, panel_text(&app, panel));
        assert!(first.contains("▸ warp"), "resume glyph: {first:?}");
    }

    #[test]
    fn the_button_survives_the_star_that_lands_under_it() {
        // The label is drawn after the field, so no frame may eat it.
        let mut app = enabled();
        let panel = Rect::new(0, 0, 26, 5);
        for _ in 0..starfield::FRAMES {
            app.hyperspace
                .tick(std::time::Instant::now() + crate::hyperspace::FRAME_INTERVAL * 2);
            assert!(panel_text(&app, panel).contains("‖ warp"));
        }
    }

    /// The panel is only shipped if it reaches the real sidebar, under the
    /// notepad and above the footer icons.
    #[test]
    fn the_panel_reaches_the_bottom_of_the_real_sidebar() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = enabled();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let panel = app.view.hyperspace_rect;
        assert!(panel.height > 0, "a full-height sidebar holds the panel");
        assert_eq!(
            panel.bottom(),
            app.view.sidebar_rect.bottom() - 1,
            "the footer icon row keeps the last row"
        );

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let button = app.view.hyperspace_pause_hit_area;
        let row: String = (button.x..button.right())
            .map(|x| buffer[(x, button.y)].symbol())
            .collect();
        assert!(row.contains("warp"), "{row:?}");
    }

    /// Switching it off has to give the rows back, not just stop drawing.
    #[test]
    fn turning_it_off_returns_the_rows_to_the_workspace_list() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = enabled();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let with = crate::ui::workspace_list_rect_for_app(&app, app.view.sidebar_rect);

        app.hyperspace.enabled = false;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let without = crate::ui::workspace_list_rect_for_app(&app, app.view.sidebar_rect);

        assert_eq!(app.view.hyperspace_rect, Rect::default());
        assert_eq!(app.view.hyperspace_pause_hit_area, Rect::default());
        assert_eq!(without.height, with.height + PANEL_ROWS);
    }
}
