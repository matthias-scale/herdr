use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::super::text::truncate_end;
use crate::{
    app::AppState,
    work_projection::{compact_elapsed, DockHomeProjection, DockHomeRow},
};

const HEADER_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 4;
const ROW_HEIGHT: u16 = 2;

fn row_rects(projection: &DockHomeProjection, area: Rect) -> Vec<Rect> {
    if projection.rows.is_empty() || area.width == 0 {
        return Vec::new();
    }
    let available = area
        .height
        .saturating_sub(HEADER_ROWS.saturating_add(FOOTER_ROWS));
    let visible = projection
        .rows
        .len()
        .min(usize::from(available / ROW_HEIGHT));
    (0..visible)
        .map(|index| {
            Rect::new(
                area.x,
                area.y
                    .saturating_add(HEADER_ROWS)
                    .saturating_add((index as u16).saturating_mul(ROW_HEIGHT)),
                area.width,
                ROW_HEIGHT,
            )
        })
        .collect()
}

/// One two-line hit target per row the home body can actually draw.
pub(crate) fn row_hit_areas(projection: &DockHomeProjection, area: Rect) -> Vec<Rect> {
    row_rects(projection, area)
}

fn owner_cell(row: &DockHomeRow) -> String {
    let mut owner = row.owner.clone().unwrap_or_else(|| "—".to_string());
    if row.extra_panes > 0 {
        owner.push_str(&format!("+{}", row.extra_panes));
    }
    owner
}

fn fixed_cell(value: &str, width: usize) -> String {
    let cell = truncate_end(value, width);
    let padding = width.saturating_sub(super::super::text::display_width(&cell));
    format!("{cell}{}", " ".repeat(padding))
}

fn metadata_widths(width: usize) -> (usize, usize, usize, usize, usize) {
    const CELL_MAX: usize = 12;
    let indent: usize = if width >= 38 { 5 } else { 3 };
    let available = width.saturating_sub(indent.saturating_add(3));
    let review = available.min(2);
    let mut remaining = available.saturating_sub(review);
    let mut owner = remaining.min(3);
    remaining = remaining.saturating_sub(owner);
    let mut ticket = remaining.min(3);
    remaining = remaining.saturating_sub(ticket);
    let age = remaining.min(4);
    remaining = remaining.saturating_sub(age);
    while remaining > 0 && (owner < CELL_MAX || ticket < CELL_MAX) {
        if owner < CELL_MAX {
            owner += 1;
            remaining -= 1;
        }
        if remaining > 0 && ticket < CELL_MAX {
            ticket += 1;
            remaining -= 1;
        }
    }
    (indent, review, owner, ticket, age)
}

fn row_lines(row: &DockHomeRow, width: usize) -> [String; 2] {
    let prefix = format!("{} #{} ", row.glyph, row.number);
    let title_width = width.saturating_sub(super::super::text::display_width(&prefix));
    let title = truncate_end(&row.title, title_width);
    let review = if row.fetched {
        row.review.as_str()
    } else {
        "?"
    };
    let (indent_width, review_width, owner_width, ticket_width, age_width) = metadata_widths(width);
    let metadata = format!(
        "{}{}{}{}{}{}{}{}",
        " ".repeat(indent_width),
        fixed_cell(review, review_width),
        // Cells are already padded to fixed widths, so a plain space aligns the
        // columns. A `·` here collides with a cell that fills its width.
        " ",
        fixed_cell(&owner_cell(row), owner_width),
        " ",
        fixed_cell(&row.ticket, ticket_width),
        " ",
        fixed_cell(&row.age, age_width),
    );
    [
        truncate_end(&format!("{prefix}{title}"), width),
        truncate_end(&metadata, width),
    ]
}

fn observed_line(projection: &DockHomeProjection, now: SystemTime) -> String {
    if let Some(reason) = projection.unavailable.as_deref() {
        return format!("unavailable: {reason}");
    }
    let Some(observed_at) = projection.observed_at else {
        // No observation at all. Name the cause instead of reporting one
        // indistinguishable `unknown` for a disabled index and a pending fetch.
        return if projection.index_enabled {
            "work index: no observation yet".to_string()
        } else {
            "work index off — set work_index.enabled".to_string()
        };
    };
    match now.duration_since(observed_at) {
        Ok(elapsed) => format!("observed {} ago", compact_elapsed(elapsed)),
        Err(_) => "observed unknown".to_string(),
    }
}

fn render_line(frame: &mut Frame, area: Rect, y: u16, text: String, style: Style) {
    if y >= area.bottom() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_end(&text, usize::from(area.width)),
            style,
        ))),
        Rect::new(area.x, y, area.width, 1),
    );
}

fn render_footer(
    app: &AppState,
    projection: &DockHomeProjection,
    frame: &mut Frame,
    area: Rect,
    start_y: u16,
) {
    let style = Style::default().fg(app.palette.overlay0);
    render_line(
        frame,
        area,
        start_y,
        "─".repeat(usize::from(area.width)),
        Style::default().fg(app.palette.surface_dim),
    );
    let prs = projection
        .unbound_prs
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_string());
    let tickets = projection
        .unbound_tickets
        .map(|count| count.to_string())
        .unwrap_or_else(|| "—".to_string());
    render_line(
        frame,
        area,
        start_y.saturating_add(1),
        format!("unbound {prs} prs · {tickets} tickets"),
        style,
    );
    render_line(
        frame,
        area,
        start_y.saturating_add(2),
        observed_line(projection, SystemTime::now()),
        style,
    );
    render_line(
        frame,
        area,
        start_y.saturating_add(3),
        "age=pr open · rv=?/—/D/RR/✓/✗".to_string(),
        style,
    );
}

pub(super) fn render_home(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let projection = app.dock_home_projection();
    render_line(
        frame,
        area,
        area.y,
        format!("review · {} bound", projection.rows.len()),
        Style::default()
            .fg(app.palette.overlay1)
            .add_modifier(Modifier::BOLD),
    );

    if projection.rows.is_empty() {
        render_line(
            frame,
            area,
            area.y.saturating_add(1),
            "no pr-bound panes".to_string(),
            Style::default().fg(app.palette.subtext0),
        );
        render_line(
            frame,
            area,
            area.y.saturating_add(2),
            "bind: herdr tab create --pr <url> --role review".to_string(),
            Style::default().fg(app.palette.overlay0),
        );
        render_footer(app, &projection, frame, area, area.y.saturating_add(3));
        return;
    }

    let selected = app.dock_home_selected_index(&projection);
    let rects = row_rects(&projection, area);
    for (index, (row, rect)) in projection.rows.iter().zip(rects.iter()).enumerate() {
        let is_selected = selected == Some(index);
        let first_style = if is_selected {
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.subtext0)
        };
        let second_style = if is_selected {
            first_style
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        let [first, second] = row_lines(row, usize::from(area.width));
        render_line(frame, *rect, rect.y, first, first_style);
        render_line(frame, *rect, rect.y.saturating_add(1), second, second_style);
    }
    let footer_y = rects
        .last()
        .map(|rect| rect.bottom())
        .unwrap_or_else(|| area.y.saturating_add(HEADER_ROWS));
    render_footer(app, &projection, frame, area, footer_y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use std::time::Duration;

    fn bound_app(fetched: bool) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("review")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .cloned()
            .expect("terminal");
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.detected_agent = Some(crate::detect::Agent::Codex);
        terminal.state = crate::detect::AgentState::Working;
        terminal
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/herdrdev/herdr/pull/125".into()]),
                ticket_ids: Some(vec!["MAT-124".into()]),
                work_title: Some("work view pr projection".into()),
                role: Some(crate::work_context::PaneWorkRole::Review),
                active_owner: Some(true),
                ..Default::default()
            })
            .expect("work context");
        if fetched {
            app.work_index_snapshot = Some(crate::work_index::Snapshot {
                items: vec![crate::work_index::WorkItem {
                    repo: "herdrdev/herdr".into(),
                    pr_number: Some(125),
                    pr_url: Some("https://github.com/herdrdev/herdr/pull/125".into()),
                    pr_title: Some("work view pr projection".into()),
                    pr_state: Some("open".into()),
                    draft: false,
                    review_decision: Some("REVIEW_REQUIRED".into()),
                    created_at: SystemTime::now().checked_sub(Duration::from_secs(241)),
                    ticket_ids: vec!["MAT-125".into()],
                    ticket_title: None,
                    ticket_state: None,
                    branch: None,
                    preview_urls: Vec::new(),
                    panes: vec![crate::work_index::WorkItemPane {
                        pane_id: pane_id.raw().to_string(),
                        agent_label: Some("codex".into()),
                        workspace_id: "ws".into(),
                        tab_id: "tab".into(),
                        role: None,
                        active_owner: true,
                        agent_status: crate::api::schema::AgentStatus::Working,
                    }],
                    source: crate::work_index::WorkItemSource::default(),
                }],
                unavailable: None,
                observed_at: SystemTime::now(),
            });
        }
        app
    }

    fn render(app: &AppState, area: Rect) -> Terminal<TestBackend> {
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| render_home(app, frame, area))
            .expect("render home");
        terminal
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn line_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn renders_the_empty_state_and_bind_hint() {
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("no pr-bound panes"), "{text:?}");
        assert!(text.contains("bind: herdr tab create --pr"), "{text:?}");
    }

    #[test]
    fn renders_one_bound_row_with_review_and_owner() {
        let terminal = render(&bound_app(true), Rect::new(0, 0, 60, 10));
        let first = line_text(&terminal, 1);
        let second = line_text(&terminal, 2);
        assert!(
            first.contains("● #125 work view pr projection"),
            "{first:?}"
        );
        let review = second.find("RR").expect("review cell");
        let owner = second.find("codex").expect("agent cell");
        let ticket = second.find("MAT-125").expect("ticket cell");
        let age = second.find("4m").expect("age cell");
        assert!(
            review < owner && owner < ticket && ticket < age,
            "{second:?}"
        );
    }

    #[test]
    fn unfetched_binding_renders_question_mark_review() {
        let terminal = render(&bound_app(false), Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("?  codex"), "{text:?}");
    }

    #[test]
    fn unavailable_reason_replaces_observed_footer() {
        let mut app = AppState::test_new();
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            unavailable: Some("github timed out".into()),
            observed_at: SystemTime::now(),
        });
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("unavailable: github timed out"), "{text:?}");
        assert!(!text.contains("observed"), "{text:?}");
    }

    #[test]
    fn footer_names_pr_open_age_and_review_codes() {
        let body_width = crate::ui::DOCK_DEFAULT_WIDTH.saturating_sub(2);
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, body_width, 10));
        let legend = line_text(&terminal, 6);
        assert_eq!(
            legend.trim_end(),
            "age=pr open · rv=?/—/D/RR/✓/✗",
            "{legend:?}"
        );
    }

    #[test]
    fn future_observation_time_renders_unknown_instead_of_guessing_zero() {
        let projection = DockHomeProjection {
            rows: Vec::new(),
            unbound_prs: None,
            unbound_tickets: None,
            observed_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(60)),
            unavailable: None,
            index_enabled: true,
        };

        assert_eq!(
            observed_line(&projection, SystemTime::UNIX_EPOCH),
            "observed unknown"
        );
    }

    #[test]
    fn selected_row_uses_bold_foreground_without_a_selection_background() {
        let app = bound_app(true);
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        let cell = &terminal.backend().buffer()[(0, 1)];
        assert_eq!(cell.fg, app.palette.text);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(cell.bg, Color::Reset);
    }

    #[test]
    fn row_hit_areas_match_every_rendered_two_line_row() {
        let app = bound_app(true);
        let projection = app.dock_home_projection();
        let areas = row_hit_areas(&projection, Rect::new(4, 7, 30, 10));
        assert_eq!(areas.len(), projection.rows.len());
        assert_eq!(areas, vec![Rect::new(4, 8, 30, 2)]);
    }

    #[test]
    fn minimum_dock_width_truncates_both_row_lines_without_wrapping() {
        let app = bound_app(true);
        // The dock's divider and handle consume two columns at DOCK_MIN_WIDTH.
        let body_width = crate::ui::DOCK_MIN_WIDTH.saturating_sub(2);
        let terminal = render(&app, Rect::new(0, 0, body_width, 10));
        let first = line_text(&terminal, 1);
        let second = line_text(&terminal, 2);
        let footer = line_text(&terminal, 3);
        let legend = line_text(&terminal, 6);

        assert_eq!(
            super::super::super::text::display_width(&first),
            usize::from(body_width)
        );
        assert_eq!(
            super::super::super::text::display_width(&second),
            usize::from(body_width)
        );
        assert!(first.ends_with('…'), "first line: {first:?}");
        assert_eq!(second, "   RR co… MA… 4m", "second line: {second:?}");
        assert_eq!(footer, "─".repeat(usize::from(body_width)));
        assert!(legend.starts_with("age=pr open"), "legend: {legend:?}");
    }

    #[test]
    fn metadata_columns_never_shrink_as_the_dock_grows() {
        let mut previous =
            metadata_widths(usize::from(crate::ui::DOCK_MIN_WIDTH.saturating_sub(2)));
        for width in usize::from(crate::ui::DOCK_MIN_WIDTH.saturating_sub(1))
            ..=usize::from(crate::ui::DOCK_MAX_WIDTH.saturating_sub(2))
        {
            let current = metadata_widths(width);
            assert!(current.1 >= previous.1, "review shrank at {width}");
            assert!(current.2 >= previous.2, "agent shrank at {width}");
            assert!(current.3 >= previous.3, "ticket shrank at {width}");
            assert!(current.4 >= previous.4, "age shrank at {width}");
            previous = current;
        }
    }
}
