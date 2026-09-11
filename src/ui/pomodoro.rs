//! The break timer's two surfaces: a compact countdown in the sidebar footer and
//! the blocking overlay raised when a phase ends.
//!
//! The overlay is deliberately not dismissible by Enter alone. Its whole value
//! is that acknowledging a break costs a sentence, so an ignored reminder stays
//! on screen.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use super::text::display_width_u16;
use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, ActionButtonSpec,
};
use crate::app::AppState;
use crate::pomodoro::PomodoroPhase;

/// Width of `⏱ 25:00` plus a leading space.
const INDICATOR_WIDTH: u16 = 9;
/// Columns the footer icon strip already owns.
const FOOTER_ICON_COLUMNS: u16 = 13;
const PROMPT_WIDTH: u16 = 62;
const PROMPT_HEIGHT: u16 = 12;

fn prompt_inner_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, PROMPT_WIDTH, PROMPT_HEIGHT).map(|popup| {
        Rect::new(
            popup.x.saturating_add(1),
            popup.y.saturating_add(1),
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    })
}

pub(crate) fn prompt_button_rects(area: Rect) -> Option<(Rect, Rect)> {
    let inner = prompt_inner_rect(area)?;
    if inner.height < 9 {
        return None;
    }
    let buttons = [
        ActionButtonSpec {
            hint: Some("↵"),
            label: "confirm",
        },
        ActionButtonSpec {
            hint: Some("^⌥b"),
            label: "snooze",
        },
    ];
    let needed_width = buttons
        .iter()
        .map(|button| super::widgets::action_button_width(button.hint, button.label))
        .sum::<u16>()
        .saturating_add(2);
    let rects = if needed_width <= inner.width {
        action_button_row_rects(inner, &buttons, 2, inner.height.saturating_sub(1))
    } else {
        buttons
            .iter()
            .enumerate()
            .flat_map(|(index, button)| {
                action_button_row_rects(
                    inner,
                    std::slice::from_ref(button),
                    0,
                    inner.height.saturating_sub(2).saturating_add(index as u16),
                )
            })
            .collect()
    };
    Some((*rects.first()?, *rects.get(1)?))
}

/// The countdown's slot at the right end of the sidebar footer row. Empty when
/// the timer is off or the sidebar is too narrow to hold it beside the icons.
pub(crate) fn pomodoro_hit_area(app: &AppState, sidebar: Rect) -> Rect {
    if !app.pomodoro.enabled || app.sidebar_collapsed || sidebar.height == 0 {
        return Rect::default();
    }
    let content_width = sidebar.width.saturating_sub(1);
    if content_width < FOOTER_ICON_COLUMNS + INDICATOR_WIDTH {
        return Rect::default();
    }
    Rect::new(
        sidebar.x + content_width - INDICATOR_WIDTH,
        sidebar.bottom().saturating_sub(1),
        INDICATOR_WIDTH,
        1,
    )
}

fn phase_color(app: &AppState, phase: PomodoroPhase) -> ratatui::style::Color {
    if phase.is_break() {
        app.palette.green
    } else {
        app.palette.text
    }
}

/// Draws the countdown. `now` comes from the frame's observation instant so the
/// label matches the rest of the frame.
pub(crate) fn render_indicator(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    now: std::time::Instant,
) {
    if area.width == 0 || area.height == 0 || !app.pomodoro.enabled {
        return;
    }
    let held = app.pomodoro.held();
    let paused = app.pomodoro.paused();
    let glyph = if paused { "‖" } else { "⏱" };
    let style = if held {
        Style::default().fg(app.palette.yellow)
    } else if paused {
        Style::default().fg(app.palette.overlay0)
    } else {
        Style::default().fg(phase_color(app, app.pomodoro.phase))
    };
    let label = if held {
        format!("{glyph} held")
    } else {
        format!("{glyph} {}", app.pomodoro.label_at(now))
    };
    let line = Line::from(vec![Span::styled(label, style)]);
    frame.render_widget(Paragraph::new(line).right_aligned(), area);
}

/// The confirmation overlay. Returns without drawing when no phase is due.
pub(crate) fn render_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(prompt) = app.pomodoro.prompt.as_ref() else {
        return;
    };
    let palette = &app.palette;
    super::veil_background(frame, area, palette);
    let Some(inner) = render_modal_shell(frame, area, PROMPT_WIDTH, PROMPT_HEIGHT, palette) else {
        return;
    };
    if inner.height < 9 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<9>(inner);

    let title = if prompt.ended.is_break() {
        "break over"
    } else if inner.width < 16 {
        "break due"
    } else {
        "time for a break"
    };
    render_modal_header(frame, rows[0], title, palette);

    let minutes = app.pomodoro.phase_duration(prompt.next).as_secs() / 60;
    let body = if inner.width < 32 {
        format!(
            "{} → {} ({minutes}m)",
            prompt.ended.label(),
            prompt.next.label()
        )
    } else {
        format!(
            "{} finished. next up: {} for {minutes} minutes.",
            prompt.ended.label(),
            prompt.next.label()
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(body, Style::default().fg(palette.text)))
            .wrap(Wrap { trim: true }),
        rows[2],
    );

    let hint = if inner.width < 32 {
        format!("min {} chars", app.pomodoro.min_confirm_chars)
    } else {
        format!(
            "type at least {} characters to confirm:",
            app.pomodoro.min_confirm_chars
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(palette.overlay0))),
        rows[3],
    );

    render_input(app, frame, rows[4], &prompt.input);

    if let Some(error) = prompt.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                error.to_string(),
                Style::default().fg(palette.red),
            )),
            rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "^c clear".to_string(),
            Style::default().fg(palette.overlay0),
        )),
        rows[6],
    );

    let Some((confirm, snooze)) = prompt_button_rects(area) else {
        return;
    };
    render_action_button(
        frame,
        confirm,
        Some("↵"),
        "confirm",
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        snooze,
        Some("^⌥b"),
        "snooze",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_input(app: &AppState, frame: &mut Frame, area: Rect, input: &str) {
    if area.width == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    let text_rect = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Paragraph::new(format!(" {input}")).style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0),
        ),
        text_rect,
    );
    let caret = area
        .x
        .saturating_add(1)
        .saturating_add(display_width_u16(input))
        .min(area.right().saturating_sub(1));
    frame.set_cursor_position((caret, area.y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.pomodoro.enabled = true;
        app
    }

    #[test]
    fn a_disabled_timer_claims_no_footer_slot() {
        let mut app = state();
        app.pomodoro.enabled = false;
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 30, 20)),
            Rect::default()
        );
    }

    #[test]
    fn the_indicator_is_dropped_before_it_overlaps_the_footer_icons() {
        let app = state();
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 22, 20)),
            Rect::default()
        );
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 30, 20)),
            Rect::new(20, 19, 9, 1)
        );
    }

    #[test]
    fn f1_held_indicator_names_the_state_in_a_distinct_colour() {
        let now = std::time::Instant::now();
        let mut app = state();
        app.pomodoro.reset(now);
        app.pomodoro
            .tick_with_host_focus(now + std::time::Duration::from_secs(25 * 60), false);
        let sidebar = Rect::new(0, 0, 26, 1);
        let slot = pomodoro_hit_area(&app, sidebar);
        assert_ne!(slot, Rect::default());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(26, 1))
            .expect("test terminal");
        terminal
            .draw(|frame| render_indicator(&app, frame, slot, now))
            .expect("draw indicator");
        let buffer = terminal.backend().buffer();
        let text: String = (slot.x..slot.right())
            .map(|x| buffer[(x, slot.y)].symbol())
            .collect();
        let held_cell = (slot.x..slot.right())
            .map(|x| &buffer[(x, slot.y)])
            .find(|cell| cell.symbol() == "h")
            .expect("held label");

        assert!(text.contains("⏱ held"), "{text:?}");
        assert_eq!(held_cell.style().fg, Some(app.palette.yellow));
        assert_ne!(app.palette.yellow, phase_color(&app, app.pomodoro.phase));
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn prompted() -> AppState {
        use crate::pomodoro::{PomodoroPhase, PomodoroPrompt};
        let mut app = AppState::test_new();
        app.pomodoro.enabled = true;
        app.pomodoro.prompt = Some(PomodoroPrompt {
            ended: PomodoroPhase::Work,
            next: PomodoroPhase::ShortBreak,
            input: String::new(),
            error: None,
        });
        app
    }

    /// Draws work first, like the panes are, then the prompt over it.
    fn veiled_frame(app: &AppState, area: Rect) -> ratatui::buffer::Buffer {
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("cargo test --all"),
                    Rect::new(0, 0, area.width, 1),
                );
                render_overlay(app, frame, area);
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    /// A break reminder is only blocking if the work behind it stops competing
    /// for attention. The DIM modifier alone is a no-op on several terminals.
    #[test]
    fn the_confirm_overlay_veils_the_work_behind_it() {
        let app = prompted();
        let area = Rect::new(0, 0, 80, 24);
        let buffer = veiled_frame(&app, area);

        let veiled = &buffer[(0, 0)];
        assert_eq!(veiled.symbol(), " ", "work behind the prompt is blanked");
        assert_eq!(veiled.style().bg, Some(app.palette.surface0));

        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(
            !text.contains("cargo test"),
            "veil hides the work: {text:?}"
        );
        assert!(
            text.contains("time for a break"),
            "modal draws above the veil"
        );
    }

    /// `surface0` follows the terminal's own background in the 16-colour theme,
    /// so using it there would leave no veil at all.
    #[test]
    fn the_veil_falls_back_to_a_concrete_colour_on_the_terminal_theme() {
        let mut app = prompted();
        app.palette = crate::app::state::Palette::terminal();
        assert_eq!(app.palette.surface0, ratatui::style::Color::Reset);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 80, 24));
        assert_eq!(buffer[(0, 0)].style().bg, Some(app.palette.surface1));
    }

    /// The reminder is only a reminder if it reaches the frame on top of
    /// whatever else was being drawn.
    #[test]
    fn the_due_reminder_draws_over_the_rest_of_the_frame() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        app.pomodoro
            .tick(now + std::time::Duration::from_secs(25 * 60));
        if let Some(prompt) = app.pomodoro.prompt.as_mut() {
            prompt.input = "stretching".into();
        }

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("time for a break"), "overlay title is drawn");
        assert!(text.contains("stretching"), "typed answer is drawn");
    }

    #[test]
    fn the_due_reminder_is_visible_in_every_sidebar_state() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 30;
        let states = [
            (
                false,
                crate::config::SidebarCollapsedModeConfig::Compact,
                "expanded",
            ),
            (
                true,
                crate::config::SidebarCollapsedModeConfig::Compact,
                "compact",
            ),
            (
                true,
                crate::config::SidebarCollapsedModeConfig::Hidden,
                "hidden",
            ),
        ];

        for width in [18, WIDTH] {
            for (collapsed, mode, name) in states {
                let mut app = prompted();
                app.sidebar_collapsed = collapsed;
                app.sidebar_collapsed_mode = mode;
                crate::ui::compute_view(&mut app, Rect::new(0, 0, width, HEIGHT));

                let mut terminal =
                    Terminal::new(TestBackend::new(width, HEIGHT)).expect("test terminal");
                terminal
                    .draw(|frame| crate::ui::render(&app, frame))
                    .expect("render prompt");
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                let expected_title = if width < 22 {
                    "break due"
                } else {
                    "time for a break"
                };
                assert!(
                    text.contains(expected_title),
                    "{width} columns, {name}: {text:?}"
                );
                assert!(
                    text.contains("↵ confirm"),
                    "{width} columns, {name}: {text:?}"
                );
                assert!(
                    text.contains("^⌥b snooze"),
                    "{width} columns, {name}: {text:?}"
                );
            }
        }
    }

    #[test]
    fn the_countdown_reaches_the_sidebar_footer_row() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let slot = app.view.pomodoro_hit_area;
        assert!(slot.width > 0, "wide sidebar keeps room for the countdown");
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let row: String = (slot.x..slot.right())
            .map(|x| buffer[(x, slot.y)].symbol())
            .collect();
        assert!(row.contains("25:0") || row.contains("24:5"), "{row:?}");
    }
}
