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
    app::{
        state::{DockHomeSection, WorkItemKey},
        AppState,
    },
    work_index::{WorkItem, WorkItemDetail},
    work_projection::{compact_elapsed, DockHomeProjection, DockHomeRow, DockHomeTicketRow},
};

const TAB_ROWS: u16 = 2;
const FOOTER_ROWS: u16 = 4;
const BODY_LINE_LIMIT: usize = 6;
const MIN_LEGIBLE_TAB_WIDTH: u16 = 3;

fn tab_label(row: &DockHomeRow) -> String {
    format!("{} #{}", row.glyph, row.number)
}

/// A tab keeps one trailing column so neighbouring labels always show a gap:
/// the equal-share allocator can hand a tab exactly its label width, which
/// renders `SCA-2456SCA-2577` with nothing between the two identifiers. At the
/// legibility floor there is no column to spare, and labels are already elided
/// to `●…`, so the ellipsis does the separating instead.
fn label_width(tab_area: Rect) -> usize {
    let width = usize::from(tab_area.width);
    if tab_area.width > MIN_LEGIBLE_TAB_WIDTH {
        width.saturating_sub(1)
    } else {
        width
    }
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
    let tab_bar = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
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

pub(crate) fn ticket_tab_layouts(
    app: &AppState,
    projection: &DockHomeProjection,
    area: Rect,
) -> Vec<(usize, Rect)> {
    if projection.ticket_rows.is_empty() {
        return Vec::new();
    }
    let tab_bar = Rect::new(area.x, area.y.saturating_add(1), area.width, 1);
    let capacity = usize::from((area.width / MIN_LEGIBLE_TAB_WIDTH).max(1));
    let visible_count = projection.ticket_rows.len().min(capacity);
    let selected = app.dock_home_selected_ticket_index(projection).unwrap_or(0);
    let start = selected
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(projection.ticket_rows.len().saturating_sub(visible_count));
    let visible = &projection.ticket_rows[start..start.saturating_add(visible_count)];
    let widths = visible
        .iter()
        .map(|row| {
            u16::try_from(display_width(&row.ticket.identifier).saturating_add(1))
                .unwrap_or(u16::MAX)
        })
        .collect::<Vec<_>>();
    crate::ui::horizontal_tab_hit_areas(tab_bar, &widths)
        .into_iter()
        .enumerate()
        .map(|(offset, area)| (start.saturating_add(offset), area))
        .collect()
}

pub(crate) fn section_layouts(area: Rect) -> [Rect; 2] {
    let row = Rect::new(area.x, area.y, area.width, 1);
    let areas = crate::ui::horizontal_tab_hit_areas(row, &[4, 8]);
    [
        areas.first().copied().unwrap_or_default(),
        areas.get(1).copied().unwrap_or_default(),
    ]
}

fn work_item_for_key<'a>(app: &'a AppState, key: &WorkItemKey) -> Option<&'a WorkItem> {
    app.work_index_snapshot.as_ref()?.items.iter().find(|item| {
        (key.pr_number.is_some()
            && item.pr_number == key.pr_number
            && item.repo.eq_ignore_ascii_case(&key.repo))
            || (key.pr_url.is_some() && item.pr_url == key.pr_url)
            || key.ticket_id.as_ref().is_some_and(|ticket_id| {
                item.ticket_ids
                    .iter()
                    .any(|candidate| candidate == ticket_id)
            })
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

fn status_line(app: &AppState, detail: &WorkItemDetail) -> Line<'static> {
    let mut values = Vec::new();
    if detail.is_draft == Some(true) {
        values.push(Span::styled(
            "draft",
            Style::default().fg(app.palette.overlay0),
        ));
    }
    if let Some(review) = detail
        .review_decision
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        if !values.is_empty() {
            values.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        let color = match review {
            "REVIEW_REQUIRED" => app.palette.peach,
            "APPROVED" => app.palette.green,
            "CHANGES_REQUESTED" => app.palette.red,
            _ => app.palette.text,
        };
        let token = match review {
            "REVIEW_REQUIRED" => "RR".to_string(),
            "APPROVED" => "approved".to_string(),
            "CHANGES_REQUESTED" => "changes requested".to_string(),
            _ => review.to_ascii_lowercase().replace('_', " "),
        };
        values.push(Span::styled(token, Style::default().fg(color)));
    }
    for label in detail.labels.iter().filter(|label| !label.is_empty()) {
        if !values.is_empty() {
            values.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        values.push(Span::styled(
            label.clone(),
            Style::default().fg(app.palette.text),
        ));
    }
    if values.is_empty() {
        Line::from(Span::styled("—", Style::default().fg(app.palette.text)))
    } else {
        Line::from(values)
    }
}

fn heading(app: &AppState, text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(app.palette.overlay0),
    ))
}

fn value_line(app: &AppState, text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(app.palette.text),
    ))
}

fn field_line(
    app: &AppState,
    fields: impl IntoIterator<Item = (String, String, ratatui::style::Color)>,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (label, value, color)) in fields.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(app.palette.overlay0),
            ));
        }
        spans.push(Span::styled(
            label,
            Style::default().fg(app.palette.overlay0),
        ));
        spans.push(Span::styled(value, Style::default().fg(color)));
    }
    Line::from(spans)
}

fn link_color(app: &AppState, value: &str) -> ratatui::style::Color {
    if value == "—" {
        app.palette.text
    } else {
        app.palette.blue
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
) -> Vec<Line<'static>> {
    if let Some(reason) = detail.unavailable.as_deref() {
        return vec![value_line(app, format!("unavailable: {reason}"))];
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
        Line::from(Span::styled(
            format!("#{number}  overview"),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        status_line(app, detail),
        value_line(app, missing(detail.title.as_deref())),
        field_line(
            app,
            [
                ("ticket ".into(), ticket, app.palette.text),
                ("author ".into(), author.into(), app.palette.text),
                ("opened ".into(), opened, app.palette.text),
                ("updated ".into(), updated, app.palette.text),
            ],
        ),
        Line::from(Span::styled(
            missing(detail.head_ref_name.as_deref()).to_string(),
            Style::default().fg(app.palette.mauve),
        )),
        Line::from(vec![
            Span::styled("  → ", Style::default().fg(app.palette.overlay0)),
            Span::styled(
                missing(detail.base_ref_name.as_deref()).to_string(),
                Style::default().fg(app.palette.mauve),
            ),
        ]),
        Line::default(),
        heading(app, "links"),
        field_line(
            app,
            [(
                " preview   ".into(),
                preview.into(),
                link_color(app, preview),
            )],
        ),
        field_line(
            app,
            [(" pr        ".into(), pr_url.into(), link_color(app, pr_url))],
        ),
        Line::default(),
        heading(app, "what it does"),
    ];
    lines.extend(
        body_lines(detail.body.as_deref(), width)
            .into_iter()
            .map(|line| value_line(app, line)),
    );
    lines.push(Line::default());
    lines.push(heading(app, "tickets"));
    match row.ticket_ids.as_slice() {
        [] => lines.push(value_line(app, " —")),
        ticket_ids => {
            for ticket_id in ticket_ids {
                let title = if ticket_ids.len() == 1 {
                    item.and_then(|item| item.ticket_title.as_deref())
                        .unwrap_or("—")
                } else {
                    "—"
                };
                lines.push(value_line(app, format!(" {ticket_id}  {title}")));
            }
        }
    }
    lines.push(Line::default());
    lines.push(field_line(
        app,
        [(
            "● review threads  ".into(),
            review_threads,
            app.palette.text,
        )],
    ));
    let check_color = detail.checks.as_ref().map_or(app.palette.text, |summary| {
        if summary.failing == 0 {
            app.palette.green
        } else {
            app.palette.red
        }
    });
    lines.push(field_line(
        app,
        [("● checks  ".into(), checks, check_color)],
    ));
    lines
}

fn detail_lines(app: &AppState, row: &DockHomeRow, width: usize) -> Vec<Line<'static>> {
    if app.work_item_detail_loading.contains(&row.key) {
        return vec![value_line(app, "loading…")];
    }
    app.work_item_detail_cache
        .get(&row.key)
        .map(|detail| loaded_detail_lines(app, row, detail, width))
        .unwrap_or_else(|| vec![value_line(app, "loading…")])
}

fn ticket_detail_lines(
    app: &AppState,
    row: &DockHomeTicketRow,
    width: usize,
) -> Vec<Line<'static>> {
    let ticket = &row.ticket;
    let now = SystemTime::now();
    let labels = if ticket.labels.is_empty() {
        "—".to_string()
    } else {
        ticket.labels.join(" · ")
    };
    let linear_url = missing(ticket.url.as_deref());
    let pr_url = missing(row.linked_pr_url.as_deref());
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{}  overview", ticket.identifier),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        value_line(app, labels),
        value_line(app, missing(ticket.title.as_deref())),
        field_line(
            app,
            [
                (
                    "state ".into(),
                    missing(ticket.state.as_deref()).into(),
                    app.palette.text,
                ),
                (
                    "assignee ".into(),
                    missing(ticket.assignee.as_deref()).into(),
                    app.palette.text,
                ),
                (
                    "opened ".into(),
                    elapsed_from(ticket.created_at, now),
                    app.palette.text,
                ),
                (
                    "updated ".into(),
                    elapsed_from(ticket.updated_at, now),
                    app.palette.text,
                ),
            ],
        ),
        Line::from(Span::styled(
            missing(ticket.branch.as_deref()).to_string(),
            Style::default().fg(app.palette.mauve),
        )),
        Line::default(),
        heading(app, "links"),
        field_line(
            app,
            [(
                " linear    ".into(),
                linear_url.into(),
                link_color(app, linear_url),
            )],
        ),
        field_line(
            app,
            [(" pr        ".into(), pr_url.into(), link_color(app, pr_url))],
        ),
        Line::default(),
        heading(app, "what it does"),
    ];
    lines.extend(
        body_lines(ticket.description.as_deref(), width)
            .into_iter()
            .map(|line| value_line(app, line)),
    );
    lines.push(Line::default());
    lines.push(heading(app, "parent"));
    lines.push(value_line(
        app,
        format!(" {}", missing(ticket.parent.as_deref())),
    ));
    lines.push(Line::default());
    lines.push(heading(app, "relations"));
    if ticket.relations.is_empty() {
        lines.push(value_line(app, " —"));
    } else {
        lines.extend(
            ticket
                .relations
                .iter()
                .map(|relation| value_line(app, format!(" {relation}"))),
        );
    }
    lines
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

    for (section, label, section_area) in [
        (DockHomeSection::Prs, "prs", section_layouts(area)[0]),
        (
            DockHomeSection::Tickets,
            "tickets",
            section_layouts(area)[1],
        ),
    ] {
        let style = if app.dock_home_section == section {
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.overlay0)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(label, style))),
            section_area,
        );
    }

    let active_empty = match app.dock_home_section {
        DockHomeSection::Prs => projection.rows.is_empty(),
        DockHomeSection::Tickets => projection.ticket_rows.is_empty(),
    };
    if active_empty {
        let reason = if let Some(reason) = projection.unavailable.as_deref() {
            format!("unavailable: {reason}")
        } else {
            match app.dock_home_section {
                DockHomeSection::Prs => "no pr-bound panes",
                DockHomeSection::Tickets if !app.work_index_linear_team_configured => {
                    "tickets off — set work_index.linear_team"
                }
                DockHomeSection::Tickets => "no matching tickets",
            }
            .to_string()
        };
        render_line(
            frame,
            area,
            area.y.saturating_add(TAB_ROWS),
            reason,
            Style::default().fg(app.palette.subtext0),
        );
        if app.dock_home_section == DockHomeSection::Prs {
            render_line(
                frame,
                area,
                area.y.saturating_add(TAB_ROWS + 1),
                "bind: herdr tab create --pr <url> --role review".to_string(),
                Style::default().fg(app.palette.overlay0),
            );
        }
        render_footer(
            app,
            &projection,
            frame,
            area,
            area.y.saturating_add(TAB_ROWS + 2),
        );
        return;
    }

    let selected_pr = app.dock_home_selected_index(&projection);
    let selected_ticket = app.dock_home_selected_ticket_index(&projection);
    match app.dock_home_section {
        DockHomeSection::Prs => {
            for (index, tab_area) in tab_layouts(app, &projection, area) {
                let row = &projection.rows[index];
                let style = if selected_pr == Some(index) {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                let label = middle_elide(&tab_label(row), label_width(tab_area));
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(label, style))),
                    tab_area,
                );
            }
        }
        DockHomeSection::Tickets => {
            for (index, tab_area) in ticket_tab_layouts(app, &projection, area) {
                let row = &projection.ticket_rows[index];
                let style = if selected_ticket == Some(index) {
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.palette.overlay0)
                };
                let label = middle_elide(&row.ticket.identifier, label_width(tab_area));
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(label, style))),
                    tab_area,
                );
            }
        }
    }

    let detail_y = area.y.saturating_add(TAB_ROWS);
    let footer_y = area.bottom().saturating_sub(FOOTER_ROWS).max(detail_y);
    let detail_height = footer_y.saturating_sub(detail_y);
    let lines = match app.dock_home_section {
        DockHomeSection::Prs => selected_pr
            .and_then(|index| projection.rows.get(index))
            .map(|row| detail_lines(app, row, usize::from(area.width))),
        DockHomeSection::Tickets => selected_ticket
            .and_then(|index| projection.ticket_rows.get(index))
            .map(|row| ticket_detail_lines(app, row, usize::from(area.width))),
    };
    if let Some(lines) = lines {
        let max_scroll = lines.len().saturating_sub(usize::from(detail_height));
        let scroll = usize::from(app.dock_scroll).min(max_scroll);
        for (offset, line) in lines
            .into_iter()
            .skip(scroll)
            .take(usize::from(detail_height))
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(
                    area.x,
                    detail_y.saturating_add(offset as u16),
                    area.width,
                    1,
                ),
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
                    ticket_details: Vec::new(),
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

    fn ticket_app(full: bool) -> AppState {
        let mut app = AppState::test_new();
        app.work_index_enabled = true;
        app.work_index_linear_team_configured = true;
        app.dock_home_section = DockHomeSection::Tickets;
        let ticket = crate::work_index::WorkTicket {
            identifier: "SCA-3084".into(),
            title: full.then(|| "ticket detail in the dock".into()),
            description: full.then(|| "A complete Linear description body.".into()),
            state: full.then(|| "In Progress".into()),
            assignee: full.then(|| "Matthias".into()),
            created_at: full.then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(60)),
            updated_at: full.then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(120)),
            branch: full.then(|| "sca-3084-dock-ticket".into()),
            labels: if full {
                vec!["fleet".into()]
            } else {
                Vec::new()
            },
            url: full.then(|| "https://linear.app/scalable/issue/SCA-3084".into()),
            parent: full.then(|| "SCA-3000  dock home".into()),
            relations: if full {
                vec!["blocks  SCA-3090  preload".into()]
            } else {
                Vec::new()
            },
        };
        let mut items = vec![crate::work_index::WorkItem {
            repo: "owner/repo".into(),
            pr_number: None,
            pr_url: None,
            pr_title: None,
            pr_state: None,
            draft: false,
            review_decision: None,
            created_at: None,
            ticket_ids: vec![ticket.identifier.clone()],
            ticket_title: ticket.title.clone(),
            ticket_state: ticket.state.clone(),
            ticket_details: vec![ticket],
            branch: None,
            preview_urls: Vec::new(),
            panes: Vec::new(),
            source: crate::work_index::WorkItemSource::default(),
        }];
        if full {
            items.push(crate::work_index::WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(42),
                pr_url: Some("https://github.com/owner/repo/pull/42".into()),
                pr_title: Some("linked pull request".into()),
                pr_state: Some("open".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                ticket_ids: vec!["SCA-3084".into()],
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: None,
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: crate::work_index::WorkItemSource::default(),
            });
        }
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items,
            unavailable: None,
            observed_at: SystemTime::now(),
        });
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
            None => {
                app.work_item_detail_loading.insert(key);
            }
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

    fn text_position(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
        let area = terminal.backend().buffer().area;
        for y in 0..area.height {
            let line = line_text(terminal, y);
            if let Some(byte_offset) = line.find(needle) {
                let x = display_width(&line[..byte_offset]) as u16;
                return (x, y);
            }
        }
        panic!("missing {needle:?}");
    }

    #[test]
    fn renders_the_empty_state_and_bind_hint() {
        let terminal = render(&AppState::test_new(), Rect::new(0, 0, 30, 10));
        let text = text(&terminal);
        assert!(text.contains("no pr-bound panes"), "{text:?}");
        assert!(text.contains("bind: herdr tab create --pr"), "{text:?}");
    }

    #[test]
    fn section_selector_defaults_to_prs() {
        let app = AppState::test_new();
        assert_eq!(app.dock_home_section, DockHomeSection::Prs);
        let terminal = render(&app, Rect::new(0, 0, 30, 10));
        assert!(line_text(&terminal, 0).starts_with("prs tickets"));
    }

    #[test]
    fn ticket_detail_renders_every_linear_field() {
        let terminal = render(&ticket_app(true), Rect::new(0, 0, 64, 40));
        let rendered = text(&terminal);
        for expected in [
            "SCA-3084  overview",
            "fleet",
            "ticket detail in the dock",
            "state In Progress · assignee Matthias",
            "sca-3084-dock-ticket",
            "https://linear.app/scalable/issue/SCA-3084",
            "https://github.com/owner/repo/pull/42",
            "A complete Linear description body.",
            "SCA-3000  dock home",
            "blocks  SCA-3090  preload",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}: {rendered:?}"
            );
        }
    }

    #[test]
    fn absent_ticket_fields_render_explicit_dashes_without_panicking() {
        let rendered = text(&render(&ticket_app(false), Rect::new(0, 0, 50, 40)));
        assert!(
            rendered.contains("state — · assignee — · opened — · updated —"),
            "{rendered:?}"
        );
        assert!(rendered.contains("linear    —"), "{rendered:?}");
        assert!(rendered.contains("parent"), "{rendered:?}");
        assert!(rendered.contains("relations"), "{rendered:?}");
    }

    #[test]
    fn empty_ticket_states_name_configuration_and_no_matches_separately() {
        let mut app = AppState::test_new();
        app.dock_home_section = DockHomeSection::Tickets;
        let off = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(off.contains("tickets off — set work_index.linear_team"));

        app.work_index_linear_team_configured = true;
        let empty = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(empty.contains("no matching tickets"));
        assert!(!empty.contains("tickets off"));

        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            unavailable: Some("Linear observation timed out".into()),
            observed_at: SystemTime::now(),
        });
        let unavailable = text(&render(&app, Rect::new(0, 0, 50, 12)));
        assert!(
            unavailable.contains("unavailable: Linear observation timed out"),
            "{unavailable:?}"
        );
        assert!(!unavailable.contains("no matching tickets"));
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
        let strip = line_text(&render(&app, Rect::new(0, 0, 60, 10)), 1);
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
        let legend = line_text(&terminal, 7);
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
            ticket_rows: Vec::new(),
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
    fn detail_hierarchy_uses_palette_roles_and_keeps_check_text() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 100, 40));
        let buffer = terminal.backend().buffer();

        let header = text_position(&terminal, "#125  overview");
        assert_eq!(buffer[header].fg, app.palette.accent);
        assert!(buffer[header].modifier.contains(Modifier::BOLD));

        let heading = text_position(&terminal, "links");
        assert_eq!(buffer[heading].fg, app.palette.overlay0);

        let url = text_position(&terminal, "https://github.com/herdrdev/herdr/pull/125");
        assert_eq!(buffer[url].fg, app.palette.blue);

        let checks = text_position(&terminal, "2 failing of 8");
        assert_eq!(buffer[checks].fg, app.palette.red);
        assert!(line_text(&terminal, checks.1).contains("● checks  2 failing of 8"));
    }

    #[test]
    fn review_and_check_states_keep_text_tokens_while_palette_changes() {
        let mut detail = full_detail();
        let app = selected_with_detail(bound_app(true), Some(detail.clone()));
        let terminal = render(&app, Rect::new(0, 0, 100, 40));
        let review_required = text_position(&terminal, "RR");
        assert_eq!(
            terminal.backend().buffer()[review_required].fg,
            app.palette.peach
        );

        detail.review_decision = Some("APPROVED".into());
        detail.checks = Some(crate::work_index::WorkItemCheckSummary {
            failing: 0,
            total: 8,
        });
        let approved_app = selected_with_detail(bound_app(true), Some(detail.clone()));
        let approved = render(&approved_app, Rect::new(0, 0, 100, 40));
        let approved_token = text_position(&approved, "approved");
        assert_eq!(
            approved.backend().buffer()[approved_token].fg,
            approved_app.palette.green
        );
        let passing = text_position(&approved, "0 failing of 8");
        assert_eq!(
            approved.backend().buffer()[passing].fg,
            approved_app.palette.green
        );

        detail.is_draft = Some(true);
        detail.review_decision = None;
        let draft_app = selected_with_detail(bound_app(true), Some(detail));
        let draft = render(&draft_app, Rect::new(0, 0, 100, 40));
        let draft_token = text_position(&draft, "draft");
        assert_eq!(
            draft.backend().buffer()[draft_token].fg,
            draft_app.palette.overlay0
        );
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
        assert_eq!(areas, vec![Rect::new(4, 8, 7, 1), Rect::new(11, 8, 7, 1)]);
    }

    #[test]
    fn renders_full_pull_request_detail_in_ghx_field_order() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let terminal = render(&app, Rect::new(0, 0, 60, 40));
        let text = text(&terminal);
        let overview = text.find("#125  overview").expect("overview");
        let links = text.find("links").expect("links");
        let body = text.find("what it does").expect("body heading");
        let tickets = body
            + text[body..]
                .find("tickets")
                .expect("tickets heading after body");
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
    fn prefetched_entry_renders_immediately_without_loading() {
        let app = selected_with_detail(bound_app(true), Some(full_detail()));
        let rendered = text(&render(&app, Rect::new(0, 0, 60, 40)));

        assert!(rendered.contains("#125  overview"), "{rendered:?}");
        assert!(!rendered.contains("loading…"), "{rendered:?}");
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
        let before = line_text(&render(&app, area), 1);
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

        let after = line_text(&render(&app, area), 1);
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
            let expected = middle_elide(&tab_label(row), label_width(tab_area));
            assert_eq!(actual.trim_end(), expected, "tab #{}", row.number);
            assert_eq!(actual.chars().next(), row.glyph.chars().next());
            assert!(actual.contains('…'));
        }
    }
}
