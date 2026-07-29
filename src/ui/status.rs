use std::path::{Path, PathBuf};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Mode, Palette, ToastKind, ToastNotification},
    app::AppState,
    config::{ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    platform::status_metrics::{NetKind, StatusMetrics},
};

/// Full-width top status row — native parity with the user's tmux powerline.
///
/// Left:  [prefix]  session:ws.pane · host ·  user · cwd ·  branch
/// Right: 󰛳 lan 󰌘 ts  wan ·  [] ↓/↑ ·  mem ·  cpu · battery ·  date · time
///
/// Layout: spans the full client width above the sidebar. On narrow widths,
/// right-side tail segments drop first, then non-essential left segments.
pub(crate) fn render_status_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    let bg = Style::default().bg(p.panel_bg);
    frame.render_widget(Paragraph::new("").style(bg), area);

    let unavailable = StatusMetrics {
        hostname: "--".into(),
        username: "--".into(),
        date: "----/--/--".into(),
        time: "--:--".into(),
        ..StatusMetrics::default()
    };
    let metrics = app
        .status_metrics
        .as_ref()
        .map(|snapshot| &snapshot.metrics)
        .unwrap_or(&unavailable);
    let prefix_active = app.mode == Mode::Prefix;

    let (left, right) = fitted_segments(
        left_segments(app, metrics, prefix_active, p),
        right_segments(metrics, p),
        area.width as usize,
    );

    let mut spans: Vec<Span> = Vec::new();
    for seg in &left {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    let used_left = segment_width(&left);
    let used_right = segment_width(&right);
    let pad = (area.width as usize)
        .saturating_sub(used_left)
        .saturating_sub(used_right);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), bg));
    }
    for seg in &right {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
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

fn fitted_segments(
    mut left: Vec<Segment>,
    mut right: Vec<Segment>,
    width: usize,
) -> (Vec<Segment>, Vec<Segment>) {
    while segment_width(&left) + segment_width(&right) > width {
        let candidate =
            left.iter()
                .enumerate()
                .filter_map(|(index, segment)| segment.elide_rank.map(|rank| (rank, true, index)))
                .chain(right.iter().enumerate().filter_map(|(index, segment)| {
                    segment.elide_rank.map(|rank| (rank, false, index))
                }))
                .min_by_key(|(rank, _, _)| *rank);
        let Some((_, is_left, index)) = candidate else {
            break;
        };
        if is_left {
            left.remove(index);
        } else {
            right.remove(index);
        }
    }
    (left, right)
}

fn left_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    prefix_active: bool,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();

    if prefix_active {
        // Match tmux `#{?client_prefix,... § ...}`.
        out.push(Segment {
            text: " § ".into(),
            style: Style::default()
                .fg(p.panel_bg)
                .bg(p.yellow)
                .add_modifier(Modifier::BOLD),
            preserve_bg: true,
            elide_rank: None,
        });
    }

    let (ws_label, tab_label, pane_label, cwd, branch) = focused_identity(app);

    // Powerline session_icon: " #S" + dimmed ":#I.#P".
    out.push(Segment {
        text: format!("  {}", app.status_session_name),
        style: Style::default().fg(p.blue),
        preserve_bg: false,
        elide_rank: None,
    });
    out.push(Segment {
        text: format!(":{ws_label}.{tab_label}.{pane_label} "),
        style: Style::default().fg(p.overlay0),
        preserve_bg: false,
        elide_rank: None,
    });

    // hostname_ssh: icon only when remote (SSH/mosh).
    let host_text = if metrics.remote_session {
        format!(" 󰣀 {} ", metrics.hostname)
    } else {
        format!(" {} ", metrics.hostname)
    };
    out.push(Segment {
        text: host_text,
        style: Style::default().fg(p.green),
        preserve_bg: false,
        elide_rank: Some(9),
    });

    out.push(Segment {
        text: format!("  {} ", metrics.username),
        style: Style::default().fg(p.teal),
        preserve_bg: false,
        elide_rank: Some(8),
    });

    if let Some(cwd) = cwd {
        let display = shorten_path(&cwd, app.status_home_dir.as_deref(), 40);
        out.push(Segment {
            text: format!(" {display} "),
            style: Style::default().fg(p.mauve),
            preserve_bg: false,
            elide_rank: Some(7),
        });
    }

    if let Some(branch) = branch {
        let branch = shorten_branch(&branch, 24);
        out.push(Segment {
            text: format!("  {branch} "),
            style: Style::default().fg(p.yellow),
            preserve_bg: false,
            elide_rank: Some(6),
        });
    }

    out
}

fn right_segments(
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();

    // network_ips: "󰛳 LAN 󰌘 TS  WAN"
    let mut ip_parts: Vec<String> = Vec::new();
    if let Some(ip) = &metrics.local_ip {
        ip_parts.push(format!("󰛳 {ip}"));
    }
    if let Some(ip) = &metrics.tailscale_ip {
        ip_parts.push(format!("󰌘 {ip}"));
    }
    if let Some(ip) = &metrics.public_ip {
        ip_parts.push(format!(" {ip}"));
    }
    if !ip_parts.is_empty() {
        out.push(Segment {
            text: format!(" {} ", ip_parts.join(" ")),
            style: Style::default().fg(p.teal),
            preserve_bg: false,
            elide_rank: Some(4),
        });
    }

    // bandwidth: wifi/eth glyph + optional VPN lock + ↓/↑ KiB/s
    if let (Some(down), Some(up)) = (metrics.net_down_kib, metrics.net_up_kib) {
        let kind_icon = match metrics.net_kind {
            NetKind::Ethernet => "󰈀",
            NetKind::Wifi | NetKind::Unknown => "",
        };
        let vpn = if metrics.vpn_active { " " } else { "" };
        out.push(Segment {
            text: format!(" {kind_icon}{vpn} ↓{down}K/s ↑{up}K/s "),
            style: Style::default().fg(p.green),
            preserve_bg: false,
            elide_rank: Some(5),
        });
    }

    let memory = match (metrics.mem_used_gib, metrics.mem_total_gib) {
        (Some(used), Some(total)) => format!(" MEM {used:.1}/{total:.1} GiB "),
        _ => " MEM --/-- GiB ".into(),
    };
    out.push(Segment {
        text: memory,
        style: Style::default().fg(p.yellow),
        preserve_bg: false,
        elide_rank: None,
    });

    out.push(Segment {
        text: metrics
            .cpu_percent
            .map(|cpu| format!(" CPU {cpu}% "))
            .unwrap_or_else(|| " CPU --% ".into()),
        style: Style::default().fg(p.red),
        preserve_bg: false,
        elide_rank: None,
    });

    if let Some(pct) = metrics.battery_percent {
        let icon = match metrics.battery_charging {
            Some(true) => "󰂄",
            _ => battery_icon(pct),
        };
        out.push(Segment {
            text: format!(" {icon} {pct}% "),
            style: Style::default().fg(p.blue),
            preserve_bg: false,
            elide_rank: Some(3),
        });
    }

    out.push(Segment {
        text: format!("  {} ", metrics.date),
        style: Style::default().fg(p.overlay0),
        preserve_bg: false,
        elide_rank: Some(2),
    });
    out.push(Segment {
        text: format!(" {} ", metrics.time),
        style: Style::default().fg(p.subtext0),
        preserve_bg: false,
        elide_rank: Some(1),
    });
    out
}

/// Nerd Font battery glyph by charge bucket.
fn battery_icon(pct: u8) -> &'static str {
    match pct {
        0..=10 => "󰁺",
        11..=20 => "󰁻",
        21..=30 => "󰁼",
        31..=40 => "󰁽",
        41..=50 => "󰁾",
        51..=60 => "󰁿",
        61..=70 => "󰂀",
        71..=80 => "󰂁",
        81..=90 => "󰂂",
        _ => "󰁹",
    }
}

fn focused_identity(app: &AppState) -> (String, String, String, Option<PathBuf>, Option<String>) {
    let Some(ws_idx) = app.active else {
        return ("1".into(), "1".into(), "1".into(), None, None);
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return ("1".into(), "1".into(), "1".into(), None, None);
    };
    let ws_label = (ws_idx + 1).to_string();
    let tab_idx = ws.active_tab_index();
    let tab_label = (tab_idx + 1).to_string();
    let pane_id = ws.focused_pane_id();
    // Prefer stable public pane numbers (tmux #P parity), not internal PaneId.
    let pane_label = pane_id
        .and_then(|id| ws.public_pane_number(id))
        .map(|n| n.to_string())
        .unwrap_or_else(|| "1".into());
    // Prefer live focused-pane terminal cwd; fall back to workspace identity.
    let cwd = pane_id
        .and_then(|pane_id| {
            ws.tabs
                .get(tab_idx)
                .and_then(|tab| tab.panes.get(&pane_id))
                .and_then(|pane| app.terminals.get(&pane.attached_terminal_id))
                .map(|terminal| terminal.cwd.clone())
        })
        .or_else(|| Some(ws.identity_cwd.clone()));
    let branch = ws.cached_git_branch.clone();
    (ws_label, tab_label, pane_label, cwd, branch)
}

fn shorten_path(path: &Path, home: Option<&Path>, max_width: usize) -> String {
    let raw = path.to_string_lossy();
    let display = if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            // Powerline `pwd` collapses $HOME to `~` (keeping the slash as `~/…`).
            let suffix = rest.to_string_lossy();
            if suffix.is_empty() {
                "~".into()
            } else {
                format!("~/{}", suffix.trim_start_matches(['/', '\\']))
            }
        } else {
            raw.into_owned()
        }
    } else {
        raw.into_owned()
    };
    if display_width(&display) <= max_width {
        return display;
    }
    // Left-truncate like powerline: keep the trailing path, prefix with `…/`.
    left_truncate_path(&display, max_width)
}

fn left_truncate_path(display: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    // Never shorter than the final path component if possible.
    let file = display.rsplit('/').next().unwrap_or(display);
    let file_w = display_width(file);
    if file_w >= max_width {
        return truncate_end(file, max_width);
    }
    let ellipsis = "…/";
    let ellipsis_w = display_width(ellipsis);
    if ellipsis_w + file_w >= max_width {
        return truncate_end(file, max_width);
    }
    let budget = max_width.saturating_sub(ellipsis_w);
    // Take a suffix of `display` that fits in budget, then force a clean `…/rest`.
    let mut width = 0usize;
    let mut start = display.len();
    for (idx, ch) in display.char_indices().rev() {
        let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_w > budget {
            break;
        }
        width += ch_w;
        start = idx;
    }
    let mut suffix = &display[start..];
    // Drop a partial leading component so we always start on a path boundary when possible.
    if let Some(slash) = suffix.find('/') {
        suffix = &suffix[slash + 1..];
    }
    if suffix.is_empty() {
        suffix = file;
    }
    format!("{ellipsis}{suffix}")
}

fn shorten_branch(branch: &str, max_width: usize) -> String {
    if display_width(branch) <= max_width {
        branch.to_string()
    } else {
        truncate_end(branch, max_width)
    }
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

pub(super) fn state_dot(state: AgentState, seen: bool, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
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
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
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
    fn state_dots_use_aligned_static_workspace_marks() {
        let palette = Palette::catppuccin();
        for (state, seen, symbol, color) in [
            (AgentState::Blocked, true, "●", palette.red),
            (AgentState::Working, true, "●", palette.yellow),
            (AgentState::Idle, false, "●", palette.teal),
            (AgentState::Idle, true, "○", palette.green),
            (AgentState::Unknown, true, "·", palette.overlay0),
        ] {
            let (actual_symbol, style) = state_dot(state, seen, &palette);
            assert_eq!(actual_symbol, symbol);
            assert_eq!(style.fg, Some(color));
        }
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
    fn shorten_path_uses_tilde_for_home() {
        // AC4: cwd parity uses immutable state captured before rendering.
        let home = "/tmp/herdr-status-home-fixture";
        let path = PathBuf::from(format!("{home}/Code/personal/home"));
        let display = shorten_path(&path, Some(Path::new(home)), 80);
        assert!(
            display.starts_with("~/") || display.starts_with("~\\"),
            "expected tilde-shortened path, got {display}"
        );
        assert!(display.contains("Code"));
    }

    #[test]
    fn required_metrics_survive_optional_segment_elision() {
        // AC3: MEM/CPU are required while optional right-tail segments elide by rank.
        let palette = Palette::catppuccin();
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let (left, right) = fitted_segments(
            vec![Segment {
                text: " session ".into(),
                style: Style::default(),
                preserve_bg: false,
                elide_rank: None,
            }],
            right_segments(&metrics, &palette),
            40,
        );
        let rendered = left
            .iter()
            .chain(&right)
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM 8.0/16.0 GiB"));
        assert!(rendered.contains("CPU 12%"));
        assert!(!rendered.contains(&metrics.date));
        assert!(!rendered.contains(&metrics.time));
    }

    #[test]
    fn status_metrics_fallback_format_is_explicit() {
        // AC3/AC5: unavailable snapshots render stable units without sampling in render.
        let palette = Palette::catppuccin();
        let segments = right_segments(&StatusMetrics::default(), &palette);
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM --/-- GiB"));
        assert!(rendered.contains("CPU --%"));
    }

    #[test]
    fn status_elision_drops_optional_tail_in_declared_order() {
        // AC3: time, date, and battery elide before network and identity context.
        let palette = Palette::catppuccin();
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let full = right_segments(&metrics, &palette);
        let full_width = segment_width(&full);
        let time_width = display_width(full.last().expect("time segment").text.as_str());
        let date_width = display_width(full[full.len() - 2].text.as_str());

        let (_, without_time) = fitted_segments(
            Vec::new(),
            right_segments(&metrics, &palette),
            full_width - time_width,
        );
        assert!(!without_time
            .iter()
            .any(|segment| segment.text.contains(&metrics.time)));
        assert!(without_time
            .iter()
            .any(|segment| segment.text.contains(&metrics.date)));

        let (_, without_date) = fitted_segments(
            Vec::new(),
            right_segments(&metrics, &palette),
            full_width - time_width - date_width,
        );
        assert!(!without_date
            .iter()
            .any(|segment| segment.text.contains(&metrics.date)));
        assert!(without_date
            .iter()
            .any(|segment| segment.text.contains("MEM ")));
        assert!(without_date
            .iter()
            .any(|segment| segment.text.contains("CPU ")));
    }

    #[test]
    fn status_identity_theme_network_battery_date_time_segments_match_state() {
        // AC4: all parity values and colors derive from AppState plus the active palette.
        let mut app = AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/home/test/work/status");
        workspace.cached_git_branch = Some("feature/native-status".into());
        workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(1);
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.status_session_name = "session".into();

        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let (workspace_label, tab_label, pane_label, _, _) = focused_identity(&app);
        let left = left_segments(&app, &metrics, false, &app.palette);
        let right = right_segments(&metrics, &app.palette);
        let left_text = left
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        let right_text = right
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();

        assert!(left_text.contains(&format!(
            "session:{workspace_label}.{tab_label}.{pane_label}"
        )));
        assert!(left_text.contains("testhost"));
        assert!(left_text.contains("testuser"));
        assert!(left_text.contains("~/work/status"));
        assert!(left_text.contains("feature/native-status"));
        assert!(right_text.contains("10.0.0.2"));
        assert!(right_text.contains("100.64.0.1"));
        assert!(right_text.contains("↓120K/s ↑34K/s"));
        assert!(right_text.contains("88%"));
        assert!(right_text.contains("2026-01-02"));
        assert!(right_text.contains("03:04"));
        assert_eq!(left[0].style.fg, Some(app.palette.blue));
        assert_eq!(
            right
                .iter()
                .find(|segment| segment.text.contains("MEM "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.yellow)
        );
        assert_eq!(
            right
                .iter()
                .find(|segment| segment.text.contains("CPU "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.red)
        );
    }
}
