#[cfg(test)]
use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::{display_width, display_width_u16};
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{
        CopyFeedback, Palette, StatusButton, StatusButtonAction, ToastKind, ToastNotification,
    },
    app::AppState,
    config::{StatusIndicatorStyle, ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    platform::status_metrics::StatusMetrics,
};

/// Full-width, right-aligned top status row.
///
/// Contents, left to right: provider quota (Claude, Codex, Kimi) · offline dot ·
/// agent dot · device · CPU · memory · disk. The row before the first surviving
/// segment is intentionally blank.
///
/// Every value is a filled column because the question the row answers is "is
/// anything close to its ceiling", not "what is the exact number". Expanded
/// mode adds the numbers and the reset times back.
///
/// Layout: spans the full client width above the sidebar and pads before the
/// first surviving segment. On narrow widths Kimi, then Codex, then Claude,
/// then the device elide in that order; CPU and memory remain required.
pub(crate) fn render_status_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    let bg = Style::default().bg(p.panel_bg);
    frame.render_widget(Paragraph::new("").style(bg), area);

    let unavailable = StatusMetrics {
        hostname: "--".into(),
        ..StatusMetrics::default()
    };
    let metrics = app
        .status_metrics
        .as_ref()
        .map(|snapshot| &snapshot.metrics)
        .unwrap_or(&unavailable);
    if usize::from(area.width) < minimum_required_status_width(app) {
        return;
    }
    let segments = fitted_segments(status_segments(app, metrics, p), area.width as usize);

    let used = segment_width(&segments);
    let pad = (area.width as usize).saturating_sub(used);
    let mut spans: Vec<Span> = Vec::new();
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), bg));
    }
    for seg in &segments {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);

    for button in &app.view.status_buttons {
        let style = if button.active {
            Style::default().fg(p.accent).bg(p.panel_bg)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.panel_bg)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(button.label.clone(), style))),
            button.rect,
        );
    }
}

/// Left-aligned quick-access buttons. The status bar's own segments are
/// right-aligned, so the left half is otherwise pure padding.
///
/// The blocked button carries its count so the filter state is readable without
/// opening another surface.
pub(crate) fn status_buttons(app: &AppState, area: Rect) -> Vec<StatusButton> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let blocked = crate::ui::sidebar::all_agent_panel_entries(app)
        .into_iter()
        .filter(crate::ui::sidebar::entry_has_red_dot)
        .count();
    let specs = [
        (
            StatusButtonAction::BlockedFilter,
            if blocked > 0 {
                format!(" blocked {blocked} ")
            } else {
                " blocked ".to_string()
            },
            app.blocked_filter,
        ),
        (
            StatusButtonAction::Dock,
            " dock ".to_string(),
            !app.dock_collapsed,
        ),
        // The toggle sits with the other affordances rather than beside the
        // usage it expands: the row's right edge moves as segments elide, and a
        // button that moves is a button nobody learns.
        (
            StatusButtonAction::StatusDetail,
            if app.status_bar_expanded {
                " \u{25be} ".to_string()
            } else {
                " \u{25b8} ".to_string()
            },
            app.status_bar_expanded,
        ),
    ];

    // The right-aligned segments are load-bearing; buttons yield to them rather
    // than overlapping, and drop whole rather than truncating to an unreadable stub.
    let reserved = segment_width(&fitted_segments(
        status_segments(app, metrics_or_unavailable(app), &app.palette),
        area.width as usize,
    ));
    let budget = (area.width as usize).saturating_sub(reserved);

    let mut buttons = Vec::new();
    let mut x = area.x;
    let mut used = 0usize;
    for (action, label, active) in specs {
        let width = display_width(&label);
        if used + width > budget {
            break;
        }
        let Ok(cell_width) = u16::try_from(width) else {
            break;
        };
        buttons.push(StatusButton {
            rect: Rect::new(x, area.y, cell_width, 1),
            label,
            action,
            active,
        });
        x = x.saturating_add(cell_width);
        used += width;
    }
    buttons
}

fn metrics_or_unavailable(app: &AppState) -> &crate::platform::status_metrics::StatusMetrics {
    static UNAVAILABLE: std::sync::OnceLock<crate::platform::status_metrics::StatusMetrics> =
        std::sync::OnceLock::new();
    app.status_metrics
        .as_ref()
        .map(|snapshot| &snapshot.metrics)
        .unwrap_or_else(|| {
            UNAVAILABLE.get_or_init(|| crate::platform::status_metrics::StatusMetrics {
                hostname: "--".into(),
                ..Default::default()
            })
        })
}

struct Segment {
    text: String,
    style: Style,
    /// When true, `style` already carries its own background (prefix pill).
    preserve_bg: bool,
    /// Lower values elide first; `None` is required at desktop widths.
    elide_rank: Option<u8>,
}

fn segment_width(segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|segment| display_width(&segment.text))
        .sum()
}

fn fitted_segments(mut segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    while segment_width(&segments) > width {
        let candidate = segments
            .iter()
            .enumerate()
            .filter_map(|(index, segment)| segment.elide_rank.map(|rank| (rank, index)))
            .min_by_key(|(rank, _)| *rank);
        let Some((_, index)) = candidate else {
            break;
        };
        segments.remove(index);
    }

    debug_assert!(segment_width(&segments) <= width);
    segments
}

pub(crate) fn minimum_required_status_width(_app: &AppState) -> usize {
    1 + display_width(" CPU \u{2588} 100 ") + display_width(" MEM \u{2588} 100 ")
}

/// The eight fill levels of a column glyph, from shortest to full.
const FILL_LEVELS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];
/// A window at or above this fill reports its number and reset time even in
/// compact mode. The one moment the exact figure matters is the moment the
/// human should not have to press a key to see it.
const ESCALATION_PERCENT: u8 = 88;
const WARN_PERCENT: u8 = 80;
const CRITICAL_PERCENT: u8 = 90;
const DOT: char = '\u{25cf}';

/// Maps a percentage onto a column glyph. Zero still draws the shortest column
/// rather than a blank, so a present-but-idle account stays visibly present.
pub(crate) fn fill_glyph(percent: u8) -> char {
    let index = usize::from(percent.min(100)) * FILL_LEVELS.len() / 101;
    FILL_LEVELS[index.min(FILL_LEVELS.len() - 1)]
}

fn load_color(percent: u8, p: &Palette) -> Color {
    match percent {
        percent if percent >= CRITICAL_PERCENT => p.red,
        percent if percent >= WARN_PERCENT => p.yellow,
        _ => p.green,
    }
}

/// One provider block: label, account code when it can be named, then one
/// column per window in the order 5h, 7d.
pub(crate) fn provider_segment_text(
    label: &str,
    usage: &crate::provider_usage::AccountUsage,
    expanded: bool,
    now_unix: Option<i64>,
) -> Option<String> {
    if usage.is_empty() {
        return None;
    }
    let mut text = format!(" {label}");
    if let Some(account) = &usage.account {
        text.push(' ');
        text.push_str(account);
    }
    for window in [usage.five_hour, usage.seven_day].into_iter().flatten() {
        text.push(' ');
        text.push(fill_glyph(window.used_percent));
        // A number and a reset time are only actionable when the window is
        // nearly spent, or when the human asked for detail. Otherwise they are
        // noise in every frame of the day.
        if expanded || window.used_percent >= ESCALATION_PERCENT {
            text.push_str(&window.used_percent.to_string());
            if let Some(reset) = window
                .resets_at
                .zip(now_unix)
                .and_then(|(resets_at, now)| crate::provider_usage::reset_label(resets_at, now))
            {
                text.push(' ');
                text.push_str(&reset);
            }
        }
    }
    text.push(' ');
    Some(text)
}

fn provider_style(usage: &crate::provider_usage::AccountUsage, color: Color, p: &Palette) -> Style {
    if usage.stale {
        return Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    }
    match usage.peak_percent() {
        Some(percent) if percent >= CRITICAL_PERCENT => Style::default().fg(p.red),
        Some(percent) if percent >= WARN_PERCENT => Style::default().fg(p.yellow),
        _ => Style::default().fg(color),
    }
}

fn status_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();
    let expanded = app.status_bar_expanded;
    let now_unix = app.status_now_unix;

    // Providers elide from the right of the group inwards: Kimi is the most
    // recent addition and the least load-bearing, Claude the most.
    for (label, usage, color, rank) in [
        ("CC", &app.provider_usage.claude, p.peach, 3u8),
        ("CX", &app.provider_usage.codex, p.blue, 2),
        ("KI", &app.provider_usage.kimi, p.mauve, 1),
    ] {
        if let Some(text) = provider_segment_text(label, usage, expanded, now_unix) {
            out.push(Segment {
                text,
                style: provider_style(usage, color, p),
                preserve_bg: false,
                elide_rank: Some(rank),
            });
        }
    }

    // Offline exists only when something is wrong, so it never elides: a row
    // too narrow to say "offline" would rather drop a provider than drop the
    // reason none of them can be reached.
    if !app.connectivity.is_online() {
        out.push(Segment {
            text: format!(" {DOT} "),
            style: Style::default().fg(p.red),
            preserve_bg: false,
            elide_rank: None,
        });
    }

    let (agents, blocked) = app.agent_dot_counts();
    if agents > 0 {
        let text = if blocked > 0 {
            format!(" {DOT} {agents}/{blocked} ")
        } else {
            format!(" {DOT} {agents} ")
        };
        out.push(Segment {
            text,
            style: Style::default().fg(if blocked > 0 { p.red } else { p.accent }),
            preserve_bg: false,
            elide_rank: Some(5),
        });
    }

    out.push(Segment {
        text: format!(" {} ", metrics.hostname),
        style: Style::default().fg(p.green),
        preserve_bg: false,
        elide_rank: Some(4),
    });

    out.push(metric_segment("CPU", metrics.cpu_percent, expanded, p));
    out.push(metric_segment("MEM", memory_percent(metrics), expanded, p));
    if crate::platform::status_metrics::disk_segment_visible(
        metrics.disk_percent,
        app.status_disk_visible,
    ) {
        out.push(metric_segment("DSK", metrics.disk_percent, expanded, p));
    }
    out
}

/// Memory as a share of the installed total, the form that needs no arithmetic
/// from the reader. The GiB figures stay in the info panel for anyone who wants
/// the absolute numbers.
pub(crate) fn memory_percent(
    metrics: &crate::platform::status_metrics::StatusMetrics,
) -> Option<u8> {
    let used = metrics.mem_used_gib?;
    let total = metrics.mem_total_gib?;
    if !used.is_finite() || !total.is_finite() || total <= 0.0 || used < 0.0 || used > total {
        return None;
    }
    Some((used / total * 100.0).round().clamp(0.0, 100.0) as u8)
}

fn metric_segment(label: &str, percent: Option<u8>, expanded: bool, p: &Palette) -> Segment {
    let (text, color) = match percent.filter(|percent| *percent <= 100) {
        Some(percent) if expanded => (
            format!(" {label} {} {percent} ", fill_glyph(percent)),
            load_color(percent, p),
        ),
        Some(percent) => (
            format!(" {label} {} ", fill_glyph(percent)),
            load_color(percent, p),
        ),
        None => (format!(" {label} - "), p.overlay0),
    };
    Segment {
        text,
        style: Style::default().fg(color),
        preserve_bg: false,
        elide_rank: None,
    }
}

#[cfg(test)]
/// Focused pane cwd and branch, still projected for consumers that need the
/// Git context. The status row no longer renders the branch: it never changed
/// within a workspace, so it cost a column without answering a question.
pub(crate) fn focused_context(app: &AppState) -> (Option<PathBuf>, Option<String>) {
    let Some(ws_idx) = app.active else {
        return (None, None);
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return (None, None);
    };
    // Runtime-resolved focused cwd, projected by the same code path that
    // resolves the Git refresh target, so the cwd and branch segments always
    // describe the same directory. Falls back to workspace identity until the
    // first projection lands.
    let cwd = app
        .status_focused_cwd
        .clone()
        .or_else(|| (!app.status_focus_projection_initialized).then(|| ws.identity_cwd.clone()));
    let branch = if app.status_git_cwd.as_ref() == cwd.as_ref() {
        app.status_git_branch.clone()
    } else if !app.status_focus_projection_initialized
        && ws.cached_identity_cwd == cwd.clone().unwrap_or_default()
    {
        ws.cached_git_branch.clone()
    } else {
        None
    };
    (cwd, branch)
}

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
) -> Rect {
    let content_width = display_width_u16(&toast.title)
        .max(display_width_u16(&toast.context))
        .saturating_add(4);
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::BottomLeft => area.x,
        ToastHerdrPosition::TopRight | ToastHerdrPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let warning_offset = u16::from(offset_for_warning);
    let y = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::TopRight => {
            area.y + warning_offset.min(area.height)
        }
        ToastHerdrPosition::BottomLeft | ToastHerdrPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + warning_offset)
        }
    };
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning, position);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_icon_symbol(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
) -> &'static str {
    match (indicator_style, state, seen) {
        (StatusIndicatorStyle::Dots, AgentState::Blocked, _)
        | (StatusIndicatorStyle::Dots, AgentState::Working, _)
        | (StatusIndicatorStyle::Dots, AgentState::Idle, false) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Dots, AgentState::Unknown, _) => "·",
        (StatusIndicatorStyle::Symbols, AgentState::Blocked, _) => "×",
        (StatusIndicatorStyle::Symbols, AgentState::Working, _) => "◐",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, false) => "✓",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Symbols, AgentState::Unknown, _) => "·",
    }
}

pub(super) fn state_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    (
        state_icon_symbol(state, seen, indicator_style),
        Style::default().fg(state_label_color(state, seen, p)),
    )
}

pub(super) fn state_icon_with_stale(
    state: AgentState,
    seen: bool,
    stale: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    let _ = stale;
    state_icon(state, seen, indicator_style, p)
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.blue,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

pub(super) fn state_label_color_with_stale(
    state: AgentState,
    seen: bool,
    stale: bool,
    p: &Palette,
) -> Color {
    let _ = stale;
    state_label_color(state, seen, p)
}

#[cfg(test)]
pub(super) fn status_report_age_label(
    reported_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<String> {
    let age = now.checked_duration_since(reported_at?)?;
    (age >= std::time::Duration::from_secs(60)).then(|| {
        let minutes = age.as_secs() / 60;
        if minutes >= 60 {
            format!("reported {}h ago", minutes / 60)
        } else {
            format!("reported {minutes}m ago")
        }
    })
}

pub(super) fn status_report_age_compact_label(
    reported_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<String> {
    let age = now.checked_duration_since(reported_at?)?;
    (age >= std::time::Duration::from_secs(60)).then(|| {
        let minutes = age.as_secs() / 60;
        if minutes >= 60 {
            format!("{}h", minutes / 60)
        } else {
            format!("{minutes}m")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastHerdrPosition};

    fn toast() -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: "done".to_string(),
            context: "workspace".to_string(),
            position: None,
            target: None,
        }
    }

    fn feedback() -> CopyFeedback {
        CopyFeedback {
            message: "copied to clipboard".to_string(),
        }
    }

    #[test]
    fn state_dots_and_labels_use_semantic_workspace_colors() {
        let palette = Palette::catppuccin();
        for (state, seen, symbol, color) in [
            (AgentState::Blocked, true, "●", palette.red),
            // ac7: active work uses the blue activity accent, not warning yellow.
            (AgentState::Working, true, "●", palette.blue),
            (AgentState::Idle, false, "●", palette.teal),
            (AgentState::Idle, true, "○", palette.green),
            (AgentState::Unknown, true, "·", palette.overlay0),
        ] {
            let (actual_symbol, style) =
                state_icon(state, seen, StatusIndicatorStyle::Dots, &palette);
            assert_eq!(actual_symbol, symbol);
            assert_eq!(style.fg, Some(color));
            assert_eq!(state_label_color(state, seen, &palette), color);
        }
    }

    #[test]
    fn state_icons_support_dot_and_distinct_symbol_styles() {
        let palette = Palette::catppuccin();
        for (indicator_style, expected_symbols) in [
            (StatusIndicatorStyle::Dots, ["●", "●", "●", "○", "·"]),
            (StatusIndicatorStyle::Symbols, ["×", "◐", "✓", "○", "·"]),
        ] {
            for ((state, seen, color), expected_symbol) in [
                (AgentState::Blocked, true, palette.red),
                // The fork's sidebar contract uses blue for active work.
                (AgentState::Working, true, palette.blue),
                (AgentState::Idle, false, palette.teal),
                (AgentState::Idle, true, palette.green),
                (AgentState::Unknown, true, palette.overlay0),
            ]
            .into_iter()
            .zip(expected_symbols)
            {
                let (actual_symbol, style) = state_icon(state, seen, indicator_style, &palette);
                assert_eq!(actual_symbol, expected_symbol);
                assert_eq!(display_width_u16(actual_symbol), 1);
                assert_eq!(style.fg, Some(color));
            }
        }
    }

    #[test]
    fn status_report_age_projection_distinguishes_recent_and_aged_reports() {
        let reported_at = std::time::Instant::now();
        let recent = reported_at + std::time::Duration::from_secs(30);
        let aged = reported_at + std::time::Duration::from_secs(90);
        let old = reported_at + std::time::Duration::from_secs(3660);

        assert_eq!(status_report_age_label(Some(reported_at), recent), None);
        assert_eq!(
            status_report_age_label(Some(reported_at), aged).as_deref(),
            Some("reported 1m ago")
        );
        assert_eq!(
            status_report_age_compact_label(Some(reported_at), old).as_deref(),
            Some("1h")
        );
        let (stale_symbol, stale_style) = state_icon_with_stale(
            AgentState::Working,
            true,
            true,
            StatusIndicatorStyle::Dots,
            &Palette::catppuccin(),
        );
        assert_eq!(stale_symbol, "●");
        assert_eq!(stale_style.fg, Some(Palette::catppuccin().blue));
    }

    #[test]
    fn toast_rect_uses_configured_corner() {
        let area = Rect::new(10, 20, 100, 40);
        let toast = toast();

        let top_left = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopLeft);
        assert_eq!(top_left.x, area.x);
        assert_eq!(top_left.y, area.y);

        let top_right = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);
        assert_eq!(top_right.x + top_right.width, area.x + area.width);
        assert_eq!(top_right.y, area.y);

        let bottom_left =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomLeft);
        assert_eq!(bottom_left.x, area.x);
        assert_eq!(bottom_left.y + bottom_left.height, area.y + area.height);

        let bottom_right =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomRight);
        assert_eq!(bottom_right.x + bottom_right.width, area.x + area.width);
        assert_eq!(bottom_right.y + bottom_right.height, area.y + area.height);
    }

    #[test]
    fn toast_rect_uses_display_width_for_cjk_labels() {
        let area = Rect::new(0, 0, 100, 20);
        let toast = ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "重构用户认证模块".to_string(),
            context: "提交 herdr 的反馈".to_string(),
            position: None,
            target: None,
        };

        let rect = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);

        let expected_content_width =
            display_width_u16(&toast.title).max(display_width_u16(&toast.context)) + 6;
        assert_eq!(rect.width, expected_content_width);
        assert_eq!(rect.x + rect.width, area.x + area.width);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = feedback();

        let top_center = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top_center.y, area.y);
        assert_eq!(
            top_center.x,
            area.x + area.width.saturating_sub(top_center.width) / 2
        );

        let bottom_center =
            copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom_center.y + bottom_center.height, area.y + area.height);
        assert_eq!(
            bottom_center.x,
            area.x + area.width.saturating_sub(bottom_center.width) / 2
        );
    }

    #[test]
    fn required_metrics_survive_optional_segment_elision() {
        // CPU/MEM are required while branch and device elide by rank.
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.status_focused_cwd = Some(PathBuf::from("/very/long/focused/folder"));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some("feature/very-long-branch".into());
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let width = minimum_required_status_width(&app);
        let segments = fitted_segments(status_segments(&app, &metrics, &app.palette), width);
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM ▄"));
        assert!(rendered.contains("CPU ▁"));
        assert!(!rendered.contains("feature/very-long"));
    }

    #[test]
    fn review_findings_narrow_desktop_keeps_required_metrics() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.mobile_width_threshold = 0;
        let required = minimum_required_status_width(&app) as u16;
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required - 1, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
        let mut terminal = Terminal::new(TestBackend::new(required, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, required, 1)))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("CPU ▁"), "{rendered}");
        assert!(rendered.contains("MEM ▄"), "{rendered}");
    }

    #[test]
    fn review_findings_status_width_is_independent_of_metric_snapshot() {
        let mut app = AppState::test_new();
        let unavailable_width = minimum_required_status_width(&app);
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: StatusMetrics {
                cpu_percent: Some(100),
                mem_used_gib: Some(17_179_869_184.0),
                mem_total_gib: Some(17_179_869_184.0),
                disk_percent: None,
                hostname: "host-with-a-long-device-name".into(),
            },
            sampled_at: std::time::Instant::now(),
        });

        assert_eq!(minimum_required_status_width(&app), unavailable_width);
        let required = fitted_segments(
            status_segments(
                &app,
                &app.status_metrics.as_ref().unwrap().metrics,
                &app.palette,
            ),
            unavailable_width,
        );
        assert!(segment_width(&required) <= unavailable_width);
    }

    #[test]
    fn narrow_desktop_never_truncates_required_metrics() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.mobile_width_threshold = 0;
        let required = minimum_required_status_width(&app) as u16;
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: StatusMetrics {
                cpu_percent: Some(100),
                mem_used_gib: Some(9_999.9),
                mem_total_gib: Some(9_999.9),
                disk_percent: None,
                hostname: "wide-metrics".into(),
            },
            sampled_at: std::time::Instant::now(),
        });
        assert_eq!(
            minimum_required_status_width(&app) as u16,
            required,
            "live samples must not move the desktop/mobile breakpoint"
        );
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required - 1, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
        let mut terminal = Terminal::new(TestBackend::new(required, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, required, 1)))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("CPU \u{2588}"), "{rendered}");
        assert!(rendered.contains("MEM \u{2588}"), "{rendered}");
        assert!(
            rendered.starts_with(' '),
            "left side must stay blank: {rendered:?}"
        );
    }

    fn assert_long_context_fits_status_row(cwd: &str, branch: &str) {
        use ratatui::{backend::TestBackend, Terminal};

        const WIDTH: usize = crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD as usize;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.status_focused_cwd = Some(PathBuf::from(cwd));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some(branch.into());
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let segments = fitted_segments(status_segments(&app, &metrics, &app.palette), WIDTH);
        assert!(segment_width(&segments) <= WIDTH);

        let mut terminal =
            Terminal::new(TestBackend::new(WIDTH as u16, 1)).expect("create status terminal");
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, WIDTH as u16, 1)))
            .expect("render status");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("MEM ▄"), "{rendered}");
        assert!(rendered.contains("CPU ▁"), "{rendered}");
        assert!(!rendered.contains("Herdr v"), "{rendered}");
    }

    #[test]
    fn minimum_desktop_status_fits_long_ascii_context_and_required_metrics() {
        assert_long_context_fits_status_row(
            "/a/very/long/folder/path/that/must/not/displace/required/metrics",
            "feature/a-very-long-branch-that-must-elide",
        );
    }

    #[test]
    fn minimum_desktop_status_fits_wide_context_and_required_metrics() {
        assert_long_context_fits_status_row(
            "/重要な/長い/フォルダー/必須/メトリクス",
            "機能/長いブランチ名",
        );
    }

    #[test]
    fn status_branch_is_bound_to_focused_pane_cwd() {
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/repo");
        workspace.cached_identity_cwd = PathBuf::from("/repo");
        workspace.cached_git_branch = Some("workspace-root".into());
        let focused = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let focused_terminal = workspace
            .terminal_id(focused)
            .expect("focused terminal")
            .clone();
        let mut app = AppState::test_new();
        app.terminals.insert(
            focused_terminal.clone(),
            crate::terminal::TerminalState::new(focused_terminal, PathBuf::from("/repo/nested")),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.sync_status_focused_cwd(&crate::terminal::TerminalRuntimeRegistry::default());
        app.status_git_cwd = Some(PathBuf::from("/repo/nested"));
        app.status_git_branch = Some("nested-branch".into());

        let (cwd, branch) = focused_context(&app);

        assert_eq!(cwd, Some(PathBuf::from("/repo/nested")));
        assert_eq!(branch.as_deref(), Some("nested-branch"));
    }

    #[test]
    fn scheduled_focus_projection_hides_stale_branch() {
        let root_cwd = PathBuf::from("/repo");
        let nested_cwd = root_cwd.join("nested");
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = root_cwd.clone();
        workspace.cached_identity_cwd = root_cwd.clone();
        workspace.cached_git_branch = Some("root-branch".into());
        let root = workspace.tabs[0].root_pane;
        let root_terminal = workspace.terminal_id(root).expect("root terminal").clone();
        let nested = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let nested_terminal = workspace
            .terminal_id(nested)
            .expect("nested terminal")
            .clone();
        workspace.tabs[0].layout.focus_pane(root);

        let mut app = AppState::test_new();
        app.terminals.insert(
            root_terminal.clone(),
            crate::terminal::TerminalState::new(root_terminal, root_cwd.clone()),
        );
        app.terminals.insert(
            nested_terminal.clone(),
            crate::terminal::TerminalState::new(nested_terminal, nested_cwd.clone()),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::default();

        assert!(app.sync_status_focused_cwd(&runtimes));
        app.status_git_cwd = Some(root_cwd.clone());
        app.status_git_branch = Some("root-branch".into());
        assert_eq!(app.status_focused_cwd, Some(root_cwd));

        assert!(app.focus_pane_in_workspace(0, nested));
        assert!(app.sync_status_focused_cwd(&runtimes));
        let (cwd, branch) = focused_context(&app);

        assert_eq!(app.status_focused_cwd, Some(nested_cwd.clone()));
        assert_eq!(cwd, Some(nested_cwd));
        assert_eq!(branch, None);
    }

    #[test]
    fn status_cwd_falls_back_to_workspace_identity_before_first_projection() {
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/repo");
        workspace.cached_identity_cwd = PathBuf::from("/repo");
        workspace.cached_git_branch = Some("workspace-root".into());
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let (cwd, branch) = focused_context(&app);

        assert_eq!(cwd, Some(PathBuf::from("/repo")));
        assert_eq!(branch.as_deref(), Some("workspace-root"));
    }

    #[test]
    fn status_metrics_fallback_format_is_explicit() {
        // AC3/AC5: unavailable snapshots render stable units without sampling in render.
        let app = AppState::test_new();
        let segments = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM -"), "{rendered}");
        assert!(rendered.contains("CPU -"), "{rendered}");
    }

    #[test]
    fn status_metrics_outside_bounded_display_contract_use_fallbacks() {
        let app = AppState::test_new();
        let baseline = status_segments(
            &app,
            &StatusMetrics {
                cpu_percent: Some(12),
                mem_used_gib: Some(8.0),
                mem_total_gib: Some(16.0),
                disk_percent: None,
                hostname: "testhost".into(),
            },
            &app.palette,
        );
        let metrics = StatusMetrics {
            cpu_percent: Some(101),
            mem_used_gib: Some(10_000.0),
            mem_total_gib: Some(10_000.0),
            disk_percent: None,
            hostname: "testhost".into(),
        };
        let rendered = status_segments(&app, &metrics, &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("CPU -"), "{rendered}");
        // A percentage has no magnitude ceiling, so a full volume of any size
        // is expressible; only the impossible CPU reading falls back.
        assert!(rendered.contains("MEM \u{2588}"), "{rendered}");
        let fallback = status_segments(&app, &metrics, &app.palette);
        assert_eq!(
            segment_width(&baseline),
            segment_width(&fallback),
            "metric values and fallbacks must not shift or re-elide the row"
        );
    }

    #[test]
    fn status_renderer_source_contract_excludes_io_and_sampling() {
        let source = include_str!("status.rs")
            .split_once("#[cfg(test)]\nmod tests")
            .expect("status renderer source")
            .0;
        for forbidden in [
            "std::fs",
            "std::net",
            "std::process",
            "Command::new",
            "sample_status_metrics",
            "http://",
            "https://",
        ] {
            assert!(!source.contains(forbidden), "found {forbidden}");
        }
    }

    #[test]
    fn status_elision_drops_providers_then_device() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.provider_usage = usage_fixture();
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let full = status_segments(&app, &metrics, &app.palette);

        // Kimi first, then Codex, then Claude, then the device: the row sheds
        // the least load-bearing account before it sheds a required metric.
        assert_eq!(
            full.iter()
                .filter_map(|segment| segment.elide_rank)
                .collect::<Vec<_>>(),
            vec![3, 2, 1, 4]
        );

        let without_kimi = fitted_segments(
            status_segments(&app, &metrics, &app.palette),
            segment_width(&full) - display_width(&full[2].text),
        );
        let rendered = without_kimi
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("KI"), "{rendered}");
        assert!(rendered.contains("CC"), "{rendered}");
        assert!(rendered.contains("testhost"), "{rendered}");

        let optional_width = full
            .iter()
            .filter(|segment| segment.elide_rank.is_some())
            .map(|segment| display_width(&segment.text))
            .sum::<usize>();
        let required = fitted_segments(
            status_segments(&app, &metrics, &app.palette),
            segment_width(&full) - optional_width,
        );
        let rendered = required
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("CC"), "{rendered}");
        assert!(!rendered.contains("testhost"), "{rendered}");
        assert!(rendered.contains("CPU \u{2581}"), "{rendered}");
        assert!(rendered.contains("MEM \u{2584}"), "{rendered}");
    }

    /// Two providers with known windows: Claude half-spent on the week, Codex
    /// idle. Fixed values so the glyph a percentage maps to is asserted, not
    /// assumed.
    fn usage_fixture() -> crate::provider_usage::ProviderUsageSnapshot {
        use crate::provider_usage::{AccountUsage, ProviderUsageSnapshot, QuotaWindow};
        ProviderUsageSnapshot {
            claude: AccountUsage {
                account: Some("SHQ".into()),
                five_hour: Some(QuotaWindow {
                    used_percent: 6,
                    resets_at: Some(2_000_000_000),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 56,
                    resets_at: Some(2_000_100_000),
                }),
                stale: false,
            },
            codex: AccountUsage {
                account: Some("SHQ".into()),
                seven_day: Some(QuotaWindow {
                    used_percent: 19,
                    resets_at: Some(2_000_100_000),
                }),
                ..AccountUsage::default()
            },
            kimi: AccountUsage {
                seven_day: Some(QuotaWindow {
                    used_percent: 24,
                    resets_at: Some(2_000_100_000),
                }),
                ..AccountUsage::default()
            },
        }
    }

    #[test]
    fn status_content_order_theme_and_omitted_segments_match_contract() {
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/home/test/work/status");
        workspace.cached_git_branch = Some("feature/native-status".into());
        workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(1);
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.status_git_cwd = Some(PathBuf::from("/home/test/work/status"));
        app.status_focused_cwd = app.status_git_cwd.clone();
        app.status_git_branch = Some("feature/native-status".into());
        app.provider_usage = usage_fixture();

        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let segments = status_segments(&app, &metrics, &app.palette);
        assert_eq!(segments.len(), 6, "three providers, device, CPU, memory");
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        let ordered = [
            "CC SHQ \u{2581} \u{2585}",
            "CX SHQ \u{2582}",
            "KI \u{2582}",
            "testhost",
            "CPU \u{2581}",
            "MEM \u{2584}",
        ];
        let mut previous = 0;
        for value in ordered {
            let index = rendered
                .find(value)
                .unwrap_or_else(|| panic!("{value}: {rendered}"));
            assert!(index >= previous, "{value} is out of order: {rendered}");
            previous = index;
        }
        // The branch went with the redesign: it never changed inside a
        // workspace, so it cost a column and answered nothing. Percentages and
        // reset times belong to expanded mode, not to every frame.
        for removed in [
            "feature/native-stat",
            "testuser",
            "10.0.0.2",
            "session:",
            "workspace:",
            "~/work/status",
            "Herdr v",
            "GiB",
            "56",
            "12%",
        ] {
            assert!(!rendered.contains(removed), "{removed}: {rendered}");
        }
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("CC "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.peach)
        );
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("CX "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.blue)
        );
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("testhost"))
                .unwrap()
                .style
                .fg,
            Some(app.palette.green)
        );
        // A load colour, not a fixed one: CPU and memory are read for how close
        // they are to their ceiling, and 12% and 50% are both far from it.
        for label in ["CPU ", "MEM "] {
            assert_eq!(
                segments
                    .iter()
                    .find(|segment| segment.text.contains(label))
                    .unwrap()
                    .style
                    .fg,
                Some(app.palette.green),
                "{label}"
            );
        }
    }

    #[test]
    fn fill_glyphs_span_the_range_without_a_blank_at_zero() {
        // Zero draws the shortest column, not nothing: an account that is
        // present and idle must look different from an account that is absent.
        assert_eq!(fill_glyph(0), '\u{2581}');
        assert_eq!(fill_glyph(50), '\u{2584}');
        assert_eq!(fill_glyph(100), '\u{2588}');
        for percent in 0..=100u8 {
            assert!(FILL_LEVELS.contains(&fill_glyph(percent)), "{percent}");
        }
        // Out-of-range input saturates rather than panicking on the index.
        assert_eq!(fill_glyph(255), '\u{2588}');
    }

    #[test]
    fn compact_hides_numbers_and_expanded_restores_them_with_reset_times() {
        let mut app = AppState::test_new();
        app.provider_usage = usage_fixture();
        app.status_now_unix = Some(1_999_990_100);

        let compact = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(compact.contains("CC SHQ \u{2581} \u{2585}"), "{compact}");
        assert!(!compact.contains("56"), "{compact}");

        app.status_bar_expanded = true;
        let expanded = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(expanded.contains("\u{2585}56"), "{expanded}");
        assert!(expanded.contains("2h45"), "{expanded}");
    }

    #[test]
    fn a_nearly_spent_window_shows_its_number_without_expanding() {
        // The one moment the exact figure matters, it appears unasked.
        let mut app = AppState::test_new();
        app.provider_usage = usage_fixture();
        app.provider_usage.claude.seven_day = Some(crate::provider_usage::QuotaWindow {
            used_percent: 94,
            resets_at: Some(1_999_999_999),
        });
        app.status_now_unix = Some(1_999_990_100);

        let rendered = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let claude = rendered
            .iter()
            .find(|segment| segment.text.contains("CC "))
            .expect("claude segment");
        assert!(claude.text.contains("\u{2588}94"), "{}", claude.text);
        assert_eq!(claude.style.fg, Some(app.palette.red));
    }

    #[test]
    fn a_stale_source_renders_dim_instead_of_asserting_its_numbers() {
        let mut app = AppState::test_new();
        app.provider_usage = usage_fixture();
        app.provider_usage.codex.stale = true;

        let segments = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let codex = segments
            .iter()
            .find(|segment| segment.text.contains("CX "))
            .expect("codex segment");
        assert_eq!(codex.style.fg, Some(app.palette.overlay0));
        assert!(codex.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_offline_dot_appears_only_while_offline_and_never_elides() {
        let mut app = AppState::test_new();
        let online = status_segments(&app, &StatusMetrics::default(), &app.palette);
        assert!(
            !online.iter().any(|segment| segment.text.contains(DOT)),
            "an online machine gets no dot"
        );

        app.connectivity.observe(false);
        app.connectivity.observe(false);
        let offline = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let dot = offline
            .iter()
            .find(|segment| segment.text.trim() == DOT.to_string())
            .expect("offline dot");
        assert_eq!(dot.style.fg, Some(app.palette.red));
        assert!(dot.elide_rank.is_none(), "the reason must outlive the data");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_agent_dot_turns_red_and_carries_the_blocked_count() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("agents")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        {
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.state = AgentState::Working;
        }

        let working = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let dot = working
            .iter()
            .find(|segment| segment.text.contains(DOT))
            .expect("agent dot");
        assert_eq!(dot.text.trim(), format!("{DOT} 1"));
        assert_eq!(dot.style.fg, Some(app.palette.accent));

        app.terminals.get_mut(&terminal_id).expect("terminal").state = AgentState::Blocked;
        let blocked = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let dot = blocked
            .iter()
            .find(|segment| segment.text.contains(DOT))
            .expect("agent dot");
        assert_eq!(dot.text.trim(), format!("{DOT} 1/1"));
        assert_eq!(dot.style.fg, Some(app.palette.red));
    }

    #[test]
    fn disk_appears_only_when_it_is_news_and_holds_through_the_hysteresis_gap() {
        use crate::platform::status_metrics::disk_segment_visible;

        assert!(!disk_segment_visible(Some(41), false));
        assert!(disk_segment_visible(Some(80), false));
        // Between 78 and 80 the segment keeps whatever state it had, so a
        // volume hovering at the threshold does not flicker every sample.
        assert!(disk_segment_visible(Some(79), true));
        assert!(!disk_segment_visible(Some(79), false));
        assert!(!disk_segment_visible(Some(77), true));
        assert!(!disk_segment_visible(None, true));

        let mut app = AppState::test_new();
        app.status_disk_visible = true;
        let metrics = StatusMetrics {
            disk_percent: Some(97),
            ..crate::platform::status_metrics::status_metrics_fixture()
        };
        let rendered = status_segments(&app, &metrics, &app.palette);
        let disk = rendered
            .iter()
            .find(|segment| segment.text.contains("DSK"))
            .expect("disk segment");
        assert!(disk.text.contains('\u{2588}'), "{}", disk.text);
        assert_eq!(disk.style.fg, Some(app.palette.red));
    }

    #[test]
    fn the_detail_button_mirrors_the_expanded_state() {
        let mut app = AppState::test_new();
        let collapsed = status_buttons(&app, Rect::new(0, 0, 120, 1));
        let toggle = collapsed.last().expect("toggle button");
        assert_eq!(toggle.action, StatusButtonAction::StatusDetail);
        assert_eq!(toggle.label.trim(), "\u{25b8}");
        assert!(!toggle.active);

        app.status_bar_expanded = true;
        let expanded = status_buttons(&app, Rect::new(0, 0, 120, 1));
        let toggle = expanded.last().expect("toggle button");
        assert_eq!(toggle.label.trim(), "\u{25be}");
        assert!(toggle.active);
    }

    /// End-to-end evidence, run by hand: every provider, metric, and account
    /// label comes from this machine's real sources rather than a fixture.
    /// Ignored in CI, where none of those sources exist.
    #[test]
    #[ignore = "reads this machine's real usage caches"]
    fn live_smoke_renders_both_modes_from_real_sources() {
        let mut app = AppState::test_new();
        app.provider_usage = crate::provider_usage::collect(
            crate::provider_usage::now_unix(),
            std::time::Instant::now(),
        );
        app.status_now_unix = crate::provider_usage::now_unix();
        app.status_disk_visible = true;

        // CPU is a delta, so it needs two samples a sampling interval apart.
        let mut sampler = crate::platform::status_metrics::StatusMetricSampler::new();
        let _ = crate::platform::sample_status_metrics(&mut sampler);
        std::thread::sleep(crate::platform::status_metrics::STATUS_METRIC_REFRESH_INTERVAL);
        let metrics = crate::platform::sample_status_metrics(&mut sampler);

        for expanded in [false, true] {
            app.status_bar_expanded = expanded;
            let row: String = status_segments(&app, &metrics, &app.palette)
                .iter()
                .map(|segment| segment.text.as_str())
                .collect();
            println!("{} |{row}", if expanded { "expanded" } else { "compact " });
        }
    }

    #[test]
    fn quick_buttons_sit_at_the_left_edge_in_a_stable_order() {
        let app = AppState::test_new();
        let buttons = status_buttons(&app, Rect::new(0, 0, 120, 1));

        let actions: Vec<StatusButtonAction> = buttons.iter().map(|button| button.action).collect();
        assert_eq!(
            actions,
            vec![
                StatusButtonAction::BlockedFilter,
                StatusButtonAction::Dock,
                StatusButtonAction::StatusDetail
            ]
        );
        assert_eq!(buttons[0].rect.x, 0);
        // Adjacent, never overlapping: a click can only ever hit one button.
        for pair in buttons.windows(2) {
            assert_eq!(pair[0].rect.x + pair[0].rect.width, pair[1].rect.x);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_blocked_button_carries_the_blocked_count_and_lights_up_when_filtered() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("quick")];
        app.active = Some(0);
        app.ensure_test_terminals();

        let idle = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(idle[0].label.trim(), "blocked");
        assert!(!idle[0].active, "the filter starts disabled");

        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .state = AgentState::Blocked;

        let blocked = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(blocked[0].label.trim(), "blocked 1");
        assert!(!blocked[0].active);
        app.blocked_filter = true;
        assert!(status_buttons(&app, Rect::new(0, 0, 120, 1))[0].active);
    }

    #[test]
    fn the_dock_button_lights_up_only_while_the_dock_is_showing() {
        let mut app = AppState::test_new();
        app.dock_collapsed = true;
        assert!(!status_buttons(&app, Rect::new(0, 0, 120, 1))[1].active);
        app.dock_collapsed = false;
        assert!(status_buttons(&app, Rect::new(0, 0, 120, 1))[1].active);
    }

    #[test]
    fn buttons_yield_to_the_metrics_rather_than_overlapping_them() {
        let app = AppState::test_new();
        let narrow = Rect::new(0, 0, minimum_required_status_width(&app) as u16 + 2, 1);

        let buttons = status_buttons(&app, narrow);

        // Whole buttons drop; none is truncated into an unreadable stub.
        assert!(buttons.len() < 2, "buttons: {buttons:?}");
        for button in &buttons {
            assert!(button.rect.width as usize == display_width(&button.label));
        }
    }

    #[test]
    fn a_zero_width_status_bar_registers_no_buttons() {
        let app = AppState::test_new();
        assert!(status_buttons(&app, Rect::new(0, 0, 0, 0)).is_empty());
    }
}
