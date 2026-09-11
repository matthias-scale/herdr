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
    render_modal_header, render_modal_shell, render_panel_shell, ActionButtonSpec,
};
use crate::app::AppState;
use crate::pomodoro::{PomodoroPhase, PomodoroPrompt, ANIMATION_FRAME_INTERVAL};

/// Width of `⏱ 25:00` plus a leading space.
const INDICATOR_WIDTH: u16 = 9;
/// Columns the footer icon strip already owns.
const FOOTER_ICON_COLUMNS: u16 = 13;
const PROMPT_WIDTH: u16 = 62;
const PROMPT_HEIGHT: u16 = 12;
const ORB_PROMPT_WIDTH: u16 = 74;
const ORB_PROMPT_HEIGHT: u16 = 17;
const ORB_COLUMN_WIDTH: u16 = 29;
const ORB_CONTENT_WIDTH: u16 = 40;
const SEND_OFF_WIDTH: u16 = 52;
const SEND_OFF_HEIGHT: u16 = 7;
const BREAK_TIPS: [&str; 3] = [
    "stand up · look at something far away",
    "unclench your jaw · drop your shoulders",
    "get some water · leave the screen",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreathStage {
    Inhale,
    Hold,
    Exhale,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BreathFrame {
    progress: f64,
    stage: BreathStage,
    cue: &'static str,
    remaining: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrbCell {
    Fill {
        symbol: &'static str,
        color_band: u8,
    },
    Halo,
}

fn breath_at(elapsed: std::time::Duration) -> BreathFrame {
    let cycle = elapsed.as_secs_f64() % 10.0;
    let ease = |value: f64| 0.5 - 0.5 * (std::f64::consts::PI * value).cos();
    if cycle < 4.0 {
        BreathFrame {
            progress: ease(cycle / 4.0),
            stage: BreathStage::Inhale,
            cue: "breathe in",
            remaining: (4.0 - cycle).ceil().max(1.0) as u8,
        }
    } else if cycle < 5.0 {
        BreathFrame {
            progress: 1.0,
            stage: BreathStage::Hold,
            cue: "hold",
            remaining: (5.0 - cycle).ceil().max(1.0) as u8,
        }
    } else {
        BreathFrame {
            progress: 1.0 - ease((cycle - 5.0) / 5.0),
            stage: BreathStage::Exhale,
            cue: "breathe out",
            remaining: (10.0 - cycle).ceil().max(1.0) as u8,
        }
    }
}

fn animation_elapsed_at(
    now: std::time::Instant,
    started_at: std::time::Instant,
) -> std::time::Duration {
    let elapsed = now.saturating_duration_since(started_at);
    let frames = elapsed.as_nanos() / ANIMATION_FRAME_INTERVAL.as_nanos();
    let nanos = frames.saturating_mul(ANIMATION_FRAME_INTERVAL.as_nanos());
    std::time::Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn orb_cell(dx: i32, dy: i32, radius: f64) -> Option<OrbCell> {
    let distance = ((f64::from(dx) * 0.5).powi(2) + f64::from(dy).powi(2)).sqrt();
    if distance < radius {
        let relative = distance / radius;
        let symbol = if relative < 0.28 {
            "█"
        } else if relative < 0.52 {
            "▓"
        } else if relative < 0.76 {
            "▒"
        } else {
            "░"
        };
        let color_band = if relative < 0.4 {
            0
        } else if relative < 0.72 {
            1
        } else {
            2
        };
        Some(OrbCell::Fill { symbol, color_band })
    } else if distance < radius + 1.25 {
        Some(OrbCell::Halo)
    } else {
        None
    }
}

fn orb_layout_available(area: Rect) -> bool {
    area.width >= ORB_PROMPT_WIDTH.saturating_add(4)
        && area.height >= ORB_PROMPT_HEIGHT.saturating_add(2)
}

fn prompt_size(area: Rect) -> (u16, u16) {
    if orb_layout_available(area) {
        (ORB_PROMPT_WIDTH, ORB_PROMPT_HEIGHT)
    } else {
        (PROMPT_WIDTH, PROMPT_HEIGHT)
    }
}

fn prompt_inner_rect(area: Rect) -> Option<Rect> {
    let (width, height) = prompt_size(area);
    centered_popup_rect(area, width, height).map(|popup| {
        Rect::new(
            popup.x.saturating_add(1),
            popup.y.saturating_add(1),
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    })
}

fn prompt_content_rect(inner: Rect, orb_visible: bool) -> Rect {
    if orb_visible {
        Rect::new(
            inner.x.saturating_add(ORB_COLUMN_WIDTH + 1),
            inner.y,
            ORB_CONTENT_WIDTH.min(inner.width.saturating_sub(ORB_COLUMN_WIDTH + 1)),
            inner.height,
        )
    } else {
        inner
    }
}

pub(crate) fn animation_visible(app: &AppState, area: Rect) -> bool {
    if app.pomodoro.prompt.is_some() {
        return prompt_inner_rect(area).is_some_and(|inner| inner.height >= 9);
    }
    app.pomodoro
        .visible_send_off_at(app.view_observed_at)
        .and_then(|_| centered_popup_rect(area, SEND_OFF_WIDTH, SEND_OFF_HEIGHT))
        .is_some_and(|popup| popup.height >= SEND_OFF_HEIGHT)
}

pub(crate) fn prompt_button_rects(area: Rect) -> Option<(Rect, Rect)> {
    let inner = prompt_inner_rect(area)?;
    if inner.height < 9 {
        return None;
    }
    let orb_visible = orb_layout_available(area);
    let content = prompt_content_rect(inner, orb_visible);
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
    let rects = if needed_width <= content.width {
        action_button_row_rects(content, &buttons, 2, content.height.saturating_sub(1))
    } else {
        let first_row = content.height.saturating_sub(2);
        buttons
            .iter()
            .enumerate()
            .flat_map(|(index, button)| {
                action_button_row_rects(
                    content,
                    std::slice::from_ref(button),
                    0,
                    first_row.saturating_add(index as u16),
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

/// The due prompt or its short post-confirm acknowledgment.
pub(crate) fn render_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    if let Some(prompt) = app.pomodoro.prompt.as_ref() {
        render_prompt(app, frame, area, prompt);
        return;
    }
    render_send_off(app, frame, area);
}

fn render_prompt(app: &AppState, frame: &mut Frame, area: Rect, prompt: &PomodoroPrompt) {
    let palette = &app.palette;
    super::veil_background(frame, area, palette);
    let (width, height) = prompt_size(area);
    let Some(inner) = render_modal_shell(frame, area, width, height, palette) else {
        return;
    };
    if inner.height < 9 {
        return;
    }
    if orb_layout_available(area) {
        render_orb_prompt(app, frame, area, inner, prompt);
    } else {
        render_compact_prompt(app, frame, area, inner, prompt);
    }
}

fn render_compact_prompt(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    prompt: &PomodoroPrompt,
) {
    let palette = &app.palette;
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

    let title = prompt_title(prompt, inner.width);
    render_modal_header(frame, rows[0], title, palette);

    let body = prompt_body(app, prompt, inner.width);
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

    render_prompt_buttons(app, frame, area);
}

fn render_orb_prompt(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    prompt: &PomodoroPrompt,
) {
    let palette = &app.palette;
    let content = prompt_content_rect(inner, true);
    render_modal_header(
        frame,
        Rect::new(content.x, content.y + 1, content.width, 1),
        prompt_title(prompt, content.width),
        palette,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "take a deep breath.",
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y + 3, content.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            prompt_body(app, prompt, content.width),
            Style::default().fg(palette.text),
        ))
        .wrap(Wrap { trim: true }),
        Rect::new(content.x, content.y + 5, content.width, 2),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "type at least {} characters to confirm:",
                app.pomodoro.min_confirm_chars
            ),
            Style::default().fg(palette.overlay0),
        )),
        Rect::new(content.x, content.y + 8, content.width, 1),
    );
    render_input(
        app,
        frame,
        Rect::new(content.x, content.y + 9, content.width, 1),
        &prompt.input,
    );
    if let Some(error) = prompt.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Span::styled(error, Style::default().fg(palette.red))),
            Rect::new(content.x, content.y + 10, content.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "^c clear",
            Style::default().fg(palette.overlay0),
        )),
        Rect::new(content.x, content.y + 11, content.width, 1),
    );

    let elapsed = animation_elapsed_at(app.view_observed_at, prompt.raised_at);
    render_orb(app, frame, inner, breath_at(elapsed));
    let tip = prompt_tip(prompt.ended, elapsed);
    frame.render_widget(
        Paragraph::new(Span::styled(tip, Style::default().fg(palette.subtext0))),
        Rect::new(content.x, content.y + 13, content.width, 1),
    );
    render_prompt_buttons(app, frame, area);
}

fn render_orb(app: &AppState, frame: &mut Frame, inner: Rect, breath: BreathFrame) {
    let palette = &app.palette;
    let radius = 2.0 + 3.5 * breath.progress;
    let center_x = inner.x + 14;
    let center_y = inner.y + 6;
    for row in 1..=11 {
        for column in 1..ORB_COLUMN_WIDTH.saturating_sub(1) {
            let x = inner.x + column;
            let y = inner.y + row;
            let Some(orb) = orb_cell(
                i32::from(x) - i32::from(center_x),
                i32::from(y) - i32::from(center_y),
                radius,
            ) else {
                continue;
            };
            let cell = &mut frame.buffer_mut()[(x, y)];
            match orb {
                OrbCell::Fill { symbol, color_band } => {
                    let color = match color_band {
                        0 => palette.teal,
                        1 => palette.blue,
                        _ => palette.mauve,
                    };
                    cell.set_symbol(symbol)
                        .set_fg(color)
                        .set_bg(palette.panel_bg);
                    if symbol == "█" {
                        cell.set_style(cell.style().add_modifier(Modifier::BOLD));
                    }
                }
                OrbCell::Halo if (x + y).is_multiple_of(3) => {
                    cell.set_symbol("·")
                        .set_fg(palette.surface1)
                        .set_bg(palette.panel_bg);
                }
                OrbCell::Halo => {}
            }
        }
    }
    let cue_color = if breath.stage == BreathStage::Hold {
        palette.mauve
    } else {
        palette.teal
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("{} · {}", breath.cue, breath.remaining),
            Style::default().fg(cue_color).add_modifier(Modifier::BOLD),
        ))
        .centered(),
        Rect::new(inner.x, inner.y + 12, ORB_COLUMN_WIDTH, 1),
    );
}

fn prompt_title(prompt: &PomodoroPrompt, width: u16) -> &'static str {
    if prompt.ended.is_break() {
        "break over"
    } else if width < 16 {
        "break due"
    } else {
        "time for a break"
    }
}

fn prompt_body(app: &AppState, prompt: &PomodoroPrompt, width: u16) -> String {
    let minutes = app.pomodoro.phase_duration(prompt.next).as_secs() / 60;
    if width < 32 {
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
    }
}

fn prompt_tip(ended: PomodoroPhase, elapsed: std::time::Duration) -> &'static str {
    if ended.is_break() {
        "back to it. one thing at a time."
    } else {
        BREAK_TIPS[(elapsed.as_secs() / 4) as usize % BREAK_TIPS.len()]
    }
}

fn render_prompt_buttons(app: &AppState, frame: &mut Frame, area: Rect) {
    let palette = &app.palette;
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

fn render_send_off(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(send_off) = app.pomodoro.visible_send_off_at(app.view_observed_at) else {
        return;
    };
    let palette = &app.palette;
    let border = if send_off.started.is_break() {
        palette.green
    } else {
        palette.mauve
    };
    let Some(popup) = centered_popup_rect(area, SEND_OFF_WIDTH, SEND_OFF_HEIGHT) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, border, palette.panel_bg) else {
        return;
    };
    if inner.height < 5 {
        return;
    }
    let title = if send_off.started.is_break() {
        "enjoy the break"
    } else {
        "welcome back"
    };
    let title_color = if send_off.started.is_break() {
        palette.green
    } else {
        palette.mauve
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            title,
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ))
        .centered(),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let tip = if send_off.started.is_break() {
        let index =
            app.pomodoro.completed_work_intervals.saturating_sub(1) as usize % BREAK_TIPS.len();
        BREAK_TIPS[index]
    } else {
        "one thing at a time."
    };
    frame.render_widget(
        Paragraph::new(Span::styled(tip, Style::default().fg(palette.subtext0))).centered(),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let elapsed = animation_elapsed_at(app.view_observed_at, send_off.shown_at);
    frame.render_widget(
        Paragraph::new(Span::styled(
            send_off_dots(breath_at(elapsed).progress),
            Style::default().fg(palette.teal),
        ))
        .centered(),
        Rect::new(inner.x, inner.y + 3, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "any key closes",
            Style::default().fg(palette.overlay0),
        ))
        .right_aligned(),
        Rect::new(inner.x, inner.y + 4, inner.width, 1),
    );
}

fn send_off_dots(progress: f64) -> String {
    let lit = (progress * 4.0).round() as i8;
    (0_i8..9)
        .map(|index| {
            let distance = (index - 4).abs();
            if distance < lit {
                '●'
            } else if distance == lit {
                '•'
            } else {
                '·'
            }
        })
        .fold(String::new(), |mut dots, symbol| {
            if !dots.is_empty() {
                dots.push(' ');
            }
            dots.push(symbol);
            dots
        })
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
    fn breath_cycle_uses_four_one_five_timing_and_easing() {
        let start = breath_at(std::time::Duration::ZERO);
        assert_eq!(start.stage, BreathStage::Inhale);
        assert_eq!(start.remaining, 4);
        assert_eq!(start.progress, 0.0);

        let inhale = breath_at(std::time::Duration::from_secs(2));
        assert_eq!(inhale.stage, BreathStage::Inhale);
        assert_eq!(inhale.remaining, 2);
        assert!((inhale.progress - 0.5).abs() < f64::EPSILON);

        let hold = breath_at(std::time::Duration::from_millis(4_500));
        assert_eq!(hold.stage, BreathStage::Hold);
        assert_eq!(hold.remaining, 1);
        assert_eq!(hold.progress, 1.0);

        let exhale = breath_at(std::time::Duration::from_millis(7_500));
        assert_eq!(exhale.stage, BreathStage::Exhale);
        assert_eq!(exhale.remaining, 3);
        assert!((exhale.progress - 0.5).abs() < f64::EPSILON);
        let early_exhale = breath_at(std::time::Duration::from_secs(6));
        assert_eq!(early_exhale.cue, "breathe out");
        assert_eq!(early_exhale.remaining, 4);
        assert_eq!(breath_at(std::time::Duration::from_secs(10)), start);
    }

    #[test]
    fn visual_time_is_quantized_to_the_animation_rate() {
        let started_at = std::time::Instant::now();
        assert_eq!(
            animation_elapsed_at(
                started_at + ANIMATION_FRAME_INTERVAL - std::time::Duration::from_millis(1),
                started_at
            ),
            std::time::Duration::ZERO
        );
        assert_eq!(
            animation_elapsed_at(started_at + ANIMATION_FRAME_INTERVAL, started_at),
            ANIMATION_FRAME_INTERVAL
        );
    }

    #[test]
    fn orb_geometry_is_elliptical_and_uses_all_density_bands() {
        assert_eq!(
            orb_cell(0, 0, 5.5),
            Some(OrbCell::Fill {
                symbol: "█",
                color_band: 0,
            })
        );
        assert!(matches!(
            orb_cell(4, 0, 5.5),
            Some(OrbCell::Fill { symbol: "▓", .. })
        ));
        assert!(matches!(
            orb_cell(7, 0, 5.5),
            Some(OrbCell::Fill { symbol: "▒", .. })
        ));
        assert!(matches!(
            orb_cell(9, 0, 5.5),
            Some(OrbCell::Fill { symbol: "░", .. })
        ));
        assert!(matches!(orb_cell(11, 0, 5.5), Some(OrbCell::Halo)));
        assert_eq!(orb_cell(14, 0, 5.5), None);
        assert!(orb_cell(8, 0, 5.5).is_some());
        assert!(orb_cell(0, 8, 5.5).is_none());
    }

    #[test]
    fn send_off_dots_pulse_from_the_center() {
        assert_eq!(send_off_dots(0.0), "· · · · • · · · ·");
        assert_eq!(send_off_dots(1.0), "• ● ● ● ● ● ● ● •");
    }

    #[test]
    fn prompt_tips_rotate_for_break_start_and_settle_for_break_over() {
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::ZERO),
            BREAK_TIPS[0]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::from_secs(4)),
            BREAK_TIPS[1]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::from_secs(8)),
            BREAK_TIPS[2]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::ShortBreak, std::time::Duration::from_secs(8)),
            "back to it. one thing at a time."
        );
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
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro.enabled = true;
        app.pomodoro.prompt = Some(PomodoroPrompt {
            ended: PomodoroPhase::Work,
            next: PomodoroPhase::ShortBreak,
            raised_at: now,
            input: String::new(),
            error: None,
        });
        app.view_observed_at = now;
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

    #[test]
    fn wide_prompt_draws_the_breathing_orb_cue_copy_and_tip() {
        let mut app = prompted();
        let raised_at = app.pomodoro.prompt.as_ref().expect("prompt").raised_at;
        app.view_observed_at = raised_at + std::time::Duration::from_secs(1);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 100, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(text.contains('█'), "orb center: {text:?}");
        assert!(text.contains('▓'), "orb middle: {text:?}");
        assert!(text.contains('▒'), "orb outer band: {text:?}");
        assert!(text.contains('░'), "orb edge: {text:?}");
        assert!(text.contains("breathe in · 3"), "cue: {text:?}");
        assert!(text.contains("take a deep breath."), "lead: {text:?}");
        assert!(
            text.contains("stand up · look at something far away"),
            "tip: {text:?}"
        );
    }

    #[test]
    fn narrow_prompt_keeps_the_existing_layout_and_buttons_without_an_orb() {
        let app = prompted();
        let area = Rect::new(0, 0, 60, 16);
        let buffer = veiled_frame(&app, area);
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(
            !text.contains("take a deep breath."),
            "fallback copy: {text:?}"
        );
        assert!(
            !text.chars().any(|ch| matches!(ch, '█' | '▓' | '▒' | '░')),
            "fallback orb: {text:?}"
        );
        assert!(text.contains("↵ confirm"), "confirm button: {text:?}");
        assert!(text.contains("^⌥b snooze"), "snooze button: {text:?}");
        let (confirm, snooze) = prompt_button_rects(area).expect("fallback buttons");
        assert_eq!(confirm.y, snooze.y);
        assert!(confirm.right() <= area.right());
        assert!(snooze.right() <= area.right());
    }

    #[test]
    fn successful_confirm_draws_the_send_off_card() {
        let started = std::time::Instant::now();
        let prompt_at = started + std::time::Duration::from_secs(25 * 60);
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            started,
        );
        app.pomodoro.tick(prompt_at);
        app.pomodoro.prompt.as_mut().expect("prompt").input = "walk".into();
        app.pomodoro.confirm(prompt_at).expect("confirm");
        app.view_observed_at = prompt_at + std::time::Duration::from_secs(1);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 100, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("enjoy the break"), "title: {text:?}");
        assert!(
            text.contains("stand up · look at something far away"),
            "tip: {text:?}"
        );
        assert!(text.contains("●"), "pulsing dots: {text:?}");
        assert!(text.contains("any key closes"), "dismiss hint: {text:?}");
    }

    #[test]
    fn confirming_a_finished_break_draws_the_welcome_back_send_off() {
        let started = std::time::Instant::now();
        let prompt_at = started + std::time::Duration::from_secs(5 * 60);
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            started,
        );
        app.pomodoro.skip(started);
        app.pomodoro.tick(prompt_at);
        app.pomodoro.prompt.as_mut().expect("prompt").input = "ready".into();
        app.pomodoro.confirm(prompt_at).expect("confirm");
        app.view_observed_at = prompt_at + std::time::Duration::from_secs(1);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 100, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("welcome back"), "title: {text:?}");
        assert!(text.contains("one thing at a time."), "tip: {text:?}");
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
