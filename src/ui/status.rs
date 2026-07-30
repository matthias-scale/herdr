use std::path::PathBuf;

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
    app::state::{CopyFeedback, Palette, ToastKind, ToastNotification},
    app::AppState,
    config::{ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    platform::status_metrics::StatusMetrics,
};

/// Full-width, right-aligned top status row.
///
/// Contents, left to right: branch · device · CPU · memory. The row before the
/// first surviving segment is intentionally blank.
///
/// Layout: spans the full client width above the sidebar and pads before the
/// first surviving segment. On narrow widths, branch then device elide in that
/// order; CPU and memory remain required.
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
    1 + display_width(" CPU 100% ") + display_width(" MEM 9999.9/9999.9 GiB ")
}

fn status_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();

    let (_, branch) = focused_context(app);

    if let Some(branch) = branch {
        let branch = shorten_branch(&branch, 20);
        out.push(Segment {
            text: format!("  {branch} "),
            style: Style::default().fg(p.yellow),
            preserve_bg: false,
            elide_rank: Some(1),
        });
    }

    out.push(Segment {
        text: format!(" {} ", metrics.hostname),
        style: Style::default().fg(p.green),
        preserve_bg: false,
        elide_rank: Some(2),
    });

    out.push(Segment {
        text: metrics
            .cpu_percent
            .filter(|cpu| *cpu <= 100)
            .map(|cpu| format!(" CPU {cpu:>3}% "))
            .unwrap_or_else(|| " CPU  --% ".into()),
        style: Style::default().fg(p.red),
        preserve_bg: false,
        elide_rank: None,
    });

    let memory = match (metrics.mem_used_gib, metrics.mem_total_gib) {
        (Some(used), Some(total))
            if used.is_finite()
                && total.is_finite()
                && used >= 0.0
                && total >= used
                && total <= 9_999.9 =>
        {
            format!(" MEM {used:>6.1}/{total:>6.1} GiB ")
        }
        _ => " MEM     --/    -- GiB ".into(),
    };
    out.push(Segment {
        text: memory,
        style: Style::default().fg(p.yellow),
        preserve_bg: false,
        elide_rank: None,
    });
    out
}

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
        assert!(rendered.contains("MEM    8.0/  16.0 GiB"));
        assert!(rendered.contains("CPU  12%"));
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
        assert!(rendered.contains("CPU  12%"), "{rendered}");
        assert!(rendered.contains("MEM    8.0/  16.0 GiB"), "{rendered}");
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
        assert!(rendered.contains("CPU 100%"), "{rendered}");
        assert!(rendered.contains("MEM 9999.9/9999.9 GiB"), "{rendered}");
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
        assert!(rendered.contains("MEM    8.0/  16.0 GiB"), "{rendered}");
        assert!(rendered.contains("CPU  12%"), "{rendered}");
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
        assert!(rendered.contains("MEM     --/    -- GiB"));
        assert!(rendered.contains("CPU  --%"));
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
                hostname: "testhost".into(),
            },
            &app.palette,
        );
        let metrics = StatusMetrics {
            cpu_percent: Some(101),
            mem_used_gib: Some(10_000.0),
            mem_total_gib: Some(10_000.0),
            hostname: "testhost".into(),
        };
        let rendered = status_segments(&app, &metrics, &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("CPU  --%"), "{rendered}");
        assert!(rendered.contains("MEM     --/    -- GiB"), "{rendered}");
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
    fn status_elision_drops_branch_then_device() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.status_focused_cwd = Some(PathBuf::from("/home/test/work/status"));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some("feature/native-status".into());
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let full = status_segments(&app, &metrics, &app.palette);

        assert_eq!(
            full.iter()
                .filter_map(|segment| segment.elide_rank)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

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
        assert!(!rendered.contains("feature/native-status"), "{rendered}");
        assert!(!rendered.contains("testhost"), "{rendered}");
        assert!(!rendered.contains("Herdr v"), "{rendered}");
        assert!(rendered.contains("CPU  12%"), "{rendered}");
        assert!(rendered.contains("MEM    8.0/  16.0 GiB"), "{rendered}");
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

        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let segments = status_segments(&app, &metrics, &app.palette);
        assert_eq!(segments.len(), 4, "only the four visible contract fields");
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        let ordered = [
            "feature/native-stat",
            "testhost",
            "CPU  12%",
            "MEM    8.0/  16.0 GiB",
        ];
        let mut previous = 0;
        for value in ordered {
            let index = rendered
                .find(value)
                .unwrap_or_else(|| panic!("{value}: {rendered}"));
            assert!(index >= previous, "{value} is out of order: {rendered}");
            previous = index;
        }
        for removed in [
            "testuser",
            "10.0.0.2",
            "100.64.0.1",
            "203.0.113.10",
            "↓",
            "↑",
            "session:",
            "workspace:",
            "tab:",
            "pane:",
            "~/work/status",
            "Herdr v",
            "88%",
            "2026-01-02",
            "03:04",
        ] {
            assert!(!rendered.contains(removed), "{removed}: {rendered}");
        }
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("feature/native-stat"))
                .unwrap()
                .style
                .fg,
            Some(app.palette.yellow)
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
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("MEM "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.yellow)
        );
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("CPU "))
                .unwrap()
                .style
                .fg,
            Some(app.palette.red)
        );
    }
}
