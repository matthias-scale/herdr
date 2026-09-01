use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::super::text::{display_width, middle_elide, truncate_end};
use crate::{
    app::{state::WorkItemKey, AppState},
    work_index::{WorkItem, WorkItemDetail},
    work_projection::{compact_elapsed, DockHomeProjection, DockHomeRow},
};

const TAB_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 4;
const BODY_LINE_LIMIT: usize = 6;
const MIN_LEGIBLE_TAB_WIDTH: u16 = 3;

fn tab_label(row: &DockHomeRow) -> String {
    format!("{} #{}", row.glyph, row.number)
}

/// The PR strip uses the dock tab bar's natural-width/equal-share allocator.
/// If equal shares would lose the state glyph or PR-number suffix, it shows a
/// selection-following window instead; hidden tabs remain reachable by keys.
pub(crate) fn tab_layouts(
    app: &AppState,
    projection: &DockHomeProjection,
    area: Rect,
) -> Vec<(usize, Rect)> {
    if projection.rows.is_empty() {
        return Vec::new();
    }
    let tab_bar = Rect::new(area.x, area.y, area.width, area.height.min(TAB_ROWS));
    let capacity = usize::from((area.width / MIN_LEGIBLE_TAB_WIDTH).max(1));
    let visible_count = projection.rows.len().min(capacity);
    let selected = app.dock_home_selected_index(projection).unwrap_or(0);
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(projection.rows.len().saturating_sub(visible_count));
    let visible = &projection.rows[start..start.saturating_add(visible_count)];
    let widths = visible
        .iter()
        .map(|row| {
            u16::try_from(display_width(&tab_label(row)).saturating_add(1)).unwrap_or(u16::MAX)
        })
        .collect::<Vec<_>>();
    crate::ui::horizontal_tab_hit_areas(tab_bar, &widths)
        .into_iter()
        .enumerate()
        .map(|(offset, area)| (start.saturating_add(offset), area))
        .collect()
}

fn work_item_for_key<'a>(app: &'a AppState, key: &WorkItemKey) -> Option<&'a WorkItem> {
    app.work_index_snapshot.as_ref()?.items.iter().find(|item| {
        (key.pr_number.is_some()
            && item.pr_number == key.pr_number
            && item.repo.eq_ignore_ascii_case(&key.repo))
            || (key.pr_url.is_some() && item.pr_url == key.pr_url)
    })
}

fn missing(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("—")
}

fn elapsed_from(timestamp: Option<SystemTime>, now: SystemTime) -> String {
    timestamp
        .and_then(|timestamp| now.duration_since(timestamp).ok())
        .map(compact_elapsed)
        .unwrap_or_else(|| "—".to_string())
}

fn status_line(detail: &WorkItemDetail) -> String {
    let mut values = Vec::new();
    if detail.is_draft == Some(true) {
        values.push("draft".to_string());
    }
    if let Some(review) = detail
        .review_decision
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        values.push(review.to_ascii_lowercase().replace('_', " "));
    }
    values.extend(
        detail
            .labels
            .iter()
            .filter(|label| !label.is_empty())
            .cloned(),
    );
    if values.is_empty() {
        "—".to_string()
    } else {
        values.join(" · ")
    }
}

fn strip_html_comments(text: &str) -> String {
    let mut remaining = text;
    let mut output = String::new();
    while let Some(start) = remaining.find("<!--") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 4..];
        let Some(end) = after_start.find("-->") else {
            return output;
        };
        remaining = &after_start[end + 3..];
    }
    output.push_str(remaining);
    output
}

fn readable_markdown_line(line: &str) -> &str {
    let trimmed = line.trim();
    let heading = trimmed.trim_start_matches('#').trim_start();
    if heading.len() != trimmed.len() {
        return heading;
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(value) = trimmed.strip_prefix(marker) {
            return value;
        }
    }
    trimmed
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in normalized.chars() {
        let ch_width = display_width(&ch.to_string());
        if current_width.saturating_add(ch_width) > width && !current.is_empty() {
            lines.push(current);
            current = String::new();
            current_width = 0;
        }
        if current.is_empty() && ch == ' ' {
            continue;
        }
        current.push(ch);
        current_width = current_width.saturating_add(ch_width);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn body_lines(body: Option<&str>, width: usize) -> Vec<String> {
    let Some(body) = body.filter(|body| !body.trim().is_empty()) else {
        return vec!["—".to_string()];
    };
    let cleaned = strip_html_comments(body);
    let mut lines = Vec::new();
    for source_line in cleaned.lines() {
        let readable = readable_markdown_line(source_line);
        if readable.is_empty() {
            if lines.last().is_some_and(|line: &String| !line.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.extend(wrap_line(readable, width));
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return vec!["—".to_string()];
    }
    if lines.len() > BODY_LINE_LIMIT {
        lines.truncate(BODY_LINE_LIMIT);
        if let Some(last) = lines.last_mut() {
            let prefix = truncate_end(last, width.saturating_sub(2));
            *last = format!("{prefix} …");
        }
    }
    lines
}

fn loaded_detail_lines(
    app: &AppState,
    row: &DockHomeRow,
    detail: &WorkItemDetail,
    width: usize,
) -> Vec<String> {
    if let Some(reason) = detail.unavailable.as_deref() {
        return vec![format!("unavailable: {reason}")];
    }
    let item = work_item_for_key(app, &row.key);
    let now = SystemTime::now();
    let number = detail
        .number
        .map(|number| number.to_string())
        .unwrap_or_else(|| "—".to_string());
    let ticket = if row.ticket_ids.is_empty() {
        "—".to_string()
    } else {
        row.ticket_ids.join(", ")
    };
    let author = missing(detail.author.as_deref());
    let opened = elapsed_from(detail.created_at, now);
    let updated = elapsed_from(detail.updated_at, now);
    let preview = row.preview_urls.first().map(String::as_str).unwrap_or("—");
    let pr_url = detail
        .url
        .as_deref()
        .or(row.key.pr_url.as_deref())
        .unwrap_or("—");
    let review_threads = detail
        .unresolved_review_threads
        .map(|count| format!("{count} unresolved"))
        .unwrap_or_else(|| "unknown".to_string());
    let checks = detail
        .checks
        .as_ref()
        .map(|checks| format!("{} failing of {}", checks.failing, checks.total))
        .unwrap_or_else(|| "—".to_string());
    let mut lines = vec![
        format!("#{number}  overview"),
        status_line(detail),
        missing(detail.title.as_deref()).to_string(),
        format!("{ticket} · {author} · opened {opened} · updated {updated}"),
        missing(detail.head_ref_name.as_deref()).to_string(),
        format!("  → {}", missing(detail.base_ref_name.as_deref())),
        String::new(),
        "links".to_string(),
        format!(" preview   {preview}"),
        format!(" pr        {pr_url}"),
        String::new(),
        "what it does".to_string(),
    ];
    lines.extend(body_lines(detail.body.as_deref(), width));
    lines.push(String::new());
    lines.push("tickets".to_string());
    match row.ticket_ids.as_slice() {
        [] => lines.push(" —".to_string()),
        ticket_ids => {
            for ticket_id in ticket_ids {
                let title = if ticket_ids.len() == 1 {
                    item.and_then(|item| item.ticket_title.as_deref())
                        .unwrap_or("—")
                } else {
                    "—"
                };
                lines.push(format!(" {ticket_id}  {title}"));
            }
        }
    }
    lines.push(String::new());
    lines.push(format!("● review threads  {review_threads}"));
    lines.push(format!("● checks  {checks}"));
    lines
        .into_iter()
        .map(|line| truncate_end(&line, width))
        .collect()
}

fn detail_lines(app: &AppState, row: &DockHomeRow, width: usize) -> Vec<String> {
    if app.work_item_detail_loading.as_ref() == Some(&row.key) {
        return vec!["loading…".to_string()];
    }
    app.work_item_detail_cache
        .get(&row.key)
        .map(|detail| loaded_detail_lines(app, row, detail, width))
        .unwrap_or_else(|| vec!["loading…".to_string()])
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

    if projection.rows.is_empty() {
        render_line(
            frame,
            area,
            area.y,
            "no pr-bound panes".to_string(),
            Style::default().fg(app.palette.subtext0),
        );
        render_line(
            frame,
            area,
            area.y.saturating_add(1),
            "bind: herdr tab create --pr <url> --role review".to_string(),
            Style::default().fg(app.palette.overlay0),
        );
        render_footer(app, &projection, frame, area, area.y.saturating_add(2));
        return;
    }

    let selected = app.dock_home_selected_index(&projection);
    let tab_layouts = tab_layouts(app, &projection, area);
    for (index, tab_area) in tab_layouts {
        let row = &projection.rows[index];
        let is_selected = selected == Some(index);
        let style = if is_selected {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        let label = middle_elide(&tab_label(row), usize::from(tab_area.width));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            tab_area,
        );
    }

    let detail_y = area.y.saturating_add(TAB_ROWS);
    let footer_y = area.bottom().saturating_sub(FOOTER_ROWS).max(detail_y);
    let detail_height = footer_y.saturating_sub(detail_y);
    if let Some(row) = selected.and_then(|index| projection.rows.get(index)) {
        let lines = detail_lines(app, row, usize::from(area.width));
        let max_scroll = lines.len().saturating_sub(usize::from(detail_height));
        let scroll = usize::from(app.dock_scroll).min(max_scroll);
        for (offset, line) in lines
            .into_iter()
            .skip(scroll)
            .take(usize::from(detail_height))
            .enumerate()
        {
            render_line(
                frame,
                area,
                detail_y.saturating_add(offset as u16),
                line,
                Style::default().fg(app.palette.subtext0),
            );
        }
    }
    render_footer(app, &projection, frame, area, footer_y);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use std::time::Duration;

    fn full_detail() -> crate::work_index::WorkItemDetail {
        crate::work_index::WorkItemDetail {
            number: Some(125),
            title: Some("detail panel for every pull request".into()),
            body: Some(
                "<!-- template instructions -->\n## Summary\n- Shows the complete pull request context without leaving Herdr.\n- Keeps long body content readable in the dock."
                    .into(),
            ),
            author: Some("ms".into()),
            base_ref_name: Some("main".into()),
            head_ref_name: Some("feat/pr-detail".into()),
            created_at: SystemTime::now().checked_sub(Duration::from_secs(14 * 86_400)),
            updated_at: SystemTime::now().checked_sub(Duration::from_secs(19 * 60)),
            labels: vec!["high-risk".into(), "risk:large-diff".into()],
            url: Some("https://github.com/herdrdev/herdr/pull/125".into()),
            review_decision: Some("REVIEW_REQUIRED".into()),
            is_draft: Some(false),
            checks: Some(crate::work_index::WorkItemCheckSummary {
                failing: 2,
                total: 8,
            }),
            unresolved_review_threads: None,
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

    fn missing_optional_detail() -> crate::work_index::WorkItemDetail {
        crate::work_index::WorkItemDetail {
            number: None,
            title: None,
            body: None,
            author: None,
            base_ref_name: None,
            head_ref_name: None,
            created_at: None,
            updated_at: None,
            labels: Vec::new(),
            url: None,
            review_decision: None,
            is_draft: None,
            checks: None,
            unresolved_review_threads: None,
            unavailable: None,
            observed_at: SystemTime::now(),
        }
    }

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

    fn bound_app_with_prs(prs: &[(u64, crate::detect::AgentState)]) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = prs
            .iter()
            .map(|(number, _)| crate::workspace::Workspace::test_new(&format!("pr-{number}")))
            .collect();
        app.active = (!app.workspaces.is_empty()).then_some(0);
        app.ensure_test_terminals();
        for (ws_idx, (number, state)) in prs.iter().enumerate() {
            let pane_id = app.workspaces[ws_idx].focused_pane_id().expect("pane");
            let terminal_id = app.workspaces[ws_idx]
                .terminal_id(pane_id)
                .cloned()
                .expect("terminal");
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.detected_agent = Some(crate::detect::Agent::Codex);
            terminal.state = *state;
            terminal
                .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                    pr_urls: Some(vec![format!(
                        "https://github.com/herdrdev/herdr/pull/{number}"
                    )]),
                    work_title: Some(format!("pull request {number}")),
                    ..Default::default()
                })
                .expect("work context");
        }
        app
    }

    fn selected_with_detail(
        mut app: AppState,
        detail: Option<crate::work_index::WorkItemDetail>,
    ) -> AppState {
        app.work_index_enabled = true;
        app.dock_home_focused = false;
        let key = app
            .dock_home_selected_row()
            .expect("selected detail row")
            .key;
        app.dock_home_selection = Some(key.clone());
        match detail {
            Some(detail) => app.work_item_detail_cache.insert(key, detail),
            None => app.work_item_detail_loading = Some(key),
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
    fn renders_one_tab_per_bound_pr_in_canonical_order() {
        let app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
            (3487, crate::detect::AgentState::Blocked),
        ]);
        let projection = app.dock_home_projection();
        assert_eq!(
            projection
                .rows
                .iter()
                .map(|row| row.number.as_str())
                .collect::<Vec<_>>(),
            vec!["129", "128", "3487"]
        );
        let strip = line_text(&render(&app, Rect::new(0, 0, 60, 10)), 0);
        let first = strip.find("● #129").expect("first canonical tab");
        let second = strip.find("○ #128").expect("second canonical tab");
        let third = strip.find("○ #3487").expect("third canonical tab");
        assert!(first < second && second < third, "{strip:?}");
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
        let legend = line_text(&terminal, 5);
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
    fn selected_tab_uses_dock_accent_and_bold_without_a_background() {
        let app = bound_app(true);
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        let cell = &terminal.backend().buffer()[(0, 0)];
        assert_eq!(cell.fg, app.palette.accent);
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(cell.bg, Color::Reset);
    }

    #[test]
    fn tab_hit_areas_match_every_rendered_tab_on_one_row() {
        let app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let projection = app.dock_home_projection();
        let areas = tab_layouts(&app, &projection, Rect::new(4, 7, 30, 10))
            .into_iter()
            .map(|(_, area)| area)
            .collect::<Vec<_>>();
        assert_eq!(areas.len(), projection.rows.len());
        assert_eq!(areas, vec![Rect::new(4, 7, 7, 1), Rect::new(11, 7, 7, 1)]);
    }

    #[test]
    fn renders_full_pull_request_detail_in_ghx_field_order() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 60, 40));
        let text = text(&terminal);
        let overview = text.find("#125  overview").expect("overview");
        let links = text.find("links").expect("links");
        let body = text.find("what it does").expect("body heading");
        let tickets = text.find("tickets").expect("tickets");
        let review = text
            .find("review threads  unknown")
            .expect("review threads");
        let checks = text.find("checks  2 failing of 8").expect("checks");

        assert!(overview < links && links < body && body < tickets);
        assert!(tickets < review && review < checks);
        assert!(text.contains("feat/pr-detail"));
        assert!(text.contains("preview   —"));
        assert!(!text.contains("template instructions"));
    }

    #[test]
    fn optional_detail_fields_render_explicit_unknown_values() {
        let app = selected_with_detail(bound_app(false), Some(missing_optional_detail()));
        let terminal = render(&app, Rect::new(0, 0, 50, 35));
        let text = text(&terminal);

        assert!(text.contains("#—  overview"), "{text:?}");
        assert!(text.contains("opened — · updated —"), "{text:?}");
        assert!(text.contains("review threads  unknown"), "{text:?}");
        assert!(text.contains("checks  —"), "{text:?}");
    }

    #[test]
    fn in_flight_detail_renders_loading_state() {
        let app = selected_with_detail(bound_app(true), None);
        let terminal = render(&app, Rect::new(0, 0, 30, 12));

        assert!(text(&terminal).contains("loading…"));
    }

    #[test]
    fn minimum_dock_width_detail_truncates_without_horizontal_wrap() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let body_width = crate::ui::DOCK_MIN_WIDTH.saturating_sub(2);
        let terminal = render(&app, Rect::new(0, 0, body_width, 40));
        let rendered = text(&terminal);

        for y in 0..terminal.backend().buffer().area.height {
            assert_eq!(
                display_width(&line_text(&terminal, y)),
                usize::from(body_width),
                "line {y} escaped the dock"
            );
        }
        assert!(rendered.contains('…'), "{rendered:?}");
        assert!(!rendered.contains("template instructions"));
    }

    #[test]
    fn ordinary_height_detail_scroll_reaches_checks_and_tickets() {
        let mut app = selected_with_detail(bound_app(true), Some(full_detail()));
        let area = Rect::new(0, 0, 40, 24);
        let initial = text(&render(&app, area));
        assert!(!initial.contains("checks  2 failing of 8"), "{initial:?}");

        app.dock_scroll = u16::MAX;
        let scrolled = text(&render(&app, area));
        assert!(scrolled.contains("tickets"), "{scrolled:?}");
        assert!(scrolled.contains("review threads  unknown"), "{scrolled:?}");
        assert!(scrolled.contains("checks  2 failing of 8"), "{scrolled:?}");
    }

    #[test]
    fn selected_tab_always_renders_its_detail_body() {
        let mut app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let projection = app.dock_home_projection();
        let selected = projection.rows[1].key.clone();
        app.dock_home_selection = Some(selected.clone());
        app.dock_home_focused = false;
        let mut detail = full_detail();
        detail.number = Some(128);
        detail.title = Some("selected pull request".into());
        app.work_item_detail_cache.insert(selected, detail);

        let rendered = text(&render(&app, Rect::new(0, 0, 60, 40)));
        assert!(rendered.contains("#128  overview"), "{rendered:?}");
        assert!(!rendered.contains("#129  overview"), "{rendered:?}");
    }

    #[test]
    fn lifecycle_glyph_changes_do_not_move_tabs() {
        let mut app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Idle),
        ]);
        let area = Rect::new(0, 0, 30, 12);
        let before = line_text(&render(&app, area), 0);
        let before_129 = before.find("#129").expect("#129 tab");
        let before_128 = before.find("#128").expect("#128 tab");

        let terminal_ids = app
            .workspaces
            .iter()
            .map(|workspace| {
                let pane_id = workspace.focused_pane_id().expect("pane");
                workspace.terminal_id(pane_id).cloned().expect("terminal")
            })
            .collect::<Vec<_>>();
        app.terminals
            .get_mut(&terminal_ids[0])
            .expect("first terminal")
            .state = crate::detect::AgentState::Idle;
        app.terminals
            .get_mut(&terminal_ids[1])
            .expect("second terminal")
            .state = crate::detect::AgentState::Working;

        let after = line_text(&render(&app, area), 0);
        assert_eq!(after.find("#129"), Some(before_129));
        assert_eq!(after.find("#128"), Some(before_128));
        assert!(after.contains("○ #129"), "{after:?}");
        assert!(after.contains("● #128"), "{after:?}");
    }

    #[test]
    fn minimum_width_windows_overflow_before_tabs_become_illegible() {
        let mut app = bound_app_with_prs(&[
            (129, crate::detect::AgentState::Working),
            (128, crate::detect::AgentState::Working),
            (3487, crate::detect::AgentState::Blocked),
            (3488, crate::detect::AgentState::Idle),
            (3360, crate::detect::AgentState::Unknown),
            (3359, crate::detect::AgentState::Working),
        ]);
        // The dock's divider and handle consume two columns at DOCK_MIN_WIDTH.
        let body_width = crate::ui::DOCK_MIN_WIDTH.saturating_sub(2);
        let area = Rect::new(0, 0, body_width, 12);
        let projection = app.dock_home_projection();
        let first_layouts = tab_layouts(&app, &projection, area);
        assert_eq!(projection.rows.len(), 6);
        assert_eq!(first_layouts.len(), 5);
        assert_eq!(first_layouts[0].0, 0);

        app.dock_home_selection = projection.rows.last().map(|row| row.key.clone());
        let layouts = tab_layouts(&app, &projection, area);
        assert_eq!(layouts.len(), 5);
        assert_eq!(layouts[0].0, 1);
        assert_eq!(layouts.last().map(|(index, _)| *index), Some(5));
        let terminal = render(&app, area);

        assert_eq!(
            layouts.iter().map(|(_, area)| area.width).sum::<u16>(),
            body_width
        );
        assert!(layouts
            .iter()
            .all(|(_, area)| area.height == 1 && area.width >= MIN_LEGIBLE_TAB_WIDTH));
        for (index, tab_area) in layouts {
            let row = &projection.rows[index];
            let actual = (tab_area.x..tab_area.right())
                .map(|x| terminal.backend().buffer()[(x, tab_area.y)].symbol())
                .collect::<String>();
            let expected = middle_elide(&tab_label(row), usize::from(tab_area.width));
            assert_eq!(actual.trim_end(), expected, "tab #{}", row.number);
            assert_eq!(actual.chars().next(), row.glyph.chars().next());
            assert!(actual.contains('…'));
        }
    }
}
